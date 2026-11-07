//! `buylisting` (docs/pesquisa-web.md 3.1–3.3): formulário multipart, fluxo `need_confirmation` e erros
//! em português para leigo.
//!
//! ATENÇÃO: este POST nunca foi executado contra a Steam real (conhecimento 1.9). As respostas dos testes
//! são montadas a partir da forma documentada, não capturadas. Nenhum teste faz compra real.
#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::Method;
use reqwest::multipart::Form;
use serde_json::Value;

use super::client::{Endpoint, Error, SteamClient, path_escape};
use super::listings::Listing;

/// Reenvio com o id da confirmação: a cada 2 s, por até 60 s (docs/projeto.md, "Compra").
pub const CONFIRM_INTERVAL: Duration = Duration::from_secs(2);
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);
/// Entradas terminais (comprado/recusado) saem do mapa depois disso; `Uncertain` fica até `resolve`.
pub const STATE_RETENTION: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, PartialEq)]
pub enum BuyOutcome {
    /// `wallet_info.success == 1`. `new_balance_cents` vem de `wallet_info.wallet_balance`, se houver.
    Bought { new_balance_cents: Option<i64> },
    /// A Steam pediu confirmação no app (`success 22`); reenvie com este id.
    NeedConfirmation { id: String },
    /// Passou o prazo sem confirmar. A compra ainda pode ser confirmada depois; não reenvie às cegas.
    ConfirmTimeout { id: String },
    /// A Steam recusou ou a compra nem saiu. Mensagem pronta para mostrar.
    Failed { message: String },
    /// O pedido saiu mas não sabemos o resultado (rede caiu, corpo ilegível). Conferir antes de repetir.
    Uncertain { message: String },
}

/// Estado de cada anúncio já tocado. Só `Awaiting` aceita reenvio (com o id da confirmação); os outros
/// recusam um novo POST: um anúncio não é comprado duas vezes por engano.
#[derive(Debug, Clone, PartialEq)]
pub enum ListingState {
    /// POST em andamento.
    InFlight,
    /// A Steam pediu confirmação no app; aguardando reenvio com este id.
    Awaiting {
        id: String,
    },
    Bought,
    /// Recusa conclusiva da Steam (anúncio vendido, etc.).
    Failed,
    /// Não sabemos se comprou (5xx, rede, corpo ilegível, prazo). Só sai por `resolve`.
    Uncertain,
}

struct Cfg {
    /// Saldo conhecido (centavos). `None` = não sei, não bloqueia.
    balance: Option<i64>,
    /// `tradefee_tax`; esperado 0 na conta BR (HIPÓTESE, pesquisa-web 3.1) até que se indique outro valor.
    tradefee_tax: i64,
    /// Estado + quando mudou; entradas terminais (Bought/Failed) velhas são descartadas.
    states: HashMap<String, (ListingState, std::time::Instant)>,
}

impl Cfg {
    fn set(&mut self, id: &str, st: Option<ListingState>, retention: Duration) {
        let now = std::time::Instant::now();
        self.states.retain(|_, (s, at)| {
            !(matches!(s, ListingState::Bought | ListingState::Failed) && now - *at >= retention)
        });
        match st {
            Some(s) => self.states.insert(id.to_string(), (s, now)),
            None => self.states.remove(id),
        };
    }
}

/// Se a operação sair sem resultado (future cancelada, pânico), não deixa o anúncio preso em `InFlight`:
/// com o POST já enviado vira `Uncertain`; antes do envio volta ao estado anterior.
struct InFlightGuard<'a> {
    cfg: &'a Mutex<Cfg>,
    id: String,
    prev: Option<ListingState>,
    retention: Duration,
    sent: bool,
    armed: bool,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut c = self.cfg.lock().unwrap_or_else(|e| e.into_inner());
        let st = if self.sent {
            Some(ListingState::Uncertain)
        } else {
            self.prev.take()
        };
        c.set(&self.id, st, self.retention);
    }
}

/// Uma compra por vez: o lock é segurado durante todo o fluxo, inclusive as tentativas de confirmação.
pub struct Buyer {
    lock: tokio::sync::Mutex<()>,
    cfg: Mutex<Cfg>,
    /// Prazo absoluto de cada operação (espera do limitador + resposta + reenvios).
    timeout: Duration,
    /// Quanto tempo ficam as entradas terminais no mapa.
    retention: Duration,
}

impl Default for Buyer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buyer {
    pub fn new() -> Self {
        Self {
            lock: tokio::sync::Mutex::new(()),
            cfg: Mutex::new(Cfg {
                balance: None,
                tradefee_tax: 0,
                states: HashMap::new(),
            }),
            timeout: CONFIRM_TIMEOUT,
            retention: STATE_RETENTION,
        }
    }

    pub fn set_balance(&self, cents: Option<i64>) {
        self.cfg.lock().unwrap().balance = cents;
    }

    pub fn balance(&self) -> Option<i64> {
        self.cfg.lock().unwrap().balance
    }

    pub fn set_tradefee_tax(&self, cents: i64) {
        self.cfg.lock().unwrap().tradefee_tax = cents.max(0);
    }

    /// Estado do anúncio (para o painel mostrar "confira na Steam").
    pub fn state_of(&self, listing_id: &str) -> Option<ListingState> {
        self.cfg
            .lock()
            .unwrap()
            .states
            .get(listing_id)
            .map(|(s, _)| s.clone())
    }

