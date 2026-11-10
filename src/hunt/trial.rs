//! Ensaio persistente: baseline, depois pares de blocos com ordem sorteada.
use super::Hunt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

const DAY: i64 = 86_400;
const TRANSITION: i64 = 6 * 3600;
const PAIRS: i64 = 8;
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS exploration_trial (
 id INTEGER PRIMARY KEY CHECK(id = 1), started_at INTEGER NOT NULL, seed INTEGER NOT NULL,
 base_policy TEXT NOT NULL, blocks_at INTEGER, stopped_reason TEXT, restored INTEGER NOT NULL DEFAULT 0
);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub enabled: bool,
    pub pilot: bool,
    pub block_id: Option<i64>,
    pub phase: String,
    pub base_policy: String,
    pub reason: String,
}

/// Chamado uma vez; reinicializações não apagam encerramento nem mudam a semente.
pub fn start(c: &Connection, now: i64, seed: i64, base_policy: &str) -> rusqlite::Result<()> {
    c.execute("INSERT OR IGNORE INTO exploration_trial(id, started_at, seed, base_policy) VALUES (1, ?1, ?2, ?3)", params![now, seed, base_policy])?;
    Ok(())
}

/// Interrupção persistente; o chamador restaura a política base registrada.
pub fn stop(c: &Connection, reason: &str) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE exploration_trial SET stopped_reason = ?1 WHERE id = 1",
        [reason],
    )?;
    Ok(())
}

fn first_pilot(seed: i64, pair: i64) -> bool {
    let mut x = (seed as u64) ^ (pair as u64).wrapping_mul(0x9e3779b97f4a7c15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58476d1ce4e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d049bb133111eb);
    ((x ^ (x >> 31)) & 1) != 0
}

pub fn info(c: &Connection, now: i64) -> rusqlite::Result<Info> {
    type TrialRow = (i64, i64, String, Option<i64>, Option<String>);
    let row: Option<TrialRow> = c.query_row(
  "SELECT started_at, seed, base_policy, blocks_at, stopped_reason FROM exploration_trial WHERE id = 1", [],
  |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))) .optional()?;
    let Some((_start, seed, base_policy, blocks_at, stopped)) = row else {
        return Ok(Info {
            enabled: false,
            pilot: false,
            block_id: None,
            phase: "paused".into(),
            base_policy: String::new(),
            reason: "ensaio ainda não iniciado".into(),
        });
    };
    if let Some(reason) = stopped {
        return Ok(Info {
            enabled: false,
            pilot: false,
            block_id: None,
            phase: "paused".into(),
            base_policy,
            reason,
        });
    }
    let Some(start) = blocks_at else {
        return Ok(Info {
            enabled: true,
            pilot: false,
            block_id: None,
            phase: "baseline".into(),
            base_policy,
            reason: "aguardando 24 h e 100 tentativas de leitura".into(),
        });
    };
    let elapsed = (now - start).max(0);
    let block = elapsed / DAY;
    if block >= 2 * PAIRS {
        return Ok(Info {
            enabled: false,
            pilot: false,
            block_id: None,
            phase: "complete".into(),
            base_policy,
            reason: "inconclusivo: aguardando maturação, cobertura e validação de memória".into(),
        });
    }
    let pilot = (first_pilot(seed, block / 2) ^ (block % 2 == 1)) && elapsed % DAY >= TRANSITION;
    Ok(Info {
        enabled: true,
        pilot,
        block_id: (elapsed % DAY >= TRANSITION).then_some(block + 1),
        phase: if pilot { "pilot" } else { "control" }.into(),
        base_policy,
        reason: if elapsed % DAY < TRANSITION {
            "transição: primeiras 6 h fora da comparação"
        } else {
            "bloco de comparação"
        }
        .into(),
    })
}

fn advance(c: &Connection, now: i64) -> rusqlite::Result<()> {
    c.execute("UPDATE exploration_trial SET blocks_at = ?1 WHERE id = 1 AND blocks_at IS NULL AND stopped_reason IS NULL AND started_at <= ?1 - 86400 AND (SELECT count(*) FROM research_attempts WHERE at >= started_at AND at <= ?1 AND route IN ('main','alt') AND outcome != 'not_sent') >= 100", [now])?;
    Ok(())
}

