//! Agenda das consultas: um bandido de vários braços (UCB1) que aprende o que rende e o laço que puxa no
//! ritmo do limitador do `SteamClient`.
//!
//! Braço = alvo (skin × desgaste × modo de busca): `d:` mais barato primeiro, `r:` faixas de seed raras,
//! sem braço adicional de float. A frequência de consulta de cada um é a própria alocação do
//! bandido: quem rende por consulta gasta ganha mais consultas. O limitador (≥ 6 s) segue sendo o gargalo:
//! Uma busca pode conferir vários braços; os preços próximos do teto geram consultas adicionais de anúncios.
//! A descoberta divide a mesma vaga: cada consulta de vigilância rende 0,25 de crédito e cada passo de
//! descoberta gasta 1 (≈ 20 % do total); enquanto há poucas tarefas (arranque), a descoberta pode usar tudo.
//!
//! Por que UCB1 e não Thompson sampling: é determinístico (teste de convergência reprodutível, relógio
//! injetável) e dispensa crate de números aleatórios (regra 5). A recompensa é normalizada pela maior média
//! para caber em [0, 1]; a cauda pesada (um T1 vale dezenas de consultas vazias) é amortecida pelo piso de
//! exploração e pelo decaimento. Três mecanismos além do UCB:
//! - **Cota de exploração** (`EXPLORE_SHARE`): 1 consulta em 8 vai para o braço menos testado, sempre; o slot
//!   gira entre os modos (barato, seed), então cada modo tem um piso próprio, não importa quantos braços tem.
//! - **Decaimento** (meia-vida `HALF_LIFE`): o passado antigo pesa menos, o mercado muda.
//! - **Seca** (`DRY_HALF_LIFE`): a média vale pela metade a cada 4 h sem recompensa nenhuma.

use super::{Hunt, catalog, detect, watch};
use crate::steam::client::Error as SteamError;
use crate::steam::listings::{self, PropFilter};
use crate::steam::search;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

/// Crédito de descoberta por consulta de vigilância: 1 passo a cada 4 consultas = 20 % do total.
pub const DISCOVERY_SHARE: f64 = 0.25;
/// Com menos tarefas que isto a descoberta tem prioridade (primeiro ciclo).
pub const BOOT_TASKS: usize = 10;
const CREDIT_CAP: f64 = 4.0;
/// Parcela das consultas reservada ao braço menos testado (12,5 %, dentro dos 10–15 % pedidos).
pub const EXPLORE_SHARE: f64 = 0.125;
/// Consultas e recompensa pesam metade a cada 24 h.
const HALF_LIFE: f64 = 24.0 * 3600.0;
/// A média vale metade a cada 4 h sem recompensa.
const DRY_HALF_LIFE: f64 = 4.0 * 3600.0;
/// "Quase lucro" vale esta fração do que o anúncio renderia se tivesse passado (lucro mínimo da lista).
pub const NEAR_WEIGHT: f64 = 0.15;
/// Até aqui o alvo ainda é "novo" para o painel.
const NEW_QUERIES: u64 = 10;
/// Acima disto sem recompensa o painel diz "sem achar nada há N h".
const DRY_REASON_SECS: i64 = 3 * 3600;
/// Piso (centavos/consulta) da escala do bônus, para a escala não zerar quando nada rendeu ainda.
const MIN_SCALE: f64 = 1.0;
/// Grava as notas alteradas a cada tanto.
const SAVE_EVERY_SECS: i64 = 30;

/// Modo de busca de um braço.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Cheap,
    Seed,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Cheap => "barato",
            Mode::Seed => "seed",
        }
    }
}

/// Uma consulta recorrente: `QueryListingsForItem` do código, com desgaste e/ou faixa de seed.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    /// `d:<market_hash_name>` (mais barato) ou `r:<código>:<desgaste>`
    /// (raro: uma tarefa por skin × desgaste).
    pub id: String,
    /// `market_hash_name` com o desgaste, para log e para filtrar a resposta.
    pub name: String,
    pub code: String,
    pub wear: Option<u8>,
    /// Raro: faixas de seed do tier 1/topo da tabela; cada consulta usa uma faixa, em rodízio
    /// (`Queue::focus`). Vazio = desconto.
    pub seeds: Vec<(u16, u16)>,
    /// Nota-base do catálogo (sem aprendizado): só desempata braços ainda não testados.
    pub score: f64,
}