    /// Reconciliação explícita de um resultado incerto (ou de uma confirmação que estourou o prazo):
    /// depois de conferir inventário/saldo na Steam, diga se comprou. `false` libera nova tentativa.
    /// Vale em `Uncertain` e `Awaiting` (e `InFlight` órfão, se nenhuma compra estiver rodando); devolve se algo mudou.
    pub fn resolve(&self, listing_id: &str, bought: bool) -> bool {
        let idle = self.lock.try_lock().is_ok();
        let mut c = self.cfg.lock().unwrap();
        match c.states.get(listing_id).map(|(s, _)| s) {
            Some(ListingState::Uncertain | ListingState::Awaiting { .. }) => {}
            Some(ListingState::InFlight) if idle => {}
            _ => return false,
        }
        let st = bought.then_some(ListingState::Bought);
        c.set(listing_id, st, self.retention);
        true
    }

    /// Espera a vez até o prazo; `None` = estourou antes de enviar qualquer coisa.
    async fn acquire(
        &self,
        deadline: tokio::time::Instant,
    ) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        tokio::time::timeout_at(deadline, self.lock.lock())
            .await
            .ok()
    }

    /// Primeiro envio (`confirmation = "0"`). Recusa sem tocar na rede se o anúncio já foi tocado ou se
    /// o total passa do saldo conhecido.
    pub async fn buy_listing(
        &self,
        client: &SteamClient,
        listing: &Listing,
        wallet_currency: i32,
    ) -> BuyOutcome {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let Some(_g) = self.acquire(deadline).await else {
            return busy();
        };
        self.send_locked(client, listing, wallet_currency, None, deadline)
            .await
    }

    /// Reenvio com o id da confirmação; só vale para um anúncio em `Awaiting`.
    pub async fn retry_confirmed(
        &self,
        client: &SteamClient,
        listing: &Listing,
        wallet_currency: i32,
        id: &str,
    ) -> BuyOutcome {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let Some(_g) = self.acquire(deadline).await else {
            return busy();
        };
        self.send_locked(client, listing, wallet_currency, Some(id), deadline)
            .await
    }

    /// Compra e, se pedir confirmação, reenvia a cada 2 s. O prazo de 60 s vale para a operação inteira,
    /// incluindo a espera do limitador e a resposta de cada POST. Segura o lock do começo ao fim.
    pub async fn buy_until_confirmed(
        &self,
        client: &SteamClient,
        listing: &Listing,
        wallet_currency: i32,
    ) -> BuyOutcome {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let Some(_g) = self.acquire(deadline).await else {
            return busy();
        };
        let first = self
            .send_locked(client, listing, wallet_currency, None, deadline)
            .await;
        let clock = TokioClock::new();
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        buy_until_confirmed(&clock, remaining, first, |id| async move {
            self.send_locked(client, listing, wallet_currency, Some(&id), deadline)
                .await
        })
        .await
    }

    /// `confirmation`: `None` = primeiro envio ("0"); `Some(id)` = reenvio confirmado.
    async fn send_locked(
        &self,
        client: &SteamClient,
        l: &Listing,
        wallet_currency: i32,
        confirmation: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> BuyOutcome {
        let (balance, tax, state) = {
            let c = self.cfg.lock().unwrap();
            (
                c.balance,
                c.tradefee_tax,
                c.states.get(&l.listing_id).map(|(s, _)| s.clone()),
            )
        };
        match (&state, confirmation) {
            (None, None) | (Some(ListingState::Awaiting { .. }), Some(_)) => {}
            (None, Some(_)) => return failed("Esse anúncio não está esperando confirmação."),
            (Some(ListingState::Awaiting { .. }), None) => {
                return failed("Esse anúncio já está esperando confirmação no app da Steam.");
            }
            (Some(ListingState::InFlight), _) => {
                return failed("Já existe uma compra desse anúncio em andamento.");
            }
            (Some(ListingState::Bought), _) => return failed("Esse anúncio já foi comprado."),
            (Some(ListingState::Failed), _) => {
                return failed(
                    "A Steam já recusou esse anúncio (provavelmente foi vendido). Atualize a lista e escolha outro.",
                );
            }
            (Some(ListingState::Uncertain), _) => {
                return BuyOutcome::Uncertain {
                    message: "O resultado dessa compra não foi confirmado. Confira seu inventário e o saldo na Steam antes de tentar de novo.".into(),
                };
            }
        }
        let total = l.seller_cents + l.fee_cents + tax;
        if l.currency != wallet_currency {
            return failed(
                "A moeda do anúncio é diferente da moeda da sua carteira. Confira a conta antes de comprar.",
            );
        }
        if let Some(b) = balance
            && total > b
        {
            return failed(format!(
                "Saldo insuficiente: a compra custa {} e você tem {}.",
                brl(total),
                brl(b)
            ));
        }
        let Some(sessionid) = client.cookie().and_then(|c| c.sessionid) else {
            return failed(
                "A sessão da Steam ainda não foi validada. Abra as configurações e confira o cookie.",
            );
        };
        let req = client
            .request(
                Method::POST,
                &format!("/market/buylisting/{}", l.listing_id),
            )
            .header(
                "referer",
                format!(
                    "https://steamcommunity.com/market/listings/730/{}",
                    path_escape(&l.name)
                ),
            )
            .header("origin", "https://steamcommunity.com")
            .multipart(form(
                l,
                wallet_currency,
                tax,
                &sessionid,
                confirmation.unwrap_or("0"),
            ));
        let mut guard = InFlightGuard {
            cfg: &self.cfg,
            id: l.listing_id.clone(),
            prev: state.clone(),
            retention: self.retention,
            sent: false,
            armed: true,
        };
        self.cfg
            .lock()
            .unwrap()
            .set(&l.listing_id, Some(ListingState::InFlight), self.retention);
        guard.sent = true;
        // O prazo cobre a espera do limitador e a resposta. Se estourar, o POST pode ter saído: incerto.
        let sent = tokio::time::timeout_at(deadline, client.send(Endpoint::Buy, req, true)).await;
        let (out, new_state) = match sent {
            Err(_) => (
                BuyOutcome::Uncertain {
                    message: "A Steam não respondeu a tempo. Confira seu inventário e o saldo antes de tentar de novo.".into(),
                },
                Some(ListingState::Uncertain),
            ),
            Ok(Ok((parts, body))) => {
                let out = parse_response(parts.status, &body);
                let st = match &out {
                    BuyOutcome::Bought { .. } => Some(ListingState::Bought),
                    BuyOutcome::NeedConfirmation { id } => Some(ListingState::Awaiting { id: id.clone() }),
                    BuyOutcome::Uncertain { .. } => Some(ListingState::Uncertain),
                    // 429: a Steam recusou sem comprar; dá para tentar de novo depois
                    _ if parts.status == 429 => state,
                    _ => Some(ListingState::Failed),
                };
                (out, st)
            }
            Ok(Err(e)) => {
                let out = map_client_error(&e);
                let st = match &out {
                    BuyOutcome::Uncertain { .. } => Some(ListingState::Uncertain),
                    // nada saiu (breaker/limite) ou sessão recusada: volta ao estado de antes
                    _ => state,
                };
                (out, st)
            }
        };
        let mut c = self.cfg.lock().unwrap();
        if let BuyOutcome::Bought { new_balance_cents } = &out {
            // sem saldo novo válido, desconta o total confirmado: a próxima compra não passa com saldo velho
            c.balance = match new_balance_cents {
                Some(nb) => Some(*nb),
                None => c.balance.map(|b| (b - total).max(0)),
            };
        }
        c.set(&l.listing_id, new_state, self.retention);
        guard.armed = false;
        out
    }
}