pub async fn step(h: &Hunt) -> anyhow::Result<()> {
    let now = h.now();
    // a agenda tem uma política só (`value`); a coluna fica para os ensaios antigos
    let base = "valor".to_owned();
    let (status, restore) = h.db.call(move |c| {
        start(c, now, now ^ 0x51a7_09d3, &base)?;
        advance(c, now)?;
        let bad: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM research_attempts a JOIN exploration_trial t ON t.id=1 WHERE a.at >= t.started_at AND a.at <= ?1 AND a.source='exploracao_externa' AND a.outcome='rate_limited') OR EXISTS(SELECT 1 FROM research_attempts a JOIN exploration_trial t ON t.id=1 WHERE a.at >= t.started_at AND a.at <= ?1 AND a.source='exploracao_externa' AND a.route != 'csfloat' AND a.outcome != 'not_sent' GROUP BY a.decision_id HAVING count(*) > ?2)", params![now, crate::research::MAX_PER_DECISION], |r| r.get(0))?;
        let recent_external: i64 = c.query_row("SELECT count(*) FROM research_attempts WHERE source='exploracao_externa' AND route != 'csfloat' AND outcome != 'not_sent' AND at >= ?1 AND at <= ?2", params![now - crate::research::WINDOW_SECONDS, now], |r| r.get(0))?;
        if bad || recent_external > crate::research::MAX_EXPLORATORY { stop(c, "limite de leitura ou resposta 429 na exploração")?; }
        let status=info(c,now)?;
        let restored: bool=c.query_row("SELECT restored FROM exploration_trial WHERE id=1",[],|r|r.get(0))?;
        Ok((status, !restored))
    }).await?;
    if !status.enabled && restore {
        h.db.call(|c| {
            c.execute("UPDATE exploration_trial SET restored=1 WHERE id=1", [])?;
            Ok(())
        })
        .await?;
    }
    h.lock().trial = Some(status);
    Ok(())
}