impl Task {
    pub fn rare(&self) -> bool {
        !self.seeds.is_empty()
    }

    pub fn mode(&self) -> Mode {
        if self.rare() { Mode::Seed } else { Mode::Cheap }
    }

    pub fn filters(&self) -> Vec<PropFilter> {
        self.seeds
            .first()
            .map(|&(lo, hi)| PropFilter::seed(lo, hi))
            .into_iter()
            .collect()
    }

    /// Chave do piso observado em `Shared::floors`: raro guarda um piso por faixa (`<id>#<de>-<até>`).
    pub fn floor_key(&self) -> String {
        match self.seeds.first() {
            Some((lo, hi)) if self.seeds.len() == 1 => format!("{}#{lo}-{hi}", self.id),
            _ => self.id.clone(),
        }
    }
}

/// O que se sabe do rendimento de um braço. `n` e `r` decaem com o tempo; o resto é vitalício (para o painel).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stat {
    /// Consultas gastas, com decaimento.
    pub n: f64,
    /// Recompensa (centavos: lucro esperado achado + quase lucro com peso), com decaimento.
    pub r: f64,
    pub queries: u64,
    /// Oportunidades novas achadas e o lucro esperado somado (centavos).
    pub finds: u32,
    pub gain_cents: i64,
    /// Anúncios que ficaram a pouco de passar.
    pub near_hits: u32,
    pub first_at: i64,
    /// Última recompensa (achado ou quase), em segundos; 0 = nunca.
    pub reward_at: i64,
    pub updated_at: i64,
}

impl Stat {
    fn decay(&mut self, now: i64) {
        let dt = (now - self.updated_at).max(0) as f64;
        if dt > 0.0 {
            let f = 0.5_f64.powf(dt / HALF_LIFE);
            self.n *= f;
            self.r *= f;
        }
        self.updated_at = self.updated_at.max(now);
    }

    /// Segundos sem recompensa (desde o primeiro teste, se nunca teve).
    fn dry_secs(&self, now: i64) -> i64 {
        (now - self.reward_at.max(self.first_at)).max(0)
    }

    /// Lucro esperado por consulta (centavos), com a seca já descontada.
    pub fn mean(&self, now: i64) -> f64 {
        if self.n <= 0.0 {
            return 0.0;
        }
        self.r / self.n * 0.5_f64.powf(self.dry_secs(now) as f64 / DRY_HALF_LIFE)
    }
}

struct Entry {
    task: Task,
    /// Próxima faixa de seed de uma tarefa rara.
    cursor: usize,
}

#[derive(Default)]
pub struct Queue {
    entries: HashMap<String, Entry>,
    /// Sobrevive à saída do braço da seleção: se ele voltar, retoma de onde parou.
    stats: HashMap<String, Stat>,
    dirty: HashSet<String>,
    credit: f64,
    explore: f64,
    /// Rodízio de modos no slot de exploração.
    explore_turn: usize,
}

