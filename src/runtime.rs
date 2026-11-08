//! runtime — liga as peças: configurações valem na hora (cookie → `SteamClient` + `session::validate` →
//! saldo da carteira; chave CSFloat; Telegram `get_me`), estado das chaves e a compra de um clique.

use crate::csfloat::CsfloatClient;
use crate::db::{Db, now_unix};
use crate::hunt::{Hunt, detect};
use crate::notify::{NotifyError, Telegram};
use crate::settings::SettingsStore;
use crate::steam::buy::{BuyOutcome, Buyer, CONFIRM_TIMEOUT, ListingState, TokioClock};
use crate::steam::client::{Cookie, Endpoint, Error as SteamError, SteamClient};
use crate::steam::listings::Listing;
use crate::steam::session;
use crate::web::api::{
    ApiError, Event, History, Opportunity, Purchase, PurchaseStatus, SaldoSource,
};
use anyhow::Result;
use axum::http::StatusCode;
use rusqlite::{OptionalExtension, params};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

/// A skin comprada fica travada 7 dias (docs/projeto.md, "Precificação").
const TRADE_LOCK_SECS: i64 = 7 * 86_400;
/// Relê a carteira de tempos em tempos: o saldo muda por fora (vendas, depósitos).
const WALLET_REFRESH: Duration = Duration::from_secs(600);
/// Saldo lido da Steam, guardado em `settings` ("id,saldo,moeda,hora") para valer após um reinício.
const WALLET_CACHE_KEY: &str = "steam_wallet_cache";
/// Idade máxima do saldo guardado: além disso a carteira pode ter mudado demais para confiar.
const WALLET_CACHE_MAX_AGE: i64 = 6 * 3600;
const UNCERTAIN: &str =
    "A Steam não confirmou; confira seu inventário e o saldo antes de tentar de novo.";

#[derive(Clone, Copy)]
struct Wallet {
    balance: i64,
    currency: i32,
    /// Quando a Steam respondeu (unix); o ajuste após uma compra não renova.
    read_at: i64,
}

/// SteamID64 do cookie (`<id>%7C%7C<jwt>`): amarra o saldo guardado à conta que o leu.
fn cookie_steamid(cookie: &str) -> &str {
    cookie.split(['%', '|']).next().unwrap_or_default()
}

/// O que já foi aplicado, para só reagir ao que mudou.
#[derive(Default)]
struct Applied {
    cookie: Option<String>,
    csfloat: Option<String>,
    telegram: (Option<String>, Option<String>),
}

/// O que precisa de rede depois de aplicar as configurações.
#[derive(Default, Clone, Copy)]
pub struct Pending {
    steam: bool,
    telegram: bool,
}

pub struct Runtime {
    pub db: Db,
    pub settings: SettingsStore,
    pub events: broadcast::Sender<Event>,
    pub hunt: Arc<Hunt>,
    pub steam: Arc<SteamClient>,
    pub csfloat: Arc<CsfloatClient>,
    pub telegram: Arc<Telegram>,
    pub buyer: Arc<Buyer>,
    applied: Mutex<Applied>,
    wallet: Mutex<Option<Wallet>>,
    /// Uma compra por vez, do pedido até o fim (inclusive a espera no celular e o ajuste do saldo).
    buy_gate: Arc<tokio::sync::Mutex<()>>,
}

/// Cola do navegador costuma trazer o nome do cookie junto.
fn clean_cookie(raw: &str) -> String {
    let v = raw.trim();
    v.strip_prefix("steamLoginSecure=")
        .unwrap_or(v)
        .trim_end_matches(';')
        .to_string()
}

