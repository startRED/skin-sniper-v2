//! Fila de consultas por prioridade e o laço que puxa no ritmo do limitador do `SteamClient`.
//!
//! A fila é trabalho-conservadora: o limitador (1 consulta a cada ≥ 1,5 s) é o gargalo, então o laço sempre
//! pega a tarefa de menor vencimento, sem esperar. O intervalo de cada tarefa funciona como peso: nota maior
//! (e acertos recentes) vencem mais cedo e são consultadas mais vezes. A descoberta divide a mesma vaga:
//! cada consulta de vigilância rende 0,25 de crédito e cada passo de descoberta gasta 1 (≈ 20 % do total);
//! enquanto há poucas tarefas (arranque), a descoberta pode usar tudo.

use super::{Hunt, catalog, detect};
use crate::steam::client::Error as SteamError;
use crate::steam::listings::{self, PropFilter};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Intervalo-base entre consultas de uma tarefa de nota média; vira só um peso quando o limitador satura.
const BASE_INTERVAL_MS: i64 = 30_000;
/// Crédito de descoberta por consulta de vigilância: 1 passo a cada 4 consultas = 20 % do total.
pub const DISCOVERY_SHARE: f64 = 0.25;
/// Com menos tarefas que isto a descoberta tem prioridade (primeiro ciclo).
pub const BOOT_TASKS: usize = 10;
const CREDIT_CAP: f64 = 4.0;

/// Uma consulta recorrente: `QueryListingsForItem` do código, com desgaste e/ou faixa de seed.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    /// `d:<market_hash_name>` (desconto) ou `r:<código>:<de>-<até>` (raro).
    pub id: String,
    /// Nome do item (desconto) ou da skin sem desgaste (raro): para log e para filtrar a resposta.
    pub name: String,
    pub code: String,
    pub wear: Option<u8>,
    pub seeds: Option<(u16, u16)>,
    /// Nota-base (sem acertos).
    pub score: f64,
}

impl Task {
    pub fn filters(&self) -> Vec<PropFilter> {
        self.seeds
            .map(|(lo, hi)| PropFilter::seed(lo, hi))
            .into_iter()
            .collect()
    }
}

struct Entry {
    task: Task,
    due_ms: i64,
    hits: u32,
}

#[derive(Default)]
pub struct Queue {
    entries: HashMap<String, Entry>,
    credit: f64,
}

impl Queue {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn eff(e: &Entry) -> f64 {
        e.task.score.max(0.01) * (1.0 + f64::from(e.hits))
    }

    /// Troca o conjunto de tarefas: quem continua mantém vencimento e acertos; quem entra vence agora.
    /// Devolve os ids das tarefas que saíram.
    pub fn set_tasks(
        &mut self,
        tasks: Vec<Task>,
        hits: &HashMap<String, u32>,
        now_ms: i64,
    ) -> Vec<String> {
        let keep: std::collections::HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        let removed: Vec<String> = self
            .entries
            .keys()
            .filter(|k| !keep.contains(k.as_str()))
            .cloned()
            .collect();
        for k in &removed {
            self.entries.remove(k);
        }
        for t in tasks {
            let h = hits.get(&t.id).copied().unwrap_or(0);
            match self.entries.get_mut(&t.id) {
                Some(e) => {
                    e.task = t;
                    e.hits = h;
                }
                None => {
                    self.entries.insert(
                        t.id.clone(),
                        Entry {
                            task: t,
                            due_ms: now_ms,
                            hits: h,
                        },
                    );
                }
            }
        }
        removed
    }

    /// A tarefa de menor vencimento (empate: maior nota, depois id).
    pub fn pick(&self) -> Option<Task> {
        self.entries
            .values()
            .min_by(|a, b| {
                a.due_ms
                    .cmp(&b.due_ms)
                    .then(Self::eff(b).total_cmp(&Self::eff(a)))
                    .then(a.task.id.cmp(&b.task.id))
            })
            .map(|e| e.task.clone())
    }