fn busy() -> BuyOutcome {
    failed(
        "Outra compra está em andamento e o prazo acabou antes de a vez chegar. Nada foi enviado; tente de novo.",
    )
}

fn failed(m: impl Into<String>) -> BuyOutcome {
    BuyOutcome::Failed { message: m.into() }
}

/// Campos de pesquisa-web 3.1: `subtotal = unPrice`, `fee = unFee`, cobrança vazia.
fn form(l: &Listing, currency: i32, tax: i64, sessionid: &str, confirmation: &str) -> Form {
    let total = l.seller_cents + l.fee_cents + tax;
    let mut f = Form::new();
    for (k, v) in form_fields(l, currency, tax, total, sessionid, confirmation) {
        f = f.text(k, v);
    }
    f
}

pub(crate) fn form_fields(
    l: &Listing,
    currency: i32,
    tax: i64,
    total: i64,
    sessionid: &str,
    confirmation: &str,
) -> Vec<(&'static str, String)> {
    let empty = String::new;
    vec![
        ("sessionid", sessionid.to_string()),
        ("currency", currency.to_string()),
        ("subtotal", l.seller_cents.to_string()),
        ("fee", l.fee_cents.to_string()),
        ("total", total.to_string()),
        ("tradefee_tax", tax.to_string()),
        ("quantity", "1".into()),
        ("first_name", empty()),
        ("last_name", empty()),
        ("billing_address", empty()),
        ("billing_address_two", empty()),
        ("billing_country", empty()),
        ("billing_city", empty()),
        ("billing_state", empty()),
        ("billing_postal_code", empty()),
        ("confirmation", confirmation.to_string()),
        ("save_my_address", "0".into()),
    ]
}