impl Runtime {
    pub fn new(
        db: Db,
        settings: SettingsStore,
        events: broadcast::Sender<Event>,
        hunt: Arc<Hunt>,
        steam: Arc<SteamClient>,
        csfloat: Arc<CsfloatClient>,
        telegram: Arc<Telegram>,
    ) -> Arc<Self> {
        Arc::new(Runtime {
            db,
            settings,
            events,
            hunt,
            steam,
            csfloat,
            telegram,
            buyer: Arc::new(Buyer::new()),
            applied: Mutex::new(Applied::default()),
            wallet: Mutex::new(None),
            buy_gate: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Aplica as configurações salvas (sem rede) e publica o estado. Devolve o que falta verificar.
    pub async fn apply_local(&self) -> Result<Pending> {
        let cookie = self
            .settings
            .get("steam_login_secure")
            .await?
            .map(|c| clean_cookie(&c))
            .filter(|c| !c.is_empty());
        let csf = self.settings.get("csfloat_key").await?;
        let token = self.settings.get("telegram_token").await?;
        let chat = self.settings.get("telegram_chat_id").await?;
        let mut pending = Pending::default();
        {
            let mut a = self.applied.lock().unwrap();
            if a.cookie != cookie {
                self.steam.set_cookie(cookie.clone().map(|c| Cookie {
                    steam_login_secure: c,
                    sessionid: None,
                }));
                self.hunt.lock().history_retry_after.clear();
                *self.wallet.lock().unwrap() = None;
                let set = cookie.is_some();
                self.hunt.set_keys(|k| {
                    k.steam.set = set;
                    k.steam.ok = None;
                    k.steam.detail = set.then(|| "verificando…".to_string());
                });
                pending.steam = set;
                a.cookie = cookie;
            }
            if a.csfloat != csf {
                self.csfloat.set_key(csf.clone());
                let set = csf.is_some();
                self.hunt.set_keys(|k| {
                    k.csfloat.set = set;
                    k.csfloat.ok = None;
                    k.csfloat.detail =
                        set.then(|| "salva; testada na primeira consulta".to_string());
                });
                a.csfloat = csf;
            }
            if a.telegram != (token.clone(), chat.clone()) {
                self.telegram.set_credentials(token.clone(), chat.clone());
                let set = token.is_some();
                self.hunt.set_keys(|k| {
                    k.telegram.set = set;
                    k.telegram.ok = None;
                    k.telegram.detail = set.then(|| "verificando…".to_string());
                });
                pending.telegram = set;
                a.telegram = (token, chat);
            }
        }
        self.apply_saldo().await?;
        self.hunt.publish_state();
        Ok(pending)
    }

    /// Saldo: o manual vale; sem ele, a carteira da Steam; sem ela (ex.: leitura barrada pela pausa de
    /// um 429 logo após reiniciar), o último saldo lido se ainda for recente; senão, zero.
    async fn apply_saldo(&self) -> Result<()> {
        let manual = self.settings.saldo().await?;
        let wallet = *self.wallet.lock().unwrap();
        let id = self
            .steam
            .cookie()
            .map(|c| cookie_steamid(&c.steam_login_secure).to_string());
        if let (Some(w), Some(id)) = (wallet, &id) {
            let v = format!("{id},{},{},{}", w.balance, w.currency, w.read_at);
            self.settings.set_raw(WALLET_CACHE_KEY, Some(v)).await?;
        }
        let cached = match (wallet, &id) {
            // sessão recusada (ok == false) não herda saldo de antes
            (None, Some(id)) if self.hunt.state().keys.steam.ok != Some(false) => {
                self.cached_balance(id).await?
            }
            _ => None,
        };
        match (manual, wallet, cached) {
            (Some(m), ..) => self.hunt.set_saldo(m, SaldoSource::Manual),
            (None, Some(w), _) => self.hunt.set_saldo(w.balance, SaldoSource::Steam),
            (None, None, Some(b)) => self.hunt.set_saldo(b, SaldoSource::Steam),
            (None, None, None) => self.hunt.set_saldo(0, SaldoSource::None),
        }
        Ok(())
    }

    /// Saldo guardado da mesma conta e com no máximo `WALLET_CACHE_MAX_AGE` de idade.
    async fn cached_balance(&self, id: &str) -> Result<Option<i64>> {
        let raw = self
            .settings
            .get(WALLET_CACHE_KEY)
            .await?
            .unwrap_or_default();
        let [cid, balance, _, at] = raw.split(',').collect::<Vec<_>>()[..] else {
            return Ok(None);
        };
        let age = now_unix() - at.parse::<i64>().unwrap_or(0);
        Ok(if cid == id && (0..=WALLET_CACHE_MAX_AGE).contains(&age) {
            balance.parse().ok()
        } else {
            None
        })
    }

    /// Aplica as configurações e verifica em segundo plano o que depende de rede.
    pub async fn settings_changed(self: &Arc<Self>) -> Result<()> {
        let pending = self.apply_local().await?;
        self.spawn_verify(pending);
        Ok(())
    }

    fn spawn_verify(self: &Arc<Self>, p: Pending) {
        if p.steam {
            let rt = self.clone();
            tokio::spawn(async move { rt.verify_session_automatically().await });
        }
        if p.telegram {
            let rt = self.clone();
            tokio::spawn(async move { rt.verify_telegram().await });
        }
    }

    /// Sondas de fundo não devem interromper o descanso de uma saída inteiramente recusada.
    async fn verify_session_automatically(&self) {
        if !(self.steam.main_blocked(Endpoint::History)
            && self.steam.main_blocked(Endpoint::Legacy))
        {
            self.verify_session().await;
        }
    }

    /// Verificação explícita continua disponível para a compra, independentemente da pausa das leituras.
    pub async fn verify_session(&self) {
        let queried = self.steam.cookie();
        if queried.is_none() {
            return;
        }
        let result = session::validate(&self.steam).await;
        if self.steam.cookie().map(|c| c.steam_login_secure)
            != queried.map(|c| c.steam_login_secure)
        {
            return; // o cookie mudou durante a consulta: quem trocou também verifica
        }
        match result {
            Ok(info) => {
                *self.wallet.lock().unwrap() = Some(Wallet {
                    balance: info.wallet_balance_cents,
                    currency: info.wallet_currency,
                    read_at: now_unix(),
                });
                self.buyer.set_tradefee_tax(0);
                let name = info
                    .account_name
                    .unwrap_or_else(|| "conta conectada".into());
                self.hunt.set_keys(|k| {
                    k.steam.ok = Some(true);
                    k.steam.detail = Some(name);
                });
            }
            Err(e) => {
                let (ok, detail) = match &e {
                    SteamError::SessionExpired => (
                        Some(false),
                        "login expirado — escaneie o QR de novo".to_string(),
                    ),
                    SteamError::MarketBlocked => (
                        Some(false),
                        "o login funcionou, mas a Steam não liberou o Mercado para esta conta (confira se ela pode vender e comprar no Mercado, com Steam Guard ativo)".to_string(),
                    ),
                    SteamError::WrongCurrency { .. } => (
                        Some(false),
                        "a carteira dessa conta não está em reais".to_string(),
                    ),
                    other => (None, format!("não consegui verificar: {other}")),
                };
                if ok == Some(false) {
                    *self.wallet.lock().unwrap() = None;
                }
                self.hunt.set_keys(|k| {
                    k.steam.ok = ok;
                    k.steam.detail = Some(detail);
                });
            }
        }
        if let Err(e) = self.apply_saldo().await {
            tracing::warn!("saldo: {e:#}");
        }
        self.hunt.publish_state();
    }

    /// A Steam recusou a renovação: avisa o painel que só um QR novo resolve.
    pub fn mark_login_expired(&self) {
        *self.wallet.lock().unwrap() = None;
        self.hunt.set_keys(|k| {
            k.steam.ok = Some(false);
            k.steam.detail = Some("login expirado — escaneie o QR de novo".to_string());
        });
        self.hunt.publish_state();
    }

    async fn verify_telegram(&self) {
        let (ok, detail) = match self.telegram.get_me().await {
            Ok(b) => (
                Some(true),
                b.username
                    .map_or_else(|| "bot conectado".into(), |u| format!("@{u}")),
            ),
            Err(NotifyError::Refused(_)) => {
                (Some(false), "token recusado pelo Telegram".to_string())
            }
            Err(NotifyError::NotConfigured) => return,
            Err(e) => (None, format!("não consegui verificar: {e}")),
        };
        self.hunt.set_keys(|k| {
            k.telegram.ok = ok;
            k.telegram.detail = Some(detail);
        });
        self.hunt.publish_state();
    }

    /// Relê a carteira a cada 10 min enquanto houver cookie.
    pub fn spawn_wallet_refresh(self: &Arc<Self>) {
        let rt = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(WALLET_REFRESH);
            tick.tick().await; // o primeiro é imediato; a verificação inicial já cuida disso
            loop {
                tick.tick().await;
                rt.verify_session_automatically().await;
            }
        });
    }

    /// Pontos de 30 dias do item (ts, mediana, vendas) e a previsão de venda da oportunidade.
    pub async fn history(&self, id: &str) -> Result<History, ApiError> {
        let key = id.to_string();
        let since = now_unix() - 30 * 86_400;
        let found = self
            .db
            .call(move |c| {
                let Some(json) = c
                    .query_row("SELECT json FROM opportunities WHERE id = ?1", [key], |r| {
                        r.get::<_, String>(0)
                    })
                    .optional()?
                else {
                    return Ok(None);
                };
                let name = serde_json::from_str::<Opportunity>(&json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        e.into(),
                    )
                })?;
                let points = c
                    .prepare(
                        "SELECT ts, median_cents, purchases FROM price_points
                         WHERE name = ?1 AND ts >= ?2 ORDER BY ts",
                    )?
                    .query_map(params![name.name, since], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(Some(History {
                    points,
                    sell: name.sell,
                }))
            })
            .await?;
        found.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "oportunidade desconhecida"))
    }

    // -----------------------------------------------------------------------------------------
    // Compra

    /// `POST /api/buy`: refaz o pedido com os dados guardados do anúncio. Devolve o estado logo depois do
    /// primeiro envio; confirmação no app da Steam termina em segundo plano e chega por evento `purchase`.
    pub async fn buy(self: &Arc<Self>, id: &str) -> Result<Purchase, ApiError> {
        let Ok(gate) = self.buy_gate.clone().try_lock_owned() else {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "termine a compra em andamento primeiro",
            ));
        };
        let pending_id = id.to_string();
        let pending: Option<String> = self.db.call(move |c| {
            c.query_row("SELECT json FROM purchases WHERE id=?1 AND json_extract(json,'$.status') IN ('pending','need_confirmation')", [pending_id], |r| r.get(0)).optional()
        }).await?;
        if let Some(json) = pending {
            let mut purchase: Purchase =
                serde_json::from_str(&json).map_err(anyhow::Error::from)?;
            purchase.message = Some(UNCERTAIN.into());
            return Ok(purchase);
        }
        let key = id.to_string();
        let row: Option<(String, Option<String>)> = self
            .db
            .call(move |c| {
                c.query_row(
                    "SELECT json, listing_json FROM opportunities WHERE id = ?1 AND gone_at IS NULL",
                    [key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
            })
            .await?;
        let Some((json, listing_json)) = row else {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "esse anúncio não está mais na lista",
            ));
        };
        let opp: Opportunity = serde_json::from_str(&json).map_err(anyhow::Error::from)?;
        let listing: Listing = listing_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .ok_or_else(|| anyhow::anyhow!("anúncio {id} sem dados guardados para a compra"))?;
        if self.steam.cookie().is_none() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "para comprar, cole o cookie da Steam nas configurações",
            ));
        }
        // Sessão e moeda da carteira vêm da página do mercado; sem elas a compra nem sai.
        let known = self.wallet.lock().unwrap().map(|w| w.currency);
        let currency = match known {
            Some(c) => c,
            None => {
                self.verify_session().await;
                let again = self.wallet.lock().unwrap().map(|w| w.currency);
                again.ok_or_else(|| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "não consegui validar a sessão da Steam; confira o cookie nas configurações",
                    )
                })?
            }
        };
        self.buyer.set_balance(Some(self.hunt.saldo()));

        let now = now_unix();
        let mut p = Purchase {
            id: opp.id.clone(),
            name: opp.name.clone(),
            icon_url: opp.icon_url.clone(),
            price_cents: opp.price_cents,
            status: PurchaseStatus::Pending,
            message: None,
            bought_at: None,
            tradable_at: None,
            sell: opp.sell.clone(),
            kind: Some(opp.kind),
            sold_cents: None,
        };
        self.save_purchase(&p, now).await?;

        let first = self
            .buyer
            .buy_listing(&self.steam, &listing, currency)
            .await;
        if let BuyOutcome::NeedConfirmation { .. } = &first {
            p.status = PurchaseStatus::NeedConfirmation;
            p.message = Some("Confirme a compra no app da Steam (celular).".into());
            if let Err(e) = self.save_purchase(&p, now).await {
                tracing::error!(
                    "não consegui gravar a confirmação da compra {}: {e:#}",
                    p.id
                );
                p.message = Some("A Steam pediu confirmação, mas o registro local falhou. Confira o app da Steam e o inventário antes de tentar outra compra.".into());
            }
            let _ = self.events.send(Event::Purchase(p.clone()));
            // reenvia com o id a cada 2 s por até 60 s, sem segurar a requisição do painel
            let (rt, mut bg) = (self.clone(), p.clone());
            tokio::spawn(async move {
                let _gate = gate; // solta só depois de confirmar/terminar e atualizar o saldo
                let out = crate::steam::buy::buy_until_confirmed(
                    &TokioClock::new(),
                    CONFIRM_TIMEOUT,
                    first,
                    |cid| {
                        let (rt, listing) = (rt.clone(), listing.clone());
                        async move {
                            rt.buyer
                                .retry_confirmed(&rt.steam, &listing, currency, &cid)
                                .await
                        }
                    },
                )
                .await;
                rt.settle(&mut bg, out).await;
            });
            return Ok(p);
        }
        self.settle(&mut p, first).await;
        Ok(p)
    }

    /// Resultado final (ou intermediário): atualiza a compra, o banco, o saldo e o painel.
    async fn settle(&self, p: &mut Purchase, out: BuyOutcome) {
        match out {
            BuyOutcome::Bought { new_balance_cents } => {
                let now = now_unix();
                p.status = PurchaseStatus::Bought;
                p.message = None;
                p.bought_at = Some(now);
                p.tradable_at = Some(now + TRADE_LOCK_SECS);
                self.after_bought(p, new_balance_cents).await;
            }
            BuyOutcome::NeedConfirmation { .. } => {
                p.status = PurchaseStatus::NeedConfirmation;
                p.message = Some("Confirme a compra no app da Steam (celular).".into());
            }
            BuyOutcome::ConfirmTimeout { .. } => {
                p.status = PurchaseStatus::Pending;
                p.message = Some(
                    "A confirmação no app da Steam não chegou a tempo. Confira o app, seu inventário e o saldo antes de tentar de novo."
                        .into(),
                );
            }
            BuyOutcome::Failed { message } => {
                p.status = PurchaseStatus::Failed;
                p.message = Some(message);
                // recusa conclusiva da Steam (vendido etc.): o anúncio não existe mais
                if self.buyer.state_of(&p.id) == Some(ListingState::Failed) {
                    self.drop_listing(&p.id, crate::web::api::GoneReason::Rejected)
                        .await;
                }
            }
            BuyOutcome::Uncertain { message } => {
                tracing::warn!(id = %p.id, "compra sem resultado confirmado: {message}");
                p.status = PurchaseStatus::Pending;
                p.message = Some(UNCERTAIN.into());
            }
        }
        if let Err(e) = self.save_purchase(p, now_unix()).await {
            tracing::error!("não consegui gravar a compra {}: {e:#}", p.id);
        }
        let _ = self.events.send(Event::Purchase(p.clone()));
    }

    async fn after_bought(&self, p: &Purchase, new_balance: Option<i64>) {
        self.drop_listing(&p.id, crate::web::api::GoneReason::Bought)
            .await;
        // Saldo: manual desconta o que foi pago; da Steam, usa o `wallet_info` da resposta.
        let r = match self.settings.saldo().await {
            Ok(Some(m)) => {
                self.settings
                    .set_saldo(Some((m - p.price_cents).max(0)))
                    .await
            }
            Ok(None) => {
                let nb = new_balance.or_else(|| self.buyer.balance());
                if let Some(w) = self.wallet.lock().unwrap().as_mut() {
                    w.balance = nb.unwrap_or((w.balance - p.price_cents).max(0));
                }
                Ok(())
            }
            Err(e) => Err(e),
        };
        if let Err(e) = r {
            tracing::warn!("saldo depois da compra: {e:#}");
        }
        if let Err(e) = self.apply_saldo().await {
            tracing::warn!("saldo depois da compra: {e:#}");
        }
        self.hunt.recompute_combo();
        self.hunt.publish_state();
    }

    /// Tira o anúncio da lista (comprado ou recusado como vendido).
    async fn drop_listing(&self, id: &str, reason: crate::web::api::GoneReason) {
        if let Err(e) = detect::mark_gone(&self.hunt, vec![id.to_string()], reason).await {
            tracing::warn!("tirar {id} da lista: {e:#}");
        }
        self.hunt.recompute_combo();
        self.hunt.publish_state();
    }

    async fn save_purchase(&self, p: &Purchase, created_at: i64) -> Result<()> {
        let (id, json) = (p.id.clone(), serde_json::to_string(p)?);
        self.db
            .call(move |c| {
                c.execute(
                    "INSERT INTO purchases(id, json, created_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(id) DO UPDATE SET json = excluded.json",
                    params![id, json, created_at],
                )
                .map(|_| ())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
