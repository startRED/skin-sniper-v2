//! `buylisting` (docs/pesquisa-web.md 3.1–3.3): formulário multipart, fluxo `need_confirmation` e erros
//! em português para leigo.
//!
//! ATENÇÃO: este POST nunca foi executado contra a Steam real (conhecimento 1.9). As respostas dos testes
//! são montadas a partir da forma documentada, não capturadas. Nenhum teste faz compra real.
#![allow(dead_code)]

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

struct Cfg {
    /// Saldo conhecido (centavos). `None` = não sei, não bloqueia.
    balance: Option<i64>,
    /// `tradefee_tax`; esperado 0 na conta BR (HIPÓTESE, pesquisa-web 3.1) até que se indique outro valor.
    tradefee_tax: i64,
}

/// Uma compra por vez: o lock é segurado durante todo o fluxo, inclusive as tentativas de confirmação.
pub struct Buyer {
    lock: tokio::sync::Mutex<()>,
    cfg: Mutex<Cfg>,
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
            }),
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

    /// Primeiro envio (`confirmation = "0"`). Recusa sem tocar na rede se o total passa do saldo conhecido.
    pub async fn buy_listing(
        &self,
        client: &SteamClient,
        listing: &Listing,
        wallet_currency: i32,
    ) -> BuyOutcome {
        let _g = self.lock.lock().await;
        self.send_locked(client, listing, wallet_currency, "0")
            .await
    }

    /// Reenvio com o id da confirmação.
    pub async fn retry_confirmed(
        &self,
        client: &SteamClient,
        listing: &Listing,
        wallet_currency: i32,
        id: &str,
    ) -> BuyOutcome {
        let _g = self.lock.lock().await;
        self.send_locked(client, listing, wallet_currency, id).await
    }

    /// Compra e, se pedir confirmação, reenvia a cada 2 s por até 60 s. Segura o lock do começo ao fim.
    pub async fn buy_until_confirmed(
        &self,
        client: &SteamClient,
        listing: &Listing,
        wallet_currency: i32,
    ) -> BuyOutcome {
        let _g = self.lock.lock().await;
        let first = self
            .send_locked(client, listing, wallet_currency, "0")
            .await;
        let clock = TokioClock::new();
        buy_until_confirmed(&clock, first, |id| async move {
            self.send_locked(client, listing, wallet_currency, &id)
                .await
        })
        .await
    }

    async fn send_locked(
        &self,
        client: &SteamClient,
        l: &Listing,
        wallet_currency: i32,
        confirmation: &str,
    ) -> BuyOutcome {
        let (balance, tax) = {
            let c = self.cfg.lock().unwrap();
            (c.balance, c.tradefee_tax)
        };
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
            .multipart(form(l, wallet_currency, tax, &sessionid, confirmation));
        match client.send(Endpoint::Buy, req, true).await {
            Ok((parts, body)) => {
                let out = parse_response(parts.status, &body);
                if let BuyOutcome::Bought {
                    new_balance_cents: Some(nb),
                } = &out
                {
                    self.cfg.lock().unwrap().balance = Some(*nb);
                }
                out
            }
            Err(e) => map_client_error(&e),
        }
    }
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
                None => failed(
                    "A Steam pediu confirmação, mas não mandou o código. Confirme no app da Steam e tente de novo.",
                ),
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
    if (500..600).contains(&status) || (!msg.is_empty() && status != 200) {
        return failed(friendly(msg, status));
    }
    if status == 200 && v.is_some() && !msg.is_empty() {
        return failed(friendly(msg, status));
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

/// Erros conhecidos (pesquisa-web 3.3) em português simples; o resto mostra o texto original.
fn friendly(msg: &str, status: u16) -> String {
    let m = msg.to_lowercase();
    if m.contains("may have been removed") || m.contains("listing") && m.contains("removed") {
        "Esse anúncio já foi vendido ou saiu do ar. Atualize a lista e escolha outro.".into()
    } else if m.contains("previous action completes") {
        "A Steam ainda está processando a compra anterior. Espere alguns segundos e tente de novo."
            .into()
    } else if m.contains("too many purchases without confirmation") {
        "Compras demais sem confirmar. Confirme as pendentes no app da Steam e espere uns 5 minutos.".into()
    } else if m.contains("too many requests") || m.contains("too many") {
        "A Steam pediu para ir mais devagar. Espere um pouco e tente de novo.".into()
    } else if m.contains("not enough") || m.contains("insufficient") || m.contains("funds") {
        "Saldo da carteira Steam insuficiente para esta compra (a taxa também conta).".into()
    } else if m.contains("must be logged in") || m.contains("sign in") || m.contains("logged in") {
        "A sessão da Steam expirou. Cole o cookie de novo nas configurações.".into()
    } else if !msg.is_empty() {
        format!("A Steam recusou a compra: {msg}")
    } else {
        format!(
            "A Steam não respondeu direito (HTTP {status}). O anúncio pode já ter sido vendido; atualize e tente de novo."
        )
    }
}

fn map_client_error(e: &Error) -> BuyOutcome {
    match e {
        Error::RateLimited { .. } => failed("A Steam pediu para ir mais devagar. Nada foi comprado; espere um pouco."),
        Error::BreakerOpen { .. } => failed("As compras estão em pausa depois de falhas seguidas. Nada foi comprado; espere um pouco."),
        Error::Http { status: 403, .. } => failed("A Steam recusou a sessão. Cole o cookie de novo nas configurações."),
        Error::Http { status, .. } => BuyOutcome::Failed { message: friendly("", *status) },
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
        if clock.now() - start >= CONFIRM_TIMEOUT {
            return BuyOutcome::ConfirmTimeout { id };
        }
        clock.sleep(CONFIRM_INTERVAL).await;
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
            BuyOutcome::Failed { .. }
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
        assert!(matches!(out, BuyOutcome::Failed { message } if message.contains("Algo novo")));
        // 502 sem corpo
        assert!(
            matches!(parse_response(502, b""), BuyOutcome::Failed { message } if message.contains("502"))
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
            BuyOutcome::NeedConfirmation { id: "7".into() },
            |_| {
                calls.set(calls.get() + 1);
                async { BuyOutcome::NeedConfirmation { id: "7".into() } }
            },
        )
        .await;
        assert_eq!(out, BuyOutcome::ConfirmTimeout { id: "7".into() });
        assert_eq!(calls.get(), 30);
        // resultado que não é confirmação passa direto, sem esperar
        let out = buy_until_confirmed(
            &clock,
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
    async fn http_502_with_message_is_mapped() {
        let (hits, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![])));
        let base = serve(r#"{"message":"There was a problem purchasing your item. The listing may have been removed."}"#, 502, hits, seen).await;
        let out = Buyer::new()
            .buy_listing(&client(&base), &listing(), 7)
            .await;
        // o cliente descarta o corpo de 5xx: a mensagem genérica de 502 cobre o caso
        assert!(
            matches!(out, BuyOutcome::Failed { message } if message.contains("502") || message.contains("vendido"))
        );
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
