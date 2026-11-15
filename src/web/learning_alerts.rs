//! Histórico dos avisos e janela fixa de sete dias, sem consultas de mercado.
use super::{AppState, api::ApiError};
use axum::{Json, extract::State, http::StatusCode};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

const DAY: i64 = 86_400;
pub const SCHEMA: &str = r#"
CREATE TABLE alert_window (started_at INTEGER NOT NULL);
INSERT INTO alert_window VALUES (unixepoch());
CREATE TABLE alert_delivery (
 listing_id TEXT NOT NULL, channel TEXT NOT NULL, state TEXT NOT NULL,
 reason TEXT, at INTEGER NOT NULL, PRIMARY KEY(listing_id, channel)
);
INSERT INTO alert_delivery SELECT listing_id, 'windows', 'desconhecido', NULL, bought_at FROM ghosts WHERE approved=1;
INSERT OR IGNORE INTO alert_delivery SELECT id, 'windows', 'desconhecido', NULL, created_at FROM opportunities;
CREATE TABLE alert_forecasts (listing_id TEXT PRIMARY KEY, name TEXT NOT NULL, at INTEGER NOT NULL, price_cents INTEGER NOT NULL, predicted_cents INTEGER NOT NULL, grp TEXT, base_cents INTEGER);
CREATE TABLE alert_outcomes (listing_id TEXT PRIMARY KEY, reason TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE group_sale_windows (name TEXT NOT NULL, fetched_at INTEGER NOT NULL, oldest_at INTEGER NOT NULL, PRIMARY KEY(name,fetched_at));
CREATE TABLE notice_tests (
 id INTEGER PRIMARY KEY, at INTEGER NOT NULL, state TEXT NOT NULL, reason TEXT,
 appeared INTEGER, answered_at INTEGER
);
"#;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub started_at: i64,
    pub closes_at: i64,
    pub rows: Vec<AlertRow>,
    pub test: Option<NoticeTest>,
    pub metrics: Vec<Metric>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRow {
    pub id: String,
    pub name: String,
    pub at: i64,
    pub price_cents: i64,
    pub windows: String,
    pub telegram: String,
    pub listing: String,
    pub group_check: String,
    pub price_check: String,
    pub sales: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoticeTest {
    pub id: i64,
    pub at: i64,
    pub state: String,
    pub reason: Option<String>,
    pub appeared: Option<bool>,
    pub answered_at: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metric {
    pub label: String,
    pub baseline: String,
    pub value: String,
    pub state: String,
}

pub(crate) fn delivery(
    c: &Connection,
    id: &str,
    channel: &str,
    state: &str,
    reason: Option<&str>,
    at: i64,
) -> rusqlite::Result<()> {
    c.execute("INSERT INTO alert_delivery VALUES (?1,?2,?3,?4,?5) ON CONFLICT(listing_id,channel) DO UPDATE SET state=excluded.state,reason=excluded.reason,at=excluded.at", params![id,channel,state,reason,at])?;
    Ok(())
}

fn delivery_text(c: &Connection, id: &str, channel: &str) -> rusqlite::Result<String> {
    Ok(c.query_row(
        "SELECT state, reason FROM alert_delivery WHERE listing_id=?1 AND channel=?2",
        params![id, channel],
        |r| {
            let state: String = r.get(0)?;
            let reason: Option<String> = r.get(1)?;
            Ok(match (state.as_str(), reason) {
                ("aceito", _) if channel == "windows" => "pedido aceito pelo Windows".into(),
                ("aceito", _) => "pedido aceito pelo Telegram".into(),
                ("falhou", Some(reason)) => format!("falhou: {reason}"),
                _ => state,
            })
        },
    )
    .optional()?
    .unwrap_or_else(|| "desconhecido".into()))
}

fn latest_test(c: &Connection) -> rusqlite::Result<Option<NoticeTest>> {
    c.query_row(
        "SELECT id,at,state,reason,appeared,answered_at FROM notice_tests ORDER BY id DESC LIMIT 1",
        [],
        |r| {
            Ok(NoticeTest {
                id: r.get(0)?,
                at: r.get(1)?,
                state: r.get(2)?,
                reason: r.get(3)?,
                appeared: r.get(4)?,
                answered_at: r.get(5)?,
            })
        },
    )
    .optional()
}

/// Só transações reais; sumiço não entra como venda. Normalização USD/base → BRL/base em inteiros.
fn group_sales(
    c: &Connection,
    name: &str,
    group: Option<&str>,
    base: Option<i64>,
    at: i64,
) -> rusqlite::Result<Vec<i64>> {
    let Some(group) = group else {
        return Ok(vec![]);
    };
    c.prepare("SELECT price_usd_cents,base_usd_cents FROM csfloat_sales WHERE name=?1 AND grp=?2 AND sold_at>?3 AND sold_at<=?4 AND price_usd_cents>0 AND base_usd_cents>0")?
        .query_map(params![name,group,at,at+2*DAY], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?)))?
        .map(|r| r.map(|(price, usd_base)| base.filter(|b| *b>0).map_or(0, |brl_base| (i128::from(price)*i128::from(brl_base)/i128::from(usd_base)).min(i128::from(i64::MAX)) as i64)))
        .collect()
}

pub(crate) fn report(c: &Connection, now: i64) -> rusqlite::Result<Report> {
    let start: i64 = c.query_row("SELECT started_at FROM alert_window", [], |r| r.get(0))?;
    let end = start + 7 * DAY;
    let mut rows = Vec::new();
    let mut mature = 0;
    let mut with_sales = 0;
    let mut priced = 0;
    let mut sustained = 0;
    let mut alerts = 0;
    let mut unknown = 0;
    let mut stmt = c.prepare("WITH ids AS (SELECT listing_id AS id FROM ghosts WHERE approved=1 UNION SELECT id FROM opportunities UNION SELECT listing_id FROM alert_forecasts)
      SELECT ids.id,COALESCE(f.name,g.name,json_extract(p.json,'$.name')),COALESCE(f.at,g.bought_at,p.created_at),
      COALESCE(f.price_cents,g.price_cents,json_extract(p.json,'$.price_cents')),COALESCE(f.grp,g.grp),COALESCE(f.base_cents,g.base_cents),COALESCE(f.predicted_cents,g.mid_price_cents),
      o.last_seen,o.gone_at,COALESCE(a.reason,json_extract(p.json,'$.gone_reason')),COALESCE(a.at,p.gone_at),f.listing_id IS NOT NULL
      FROM ids LEFT JOIN ghosts g ON g.listing_id=ids.id AND g.approved=1 LEFT JOIN opportunities p ON p.id=ids.id
      LEFT JOIN alert_forecasts f ON f.listing_id=ids.id LEFT JOIN observations o ON o.listing_id=ids.id LEFT JOIN alert_outcomes a ON a.listing_id=ids.id
      ORDER BY 3 DESC,1")?;
    let raw = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<i64>>(6)?,
            r.get::<_, Option<i64>>(7)?,
            r.get::<_, Option<i64>>(8)?,
            r.get::<_, Option<String>>(9)?,
            r.get::<_, Option<i64>>(10)?,
            r.get::<_, bool>(11)?,
        ))
    })?;
    for raw in raw {
        let (
            id,
            name,
            at,
            price_cents,
            group,
            base,
            predicted,
            last_seen,
            gone,
            reason,
            ended,
            requested,
        ) = raw?;
        let windows = delivery_text(c, &id, "windows")?;
        let telegram = delivery_text(c, &id, "telegram")?;
        let listing = if reason.as_deref() == Some("unprofitable") {
            "deixou de compensar".into()
        } else if let Some(gone) = gone.or_else(|| {
            (reason.as_deref() == Some("unavailable"))
                .then_some(ended)
                .flatten()
        }) {
            format!(
                "saiu do ar em {} min (comprado ou retirado)",
                (gone - at).max(0) / 60
            )
        } else if reason.is_none() && last_seen.is_some_and(|seen| seen > at) {
            "ainda anunciado".into()
        } else {
            "desconhecido (faixa não foi relida)".into()
        };
        let mut sales = group_sales(c, &name, group.as_deref(), base, at)?;
        let ready = now >= at + 2 * DAY;
        let covered: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM group_sale_windows WHERE name=?1 AND fetched_at>=?2 AND oldest_at<=?3)", params![name, at+2*DAY, at], |r|r.get(0))?;
        let group_check = if !ready {
            "ainda sem dados"
        } else if !sales.is_empty() {
            "o grupo teve venda"
        } else if group.is_some() && covered {
            "não teve"
        } else {
            "ainda sem dados"
        };
        sales.sort_unstable();
        let median = sales.get(sales.len() / 2).copied().map(|upper| {
            if sales.len() % 2 == 0 {
                i128::from(upper) + i128::from(sales[sales.len() / 2 - 1])
            } else {
                i128::from(upper) * 2
            }
        });
        let price_check = if !ready
            || !covered
            || sales.len() < 3
            || base.is_none()
            || predicted.is_none_or(|p| p <= 0)
        {
            "sem prova de preço"
        } else if median.unwrap_or(0) * 5 >= i128::from(predicted.unwrap_or(0)) * 9 {
            "sustenta"
        } else {
            "não sustenta"
        };
        if requested && at >= start && at < end {
            alerts += 1;
            unknown += i64::from(windows == "desconhecido");
            if ready && group_check != "ainda sem dados" {
                mature += 1;
                with_sales += i64::from(!sales.is_empty());
            }
            if price_check != "sem prova de preço" {
                priced += 1;
                sustained += i64::from(price_check == "sustenta");
            }
        }
        rows.push(AlertRow {
            id,
            name,
            at,
            price_cents,
            windows,
            telegram,
            listing,
            group_check: group_check.into(),
            price_check: price_check.into(),
            sales: if ready { sales.len() as i64 } else { 0 },
        });
    }
    let mut arm = [(0_i64, 0_i64); 2];
    for (control, slot) in arm.iter_mut().enumerate() {
        *slot=c.query_row("SELECT count(*),COALESCE(sum(fresh),0) FROM reads WHERE at>=?1 AND at<?2 AND arm LIKE 'r:%' AND gap>0 AND control=?3",params![start,end,control as i64],|r|Ok((r.get(0)?,r.get(1)?)))?;
    }
    let mut daily = Vec::new();
    for day in 0..7 {
        daily.push(c.query_row("SELECT COALESCE(sum(fresh),0) FROM reads WHERE at>=?1 AND at<?2 AND arm LIKE 'r:%' AND gap>0",params![start+day*DAY,start+(day+1)*DAY],|r|r.get::<_,i64>(0))?);
    }
    daily.sort_unstable();
    let new: i64 = c.query_row(
        "SELECT count(*) FROM observations WHERE first_seen>=?1 AND first_seen<?2",
        params![start, end],
        |r| r.get(0),
    )?;
    let unknown_age: i64 = c.query_row("SELECT count(*) FROM observations WHERE first_seen>=?1 AND first_seen<?2 AND age_max IS NULL", params![start,end], |r|r.get(0))?;
    let hours = (now.min(end) - start).max(0) as f64 / 3600.0;
    let rate = if hours > 0.0 { new as f64 / hours } else { 0.0 };
    let test = latest_test(c)?;
    let screen_answer: Option<bool> = c.query_row("SELECT appeared FROM notice_tests WHERE answered_at IS NOT NULL AND at>=?1 ORDER BY answered_at DESC,id DESC LIMIT 1", [start], |r|r.get(0)).optional()?;
    let mut metrics = Vec::new();
    let mut add =
        |label: &str, baseline: &str, value: String, enough: bool, passes: bool, closed: bool| {
            metrics.push(Metric {
                label: label.into(),
                baseline: baseline.into(),
                value,
                state: if now <= start {
                    "não medido"
                } else if !enough {
                    "inconclusivo"
                } else if !closed {
                    "em acompanhamento"
                } else if passes {
                    "cumprido"
                } else {
                    "não cumprido"
                }
                .into(),
            });
        };
    let rates = arm.map(|(n, f)| {
        if n > 0 {
            format!("{:.1}% ({f}/{n})", 100.0 * f as f64 / n as f64)
        } else {
            "não medido".into()
        }
    });
    add(
        "Raros novos no saldo por leitura",
        "5% agenda × 36% rodízio",
        format!("{} agenda × {} rodízio", rates[0], rates[1]),
        arm.iter().all(|a| a.0 > 0),
        i128::from(arm[0].1) * i128::from(arm[1].0) >= i128::from(arm[1].1) * i128::from(arm[0].0),
        now >= end,
    );
    add(
        "Raros novos no saldo por dia",
        "12 a 43/dia; precisa passar de 43",
        if now >= end {
            format!("mediana {} em 7 dias", daily[3])
        } else {
            "aguardando 7 dias completos".into()
        },
        now >= end,
        daily[3] > 43,
        now >= end,
    );
    add(
        "Estado dos avisos e teste de tela",
        "sem registro por canal",
        format!(
            "{unknown} desconhecidos em {alerts} avisos; teste {}",
            screen_answer.map_or("sem resposta", |yes| if yes {
                "apareceu"
            } else {
                "não apareceu"
            })
        ),
        alerts > 0,
        unknown == 0 && screen_answer == Some(true),
        now >= end,
    );
    add(
        "Avisos por dia",
        "14 em 9 dias (~1,6/dia); meta 12 em 7 dias",
        format!("{alerts} em 7 dias"),
        true,
        alerts >= 12,
        now >= end,
    );
    add(
        "Conferência de 48 h",
        "3 de 14; só avisos com 48 h",
        format!(
            "{with_sales} de {mature} com venda do grupo{}",
            if mature < 10 { "; amostra pequena" } else { "" }
        ),
        mature >= 10,
        with_sales * 14 > mature * 3,
        now >= end + 2 * DAY,
    );
    add(
        "Anúncios novos lidos por hora",
        "8 a 12/h; precisa passar de 12",
        format!(
            "{rate:.1}/h ({new} anúncios; {unknown_age} sem idade conhecida, podem incluir estoque da primeira leitura)"
        ),
        hours > 0.0 && unknown_age == 0,
        rate > 12.0,
        now >= end,
    );
    add(
        "Preço nas vendas do grupo em 48 h",
        "sem prova de preço na base",
        format!("{sustained} de {priced} sustentam; mínimo 5 avisos com 3 vendas cada"),
        priced >= 5,
        sustained * 2 > priced,
        now >= end + 2 * DAY,
    );
    Ok(Report {
        started_at: start,
        closes_at: end + 2 * DAY,
        rows,
        test,
        metrics,
    })
}