/// Interpreta a resposta do `buylisting`. Sucesso só com `wallet_info.success == 1` (HTTP 200).
pub fn parse_response(status: u16, body: &[u8]) -> BuyOutcome {
    let v: Option<Value> = serde_json::from_slice(body).ok();
    let msg = v
        .as_ref()
        .and_then(|v| v.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(v) = &v {
        if v.get("need_confirmation").and_then(Value::as_bool) == Some(true)
            || v.get("success").and_then(Value::as_i64) == Some(22)
        {
            return match confirmation_id(v) {
                Some(id) => BuyOutcome::NeedConfirmation { id },
                None => BuyOutcome::Uncertain {
                    message: "A Steam pediu confirmação sem mandar o código. Confira o app, o inventário e o saldo antes de tentar de novo.".into(),
                },
            };
        }
        if status == 200 && v.pointer("/wallet_info/success").and_then(Value::as_i64) == Some(1) {
            let nb = v.pointer("/wallet_info/wallet_balance").and_then(|b| {
                b.as_i64()
                    .or_else(|| b.as_str().and_then(|s| s.trim().parse().ok()))
            });
            return BuyOutcome::Bought {
                new_balance_cents: nb.filter(|b| *b >= 0),
            };
        }
    }
    if status == 429 {
        return failed("A Steam pediu para ir mais devagar. Espere um pouco e tente de novo.");
    }
    if (500..600).contains(&status) {
        // 5xx só é recusa quando a mensagem é um erro conhecido; senão a compra pode ter saído
        return match known_message(msg) {
            Some(m) => failed(m),
            None => uncertain_5xx(status),
        };
    }
    if !msg.is_empty() && status != 200 {
        return failed(friendly(msg));
    }
    if status == 200 && v.is_some() && !msg.is_empty() {
        return failed(friendly(msg));
    }
    // 200 sem wallet_info ou corpo ilegível: o pedido saiu e não sabemos o que aconteceu.
    BuyOutcome::Uncertain {
        message: "A Steam respondeu de um jeito inesperado. Confira o inventário e o saldo antes de tentar de novo.".into(),
    }
}

fn confirmation_id(v: &Value) -> Option<String> {
    let c = v.get("confirmation")?;
    let id = c.get("confirmation_id").or(Some(c))?;
    id.as_str()
        .map(str::to_string)
        .or_else(|| id.as_i64().map(|n| n.to_string()))
        .filter(|s| !s.is_empty())
}

/// Erros conhecidos (pesquisa-web 3.3) em português simples; `None` para texto que não reconhecemos.
fn known_message(msg: &str) -> Option<String> {
    let m = msg.to_lowercase();
    let s = if m.contains("may have been removed") || m.contains("listing") && m.contains("removed")
    {
        "Esse anúncio já foi vendido ou saiu do ar. Atualize a lista e escolha outro."
    } else if m.contains("previous action completes") {
        "A Steam ainda está processando a compra anterior. Espere alguns segundos e tente de novo."
    } else if m.contains("too many purchases without confirmation") {
        "Compras demais sem confirmar. Confirme as pendentes no app da Steam e espere uns 5 minutos."
    } else if m.contains("too many") {
        "A Steam pediu para ir mais devagar. Espere um pouco e tente de novo."
    } else if m.contains("not enough") || m.contains("insufficient") || m.contains("funds") {
        "Saldo da carteira Steam insuficiente para esta compra (a taxa também conta)."
    } else if m.contains("logged in") || m.contains("sign in") {
        "A sessão da Steam expirou. Cole o cookie de novo nas configurações."
    } else {
        return None;
    };
    Some(s.into())
}

/// Conhecido em português; senão mostra o texto original da Steam.
fn friendly(msg: &str) -> String {
    known_message(msg).unwrap_or_else(|| format!("A Steam recusou a compra: {msg}"))
}

fn uncertain_5xx(status: u16) -> BuyOutcome {
    BuyOutcome::Uncertain {
        message: format!(
            "A Steam não confirmou a compra (HTTP {status}). Confira seu inventário antes de tentar de novo."
        ),
    }
}

fn map_client_error(e: &Error) -> BuyOutcome {
    match e {
        Error::RateLimited { .. } => failed("A Steam pediu para ir mais devagar. Nada foi comprado; espere um pouco."),
        Error::BreakerOpen { .. } => failed("As compras estão em pausa depois de falhas seguidas. Nada foi comprado; espere um pouco."),
        Error::Http { status: 403, .. } => failed("A Steam recusou a sessão. Cole o cookie de novo nas configurações."),
        // 5xx sem corpo útil: a compra pode ter saído
        Error::Http { status, .. } => uncertain_5xx(*status),
        // o pedido pode ter chegado à Steam antes da queda
        Error::Transport(_) => BuyOutcome::Uncertain { message: "A conexão caiu durante a compra. Confira o inventário e o saldo antes de tentar de novo.".into() },
        other => failed(other.to_string()),
    }
}

/// `1234` → `R$ 12,34`.
pub fn brl(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let c = cents.abs();
    format!("{sign}R$ {},{:02}", c / 100, c % 100)
}

// ---------------------------------------------------------------------------------------------
// Laço de confirmação com relógio injetável

pub trait Clock {
    /// Tempo decorrido desde um ponto fixo.
    fn now(&self) -> Duration;
    fn sleep(&self, d: Duration) -> impl Future<Output = ()>;
}

pub struct TokioClock(std::time::Instant);

impl TokioClock {
    pub fn new() -> Self {
        Self(std::time::Instant::now())
    }
}

impl Default for TokioClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TokioClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
    async fn sleep(&self, d: Duration) {
        tokio::time::sleep(d).await;
    }
}