    /// Marca a consulta como feita: reagenda pelo peso e rende crédito à descoberta.
    pub fn done(&mut self, id: &str, now_ms: i64) {
        let mean = if self.entries.is_empty() {
            1.0
        } else {
            self.entries.values().map(Self::eff).sum::<f64>() / self.entries.len() as f64
        };
        if let Some(e) = self.entries.get_mut(id) {
            let interval = (BASE_INTERVAL_MS as f64 * mean / Self::eff(e))
                .clamp(BASE_INTERVAL_MS as f64 / 4.0, BASE_INTERVAL_MS as f64 * 8.0);
            e.due_ms = now_ms + interval as i64;
        }
        self.credit = (self.credit + DISCOVERY_SHARE).min(CREDIT_CAP);
    }

    /// Acerto recente: a tarefa passa a vencer mais cedo.
    pub fn add_hit(&mut self, id: &str) {
        if let Some(e) = self.entries.get_mut(id) {
            e.hits += 1;
        }
    }

    pub fn may_discover(&self) -> bool {
        self.credit >= 1.0 || self.entries.len() < BOOT_TASKS
    }

    pub fn spend_discovery(&mut self) {
        self.credit = (self.credit - 1.0).max(0.0);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Tick {
    /// Nada a fazer agora (sem saldo, sem tarefas, descoberta concluída).
    Idle,
    Discovery,
    Watched(String),
}

fn now_ms(h: &Hunt) -> i64 {
    h.now() * 1000
}

/// Um passo do laço: descoberta (se há crédito) ou a próxima consulta de vigilância.
pub async fn tick(h: &Arc<Hunt>) -> Tick {
    detect::roll_cycle(h);
    if h.saldo() <= 0 {
        h.set_message(Some(
            "sem saldo: informe um valor ou configure o cookie da Steam".into(),
        ));
        return Tick::Idle;
    }
    if let Err(e) = catalog::maybe_reselect(h).await {
        tracing::warn!("seleção falhou: {e:#}");
    }
    let (may_discover, pick) = {
        let s = h.lock();
        (s.queue.may_discover(), s.queue.pick())
    };
    if may_discover || pick.is_none() {
        match catalog::step(h).await {
            Ok(true) => {
                h.lock().queue.spend_discovery();
                set_idle_message(h);
                return Tick::Discovery;
            }
            Ok(false) => {}
            Err(e) => {
                backoff(h, &e).await;
                return Tick::Discovery;
            }
        }
    }
    let Some(task) = pick else {
        set_idle_message(h);
        return Tick::Idle;
    };
    // Reagenda antes de consultar: uma falha não deixa a mesma tarefa ocupando a frente da fila.
    h.lock().queue.done(&task.id, now_ms(h));
    match run_task(h, &task).await {
        Ok(()) => {
            set_idle_message(h);
        }
        Err(e) => backoff(h, &e).await,
    }
    Tick::Watched(task.id)
}

fn set_idle_message(h: &Hunt) {
    let watching = h.lock().queue.len();
    h.set_message((watching == 0).then(|| "procurando o que caçar com esse saldo…".to_string()));
}

async fn run_task(h: &Arc<Hunt>, task: &Task) -> anyhow::Result<()> {
    let page =
        listings::query_listings_wear(&h.steam, &task.code, 0, task.wear, &task.filters()).await?;
    detect::process(h, task, &page).await?;
    Ok(())
}

/// Erro de consulta: 429 e breaker já seguram o limitador; o resto espera um pouco para não girar em falso.
async fn backoff(h: &Hunt, e: &anyhow::Error) {
    let pause = match e.downcast_ref::<SteamError>() {
        Some(SteamError::RateLimited { .. }) => {
            tracing::warn!("Steam respondeu 429; o limitador segura a próxima consulta");
            return;
        }
        Some(SteamError::BreakerOpen { endpoint, .. }) => {
            tracing::warn!("{endpoint:?} em pausa (breaker)");
            h.set_message(Some(format!(
                "{endpoint:?} da Steam em pausa; tentando de novo"
            )));
            Duration::from_secs(5)
        }
        _ => {
            tracing::warn!("consulta falhou: {e:#}");
            Duration::from_secs(2)
        }
    };
    tokio::time::sleep(pause).await;
}

/// Laço da caça. Termina quando o processo termina.
pub async fn run(h: Arc<Hunt>) {
    loop {
        if tick(&h).await == Tick::Idle {
            tokio::select! {
                () = h.wake.notified() => {}
                () = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
        }
        // `queries_per_min` e `last_query_at` mudam sem evento próprio: empurra o estado a cada 10 s.
        if h.now() - h.lock().last_state_pub >= 10 {
            h.publish_state();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, score: f64) -> Task {
        Task {
            id: id.into(),
            name: id.into(),
            code: "G1234567".into(),
            wear: None,
            seeds: None,
            score,
        }
    }

    #[test]
    fn higher_score_and_hits_are_picked_more_often() {
        let mut q = Queue::default();
        q.set_tasks(
            vec![task("a", 40.0), task("b", 10.0), task("c", 10.0)],
            &HashMap::new(),
            0,
        );
        q.add_hit("c");
        q.add_hit("c");
        q.add_hit("c"); // c: 10 × 4 = 40, igual a
        let mut now = 0;
        let mut count: HashMap<String, u32> = HashMap::new();
        for _ in 0..300 {
            let t = q.pick().unwrap();
            q.done(&t.id, now);
            *count.entry(t.id).or_default() += 1;
            now += 3_000; // limitador a 3 s
        }
        let (a, b, c) = (count["a"], count["b"], count["c"]);
        assert!(a > b * 2 && c > b * 2, "{a} {b} {c}");
        assert!(b >= 10, "quem tem nota menor ainda é consultado: {b}");
    }

    #[test]
    fn set_tasks_keeps_schedule_and_reports_removed() {
        let mut q = Queue::default();
        q.set_tasks(vec![task("a", 1.0), task("b", 1.0)], &HashMap::new(), 0);
        q.done("a", 1_000);
        let removed = q.set_tasks(vec![task("a", 2.0), task("c", 1.0)], &HashMap::new(), 2_000);
        assert_eq!(removed, vec!["b".to_string()]);
        assert_eq!(q.len(), 2);
        // c é novo e vence já; a manteve o vencimento futuro
        assert_eq!(q.pick().unwrap().id, "c");
    }

    #[test]
    fn discovery_gets_about_twenty_percent_after_boot() {
        let mut q = Queue::default();
        let tasks: Vec<Task> = (0..BOOT_TASKS)
            .map(|i| task(&format!("t{i}"), 1.0))
            .collect();
        q.set_tasks(tasks, &HashMap::new(), 0);
        assert!(!q.may_discover(), "sem crédito, fora do arranque");
        let (mut watch, mut disc) = (0, 0);
        for _ in 0..1000 {
            if q.may_discover() {
                q.spend_discovery();
                disc += 1;
            } else {
                let t = q.pick().unwrap();
                q.done(&t.id, 0);
                watch += 1;
            }
        }
        let share = f64::from(disc) / f64::from(disc + watch);
        assert!((0.19..=0.21).contains(&share), "parcela {share}");
    }

    #[test]
    fn boot_lets_discovery_run_free() {
        let mut q = Queue::default();
        q.set_tasks(vec![task("a", 1.0)], &HashMap::new(), 0);
        for _ in 0..50 {
            assert!(q.may_discover());
            q.spend_discovery();
        }
    }

    #[test]
    fn rare_task_filters_by_seed_range() {
        let mut t = task("r", 1.0);
        assert!(t.filters().is_empty());
        t.seeds = Some((58, 59));
        assert_eq!(t.filters(), vec![PropFilter::seed(58, 59)]);
    }
}