/// Contagem separada por bloco: tentativas físicas e anúncios únicos não se multiplicam por JOIN.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: i64,
    pub pilot: bool,
    pub reads: i64,
    pub finds: i64,
    pub predicted_cents: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub blocks: Vec<Block>,
    pub pairs: usize,
    pub mean: Option<f64>,
    pub lower: Option<f64>,
    pub conclusion: String,
    pub reason: String,
}
pub fn report(c: &Connection, now: i64) -> rusqlite::Result<Report> {
    let state: Option<(i64, i64)> = c
        .query_row(
            "SELECT blocks_at, seed FROM exploration_trial WHERE id=1 AND blocks_at IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let mut blocks = Vec::new();
    if let Some((start, seed)) = state {
        for id in 1..=2 * PAIRS {
            if now < start + id * DAY {
                break;
            }
            let reads = c.query_row("SELECT count(*) FROM research_attempts a JOIN research_decisions d ON d.id=a.decision_id WHERE d.block_id=?1 AND a.route IN ('main','alt') AND a.outcome != 'not_sent'", [id], |r| r.get(0))?;
            let (finds,predicted_cents)=c.query_row("SELECT count(*), coalesce(sum(f.profit_cents),0) FROM research_finds f JOIN research_decisions d ON d.id=f.decision_id WHERE d.block_id=?1", [id], |r| Ok((r.get(0)?,r.get(1)?)))?;
            blocks.push(Block {
                id,
                pilot: first_pilot(seed, (id - 1) / 2) ^ ((id - 1) % 2 == 1),
                reads,
                finds,
                predicted_cents,
            });
        }
    }
    let mut differences = Vec::new();
    for pair in blocks.as_chunks::<2>().0 {
        let (a, b) = (&pair[0], &pair[1]);
        if a.reads > 0 && b.reads > 0 {
            let delta = a.finds as f64 / a.reads as f64 - b.finds as f64 / b.reads as f64;
            differences.push(if a.pilot { delta } else { -delta });
        }
    }
    // shortcut: memória compartilhada ainda sem prova, não promove até validar independência por bloco.
    let status = info(c, now)?;
    let safe = status.phase != "paused" || status.reason == "ensaio ainda não iniciado";
    let evidence = evaluate(&differences, 0.0, false, false, false, safe);
    Ok(Report {
        blocks,
        pairs: evidence.pairs,
        mean: evidence.mean,
        lower: evidence.lower,
        conclusion: evidence.conclusion.into(),
        reason: if safe {
            "memória, maturação e cobertura ainda não comprovadas".into()
        } else {
            status.reason
        },
    })
}

/// Estimativa por pares: resultado positivo exige limites operacionais, cobertura e memória comprovados.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    pub pairs: usize,
    pub mean: Option<f64>,
    pub lower: Option<f64>,
    pub conclusion: &'static str,
}
pub fn evaluate(
    differences: &[f64],
    minimum: f64,
    mature: bool,
    covered: bool,
    memory_ok: bool,
    safe: bool,
) -> Evidence {
    let n = differences.len();
    let valid = differences.iter().all(|v| v.is_finite()) && minimum.is_finite();
    let mean = if n > 0 && valid {
        Some(differences.iter().sum::<f64>() / n as f64)
    } else {
        None
    };
    let lower = mean.filter(|_| n >= 8).map(|m| {
        let variance = differences.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (n - 1) as f64;
        // 3 erros padrão é conservador para o mínimo de oito pares.
        m - 3.0 * (variance / n as f64).sqrt()
    });
    let conclusion = if !safe {
        "inseguro"
    } else if mature && covered && memory_ok && lower.is_some_and(|x| x > minimum) {
        "elegivel"
    } else {
        "inconclusivo"
    };
    Evidence {
        pairs: n,
        mean,
        lower,
        conclusion,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn balanced_and_persistent() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        start(&c, 100, 42, "ucb").unwrap();
        start(&c, 200, 7, "harvest").unwrap();
        assert_eq!(info(&c, 100).unwrap().phase, "baseline");
        c.execute("UPDATE exploration_trial SET blocks_at=100", [])
            .unwrap();
        for pair in 0..PAIRS {
            let a = info(&c, 100 + pair * 2 * DAY + TRANSITION).unwrap();
            let b = info(&c, 100 + (pair * 2 + 1) * DAY + TRANSITION).unwrap();
            assert_ne!(a.pilot, b.pilot);
            assert_eq!(a.base_policy, "ucb");
        }
        stop(&c, "limite violado").unwrap();
        start(&c, 300, 8, "x").unwrap();
        assert!(!info(&c, 300).unwrap().enabled);
    }
    #[test]
    fn baseline_requires_both_time_and_real_attempts() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch("CREATE TABLE research_attempts(at INTEGER, route TEXT, outcome TEXT);")
            .unwrap();
        start(&c, 100, 42, "ucb").unwrap();
        for _ in 0..99 {
            c.execute(
                "INSERT INTO research_attempts VALUES (101, 'main', 'ok')",
                [],
            )
            .unwrap();
        }
        advance(&c, 100 + DAY).unwrap();
        assert_eq!(info(&c, 100 + DAY).unwrap().phase, "baseline");
        c.execute(
            "INSERT INTO research_attempts VALUES (101, 'alt', 'ok')",
            [],
        )
        .unwrap();
        advance(&c, 100 + DAY - 1).unwrap();
        assert_eq!(info(&c, 100 + DAY - 1).unwrap().phase, "baseline");
        advance(&c, 100 + DAY).unwrap();
        assert_eq!(info(&c, 100 + DAY).unwrap().phase, "control");
        assert!(info(&c, 100 + DAY).unwrap().block_id.is_none());
    }
    #[test]
    fn report_counts_find_once_even_with_retries_and_never_promotes() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch("CREATE TABLE research_decisions(id INTEGER, block_id INTEGER); CREATE TABLE research_attempts(decision_id INTEGER, route TEXT, outcome TEXT); CREATE TABLE research_finds(decision_id INTEGER, profit_cents INTEGER);").unwrap();
        start(&c, 100, 42, "ucb").unwrap();
        c.execute("UPDATE exploration_trial SET blocks_at=100", [])
            .unwrap();
        c.execute("INSERT INTO research_decisions VALUES (1,1)", [])
            .unwrap();
        c.execute(
            "INSERT INTO research_attempts VALUES (1,'main','ok'), (1,'alt','ok')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO research_finds VALUES (1,500)", [])
            .unwrap();
        let r = report(&c, 100 + 2 * DAY).unwrap();
        assert_eq!(
            (
                r.blocks[0].reads,
                r.blocks[0].finds,
                r.blocks[0].predicted_cents
            ),
            (2, 1, 500)
        );
        assert_eq!(r.conclusion, "inconclusivo");
    }
    #[test]
    fn cannot_promote_without_evidence() {
        let d = [2.0; 8];
        assert_eq!(
            evaluate(&d, 1.0, true, true, true, true).conclusion,
            "elegivel"
        );
        assert_eq!(
            evaluate(&d[..7], 1.0, true, true, true, true).conclusion,
            "inconclusivo"
        );
        assert_eq!(
            evaluate(&d, 1.0, true, true, false, true).conclusion,
            "inconclusivo"
        );
        assert_eq!(
            evaluate(&d, 1.0, true, true, true, false).conclusion,
            "inseguro"
        );
    }
}
