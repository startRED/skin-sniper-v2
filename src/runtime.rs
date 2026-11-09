//! runtime — aplica preferências locais, saldo manual e integrações de terceiros.

use crate::csfloat::CsfloatClient;
use crate::db::{Db, now_unix};
use crate::hunt::Hunt;
use crate::notify::{NotifyError, Telegram};
use crate::settings::SettingsStore;
use crate::steam::client::SteamClient;
use crate::web::api::{ApiError, Event, History, Opportunity, SaldoSource};
use anyhow::Result;
use axum::http::StatusCode;
use rusqlite::{OptionalExtension, params};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// O que já foi aplicado, para só reagir ao que mudou.
#[derive(Default)]
struct Applied {
    csfloat: Option<String>,
    telegram: (Option<String>, Option<String>),
}

/// O que precisa de rede depois de aplicar as configurações.
#[derive(Default, Clone, Copy)]
pub struct Pending {
    telegram: bool,
}

pub struct Runtime {
    pub db: Db,
    pub settings: SettingsStore,
    pub hunt: Arc<Hunt>,
    pub csfloat: Arc<CsfloatClient>,
    pub telegram: Arc<Telegram>,
    applied: Mutex<Applied>,
}

impl Runtime {
    pub fn new(
        db: Db,
        settings: SettingsStore,
        _events: broadcast::Sender<Event>,
        hunt: Arc<Hunt>,
        _steam: Arc<SteamClient>,
        csfloat: Arc<CsfloatClient>,
        telegram: Arc<Telegram>,
    ) -> Arc<Self> {
        Arc::new(Runtime {
            db,
            settings,
            hunt,
            csfloat,
            telegram,
            applied: Mutex::new(Applied::default()),
        })
    }

    /// Aplica as configurações salvas (sem rede) e publica o estado. Devolve o que falta verificar.
    pub async fn apply_local(&self) -> Result<Pending> {
        let csf = self.settings.get("csfloat_key").await?;
        let token = self.settings.get("telegram_token").await?;
        let chat = self.settings.get("telegram_chat_id").await?;
        let mut pending = Pending::default();
        {
            let mut a = self.applied.lock().unwrap();
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

    async fn apply_saldo(&self) -> Result<()> {
        match self.settings.saldo().await? {
            Some(cents) => self.hunt.set_saldo(cents, SaldoSource::Manual),
            None => self.hunt.set_saldo(0, SaldoSource::None),
        }
        Ok(())
    }

    /// Aplica as configurações e verifica em segundo plano o que depende de rede.
    pub async fn settings_changed(self: &Arc<Self>) -> Result<()> {
        let pending = self.apply_local().await?;
        self.spawn_verify(pending);
        Ok(())
    }

    fn spawn_verify(self: &Arc<Self>, p: Pending) {
        if p.telegram {
            let rt = self.clone();
            tokio::spawn(async move { rt.verify_telegram().await });
        }
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
}

#[cfg(test)]
mod tests;
