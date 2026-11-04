//! settings — chaves e preferências na tabela `settings`. Para o painel, segredos saem mascarados
//! (`••••` + 4 últimos); valor nunca vai para o log. Mudança de chave avisa quem assinou.

use crate::db::Db;
use crate::web::api::{Settings, SettingsInput};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::fmt;
use tokio::sync::broadcast;

const SALDO_KEY: &str = "saldo_manual_cents";
const DEFAULT_MIN_PROFIT: i64 = 100;
const MASK: &str = "••••";

/// Entrada recusada (vira 400 na API).
#[derive(Debug)]
pub struct Invalid(pub &'static str);

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Invalid {}

fn mask(v: &str) -> String {
    let n = v.chars().count();
    // Valor curto não mostra nada: 4 caracteres seria o segredo inteiro.
    if n <= 8 {
        MASK.to_string()
    } else {
        format!("{MASK}{}", v.chars().skip(n - 4).collect::<String>())
    }
}

#[derive(Clone)]
pub struct SettingsStore {
    db: Db,
    changed: broadcast::Sender<String>,
}

impl SettingsStore {
    pub fn new(db: Db) -> Self {
        SettingsStore {
            db,
            changed: broadcast::channel(32).0,
        }
    }

    /// Recebe o nome de cada chave que mudou (ex.: `"steam_login_secure"`, `"saldo_manual_cents"`).
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.changed.subscribe()
    }

    /// Valor cru, para os módulos que usam a chave de verdade. Nunca repasse ao painel nem ao log.
    pub async fn get(&self, key: &'static str) -> Result<Option<String>> {
        self.db
            .call(move |c| {
                c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                    r.get(0)
                })
                .optional()
            })
            .await
    }

    pub async fn view(&self) -> Result<Settings> {
        let rows: Vec<(String, String)> = self
            .db
            .call(|c| {
                c.prepare("SELECT key, value FROM settings")?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect()
            })
            .await?;
        let get = |k: &str| {
            rows.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        Ok(Settings {
            steam_login_secure: get("steam_login_secure").map(|v| mask(&v)),
            csfloat_key: get("csfloat_key").map(|v| mask(&v)),
            telegram_token: get("telegram_token").map(|v| mask(&v)),
            telegram_chat_id: get("telegram_chat_id"),
            panel_url: get("panel_url"),
            sound: get("sound").is_none_or(|v| v == "true"),
            notify_min_profit_cents: get("notify_min_profit_cents")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MIN_PROFIT),
        })
    }

    pub async fn apply(&self, input: SettingsInput) -> Result<Settings> {
        if input
            .notify_min_profit_cents
            .flatten()
            .is_some_and(|c| c < 0)
        {
            return Err(Invalid("notify_min_profit_cents não pode ser negativo").into());
        }
        let text = |v: Option<Option<String>>| {
            v.map(|o| o.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        };
        let fields: [(&'static str, Option<Option<String>>); 7] = [
            ("steam_login_secure", text(input.steam_login_secure)),
            ("csfloat_key", text(input.csfloat_key)),
            ("telegram_token", text(input.telegram_token)),
            ("telegram_chat_id", text(input.telegram_chat_id)),
            ("panel_url", text(input.panel_url)),
            ("sound", input.sound.map(|o| o.map(|b| b.to_string()))),
            (
                "notify_min_profit_cents",
                input
                    .notify_min_profit_cents
                    .map(|o| o.map(|c| c.to_string())),
            ),
        ];
        // O painel devolve segredos mascarados; salvar a máscara de volta destruiria a chave.
        let writes: Vec<_> = fields
            .into_iter()
            .filter_map(|(k, v)| Some((k, v?)))
            .filter(|(_, v)| !v.as_deref().is_some_and(|s| s.starts_with(MASK)))
            .collect();
        let keys: Vec<&'static str> = writes.iter().map(|(k, _)| *k).collect();
        self.db
            .call(move |c| {
                let tx = c.transaction()?;
                for (k, v) in writes {
                    match v {
                        Some(v) => tx.execute(
                            "INSERT INTO settings(key, value) VALUES (?1, ?2)
                             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                            params![k, v],
                        )?,
                        None => tx.execute("DELETE FROM settings WHERE key = ?1", [k])?,
                    };
                }
                tx.commit()
            })
            .await?;
        for k in keys {
            let _ = self.changed.send(k.to_string());
        }
        self.view().await
    }

    pub async fn saldo(&self) -> Result<Option<i64>> {
        Ok(self.get(SALDO_KEY).await?.and_then(|v| v.parse().ok()))
    }

    /// `None` apaga o saldo manual (volta a ler da Steam).
    pub async fn set_saldo(&self, cents: Option<i64>) -> Result<()> {
        self.db
            .call(move |c| match cents {
                Some(v) => c.execute(
                    "INSERT INTO settings(key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![SALDO_KEY, v.to_string()],
                ),
                None => c.execute("DELETE FROM settings WHERE key = ?1", [SALDO_KEY]),
            })
            .await?;
        let _ = self.changed.send(SALDO_KEY.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::temp_db;

    fn input(json: &str) -> SettingsInput {
        serde_json::from_str(json).unwrap()
    }

    #[tokio::test]
    async fn masks_secrets_and_null_clears() {
        let (db, _dir) = temp_db();
        let s = SettingsStore::new(db);
        let mut rx = s.subscribe();

        let v = s
            .apply(input(
                r#"{"csfloat_key":"abcdefgh1234gLO9","telegram_chat_id":"42","sound":false}"#,
            ))
            .await
            .unwrap();
        assert_eq!(v.csfloat_key.as_deref(), Some("••••gLO9"));
        assert_eq!(v.telegram_chat_id.as_deref(), Some("42"));
        assert!(!v.sound);
        assert_eq!(v.notify_min_profit_cents, 100);
        assert_eq!(rx.recv().await.unwrap(), "csfloat_key");
        // Valor cru continua disponível para os módulos.
        assert_eq!(
            s.get("csfloat_key").await.unwrap().as_deref(),
            Some("abcdefgh1234gLO9")
        );

        // Salvar a máscara de volta não destrói a chave; campo ausente não mexe.
        s.apply(input(r#"{"csfloat_key":"••••gLO9"}"#))
            .await
            .unwrap();
        assert_eq!(
            s.get("csfloat_key").await.unwrap().as_deref(),
            Some("abcdefgh1234gLO9")
        );

        // null apaga e volta ao padrão.
        let v = s
            .apply(input(
                r#"{"csfloat_key":null,"sound":null,"notify_min_profit_cents":250}"#,
            ))
            .await
            .unwrap();
        assert_eq!(v.csfloat_key, None);
        assert!(v.sound);
        assert_eq!(v.notify_min_profit_cents, 250);
        assert!(s.get("csfloat_key").await.unwrap().is_none());
    }

    #[test]
    fn short_secret_hides_everything_and_debug_has_no_values() {
        assert_eq!(mask("abc"), "••••");
        assert_eq!(mask("123456789"), "••••6789");
        let dbg = format!("{:?}", input(r#"{"telegram_token":"SEGREDO-SEGREDO"}"#));
        assert!(!dbg.contains("SEGREDO"));
    }

    #[tokio::test]
    async fn saldo_roundtrip_and_validation() {
        let (db, _dir) = temp_db();
        let s = SettingsStore::new(db);
        assert_eq!(s.saldo().await.unwrap(), None);
        s.set_saldo(Some(3608)).await.unwrap();
        assert_eq!(s.saldo().await.unwrap(), Some(3608));
        s.set_saldo(None).await.unwrap();
        assert_eq!(s.saldo().await.unwrap(), None);
        let err = s
            .apply(input(r#"{"notify_min_profit_cents":-1}"#))
            .await
            .unwrap_err();
        assert!(err.downcast_ref::<Invalid>().is_some());
    }
}