pub async fn test_notice(State(app): State<AppState>) -> Result<Json<NoticeTest>, ApiError> {
    let now = crate::db::now_unix();
    let id = app
        .db
        .call(move |c| {
            if let Some(previous) = latest_test(c)?
                && now - previous.at < 15
            {
                return Ok(None);
            }
            c.execute(
                "INSERT INTO notice_tests(at,state) VALUES (?1,'pendente')",
                [now],
            )?;
            Ok(Some(c.last_insert_rowid()))
        })
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "Aguarde 15 segundos antes de testar de novo.",
            )
        })?;
    let result = crate::notify::desktop(
        "Skin Sniper · teste de aviso",
        "Se apareceu, confirme no painel.",
        "http://localhost:8788/#aprendizado",
    )
    .await;
    let (state, reason) = match result {
        Ok(true) => ("pedido aceito pelo Windows", None),
        Ok(false) => (
            "falhou",
            Some("Aviso do Windows indisponível neste sistema.".to_string()),
        ),
        Err(reason) => ("falhou", Some(reason)),
    };
    app.db
        .call(move |c| {
            c.execute(
                "UPDATE notice_tests SET state=?2,reason=?3 WHERE id=?1",
                params![id, state, reason],
            )?;
            latest_test(c)
        })
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "Teste não encontrado."))
}
#[derive(Deserialize)]
pub struct Answer {
    id: i64,
    appeared: bool,
}
pub async fn answer(
    State(app): State<AppState>,
    Json(input): Json<Answer>,
) -> Result<Json<NoticeTest>, ApiError> {
    let now = crate::db::now_unix();
    let changed=app.db.call(move |c| c.execute("UPDATE notice_tests SET appeared=?2,answered_at=?3 WHERE id=?1 AND id=(SELECT MAX(id) FROM notice_tests) AND state!='pendente' AND answered_at IS NULL",params![input.id,input.appeared,now])).await?;
    if changed == 0 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Este teste já foi respondido ou ainda está sendo enviado.",
        ));
    }
    Ok(Json(app.db.call(|c| latest_test(c)).await?.ok_or_else(
        || ApiError::new(StatusCode::NOT_FOUND, "Teste não encontrado."),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_answer_is_dated_single_use_and_saved_on_disk() {
        let (db, dir) = crate::db::temp_db();
        let app = AppState::new(db.clone(), crate::settings::SettingsStore::new(db.clone()));
        let sent = test_notice(State(app.clone()))
            .await
            .map_err(|e| e.1)
            .unwrap()
            .0;
        assert_eq!(sent.state, "pedido aceito pelo Windows");
        let saved = answer(
            State(app.clone()),
            Json(Answer {
                id: sent.id,
                appeared: false,
            }),
        )
        .await
        .map_err(|e| e.1)
        .unwrap()
        .0;
        assert_eq!(saved.appeared, Some(false));
        assert!(saved.answered_at.is_some());
        assert!(
            answer(
                State(app),
                Json(Answer {
                    id: sent.id,
                    appeared: true
                })
            )
            .await
            .is_err()
        );
        let conn = Connection::open(dir.path().join("t.db")).unwrap();
        assert_eq!(latest_test(&conn).unwrap().unwrap().appeared, Some(false));
    }

    #[tokio::test]
    async fn acceptance_minimums_and_missing_coverage_do_not_claim_improvement() {
        let (db, _dir) = crate::db::temp_db();
        db.call(|c| {
            c.execute("UPDATE alert_window SET started_at=1000", [])?;
            for i in 0..12 {
                c.execute("INSERT INTO ghosts(listing_id,name,kind,grp,bought_at,price_cents,mid_price_cents,mid_net_cents,base_cents,approved) VALUES (?1,?1,'raro','g',1001,100,200,170,100,1)",[i.to_string()])?;
            }
            c.execute("INSERT INTO alert_forecasts SELECT listing_id,name,bought_at,price_cents,mid_price_cents,grp,base_cents FROM ghosts",[])?;
            let close=1000+9*DAY;
            let r=report(c,close)?;
            assert_eq!(r.metrics[3].state,"cumprido");
            assert!(r.rows.iter().all(|r|r.group_check=="ainda sem dados"));
            for i in 0..9 {c.execute("INSERT INTO group_sale_windows VALUES (?1,?2,1000)",params![i.to_string(),close])?;}
            assert!(report(c,close)?.metrics[4].value.contains("amostra pequena"));
            c.execute("INSERT INTO group_sale_windows VALUES ('9',?1,1000)",[close])?;
            assert_eq!(report(c,close)?.metrics[4].state,"não cumprido");
            for i in 0..4 { for at in 1100..1103 {
                c.execute("INSERT INTO csfloat_sales(name,sold_at,price_usd_cents,base_usd_cents,multiple,grp) VALUES (?1,?2,180,100,1.8,'g')",params![i.to_string(),at])?;
            }}
            assert_eq!(report(c,close)?.metrics[6].state,"inconclusivo");
            for at in 1100..1103 {c.execute("INSERT INTO csfloat_sales(name,sold_at,price_usd_cents,base_usd_cents,multiple,grp) VALUES ('4',?1,180,100,1.8,'g')",[at])?;}
            assert_eq!(report(c,close)?.metrics[6].state,"cumprido");
            c.execute("DELETE FROM ghosts WHERE listing_id='11'",[])?; c.execute("DELETE FROM alert_forecasts WHERE listing_id='11'",[])?;
            assert_eq!(report(c,close)?.metrics[3].state,"não cumprido");
            assert!(report(c,1001+2*DAY-1)?.rows.iter().all(|r|r.group_check=="ainda sem dados"));
            c.execute("DELETE FROM csfloat_sales WHERE sold_at=1102",[])?;
            assert!(report(c,close)?.rows.iter().all(|r|r.price_check=="sem prova de preço"));
            Ok(())
        }).await.unwrap();
    }
    #[tokio::test]
    async fn window_empty_and_old_unknown_are_honest() {
        let (db, _dir) = crate::db::temp_db();
        db.call(|c| {c.execute("UPDATE alert_window SET started_at=1000",[])?;
            c.execute("INSERT INTO ghosts(listing_id,name,kind,bought_at,price_cents,mid_price_cents,mid_net_cents,approved) VALUES ('old','skin','raro',900,100,200,170,1)",[])?;
            let r=report(c,1000)?;assert_eq!(r.metrics.len(),7);assert!(r.metrics.iter().all(|m|m.state=="não medido"));
            assert_eq!(r.rows[0].windows,"desconhecido");assert_eq!(r.rows[0].listing,"desconhecido (faixa não foi relida)");
            let r=report(c,1000+9*DAY)?;assert!(r.metrics[4].value.contains("amostra pequena"));assert_eq!(r.metrics[6].state,"inconclusivo");Ok(())}).await.unwrap();
    }
    #[tokio::test]
    async fn real_sales_price_threshold_and_notifications_survive_reads() {
        let (db, _dir) = crate::db::temp_db();
        db.call(|c| {c.execute("UPDATE alert_window SET started_at=1000",[])?;
            c.execute("INSERT INTO ghosts(listing_id,name,kind,grp,bought_at,price_cents,mid_price_cents,mid_net_cents,base_cents,approved) VALUES ('a','skin','raro','g',1001,100,200,170,100,1)",[])?;
            c.execute("INSERT INTO group_sale_windows VALUES ('skin',?1,1000)",[1001+2*DAY])?;
            for at in 1002..1005 { c.execute("INSERT INTO csfloat_sales(name,sold_at,price_usd_cents,base_usd_cents,multiple,grp) VALUES ('skin',?1,180,100,1.8,'g')",[at])?; }
            delivery(c,"a","windows","falhou",Some("desativado"),1001)?;
            let r=report(c,1001+2*DAY)?;assert_eq!(r.rows[0].windows,"falhou: desativado");assert_eq!(r.rows[0].group_check,"o grupo teve venda");assert_eq!(r.rows[0].price_check,"sustenta");
            c.execute("UPDATE csfloat_sales SET price_usd_cents=179",[])?;
            assert_eq!(report(c,1001+2*DAY)?.rows[0].price_check,"não sustenta");
            assert_eq!(report(c,1002)?.rows[0].group_check,"ainda sem dados");Ok(())}).await.unwrap();
    }
}