/// Se `first` pede confirmação, reenvia (`retry(id)`) a cada 2 s até 60 s. Qualquer resultado que não
/// seja um novo pedido de confirmação encerra. Passado o prazo devolve `ConfirmTimeout`.
pub async fn buy_until_confirmed<C, F, Fut>(
    clock: &C,
    timeout: Duration,
    first: BuyOutcome,
    mut retry: F,
) -> BuyOutcome
where
    C: Clock,
    F: FnMut(String) -> Fut,
    Fut: Future<Output = BuyOutcome>,
{
    let BuyOutcome::NeedConfirmation { mut id } = first else {
        return first;
    };
    let start = clock.now();
    loop {
        let left = timeout.saturating_sub(clock.now() - start);
        if left.is_zero() {
            return BuyOutcome::ConfirmTimeout { id };
        }
        // o sono nunca passa do prazo; ao acordar sem tempo, não reenvia
        clock.sleep(CONFIRM_INTERVAL.min(left)).await;
        if clock.now() - start >= timeout {
            return BuyOutcome::ConfirmTimeout { id };
        }
        match retry(id.clone()).await {
            BuyOutcome::NeedConfirmation { id: new } => id = new,
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam::client::Cookie;
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Todas as respostas abaixo seguem a FORMA DOCUMENTADA (pesquisa-web 3.1–3.3, conhecimento 1.9), não
    // capturas: o `buylisting` nunca foi executado contra a Steam real. Nenhum teste compra de verdade.
    fn doc(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn listing() -> Listing {
        Listing {
            listing_id: "5276355668".into(),
            asset_id: "54226282359".into(),
            name: "Glock-18 | Moonrise (Field-Tested)".into(),
            price_cents: 588,
            seller_cents: 512,
            fee_cents: 76,
            currency: 7,
            float: Some(0.2),
            seed: Some(59),
            paint_index: Some(694),
            icon_url: String::new(),
            inspect_url: None,
            stickers: vec![],
            keychains: vec![],
            market_tradable_restriction: Some(7),
            market_marketable_restriction: Some(7),
        }
    }

    #[test]
    fn missing_confirmation_code_is_uncertain_not_a_definitive_failure() {
        assert!(matches!(
            parse_response(200, br#"{"need_confirmation":true}"#),
            BuyOutcome::Uncertain { .. }
        ));
        assert!(matches!(
            parse_response(200, br#"{"success":22}"#),
            BuyOutcome::Uncertain { .. }
        ));
    }

    #[test]
    fn success_returns_new_balance() {
        let out = parse_response(200, &doc("steam_buy_success_documented_form.json"));
        assert_eq!(
            out,
            BuyOutcome::Bought {
                new_balance_cents: Some(1000)
            }
        );
    }

    #[test]
    fn need_confirmation_with_string_or_number_id() {
        let out = parse_response(
            200,
            &doc("steam_buy_need_confirmation_documented_form.json"),
        );
        assert_eq!(
            out,
            BuyOutcome::NeedConfirmation {
                id: "9876543210".into()
            }
        );
        let n = br#"{"success":22,"need_confirmation":true,"confirmation":{"confirmation_id":42}}"#;
        assert_eq!(
            parse_response(200, n),
            BuyOutcome::NeedConfirmation { id: "42".into() }
        );
        let no_id = br#"{"success":22,"need_confirmation":true}"#;
        assert!(matches!(
            parse_response(200, no_id),
            BuyOutcome::Uncertain { .. }
        ));
    }

    #[test]
    fn known_errors_become_plain_portuguese() {
        let cases = [
            (
                "There was a problem purchasing your item. The listing may have been removed. Refresh the page and try again.",
                "vendido",
            ),
            (
                "You cannot buy any items until your previous action completes.",
                "processando",
            ),
            (
                "You have too many purchases without confirmation.",
                "Confirme",
            ),
        ];
        for (m, expect) in cases {
            for status in [502, 200] {
                let body = serde_json::json!({ "message": m }).to_string();
                match parse_response(status, body.as_bytes()) {
                    BuyOutcome::Failed { message } => {
                        assert!(message.contains(expect), "{message}")
                    }
                    other => panic!("{other:?}"),
                }
            }
        }
        // mensagem desconhecida mostra o texto original
        let out = parse_response(502, br#"{"message":"Algo novo"}"#);
        // 5xx com texto desconhecido não é recusa conclusiva: a compra pode ter saído
        assert!(matches!(out, BuyOutcome::Uncertain { message } if message.contains("502")));
        // 4xx com texto desconhecido mostra o original
        let out = parse_response(400, br#"{"message":"Algo novo"}"#);
        assert!(matches!(out, BuyOutcome::Failed { message } if message.contains("Algo novo")));
        // 502 sem corpo
        assert!(
            matches!(parse_response(502, b""), BuyOutcome::Uncertain { message } if message.contains("não confirmou"))
        );
    }

    #[test]
    fn unreadable_200_is_uncertain_not_failure() {
        assert!(matches!(
            parse_response(200, b""),
            BuyOutcome::Uncertain { .. }
        ));
        assert!(matches!(
            parse_response(200, b"<html>"),
            BuyOutcome::Uncertain { .. }
        ));
        assert!(matches!(
            parse_response(200, br#"{"wallet_info":{"success":2}}"#),
            BuyOutcome::Uncertain { .. }
        ));
    }

    #[test]
    fn form_follows_documented_fields() {
        let f: std::collections::HashMap<_, _> = form_fields(&listing(), 7, 0, 588, "SID", "0")
            .into_iter()
            .collect();
        assert_eq!(f["subtotal"], "512");
        assert_eq!(f["fee"], "76");
        assert_eq!(f["total"], "588");
        assert_eq!(f["tradefee_tax"], "0");
        assert_eq!(f["quantity"], "1");
        assert_eq!(f["confirmation"], "0");
        assert_eq!(f["currency"], "7");
        assert_eq!(f["sessionid"], "SID");
        for k in [
            "first_name",
            "last_name",
            "billing_address",
            "billing_address_two",
            "billing_country",
            "billing_city",
            "billing_state",
            "billing_postal_code",
        ] {
            assert_eq!(f[k], "", "{k}");
        }
        assert_eq!(f["save_my_address"], "0");
        assert_eq!(f.len(), 17);
    }

    #[test]
    fn brl_format() {
        assert_eq!(brl(588), "R$ 5,88");
        assert_eq!(brl(5), "R$ 0,05");
        assert_eq!(brl(-120), "-R$ 1,20");
    }

    // relógio virtual: dormir só avança o tempo
    struct Fake(Cell<Duration>);
    impl Clock for Fake {
        fn now(&self) -> Duration {
            self.0.get()
        }
        async fn sleep(&self, d: Duration) {
            self.0.set(self.0.get() + d);
        }
    }

    #[tokio::test]
    async fn confirmation_loop_resends_every_2s_then_succeeds() {
        let clock = Fake(Cell::new(Duration::ZERO));
        let calls = Cell::new(0);
        let out = buy_until_confirmed(
            &clock,
            CONFIRM_TIMEOUT,
            BuyOutcome::NeedConfirmation { id: "7".into() },
            |id| {
                assert_eq!(id, "7");
                calls.set(calls.get() + 1);
                let done = calls.get() == 3;
                async move {
                    if done {
                        BuyOutcome::Bought {
                            new_balance_cents: Some(5),
                        }
                    } else {
                        BuyOutcome::NeedConfirmation { id: "7".into() }
                    }
                }
            },
        )
        .await;
        assert_eq!(
            out,
            BuyOutcome::Bought {
                new_balance_cents: Some(5)
            }
        );
        assert_eq!(calls.get(), 3);
        assert_eq!(clock.now(), Duration::from_secs(6));
    }

    #[tokio::test]
    async fn confirmation_loop_times_out_at_60s() {
        let clock = Fake(Cell::new(Duration::ZERO));
        let calls = Cell::new(0);
        let out = buy_until_confirmed(
            &clock,
            CONFIRM_TIMEOUT,
            BuyOutcome::NeedConfirmation { id: "7".into() },
            |_| {
                calls.set(calls.get() + 1);
                async { BuyOutcome::NeedConfirmation { id: "7".into() } }
            },
        )
        .await;
        assert_eq!(out, BuyOutcome::ConfirmTimeout { id: "7".into() });
        // nenhum reenvio no instante do prazo: 2 s, 4 s … 58 s
        assert_eq!(calls.get(), 29);
        assert_eq!(clock.now(), CONFIRM_TIMEOUT);
        // resultado que não é confirmação passa direto, sem esperar
        let out = buy_until_confirmed(
            &clock,
            CONFIRM_TIMEOUT,
            BuyOutcome::Failed {
                message: "x".into(),
            },
            |_| async { unreachable!() },
        )
        .await;
        assert!(matches!(out, BuyOutcome::Failed { .. }));
    }

    type Seen = Arc<Mutex<Vec<(String, String, String, Vec<u8>)>>>;

    async fn serve(body: &'static str, status: u16, hits: Arc<AtomicUsize>, seen: Seen) -> String {
        use axum::{Router, body::Bytes, http::HeaderMap, routing::post};
        let app = Router::new().route(
            "/market/buylisting/{id}",
            post(move |h: HeaderMap, b: Bytes| {
                let (hits, seen) = (hits.clone(), seen.clone());
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let g = |n: &str| {
                        h.get(n)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_string()
                    };
                    seen.lock().unwrap().push((
                        g("referer"),
                        g("origin"),
                        g("content-type"),
                        b.to_vec(),
                    ));
                    (axum::http::StatusCode::from_u16(status).unwrap(), body)
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn client(base: &str) -> SteamClient {
        let c = SteamClient::with_base(base).unwrap();
        c.set_cookie(Some(Cookie {
            steam_login_secure: "x".into(),
            sessionid: Some("SID123".into()),
        }));
        c
    }

    #[tokio::test]
    async fn sends_multipart_with_referer_and_origin_and_updates_balance() {
        let (hits, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![])));
        let base = serve(
            r#"{"wallet_info":{"success":1,"wallet_balance":"412"}}"#,
            200,
            hits.clone(),
            seen.clone(),
        )
        .await;
        let c = client(&base);
        let b = Buyer::new();
        b.set_balance(Some(1000));
        let out = b.buy_listing(&c, &listing(), 7).await;
        assert_eq!(
            out,
            BuyOutcome::Bought {
                new_balance_cents: Some(412)
            }
        );
        assert_eq!(b.balance(), Some(412));
        let (referer, origin, ctype, body) = seen.lock().unwrap()[0].clone();
        assert_eq!(
            referer,
            "https://steamcommunity.com/market/listings/730/Glock-18%20%7C%20Moonrise%20%28Field-Tested%29"
        );
        assert_eq!(origin, "https://steamcommunity.com");
        assert!(ctype.starts_with("multipart/form-data"));
        let text = String::from_utf8_lossy(&body);
        for (k, v) in [
            ("subtotal", "512"),
            ("fee", "76"),
            ("total", "588"),
            ("confirmation", "0"),
            ("sessionid", "SID123"),
        ] {
            assert!(
                text.contains(&format!("name=\"{k}\"\r\n\r\n{v}\r\n")),
                "{k}: {text}"
            );
        }
    }

    #[tokio::test]
    async fn refuses_when_total_exceeds_known_balance_without_network() {
        let (hits, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![])));
        let base = serve("{}", 200, hits.clone(), seen).await;
        let c = client(&base);
        let b = Buyer::new();
        b.set_balance(Some(587));
        let out = b.buy_listing(&c, &listing(), 7).await;
        assert!(
            matches!(out, BuyOutcome::Failed { message } if message.contains("Saldo insuficiente"))
        );
        // moeda diferente da carteira também recusa
        b.set_balance(None);
        assert!(matches!(
            b.buy_listing(&c, &listing(), 1).await,
            BuyOutcome::Failed { .. }
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn without_validated_session_nothing_is_sent() {
        let (hits, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![])));
        let base = serve("{}", 200, hits.clone(), seen).await;
        let c = SteamClient::with_base(&base).unwrap();
        assert!(matches!(
            Buyer::new().buy_listing(&c, &listing(), 7).await,
            BuyOutcome::Failed { .. }
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn confirmation_sleep_never_passes_the_deadline() {
        let clock = Fake(Cell::new(Duration::ZERO));
        let calls = Cell::new(0);
        // sobram 3 s: dorme 2 s, reenvia, dorme só 1 s e para sem reenviar
        let out = buy_until_confirmed(
            &clock,
            Duration::from_secs(3),
            BuyOutcome::NeedConfirmation { id: "7".into() },
            |_| {
                calls.set(calls.get() + 1);
                async { BuyOutcome::NeedConfirmation { id: "7".into() } }
            },
        )
        .await;
        assert_eq!(out, BuyOutcome::ConfirmTimeout { id: "7".into() });
        assert_eq!(calls.get(), 1);
        assert_eq!(clock.now(), Duration::from_secs(3));
    }

    #[tokio::test]
    async fn deadline_starts_at_the_call_and_covers_waiting_for_the_lock() {
        let (hits, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![])));
        let base = serve("{}", 200, hits.clone(), seen).await;
        let c = client(&base);
        let mut b = Buyer::new();
        b.timeout = Duration::from_millis(200);
        let held = b.lock.lock().await; // outra compra segurando a vez
        let t = std::time::Instant::now();
        let out = b.buy_listing(&c, &listing(), 7).await;
        assert!(t.elapsed() < Duration::from_secs(2));
        assert!(
            matches!(out, BuyOutcome::Failed { message } if message.contains("Nada foi enviado"))
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert_eq!(b.state_of("5276355668"), None);
        drop(held);
    }

    #[tokio::test]
    async fn cancelled_future_after_send_leaves_uncertain_and_resolve_accepts_it() {
        use axum::{Router, routing::post};
        let app = Router::new().route(
            "/market/buylisting/{id}",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "{}"
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let c = client(&format!("http://{addr}"));
        let b = Buyer::new();
        let lst = listing();
        // cancela a future no meio do POST
        let r = tokio::time::timeout(Duration::from_millis(300), b.buy_listing(&c, &lst, 7)).await;
        assert!(r.is_err());
        assert_eq!(b.state_of("5276355668"), Some(ListingState::Uncertain));
        assert!(b.resolve("5276355668", false));
        assert_eq!(b.state_of("5276355668"), None);
    }

    #[test]
    fn guard_before_send_restores_previous_state() {
        let b = Buyer::new();
        b.cfg
            .lock()
            .unwrap()
            .set("1", Some(ListingState::InFlight), STATE_RETENTION);
        let g = InFlightGuard {
            cfg: &b.cfg,
            id: "1".into(),
            prev: None,
            retention: STATE_RETENTION,
            sent: false,
            armed: true,
        };
        drop(g);
        assert_eq!(b.state_of("1"), None);
        // InFlight órfão (nenhuma compra rodando) também pode ser resolvido
        b.cfg
            .lock()
            .unwrap()
            .set("2", Some(ListingState::InFlight), STATE_RETENTION);
        assert!(b.resolve("2", true));
        assert_eq!(b.state_of("2"), Some(ListingState::Bought));
    }

    #[test]
    fn old_terminal_entries_are_dropped_but_uncertain_stays() {
        let mut b = Buyer::new();
        b.retention = Duration::ZERO;
        {
            let mut c = b.cfg.lock().unwrap();
            c.set("a", Some(ListingState::Bought), Duration::ZERO);
            c.set("b", Some(ListingState::Failed), Duration::ZERO);
            c.set("u", Some(ListingState::Uncertain), Duration::ZERO);
            c.set("n", Some(ListingState::Failed), Duration::ZERO);
        }
        let c = b.cfg.lock().unwrap();
        assert_eq!(
            c.states.len(),
            2,
            "{:?}",
            c.states.keys().collect::<Vec<_>>()
        );
        assert!(c.states.contains_key("u") && c.states.contains_key("n"));
    }

    #[tokio::test]
    async fn http_502_is_uncertain_and_blocks_a_second_post() {
        let (hits, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![])));
        let base = serve("", 502, hits.clone(), seen).await;
        let c = client(&base);
        let b = Buyer::new();
        let out = b.buy_listing(&c, &listing(), 7).await;
        assert!(
            matches!(&out, BuyOutcome::Uncertain { message } if message.contains("confira") || message.contains("Confira"))
        );
        assert_eq!(b.state_of("5276355668"), Some(ListingState::Uncertain));
        // segundo envio do mesmo anúncio: recusado sem rede
        assert!(matches!(
            b.buy_listing(&c, &listing(), 7).await,
            BuyOutcome::Uncertain { .. }
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // só a reconciliação explícita libera
        assert!(b.resolve("5276355668", false));
        assert_eq!(b.state_of("5276355668"), None);
    }

    #[tokio::test]
    async fn same_listing_is_posted_once_even_when_concurrent_and_after_success() {
        let (hits, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![])));
        let base = serve(
            r#"{"wallet_info":{"success":1,"wallet_balance":"412"}}"#,
            200,
            hits.clone(),
            seen,
        )
        .await;
        let c = client(&base);
        let b = Buyer::new();
        let (l1, l2) = (listing(), listing());
        let (x, y) = tokio::join!(b.buy_listing(&c, &l1, 7), b.buy_listing(&c, &l2, 7));
        let bought = [&x, &y]
            .iter()
            .filter(|o| matches!(o, BuyOutcome::Bought { .. }))
            .count();
        assert_eq!(bought, 1, "{x:?} / {y:?}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(b.state_of("5276355668"), Some(ListingState::Bought));
        assert!(
            matches!(b.buy_listing(&c, &listing(), 7).await, BuyOutcome::Failed { message } if message.contains("já foi comprado"))
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn definitive_refusal_blocks_resend_and_awaiting_accepts_only_retry() {
        let (hits, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![])));
        let base = serve(
            r#"{"message":"The listing may have been removed."}"#,
            400,
            hits.clone(),
            seen,
        )
        .await;
        let c = client(&base);
        let b = Buyer::new();
        assert!(matches!(
            b.buy_listing(&c, &listing(), 7).await,
            BuyOutcome::Failed { .. }
        ));
        assert_eq!(b.state_of("5276355668"), Some(ListingState::Failed));
        assert!(matches!(
            b.buy_listing(&c, &listing(), 7).await,
            BuyOutcome::Failed { .. }
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // retry sem estar esperando confirmação também recusa
        assert!(matches!(
            b.retry_confirmed(&c, &listing(), 7, "1").await,
            BuyOutcome::Failed { .. }
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn success_without_balance_deducts_total_from_known_balance() {
        let (hits, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![])));
        let base = serve(r#"{"wallet_info":{"success":1}}"#, 200, hits, seen).await;
        let c = client(&base);
        let b = Buyer::new();
        b.set_balance(Some(1000));
        assert_eq!(
            b.buy_listing(&c, &listing(), 7).await,
            BuyOutcome::Bought {
                new_balance_cents: None
            }
        );
        assert_eq!(b.balance(), Some(412)); // 1000 - (512 + 76)
        let mut other = listing();
        other.listing_id = "999".into();
        let out = b.buy_listing(&c, &other, 7).await;
        assert!(
            matches!(out, BuyOutcome::Failed { message } if message.contains("Saldo insuficiente"))
        );
    }

    #[tokio::test]
    async fn deadline_covers_the_whole_request_and_leaves_uncertain() {
        use axum::{Router, routing::post};
        let app = Router::new().route(
            "/market/buylisting/{id}",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "{}"
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let c = client(&format!("http://{addr}"));
        let mut b = Buyer::new();
        b.timeout = Duration::from_millis(300);
        let t = std::time::Instant::now();
        let out = b.buy_until_confirmed(&c, &listing(), 7).await;
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(matches!(out, BuyOutcome::Uncertain { .. }), "{out:?}");
        assert_eq!(b.state_of("5276355668"), Some(ListingState::Uncertain));
    }

    #[tokio::test]
    #[ignore = "COMPRA REAL: só à mão, com SKIN_SNIPER_LIVE_BUY=SIM_COMPRAR_DE_VERDADE, STEAM_LOGIN_SECURE e BUY_ITEM"]
    async fn live_buy_cheapest_listing() {
        use crate::steam::{listings, session};
        assert_eq!(
            std::env::var("SKIN_SNIPER_LIVE_BUY").as_deref(),
            Ok("SIM_COMPRAR_DE_VERDADE")
        );
        let c = SteamClient::new().unwrap();
        c.set_cookie(Some(Cookie {
            steam_login_secure: std::env::var("STEAM_LOGIN_SECURE").unwrap(),
            sessionid: None,
        }));
        let info = session::validate(&c).await.unwrap();
        let name = std::env::var("BUY_ITEM")
            .expect("BUY_ITEM = market_hash_name do item mais barato possível");
        let code = listings::resolve_item_code(&c, &name).await.unwrap();
        let page = listings::query_listings(&c, &code, 0, &[]).await.unwrap();
        let l = page
            .listings
            .iter()
            .filter(|l| l.name == name)
            .min_by_key(|l| l.price_cents)
            .expect("sem anúncio");
        let b = Buyer::new();
        b.set_balance(Some(info.wallet_balance_cents));
        println!(
            "{:?}",
            b.buy_until_confirmed(&c, l, info.wallet_currency).await
        );
    }
}