impl Queue {
    pub fn tasks(&self) -> Vec<Task> {
        self.entries.values().map(|e| e.task.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Troca o conjunto de tarefas: quem continua mantém o aprendizado; quem entra começa sem consultas (e é
    /// testado já). Devolve os ids das tarefas que saíram.
    pub fn set_tasks(&mut self, tasks: Vec<Task>) -> Vec<String> {
        let keep: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
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
            match self.entries.get_mut(&t.id) {
                Some(e) => e.task = t,
                None => {
                    self.entries
                        .insert(t.id.clone(), Entry { task: t, cursor: 0 });
                }
            }
        }
        removed
    }

    /// Carrega as notas guardadas no banco (arranque).
    pub fn load_stats(&mut self, rows: Vec<(String, Stat)>) {
        self.stats.extend(rows);
    }

    /// Notas alteradas desde a última vez, para gravar.
    pub fn take_dirty(&mut self) -> Vec<(String, Stat)> {
        let ids = std::mem::take(&mut self.dirty);
        ids.into_iter()
            .filter_map(|id| self.stats.get(&id).map(|s| (id, s.clone())))
            .collect()
    }

    /// Índice UCB1 de um braço: lucro esperado por consulta + bônus de incerteza na escala da maior média.
    fn index(&self, id: &str, now: i64, total_n: f64, scale: f64) -> f64 {
        let Some(s) = self.stats.get(id).filter(|s| s.queries > 0) else {
            return f64::INFINITY; // nunca consultado
        };
        let n = s.n.max(0.5);
        s.mean(now) + scale * (2.0 * total_n.max(2.0).ln() / n).sqrt()
    }

    /// Decide o próximo alvo e já conta a consulta (como antes: reagenda antes de consultar, uma falha não
    /// deixa o mesmo alvo ocupando a frente). Uma chamada = uma consulta; rende crédito à descoberta.
    pub fn next(&mut self, now: i64) -> Option<Task> {
        if self.entries.is_empty() {
            return None;
        }
        for id in self.entries.keys() {
            if let Some(s) = self.stats.get_mut(id) {
                s.decay(now);
            }
        }
        let n_of = |q: &Queue, id: &str| q.stats.get(id).map_or(0.0, |s| s.n);
        let total_n: f64 = self.entries.keys().map(|k| n_of(self, k)).sum();
        let scale = self
            .entries
            .keys()
            .filter_map(|k| self.stats.get(k))
            .map(|s| s.mean(now))
            .fold(MIN_SCALE, f64::max);
        // desempate estável: maior nota-base, depois id
        let tie = |a: &Entry, b: &Entry| {
            b.task
                .score
                .total_cmp(&a.task.score)
                .then(a.task.id.cmp(&b.task.id))
        };
        self.explore += EXPLORE_SHARE;
        let exploring = self.explore >= 1.0;
        let pick = if exploring {
            // O slot de exploração gira entre os modos presentes (piso por modo, qualquer que seja o número de
            // braços de cada um) e, dentro do modo, vai para o menos testado (consultas decaídas).
            let mut modes: Vec<Mode> = self.entries.values().map(|e| e.task.mode()).collect();
            modes.sort();
            modes.dedup();
            let mode = modes[self.explore_turn % modes.len()];
            self.explore_turn = self.explore_turn.wrapping_add(1);
            self.entries
                .values()
                .filter(|e| e.task.mode() == mode)
                .min_by(|a, b| {
                    n_of(self, &a.task.id)
                        .total_cmp(&n_of(self, &b.task.id))
                        .then_with(|| tie(a, b))
                })
        } else {
            self.entries.values().min_by(|a, b| {
                let (ia, ib) = (
                    self.index(&a.task.id, now, total_n, scale),
                    self.index(&b.task.id, now, total_n, scale),
                );
                ib.total_cmp(&ia).then_with(|| tie(a, b))
            })
        };
        let id = pick?.task.id.clone();
        if exploring {
            self.explore -= 1.0;
        }
        let s = self.stats.entry(id.clone()).or_insert_with(|| Stat {
            first_at: now,
            updated_at: now,
            ..Stat::default()
        });
        s.n += 1.0;
        s.queries += 1;
        self.dirty.insert(id.clone());
        self.credit = (self.credit + DISCOVERY_SHARE).min(CREDIT_CAP);
        self.entries.get(&id).map(|e| e.task.clone())
    }

    /// Resultado da consulta: lucro esperado das oportunidades novas (centavos) e os quase-lucros (crédito
    /// já ponderado por `NEAR_WEIGHT`).
    pub fn reward(&mut self, id: &str, now: i64, found: Found) {
        let Some(s) = self.stats.get_mut(id) else {
            return;
        };
        s.decay(now);
        let r = found.gain_cents.max(0) as f64 + found.near_credit;
        s.r += r;
        s.finds += found.finds;
        s.gain_cents += found.gain_cents.max(0);
        s.near_hits += found.near_hits;
        if r > 0.0 {
            s.reward_at = now;
        }
        self.dirty.insert(id.to_string());
    }

    /// Lucro esperado por consulta (centavos) e o motivo em palavras do dia a dia.
    pub fn describe(&self, id: &str, now: i64) -> (f64, String) {
        let Some(s) = self.stats.get(id).filter(|s| s.queries > 0) else {
            return (0.0, "testando alvo novo".into());
        };
        let reason = if s.queries < NEW_QUERIES {
            "testando alvo novo".to_string()
        } else if s.dry_secs(now) >= DRY_REASON_SECS {
            format!("sem achar nada há {} h", s.dry_secs(now) / 3600)
        } else if s.gain_cents > 0 {
            format!(
                "rendeu {} em {} consultas",
                crate::steam::buy::brl(s.gain_cents),
                s.queries
            )
        } else if s.near_hits > 0 {
            format!("quase deu lucro {} vezes", s.near_hits)
        } else {
            format!("sem achar nada em {} consultas", s.queries)
        };
        (s.mean(now), reason)
    }

    /// A tarefa estreitada à próxima faixa de seed (rodízio); desconto volta igual.
    pub fn focus(&mut self, task: Task) -> Task {
        let Some(e) = self.entries.get_mut(&task.id).filter(|_| task.rare()) else {
            return task;
        };
        let range = task.seeds[e.cursor % task.seeds.len()];
        e.cursor = e.cursor.wrapping_add(1);
        Task {
            seeds: vec![range],
            ..task
        }
    }

    fn record_scan(&mut self, id: &str, now: i64) {
        let stat = self.stats.entry(id.to_string()).or_insert_with(|| Stat {
            first_at: now,
            updated_at: now,
            ..Stat::default()
        });
        stat.decay(now);
        stat.n += 1.0;
        stat.queries += 1;
        self.dirty.insert(id.to_string());
    }

    pub fn may_discover(&self) -> bool {
        self.credit >= 1.0 || self.entries.len() < BOOT_TASKS
    }

    pub fn spend_discovery(&mut self) {
        self.credit = (self.credit - 1.0).max(0.0);
    }
}

/// O que uma consulta rendeu.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Found {
    pub finds: u32,
    /// Lucro esperado (preço médio) das oportunidades novas, em centavos.
    pub gain_cents: i64,
    pub near_hits: u32,
    /// Soma do crédito dos quase-lucros, em centavos (`NEAR_WEIGHT` × lucro mínimo que faltou cobrir).
    pub near_credit: f64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Tick {
    /// Nada a fazer agora (sem saldo, sem tarefas, descoberta concluída).
    Idle,
    Discovery,
    Watched(String),
    /// Leu um livro de pedidos (`orders`).
    Book,
}

/// Um passo do laço: descoberta (se há crédito) ou a próxima consulta de vigilância.
const NO_SALDO: &str = "sem saldo: informe um valor ou configure o cookie da Steam";

pub async fn tick(h: &Arc<Hunt>) -> Tick {
    detect::roll_cycle(h);
    if h.saldo() <= 0 {
        h.set_message(Some(NO_SALDO.into()));
        return Tick::Idle;
    }
    // O saldo chegou (ex.: a Steam respondeu depois de uma pausa): o aviso de falta não pode ficar
    // no painel enquanto a descoberta espera o limitador.
    let cleared = {
        let mut s = h.lock();
        let stale = s.message.as_deref() == Some(NO_SALDO);
        if stale {
            s.message = None;
        }
        stale
    };
    if cleared {
        h.publish_state();
    }
    if let Err(e) = catalog::maybe_reselect(h).await {
        tracing::warn!("seleção falhou: {e:#}");
    }
    let (may_discover, empty) = {
        let s = h.lock();
        (s.queue.may_discover(), s.queue.len() == 0)
    };
    if may_discover || empty {
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
    if super::orders::maybe_step(h).await {
        return Tick::Book;
    }
    // `next` já conta a consulta: uma falha não deixa o mesmo alvo ocupando a frente.
    let next = h.lock().queue.next(h.now());
    let Some(task) = next else {
        set_idle_message(h);
        return Tick::Idle;
    };
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

// shortcut: 10 % é hipótese para ruído da razão e arredondamento; calibrar com preços reais.
const SCAN_TOLERANCE_PERCENT: i64 = 10;

fn base_name(name: &str) -> &str {
    let name = name.strip_prefix("StatTrak™ ").unwrap_or(name);
    let name = name.strip_prefix("Souvenir ").unwrap_or(name);
    name.rsplit_once(" (").map_or(name, |(base, _)| base)
}

fn same_query(a: &Task, b: &Task) -> bool {
    a.code == b.code && a.wear == b.wear && a.filters() == b.filters()
}

fn affordable(price: i64, max: i64) -> bool {
    i128::from(price) * 100 <= i128::from(max) * (100 + i128::from(SCAN_TOLERANCE_PERCENT))
}

pub(super) async fn run_task(h: &Arc<Hunt>, task: &Task) -> anyhow::Result<()> {
    let task = h.lock().queue.focus(task.clone());
    if task.rare() {
        return query_group(h, &task, vec![task.clone()]).await;
    }
    let (tasks, ratio, active) = {
        let s = h.lock();
        (
            s.queue.tasks(),
            s.ratio.map(|r| r.0),
            s.active
                .values()
                .map(|a| a.task.clone())
                .collect::<HashSet<_>>(),
        )
    };
    let Some(ratio) = ratio.filter(|r| r.is_finite() && *r > 0.0) else {
        return query_group(h, &task, tasks).await;
    };
    let base = base_name(&task.name);
    let group: Vec<Task> = tasks
        .iter()
        .filter(|t| !t.rare() && base_name(&t.name) == base)
        .cloned()
        .collect();
    // Uma busca só economiza quando cobre duas ou mais versões vigiadas; com uma só, ela custaria a mais.
    if group.len() < 2 {
        return query_group(h, &task, tasks).await;
    }
    let q = [
        ("query", base.replace(" | ", " ")),
        ("start", "0".into()),
        ("count", "20".into()),
        ("search_descriptions", "0".into()),
        ("sort_column", "popular".into()),
        ("sort_dir", "desc".into()),
        ("appid", "730".into()),
        ("norender", "1".into()),
    ];
    let v = h
        .steam
        .get_json(
            crate::steam::client::Endpoint::Search,
            "/market/search/render/",
            &q,
        )
        .await?;
    let page = search::parse_page(&v)?;
    let limits: HashMap<String, Option<i64>> = watch::list(h)
        .await?
        .into_iter()
        .filter(|r| r.mode == "barato")
        .map(|r| (r.name, r.max_buy_cents))
        .collect();
    let mut eligible = Vec::new();
    for t in &group {
        let price = page
            .entries
            .iter()
            .find(|e| e.name == t.name)
            .map(|e| catalog::usd_to_brl(e.sell_price_usd_cents, ratio));
        let max = limits.get(&t.name).copied().flatten();
        if price.is_some() && t.id != task.id {
            h.lock().queue.record_scan(&t.id, h.now());
        }
        if active.contains(&t.id) || price.zip(max).is_none_or(|(p, m)| affordable(p, m)) {
            if price.is_some() || t.id == task.id || active.contains(&t.id) {
                eligible.push(t.clone());
            }
        } else {
            h.lock().cycle.saved_listings += 1;
        }
    }
    while let Some(t) = eligible.pop() {
        let duplicates: Vec<Task> = eligible
            .iter()
            .filter(|other| same_query(&t, other))
            .cloned()
            .collect();
        eligible.retain(|other| !same_query(&t, other));
        let mut targets = vec![t.clone()];
        targets.extend(duplicates);
        {
            let mut s = h.lock();
            s.queue.credit = (s.queue.credit + DISCOVERY_SHARE).min(CREDIT_CAP);
        }
        query_group(h, &t, targets).await?;
    }
    Ok(())
}

async fn query_group(h: &Arc<Hunt>, task: &Task, tasks: Vec<Task>) -> anyhow::Result<()> {
    let mut targets: Vec<Task> = tasks.into_iter().filter(|t| same_query(task, t)).collect();
    {
        let mut s = h.lock();
        let others = s.queue.tasks();
        for other in others {
            if targets.iter().any(|t| t.id == other.id) {
                continue;
            }
            let focused = if other.rare() {
                let Some(entry) = s.queue.entries.get(&other.id) else {
                    continue;
                };
                Task {
                    seeds: vec![other.seeds[entry.cursor % other.seeds.len()]],
                    ..other.clone()
                }
            } else {
                other.clone()
            };
            if same_query(task, &focused) {
                targets.push(s.queue.focus(other));
            }
        }
    }
    let page =
        listings::query_listings_wear(&h.steam, &task.code, 0, task.wear, &task.filters()).await?;
    for t in &targets {
        let out = detect::process(h, t, &page).await?;
        h.lock().queue.reward(&t.id, h.now(), out.rewarded);
    }
    Ok(())
}

/// Ritmo do último 429 (texto de `SteamClient::restore_pace`), retomado ao subir.
const PACE_KEY: &str = "steam_pace";

/// Retoma o ritmo salvo e passa a salvar a cada 429, venha de qual endpoint vier.
pub async fn load_pace(h: &Hunt) -> anyhow::Result<()> {
    if let Some(v) = catalog::get_setting(&h.db, PACE_KEY).await? {
        h.steam.restore_pace(&v);
    }
    let db = h.db.clone();
    // Gravações em fila, cada uma com o ritmo mais novo: uma gravação antiga que termina por último não
    // apaga a mais nova.
    let latest = Arc::new(std::sync::Mutex::new(String::new()));
    let turn = Arc::new(tokio::sync::Mutex::new(()));
    h.steam.on_pace_change(move |v| {
        *latest.lock().unwrap() = v;
        let (db, latest, turn) = (db.clone(), latest.clone(), turn.clone());
        tokio::spawn(async move {
            let _turn = turn.lock().await;
            let v = latest.lock().unwrap().clone();
            if let Err(e) = catalog::set_setting(&db, PACE_KEY, v).await {
                tracing::warn!("ritmo da Steam não salvou: {e:#}");
            }
        });
    });
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

/// Grava no banco as notas alteradas.
pub async fn save_stats(h: &Hunt) -> anyhow::Result<()> {
    let rows = h.lock().queue.take_dirty();
    if rows.is_empty() {
        return Ok(());
    }
    h.db.call(move |c| {
        let tx = c.transaction()?;
        for (id, s) in rows {
            tx.execute(
                "INSERT OR REPLACE INTO arm_stats(id, n, r, queries, finds, gain_cents, near_hits, first_at, reward_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    id,
                    s.n,
                    s.r,
                    s.queries as i64,
                    s.finds,
                    s.gain_cents,
                    s.near_hits,
                    s.first_at,
                    s.reward_at,
                    s.updated_at
                ],
            )?;
        }
        tx.commit()
    })
    .await
}

/// Lê as notas guardadas (arranque).
pub async fn load_stats(h: &Hunt) -> anyhow::Result<()> {
    let rows = h
        .db
        .call(|c| {
            c.prepare(
                "SELECT id, n, r, queries, finds, gain_cents, near_hits, first_at, reward_at, updated_at FROM arm_stats",
            )?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Stat {
                        n: r.get(1)?,
                        r: r.get(2)?,
                        queries: r.get::<_, i64>(3)? as u64,
                        finds: r.get(4)?,
                        gain_cents: r.get(5)?,
                        near_hits: r.get(6)?,
                        first_at: r.get(7)?,
                        reward_at: r.get(8)?,
                        updated_at: r.get(9)?,
                    },
                ))
            })?
            .collect()
        })
        .await?;
    h.lock().queue.load_stats(rows);
    Ok(())
}

