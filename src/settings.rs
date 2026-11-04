//! settings — chaves e preferências na tabela `settings`. Para o painel, segredos saem mascarados
//! (`••••` + 4 últimos); valor nunca vai para o log. Mudança de chave avisa quem assinou.

use crate::db::Db;
use crate::web::api::{Settings, SettingsInput};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::fmt;
use tokio::sync::broadcast;

const SALDO_KEY: &str = "saldo_manual_cents";
/// Token de renovação do login por QR (interno; o painel nunca o vê).
pub const REFRESH_KEY: &str = "steam_refresh_token";
const COOKIE_KEY: &str = "steam_login_secure";
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

    #[cfg(test)]
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
        let mut keys: Vec<&'static str> = writes.iter().map(|(k, _)| *k).collect();
        // Cookie trocado ou apagado à mão: o refresh token da conta antiga não pode sobreviver e
        // restaurar o login velho na próxima renovação.
        let drop_refresh = keys.contains(&COOKIE_KEY);
        if drop_refresh {
            keys.push(REFRESH_KEY);
        }
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
                if drop_refresh {
                    tx.execute("DELETE FROM settings WHERE key = ?1", [REFRESH_KEY])?;
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
        self.set_raw(SALDO_KEY, cents.map(|c| c.to_string())).await
    }

    /// Grava várias chaves internas numa transação, só se `guard` ainda vale (a chave guardada tem o
    /// valor esperado). `false` = nada foi gravado. Uso: renovação que não pode sobrescrever outra conta.
    pub async fn write_if(
        &self,
        guard: (&'static str, Option<String>),
        writes: Vec<(&'static str, Option<String>)>,
    ) -> Result<bool> {
        let keys: Vec<_> = writes.iter().map(|(k, _)| *k).collect();
        let done = self
            .db
            .call(move |c| {
                let tx = c.transaction()?;
                let now: Option<String> = tx
                    .query_row(
                        "SELECT value FROM settings WHERE key = ?1",
                        [guard.0],
                        |r| r.get(0),
                    )
                    .optional()?;
                if now != guard.1 {
                    return Ok(false);
                }
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
                tx.commit()?;
                Ok(true)
            })
            .await?;
        if done {
            for k in keys {
                let _ = self.changed.send(k.to_string());
            }
        }
        Ok(done)
    }

    /// Grava (ou apaga, com `None`) uma chave interna que o painel não edita, como o token de renovação.
    pub async fn set_raw(&self, key: &'static str, value: Option<String>) -> Result<()> {
        self.db
            .call(move |c| match value {
                Some(v) => c.execute(
                    "INSERT INTO settings(key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![key, v],
                ),
                None => c.execute("DELETE FROM settings WHERE key = ?1", [key]),
            })
            .await?;
        let _ = self.changed.send(key.to_string());
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
    async fn manual_cookie_change_or_clear_drops_refresh_token_atomically() {
        let (db, _dir) = temp_db();
        let s = SettingsStore::new(db);
        for json in [
            r#"{"steam_login_secure":"outra-conta"}"#,
            r#"{"steam_login_secure":null}"#,
        ] {
            s.set_raw(REFRESH_KEY, Some("refresh-da-conta-antiga".into()))
                .await
                .unwrap();
            s.apply(input(json)).await.unwrap();
            assert!(s.get(REFRESH_KEY).await.unwrap().is_none(), "{json}");
        }
        // mexer em outra chave, ou salvar a máscara de volta, não apaga o refresh token
        s.set_raw(REFRESH_KEY, Some("r".into())).await.unwrap();
        s.apply(input(r#"{"sound":false,"steam_login_secure":"••••abcd"}"#))
            .await
            .unwrap();
        assert_eq!(s.get(REFRESH_KEY).await.unwrap().as_deref(), Some("r"));
    }

    #[tokio::test]
    async fn write_if_only_writes_when_guard_holds() {
        let (db, _dir) = temp_db();
        let s = SettingsStore::new(db);
        s.set_raw(REFRESH_KEY, Some("a".into())).await.unwrap();
        let w = vec![("steam_login_secure", Some("x".to_string()))];
        assert!(
            !s.write_if((REFRESH_KEY, Some("b".into())), w.clone())
                .await
                .unwrap()
        );
        assert!(s.get("steam_login_secure").await.unwrap().is_none());
        assert!(
            s.write_if((REFRESH_KEY, Some("a".into())), w)
                .await
                .unwrap()
        );
        assert_eq!(
            s.get("steam_login_secure").await.unwrap().as_deref(),
            Some("x")
        );
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