/// Laço da caça. Termina quando o processo termina.
pub async fn run(h: Arc<Hunt>) {
    let mut saved = h.now();
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
        if h.now() - saved >= SAVE_EVERY_SECS {
            saved = h.now();
            if let Err(e) = save_stats(&h).await {
                tracing::warn!("gravar notas da agenda falhou: {e:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_threshold_includes_tolerance_and_groups_variants() {
        assert!(affordable(110, 100));
        assert!(!affordable(111, 100));
        assert_eq!(
            base_name("StatTrak™ Glock-18 | Moonrise (Factory New)"),
            base_name("Glock-18 | Moonrise (Field-Tested)")
        );
        assert_eq!(
            base_name("Souvenir Glock-18 | Moonrise (Battle-Scarred)"),
            "Glock-18 | Moonrise"
        );
    }

    fn task(id: &str, score: f64) -> Task {
        Task {
            id: id.into(),
            name: id.into(),
            code: "G1234567".into(),
            wear: Some(2),
            seeds: vec![],
            score,
        }
    }

    /// Gerador congruencial: o teste é reproduzível sem crate de aleatórios.
    struct Lcg(u64);
    impl Lcg {
        fn unit(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// Braços de rendimento conhecido: (id, chance de achar por consulta, lucro por achado em centavos).
    const ARMS: [(&str, f64, i64); 6] = [
        ("best", 0.20, 100), // 20 centavos por consulta
        ("good", 0.10, 60),  // 6
        ("meh", 0.05, 40),   // 2
        ("dud1", 0.0, 0),
        ("dud2", 0.0, 0),
        ("dud3", 0.0, 0),
    ];

    /// Roda `n` consultas a 3 s cada; `payout` decide o rendimento do braço no instante `t`.
    fn simulate(
        q: &mut Queue,
        rng: &mut Lcg,
        t0: i64,
        n: usize,
        payout: impl Fn(&str, i64) -> (f64, i64),
    ) -> HashMap<String, u32> {
        let mut count: HashMap<String, u32> = HashMap::new();
        for i in 0..n {
            let t = t0 + 3 * i as i64;
            let id = q.next(t).unwrap().id;
            let (p, gain) = payout(&id, t);
            if rng.unit() < p {
                let found = Found {
                    finds: 1,
                    gain_cents: gain,
                    ..Found::default()
                };
                q.reward(&id, t, found);
            }
            *count.entry(id).or_default() += 1;
        }
        count
    }

    fn arms_queue() -> Queue {
        let mut q = Queue::default();
        q.set_tasks(ARMS.iter().map(|a| task(a.0, 1.0)).collect());
        q
    }

    fn fixed(id: &str, _t: i64) -> (f64, i64) {
        let a = ARMS.iter().find(|a| a.0 == id).unwrap();
        (a.1, a.2)
    }

    #[test]
    fn converges_to_the_best_arm_without_killing_exploration() {
        let (mut q, mut rng) = (arms_queue(), Lcg(7));
        let n = 6_000;
        let c = simulate(&mut q, &mut rng, 0, n, fixed);
        let share = |id: &str| f64::from(c[id]) / n as f64;
        assert!(share("best") > 0.5, "o melhor leva a maior parte: {c:?}");
        assert!(share("best") > 5.0 * share("good"), "{c:?}");
        // o aprendizado enxerga a ordem real mesmo com poucas consultas nos de baixo
        let m = |id: &str| q.describe(id, n as i64 * 3).0;
        assert!(m("best") > m("good") && m("good") > m("meh") && m("meh") > m("dud1"));
        // exploração: nenhum braço morre, e o que não é o melhor recebe ≥ 10 % no total
        for (id, ..) in ARMS {
            assert!(share(id) >= 0.01, "{id} ficou sem consultas: {c:?}");
        }
        assert!(1.0 - share("best") >= 0.10, "{c:?}");
    }

    #[test]
    fn exploration_slot_goes_to_the_least_tested_arm() {
        let (mut q, mut rng) = (arms_queue(), Lcg(1));
        simulate(&mut q, &mut rng, 0, 600, fixed);
        // novo braço entra: nas próximas consultas ele é testado já
        let mut tasks = q.tasks();
        tasks.push(task("fresh", 1.0));
        q.set_tasks(tasks);
        assert_eq!(q.next(2_000).unwrap().id, "fresh");
        assert_eq!(q.describe("fresh", 2_000).1, "testando alvo novo");
    }

    #[test]
    fn an_arm_that_dries_up_loses_space() {
        let (mut q, mut rng) = (arms_queue(), Lcg(3));
        let h = 3600;
        // 4 h com "best" rendendo; depois ele seca e "good" segue igual
        let early = simulate(&mut q, &mut rng, 0, 1_200, |id, t| {
            if id == "best" && t >= 4 * h {
                (0.0, 0)
            } else {
                fixed(id, t)
            }
        });
        let late = simulate(&mut q, &mut rng, 4 * h, 6_000, |id, t| {
            if id == "best" { (0.0, 0) } else { fixed(id, t) }
        });
        let (e, l) = (
            f64::from(early["best"]) / 1_200.0,
            f64::from(late["best"]) / 6_000.0,
        );
        assert!(l < e / 2.0, "antes {e:.2}, depois {l:.2}");
        assert!(late["good"] > late["best"], "{late:?}");
        let why = q.describe("best", 4 * h + 18_000).1;
        assert!(why.starts_with("sem achar nada há"), "{why}");
    }

    #[test]
    fn near_misses_give_a_new_arm_a_head_start() {
        let mut q = Queue::default();
        q.set_tasks(vec![task("a", 1.0), task("b", 1.0)]);
        for t in 0..40 {
            let id = q.next(t).unwrap().id;
            if id == "a" {
                let near = Found {
                    near_hits: 1,
                    near_credit: 3.0, // 15 % de 20 centavos
                    ..Found::default()
                };
                q.reward("a", t, near);
            }
        }
        let (a, why) = q.describe("a", 40);
        assert!(a > 0.0 && why.starts_with("quase deu lucro"), "{a} {why}");
        assert!(q.describe("b", 40).0 == 0.0);
    }

    #[test]
    fn reasons_read_like_plain_portuguese() {
        let mut q = Queue::default();
        q.set_tasks(vec![task("a", 1.0)]);
        for t in 0..30 {
            q.next(t);
        }
        let found = Found {
            finds: 1,
            gain_cents: 120,
            ..Found::default()
        };
        q.reward("a", 30, found);
        assert_eq!(q.describe("a", 31).1, "rendeu R$ 1,20 em 30 consultas");
    }

    #[test]
    fn stats_survive_a_restart_and_unselected_arms_keep_theirs() {
        let mut q = arms_queue();
        let mut rng = Lcg(5);
        simulate(&mut q, &mut rng, 0, 300, fixed);
        let saved = q.take_dirty();
        assert_eq!(saved.len(), ARMS.len());
        let mut q2 = arms_queue();
        q2.load_stats(saved);
        assert_eq!(q2.describe("best", 900).1, q.describe("best", 900).1);
        // saiu da seleção e voltou: o aprendizado continua
        q2.set_tasks(vec![task("dud1", 1.0)]);
        q2.set_tasks(arms_queue().tasks());
        assert!(q2.stats["best"].queries > 0);
    }

    #[test]
    fn one_call_is_one_query_and_feeds_discovery_credit() {
        let mut q = arms_queue();
        // fora do arranque (≥ BOOT_TASKS tarefas), a descoberta fica com ~20 %
        q.set_tasks(
            (0..BOOT_TASKS)
                .map(|i| task(&format!("t{i}"), 1.0))
                .collect(),
        );
        let (mut watch, mut disc) = (0, 0);
        for i in 0..1000 {
            if q.may_discover() {
                q.spend_discovery();
                disc += 1;
            } else {
                q.next(i).unwrap();
                watch += 1;
            }
        }
        let share = f64::from(disc) / f64::from(disc + watch);
        assert!((0.19..=0.21).contains(&share), "parcela {share}");
    }

    #[test]
    fn boot_lets_discovery_run_free() {
        let mut q = Queue::default();
        q.set_tasks(vec![task("a", 1.0)]);
        for _ in 0..50 {
            assert!(q.may_discover());
            q.spend_discovery();
        }
    }

    #[test]
    fn set_tasks_reports_removed() {
        let mut q = Queue::default();
        q.set_tasks(vec![task("a", 1.0), task("b", 1.0)]);
        let removed = q.set_tasks(vec![task("a", 2.0), task("c", 1.0)]);
        assert_eq!(removed, vec!["b".to_string()]);
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn rare_task_filters_by_seed_range() {
        let mut t = task("r", 1.0);
        assert!(t.filters().is_empty());
        t.seeds = vec![(58, 59), (66, 66)];
        let mut q = Queue::default();
        q.set_tasks(vec![t.clone()]);
        // rodízio: uma faixa por consulta, e o piso é guardado por faixa
        let (a, b, c) = (q.focus(t.clone()), q.focus(t.clone()), q.focus(t.clone()));
        assert_eq!(a.filters(), vec![PropFilter::seed(58, 59)]);
        assert_eq!(b.filters(), vec![PropFilter::seed(66, 66)]);
        assert_eq!(c.seeds, vec![(58, 59)]);
        assert_eq!(b.floor_key(), "r#66-66");
        assert_eq!(task("d", 1.0).floor_key(), "d");
    }

    fn rare(id: &str) -> Task {
        Task {
            seeds: vec![(1, 2)],
            ..task(id, 1.0)
        }
    }

    /// Cada modo presente tem um piso próprio, não importa quantos braços tem (revisão do Codex: igualar
    /// pesos deixava 1 raro entre 40 descontos com 4 % e 24 raros contra 1 desconto com 90 %).
    #[test]
    fn every_mode_keeps_its_floor_whatever_its_arm_count() {
        let floor = EXPLORE_SHARE / 2.0 * 0.9; // cota de exploração dividida por 2 modos
        let run = |tasks: Vec<Task>, mode: Mode| {
            let mut q = Queue::default();
            q.set_tasks(tasks);
            let n = 4_000;
            let mut hit = 0;
            for i in 0..n {
                hit += i32::from(q.next(3 * i).unwrap().mode() == mode);
            }
            f64::from(hit) / n as f64
        };
        // 1 raro entre 40 descontos, sem rendimento nenhum
        let mut tasks: Vec<Task> = (0..40).map(|i| task(&format!("d:{i}"), 1.0)).collect();
        tasks.push(rare("r:x"));
        let rare_share = run(tasks, Mode::Seed);
        assert!(rare_share >= floor, "raro {rare_share:.3}");
        // 24 raros contra 1 desconto: o desconto não some
        let mut tasks: Vec<Task> = (0..24).map(|i| rare(&format!("r:{i}"))).collect();
        tasks.push(task("d:solo", 1.0));
        let cheap_share = run(tasks, Mode::Cheap);
        assert!(cheap_share >= floor, "desconto {cheap_share:.3}");
    }
}
