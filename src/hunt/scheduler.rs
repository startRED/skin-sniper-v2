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

use super::harvest::{self, Policy};
use super::{Hunt, catalog, detect, learn, watch};
use crate::steam::book;
use crate::steam::client::{Endpoint, Error as SteamError};
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
/// Até aqui o alvo ainda é "novo" para o painel.
const NEW_QUERIES: u64 = 10;
/// Peso, em consultas, da referência na média encolhida de um alvo: com poucas consultas ele fica perto dela.
const BOOST_PRIOR_QUERIES: f64 = 20.0;
/// Limites do fator de seleção: quem rende pouco ainda pode voltar, quem rende muito não engole a vaga dos outros.
const BOOST_RANGE: (f64, f64) = (0.25, 4.0);
/// Acima disto sem recompensa o painel diz "sem achar nada há N h".
const DRY_REASON_SECS: i64 = 3 * 3600;
/// Piso (centavos/consulta) da escala do bônus, para a escala não zerar quando nada rendeu ainda.
const MIN_SCALE: f64 = 1.0;
/// Grava as notas alteradas a cada tanto.
const SAVE_EVERY_SECS: i64 = 30;
/// Um raro pode sumir em poucos minutos: acompanhamento confere a seed achada, sem girar as outras faixas.
const ACTIVE_RECHECK_SECS: i64 = 60;

/// Modo de busca de um braço.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Cheap,
    Seed,
    Float,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Cheap => "barato",
            Mode::Seed => "seed",
            Mode::Float => "float",
        }
    }
}

/// Uma consulta recorrente: `QueryListingsForItem` do código, com desgaste e/ou faixa de seed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Task {
    /// `d:<market_hash_name>` (mais barato) ou `r:<código>`
    /// (raro: uma tarefa por skin, todos os desgastes).
    pub id: String,
    /// `market_hash_name` com o desgaste, para log e para filtrar a resposta.
    pub name: String,
    pub code: String,
    pub wear: Option<u8>,
    /// Raro: faixas de seed do tier 1/topo da tabela; cada consulta usa uma faixa, em rodízio
    /// (`Queue::focus`). Vazio = desconto.
    pub seeds: Vec<(u16, u16)>,
    /// Float de elite: faixa `(de, até)` do filtro de float (`f:<market_hash_name>`). `None` nos outros modos.
    pub float: Option<(f64, f64)>,
    /// Nota-base do catálogo (sem aprendizado): só desempata braços ainda não testados.
    pub score: f64,
}

impl Task {
    pub fn rare(&self) -> bool {
        !self.seeds.is_empty() || self.float.is_some()
    }

    pub fn mode(&self) -> Mode {
        match (self.seeds.is_empty(), self.float) {
            (false, _) => Mode::Seed,
            (true, Some(_)) => Mode::Float,
            (true, None) => Mode::Cheap,
        }
    }

    pub fn filters(&self) -> Vec<PropFilter> {
        self.seeds
            .first()
            .map(|&(lo, hi)| PropFilter::seed(lo, hi))
            .or(self.float.map(|(lo, hi)| PropFilter::float(lo, hi)))
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
    /// Colheita: exposição somada das consultas (s, com o mesmo decaimento de `r`) e a última consulta.
    pub exposure: f64,
    pub last_at: i64,
}

impl Stat {
    fn decay(&mut self, now: i64) {
        let dt = (now - self.updated_at).max(0) as f64;
        if dt > 0.0 {
            let f = 0.5_f64.powf(dt / HALF_LIFE);
            self.n *= f;
            self.r *= f;
            self.exposure *= f;
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

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Progress {
    credit: f64,
    explore: f64,
    turn: usize,
    cursors: HashMap<String, usize>,
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
    /// Colheita: vida do achado (s; 0 = padrão), política do dia e, fora da exploração, (escolhas, iguais ao UCB1).
    pub life: f64,
    pub policy: Policy,
    pub shadow: (u64, u64),
    pub external: Option<String>,
    pub external_ready: bool,
    pub last_exploring: bool,
    cursors: HashMap<String, usize>,
    selection_revision: u64,
    query_revisions: HashMap<String, u64>,
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
        self.selection_revision = self.selection_revision.wrapping_add(1);
        let bases: HashSet<String> = tasks
            .iter()
            .map(|t| base_name(&t.name).to_string())
            .collect();
        self.query_revisions.retain(|base, _| bases.contains(base));
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
                    let cursor = self.cursors.get(&t.id).copied().unwrap_or(0);
                    self.entries.insert(t.id.clone(), Entry { task: t, cursor });
                }
            }
        }
        removed
    }

    pub(super) fn revision(&self, task: &Task) -> (u64, u64) {
        (
            self.selection_revision,
            self.query_revisions
                .get(base_name(&task.name))
                .copied()
                .unwrap_or(0),
        )
    }

    fn begin_query(&mut self, task: &Task) {
        let n = self
            .query_revisions
            .entry(base_name(&task.name).to_string())
            .or_default();
        *n = n.wrapping_add(1);
    }

    pub(super) fn last_at(&self, id: &str) -> Option<i64> {
        self.stats.get(id).map(|s| s.last_at)
    }

    /// Carrega as notas guardadas no banco (arranque).
    pub fn load_stats(&mut self, rows: Vec<(String, Stat)>) {
        self.stats.extend(rows);
    }

    /// Fator aprendido de cada alvo `d:` para a seleção do catálogo: quanto a recompensa por consulta dele (com
    /// decaimento até `now`, encolhida para a referência quando há poucas consultas) passa da mediana dos alvos
    /// já testados. Sem referência (ninguém rendeu ainda) devolve vazio, e a seleção segue só a nota-base.
    pub fn boosts(&self, now: i64) -> HashMap<String, f64> {
        // (id, consultas, n, r) já decaídos, sem mexer nas notas
        let arms: Vec<(&str, u64, f64, f64)> = self
            .stats
            .iter()
            .filter(|(id, s)| id.starts_with("d:") && s.queries > 0)
            .map(|(id, s)| {
                let f = 0.5_f64.powf((now - s.updated_at).max(0) as f64 / HALF_LIFE);
                (id.as_str(), s.queries, s.n * f, s.r * f)
            })
            .collect();
        let mut rates: Vec<f64> = arms
            .iter()
            .filter(|a| a.1 >= NEW_QUERIES && a.2 > 0.0)
            .map(|a| a.3 / a.2)
            .collect();
        rates.sort_by(f64::total_cmp);
        let mid = rates.len() / 2;
        let reference = match rates.len() {
            0 => 0.0,
            n if n % 2 == 0 => (rates[mid - 1] + rates[mid]) / 2.0,
            _ => rates[mid],
        };
        if reference <= 0.0 {
            return HashMap::new();
        }
        arms.into_iter()
            .map(|(id, _, n, r)| {
                let shrunk = (r + BOOST_PRIOR_QUERIES * reference) / (n + BOOST_PRIOR_QUERIES);
                let factor = (shrunk / reference).clamp(BOOST_RANGE.0, BOOST_RANGE.1);
                (id.to_string(), factor)
            })
            .collect()
    }

    /// Notas alteradas desde a última vez, para gravar.
    pub fn take_dirty(&mut self) -> Vec<(String, Stat)> {
        let ids = std::mem::take(&mut self.dirty);
        ids.into_iter()
            .filter_map(|id| self.stats.get(&id).map(|s| (id, s.clone())))
            .collect()
    }

    fn life(&self) -> f64 {
        if self.life > 0.0 {
            self.life
        } else {
            harvest::DEFAULT_LIFE
        }
    }

    /// Recompensa esperada de consultar o braço agora (`harvest::value`).
    fn harvest_value(&self, id: &str, now: i64) -> f64 {
        let Some(s) = self.stats.get(id).filter(|s| s.queries > 0) else {
            return f64::INFINITY;
        };
        // escolhido sem nenhum resultado medido (consulta falhou, ou nota de antes da colheita): fica para o
        // desempate pelo UCB1 em vez de prender a agenda nele
        if s.exposure <= 0.0 {
            return 0.0;
        }
        harvest::value(s.r, s.exposure, now - s.last_at, self.life())
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
    #[cfg(test)]
    pub fn next(&mut self, now: i64) -> Option<Task> {
        self.next_matching(now, |_| true)
    }

    /// `next` só entre os alvos que `keep` aceita (sem os raros enquanto a leitura de anúncios está barrada).
    pub fn next_matching(&mut self, now: i64, keep: impl Fn(&Task) -> bool) -> Option<Task> {
        let external_id = self.external.clone();
        let eligible = |t: &Task| keep(t) && external_id.as_ref() != Some(&t.id);
        if !self.entries.values().any(|e| eligible(&e.task)) {
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
        self.last_exploring = exploring;
        let mut shadow = None;
        let pick = if exploring {
            // O slot de exploração gira entre os modos presentes (piso por modo, qualquer que seja o número de
            // braços de cada um) e, dentro do modo, vai para o menos testado (consultas decaídas).
            let mut modes: Vec<Mode> = self
                .entries
                .values()
                .filter(|e| eligible(&e.task))
                .map(|e| e.task.mode())
                .collect();
            modes.sort();
            modes.dedup();
            let mode = modes[self.explore_turn % modes.len()];
            self.explore_turn = self.explore_turn.wrapping_add(1);
            let external = self
                .external
                .as_ref()
                .and_then(|id| self.entries.get(id))
                .filter(|e| {
                    self.external_ready
                        && keep(&e.task)
                        && mode == Mode::Cheap
                        && (self.explore_turn / modes.len()).is_multiple_of(2)
                });
            external.or_else(|| {
                self.entries
                    .values()
                    .filter(|e| eligible(&e.task) && e.task.mode() == mode)
                    .min_by(|a, b| {
                        n_of(self, &a.task.id)
                            .total_cmp(&n_of(self, &b.task.id))
                            .then_with(|| tie(a, b))
                    })
            })
        } else {
            let ucb = |e: &Entry| self.index(&e.task.id, now, total_n, scale);
            let by_ucb = self
                .entries
                .values()
                .filter(|e| eligible(&e.task))
                .min_by(|a, b| ucb(b).total_cmp(&ucb(a)).then_with(|| tie(a, b)));
            if self.policy == Policy::Ucb {
                by_ucb
            } else {
                // colheita: maior estoque acumulado; empate (ex.: ninguém rendeu) segue o UCB1
                let hv = |e: &Entry| self.harvest_value(&e.task.id, now);
                let by_harvest =
                    self.entries
                        .values()
                        .filter(|e| eligible(&e.task))
                        .min_by(|a, b| {
                            hv(b)
                                .total_cmp(&hv(a))
                                .then_with(|| ucb(b).total_cmp(&ucb(a)))
                                .then_with(|| tie(a, b))
                        });
                let same = by_ucb.map(|e| &e.task.id) == by_harvest.map(|e| &e.task.id);
                shadow = Some(same);
                if self.policy == Policy::Harvest {
                    by_harvest
                } else {
                    by_ucb
                }
            }
        };
        let id = pick?.task.id.clone();
        if let Some(same) = shadow {
            self.shadow.0 += 1;
            self.shadow.1 += u64::from(same);
        }
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

    fn undo_unsent(
        &mut self,
        id: &str,
        now: i64,
        credit: (f64, f64, usize, HashMap<String, usize>),
    ) {
        if let Some(s) = self.stats.get_mut(id) {
            s.n = (s.n - 1.0).max(0.0);
            s.queries = s.queries.saturating_sub(1);
            if s.queries == 0 {
                s.first_at = now;
            }
            self.dirty.insert(id.to_string());
        }
        (self.credit, self.explore, self.explore_turn, self.cursors) = credit;
        for (id, e) in &mut self.entries {
            e.cursor = self.cursors.get(id).copied().unwrap_or(0);
        }
    }

    /// Resultado da consulta: lucro esperado das oportunidades novas (centavos) e o crédito de proximidade
    /// dos anúncios novos que não passaram (`learn::closeness_credit`).
    pub fn reward(&mut self, id: &str, now: i64, found: Found) {
        let Some(s) = self.stats.get_mut(id) else {
            return;
        };
        s.decay(now);
        let gap = (s.last_at > 0).then(|| now - s.last_at);
        s.exposure += harvest::exposure(
            gap,
            if self.life > 0.0 {
                self.life
            } else {
                harvest::DEFAULT_LIFE
            },
        );
        s.last_at = now;
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
        let Some(e) = self
            .entries
            .get_mut(&task.id)
            .filter(|_| !task.seeds.is_empty())
        else {
            return task;
        };
        if task.seeds.len() == 1 && e.task.seeds.len() > 1 {
            return task;
        }
        let range = task.seeds[e.cursor % task.seeds.len()];
        e.cursor = e.cursor.wrapping_add(1);
        self.cursors.insert(task.id.clone(), e.cursor);
        Task {
            seeds: vec![range],
            ..task
        }
    }

    pub(super) fn record_scan(&mut self, id: &str, now: i64) {
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
    /// Anúncios que ficaram na janela de quase-lucro (R$ 0,30 ou 10 % do preço); só o texto do painel.
    pub near_hits: u32,
    /// Soma do crédito de proximidade dos anúncios novos que não passaram, em centavos (`learn::closeness_credit`).
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
    // patterns: só o CSFloat; roda também sem saldo e com a Steam em pausa (429), quando a caça para
    super::patterns::maybe_step(h).await;
    // professor: só o banco; roda também sem saldo e com a Steam em pausa
    super::teacher::maybe_step(h).await;
    super::harvest::maybe_step(h).await;
    if let Err(e) = super::explore::refresh(h).await {
        tracing::warn!("exploração aguardando registro confiável: {e:#}");
        h.lock().queue.external_ready = false;
    }
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
    // Cada balde da Steam tem a sua pausa: com a leitura de anúncios barrada, a caça segue pelo livro de ofertas.
    let listings_blocked = h.steam.blocked(Endpoint::Listings);
    let book_blocked = h.steam.blocked(Endpoint::Book);
    if listings_blocked && book_blocked {
        set_idle_message(h);
        return Tick::Idle;
    }
    if let Err(e) = catalog::maybe_reselect(h).await {
        tracing::warn!("seleção falhou: {e:#}");
    }
    let (may_discover, empty, urgent) = {
        let s = h.lock();
        let urgent = overdue_task(&s, h.now(), listings_blocked).is_some();
        (s.queue.may_discover(), s.queue.len() == 0, urgent)
    };
    // a descoberta lê busca e histórico (os dois baldes)
    if (may_discover || empty)
        && (!listings_blocked || !h.steam.main_blocked(Endpoint::Legacy))
        && !book_blocked
        && !urgent
    {
        let tracked = crate::research::scope_tracked(
            h.db.clone(),
            decision(h, "descoberta", "catalogo"),
            async {
                if catalog::refresh_watched_history(h).await? {
                    Ok(true)
                } else if !listings_blocked {
                    catalog::step(h).await
                } else {
                    catalog::refresh_candidate_history(h).await
                }
            },
        )
        .await;
        let (result, attempts) = match tracked {
            Ok((result, context)) => {
                let attempts = crate::research::attempts(h.db.clone(), context.id)
                    .await
                    .unwrap_or(1);
                (result, attempts)
            }
            Err(e) => (Err(e), 0),
        };
        match result {
            Ok(true) => {
                h.lock().queue.spend_discovery();
                set_idle_message(h);
                return Tick::Discovery;
            }
            Ok(false) => {}
            Err(e) => {
                backoff(h, &e).await;
                if attempts > 0 {
                    h.lock().queue.spend_discovery();
                    return Tick::Discovery;
                }
            }
        }
    }
    if !book_blocked
        && !urgent
        && crate::research::scope(h.db.clone(), decision(h, "normal", "livro"), async {
            Ok(super::orders::maybe_step(h).await)
        })
        .await
        .unwrap_or(false)
    {
        return Tick::Book;
    }
    // A seleção é provisória até uma requisição sair; a falha continua contando se a Steam recebeu a tentativa.
    let (next, credit) = {
        let now = h.now();
        let mut s = h.lock();
        let credit = (
            s.queue.credit,
            s.queue.explore,
            s.queue.explore_turn,
            s.queue.cursors.clone(),
        );
        // Conferir ativo atrasado tem preferência sobre qualquer teste; usa a mesma vaga do laço.
        let overdue = overdue_task(&s, now, listings_blocked);
        if let Some(task) = overdue {
            s.queue.record_scan(&task.id, now);
            s.queue.last_exploring = false;
            (Some(task), credit)
        } else {
            let recent: HashSet<String> = s
                .floors
                .iter()
                .filter(|(id, (_, at))| {
                    now - at < super::COMMON_REVISIT_SECS
                        && !s.active.values().any(|a| a.task == id.as_str())
                })
                .map(|(id, _)| id.clone())
                .collect();
            (
                s.queue.next_matching(now, |t| {
                    (!listings_blocked || !t.rare()) && (t.rare() || !recent.contains(&t.id))
                }),
                credit,
            )
        }
    };
    let Some(task) = next else {
        set_idle_message(h);
        return Tick::Idle;
    };
    let source = {
        let s = h.lock();
        if s.active.values().any(|a| a.task == task.id) {
            "acompanhamento"
        } else if s.queue.external.as_ref() == Some(&task.id) {
            "exploracao_externa"
        } else if s.queue.last_exploring {
            "exploracao_interna"
        } else {
            "normal"
        }
    };
    let tracked = crate::research::scope_tracked(
        h.db.clone(),
        decision(h, source, &task.id),
        run_task(h, &task),
    )
    .await;
    let (result, attempts) = match tracked {
        Ok((result, context)) => match crate::research::attempts(h.db.clone(), context.id).await {
            Ok(n) => (result, n),
            Err(e) => {
                tracing::warn!("contagem de consultas indisponível: {e:#}");
                (result, 1)
            }
        },
        Err(e) => (Err(e), 0),
    };
    match result {
        Ok(()) => {
            set_idle_message(h);
        }
        Err(e)
            if e.downcast_ref::<SteamError>()
                .is_some_and(|e| matches!(e, SteamError::BudgetExhausted)) =>
        {
            h.lock().queue.external_ready = false;
        }
        Err(e) => backoff(h, &e).await,
    }
    // nada saiu para a Steam (ex.: anúncios barrados e o livro sem teto): o laço descansa em vez de girar em falso
    if attempts == 0 {
        h.lock().queue.undo_unsent(&task.id, h.now(), credit);
        return Tick::Idle;
    }
    if source == "exploracao_externa"
        && let Err(e) = super::explore::visited(h, &task, attempts).await
    {
        tracing::warn!("visita exploratória não gravada: {e:#}");
    }
    Tick::Watched(task.id)
}

pub(super) fn overdue_task(s: &super::Shared, now: i64, listings_blocked: bool) -> Option<Task> {
    s.active
        .values()
        .filter_map(|a| {
            let checked = a.opp.checked_at.unwrap_or(a.opp.seen_at);
            let entry = s.queue.entries.get(&a.task)?;
            (now - checked.max(a.attempted_at) >= ACTIVE_RECHECK_SECS
                && (!listings_blocked || !entry.task.rare()))
            .then_some((checked, a, &entry.task))
        })
        .min_by_key(|(checked, a, _)| (*checked, &a.opp.id))
        .map(|(_, a, t)| {
            let mut task = t.clone();
            if let Some(seed) = a.opp.seed
                && let Some(range) = t
                    .seeds
                    .iter()
                    .find(|(lo, hi)| (i64::from(*lo)..=i64::from(*hi)).contains(&seed))
            {
                task.seeds = vec![*range];
            }
            task
        })
}

fn decision(h: &Hunt, source: &str, target: &str) -> crate::research::Decision {
    let s = h.lock();
    let (policy, block_id) = s.trial.as_ref().map_or(("baseline".to_owned(), None), |t| {
        (t.phase.clone(), t.block_id)
    });
    crate::research::Decision {
        source: source.to_owned(),
        policy,
        target: target.to_owned(),
        scale: s.closeness_scale,
        block_id,
    }
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

fn can_cover(task: &Task, external: &Option<String>) -> bool {
    external.as_ref() != Some(&task.id)
        || crate::research::current().is_some_and(|c| c.source == "exploracao_externa")
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
            s.queue
                .tasks()
                .into_iter()
                .filter(|t| can_cover(t, &s.queue.external))
                .collect::<Vec<_>>(),
            s.ratio.map(|r| r.0),
            s.active
                .values()
                .map(|a| a.task.clone())
                .collect::<HashSet<_>>(),
        )
    };
    // Livro e busca gastariam a mesma fila que já pode entregar os anúncios completos.
    if h.steam.prefer_public_listings() {
        return query_group(h, &task, tasks).await;
    }
    let Some(ratio) = ratio.filter(|r| r.is_finite() && *r > 0.0) else {
        return book_then_listings(h, &task, tasks, &active).await;
    };
    let base = base_name(&task.name);
    let group: Vec<Task> = tasks
        .iter()
        .filter(|t| !t.rare() && base_name(&t.name) == base)
        .cloned()
        .collect();
    // Uma busca só economiza quando cobre duas ou mais versões vigiadas; com uma só, ela custaria a mais.
    if group.len() < 2 || h.steam.main_blocked(Endpoint::Search) {
        return book_then_listings(h, &task, tasks, &active).await;
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
    let v = match h
        .steam
        .get_json(
            crate::steam::client::Endpoint::Search,
            "/market/search/render/",
            &q,
        )
        .await
    {
        Ok(v) => v,
        Err(e @ (SteamError::RateLimited { .. } | SteamError::BreakerOpen { .. })) => {
            tracing::warn!("busca indisponível; continua pelos anúncios: {e}");
            return book_then_listings(h, &task, tasks, &active).await;
        }
        Err(e) => return Err(e.into()),
    };
    let page = search::parse_page(&v)?;
    let limits: HashMap<String, Option<i64>> = watch::list_for_scan(h)
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
        let before = crate::research::page_attempts().await?;
        let result = query_group(h, &t, targets).await;
        if crate::research::page_attempts().await? > before {
            let mut s = h.lock();
            s.queue.credit = (s.queue.credit + DISCOVERY_SHARE).min(CREDIT_CAP);
        }
        result?;
    }
    Ok(())
}

/// Alvo comum sem outra versão para a busca cobrir: o livro de ofertas diz o menor anúncio em reais, e a Steam o
/// conta num balde à parte da leitura de anúncios (o que leva os bloqueios de horas). Acima do teto de compra, os
/// anúncios não são lidos; sem teto, com o livro barrado ou com oportunidade ativa, lê como antes.
async fn book_then_listings(
    h: &Arc<Hunt>,
    task: &Task,
    tasks: Vec<Task>,
    active: &HashSet<String>,
) -> anyhow::Result<()> {
    let mut lead = None;
    if !active.contains(&task.id) && !h.steam.blocked(Endpoint::Book) {
        let max = watch::list_for_scan(h)
            .await?
            .into_iter()
            .find(|r| r.name == task.name && r.mode == "barato")
            .and_then(|r| r.max_buy_cents);
        if let Some(max) = max {
            match book::fetch_book(&h.steam, &task.name).await {
                Ok(b) => {
                    let now = h.now();
                    let (lowest, second) = (b.min_sell_cents, b.second_ask());
                    // o livro lido também serve aos pedidos sugeridos (`orders`), ao teto (concorrente) e ao registro
                    let before = super::orders::remember_book(h, &task.name, now, Some(b), false)
                        .await
                        .and_then(|b| b.min_sell_cents);
                    let mut s = h.lock();
                    match lowest {
                        None => {
                            s.cycle.saved_listings += 1;
                            return Ok(());
                        }
                        Some(p) if !affordable(p, max) => {
                            // menor anúncio novo ensina a agenda como um anúncio que não passou: g ≈ (teto − preço) ÷ preço
                            let near_credit = if before == Some(p) {
                                0.0
                            } else {
                                learn::closeness_credit(
                                    p,
                                    catalog::min_profit(p) + max - p,
                                    s.closeness_scale,
                                )
                            };
                            s.cycle.saved_listings += 1;
                            s.queue.reward(
                                &task.id,
                                now,
                                Found {
                                    near_credit,
                                    ..Found::default()
                                },
                            );
                            return Ok(());
                        }
                        Some(p) => lead = Some((p, max, second)),
                    }
                }
                Err(e) => tracing::warn!("livro de {} falhou; lê os anúncios: {e}", task.name),
            }
        }
    }
    if h.steam.blocked(Endpoint::Listings) && h.steam.blocked(Endpoint::Legacy) {
        if let Some((p, max, second)) = lead {
            tracing::info!(
                preco = p,
                teto = max,
                segundo = ?second,
                "livro: {} tem anúncio que cabe no teto, mas a leitura de anúncios está barrada pela Steam",
                task.name
            );
        }
        return Ok(());
    }
    query_group(h, task, tasks).await
}

async fn query_group(h: &Arc<Hunt>, task: &Task, tasks: Vec<Task>) -> anyhow::Result<()> {
    h.lock().queue.begin_query(task);
    let mut targets: Vec<Task> = tasks.into_iter().filter(|t| same_query(task, t)).collect();
    {
        let mut s = h.lock();
        let others = s.queue.tasks();
        for other in others {
            if targets.iter().any(|t| t.id == other.id) {
                continue;
            }
            if !can_cover(&other, &s.queue.external) {
                continue;
            }
            let focused = if !other.seeds.is_empty() {
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
    {
        let mut s = h.lock();
        for a in s
            .active
            .values_mut()
            .filter(|a| targets.iter().any(|t| t.id == a.task))
        {
            if task.seeds.first().is_none_or(|&(lo, hi)| {
                a.opp
                    .seed
                    .is_some_and(|seed| (i64::from(lo)..=i64::from(hi)).contains(&seed))
            }) {
                a.attempted_at = h.now();
            }
        }
    }
    let (mut page, mut named_only) = if task.rare() {
        (
            listings::query_listings_wear(&h.steam, &task.code, 0, task.wear, &task.filters())
                .await?,
            false,
        )
    } else {
        listings::query_listings_named(&h.steam, &task.name, &task.code, 0, task.wear).await?
    };
    let mut queried = task;
    let mut seen = HashSet::new();
    loop {
        let now = h.now();
        let mut key = super::ledger::query_key(queried);
        if named_only {
            // A página por nome não prova sumiço nem idade na consulta nativa que cobre todas as versões.
            key.push_str(&format!("|legacy:{}", queried.name));
        }
        super::ledger::count_hour(h, now, "queries", 1).await;
        match super::ledger::record(h, &key, &page, now).await {
            Ok(r) if r != super::ledger::Recorded::default() => tracing::debug!(?r, "registro"),
            Ok(_) => {}
            Err(e) => tracing::warn!("registro de observações falhou: {e:#}"),
        }
        let mut near = 0;
        for t in targets
            .iter()
            .filter(|t| !named_only || t.name == queried.name)
        {
            let out = detect::process(h, t, &page).await?;
            near += out.rewarded.near_hits;
            h.lock().queue.reward(&t.id, h.now(), out.rewarded);
        }
        super::ledger::count_hour(h, now, "near", near).await;
        if !named_only {
            break;
        }
        seen.insert(queried.name.as_str());
        let Some(next) = targets.iter().find(|t| !seen.contains(t.name.as_str())) else {
            break;
        };
        queried = next;
        (page, named_only) =
            listings::query_listings_named(&h.steam, &queried.name, &queried.code, 0, queried.wear)
                .await?;
    }
    Ok(())
}

/// Ritmo do último 429 (texto de `SteamClient::restore_pace`), retomado ao subir.
const PACE_KEY: &str = "steam_pace";

/// Retoma o ritmo salvo e passa a salvar a cada 429, venha de qual endpoint vier.
pub async fn load_pace(h: &Hunt) -> anyhow::Result<()> {
    let saved = catalog::get_setting(&h.db, PACE_KEY).await?;
    if let Some(v) = &saved {
        h.steam.restore_pace(v);
    }
    if saved.as_deref().is_none_or(|v| !v.contains(';')) {
        // Versões antigas não salvaram essas filas; as recusas recentes permitem uma migração cautelosa.
        let limits: Vec<(String, i64)> = h.db.call(|c| {
            c.prepare("SELECT route, max(at) FROM research_attempts WHERE endpoint IN ('legacy','book','search') AND outcome='rate_limited' AND route IN ('main','alt') AND at>=?1 GROUP BY route")?
                .query_map([crate::db::now_unix() - 1800], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        }).await?;
        h.steam.restore_public_backoff(
            limits.iter().find(|(r, _)| r == "main").map(|(_, at)| *at),
            limits.iter().find(|(r, _)| r == "alt").map(|(_, at)| *at),
        );
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
    h.steam.checkpoint_pace();
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
    h.steam.checkpoint_pace();
    let (rows, progress) = {
        let mut s = h.lock();
        let q = &mut s.queue;
        let progress = serde_json::to_string(&Progress {
            credit: q.credit,
            explore: q.explore,
            turn: q.explore_turn,
            cursors: q.cursors.clone(),
        })?;
        (q.take_dirty(), progress)
    };
    h.db.call(move |c| {
        let tx = c.transaction()?;
        for (id, s) in rows {
            tx.execute(
                "INSERT OR REPLACE INTO arm_stats(id, n, r, queries, finds, gain_cents, near_hits, first_at, reward_at, updated_at,
                    exposure, last_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                    s.updated_at,
                    s.exposure,
                    s.last_at
                ],
            )?;
        }
        tx.execute("INSERT INTO settings(key,value) VALUES ('queue_progress',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[progress])?;
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
                "SELECT id, n, r, queries, finds, gain_cents, near_hits, first_at, reward_at, updated_at, exposure, last_at
                 FROM arm_stats",
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
                        exposure: r.get(10)?,
                        last_at: r.get(11)?,
                    },
                ))
            })?
            .collect()
        })
        .await?;
    h.lock().queue.load_stats(rows);
    if let Some(json) = catalog::get_setting(&h.db, "queue_progress").await?
        && let Ok(progress) = serde_json::from_str::<Progress>(&json)
    {
        let mut s = h.lock();
        s.queue.credit = progress.credit.clamp(0.0, CREDIT_CAP);
        s.queue.explore = progress.explore.clamp(0.0, 1.0);
        s.queue.explore_turn = progress.turn;
        s.queue.cursors = progress.cursors;
    }
    Ok(())
}

/// Laço da caça. Termina quando o processo termina.
pub async fn run(h: Arc<Hunt>) {
    let mut saved = h.now();
    let mut parallel = super::parallel::Reader::default();
    loop {
        parallel.step(&h).await;
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
    fn external_candidate_cannot_win_normal_slots_or_run_without_credit() {
        let mut q = Queue::default();
        q.set_tasks(vec![task("normal", 1.0), task("outside", 1000.0)]);
        q.external = Some("outside".into());
        for now in 0..200 {
            assert_eq!(q.next(now).unwrap().id, "normal");
        }
        q.external_ready = true;
        let mut tests = 0;
        for now in 200..1200 {
            if q.next(now).unwrap().id == "outside" {
                assert!(q.last_exploring);
                tests += 1;
            }
        }
        assert!(tests > 0 && tests <= 125);
    }

    #[tokio::test]
    async fn normal_group_cannot_pull_in_an_external_candidate() {
        let (db, _dir) = crate::db::temp_db();
        let external = Some("outside".into());
        let d = crate::research::Decision {
            source: "normal".into(),
            policy: "control".into(),
            target: "normal".into(),
            scale: 0.03,
            block_id: Some(1),
        };
        crate::research::scope(db.clone(), d.clone(), async {
            assert!(!can_cover(&task("outside", 1.0), &external));
            assert!(can_cover(&task("normal", 1.0), &external));
            Ok(())
        })
        .await
        .unwrap();
        let mut d = d;
        d.source = "exploracao_externa".into();
        crate::research::scope(db, d, async {
            assert!(can_cover(&task("outside", 1.0), &external));
            Ok(())
        })
        .await
        .unwrap();
    }

    #[test]
    fn seed_cursor_survives_removal_and_persisted_progress() {
        let mut t = task("r:seed", 1.0);
        t.seeds = vec![(1, 1), (9, 9)];
        let mut q = Queue::default();
        q.set_tasks(vec![t.clone()]);
        assert_eq!(q.focus(t.clone()).seeds, vec![(1, 1)]);
        let json = serde_json::to_string(&Progress {
            cursors: q.cursors.clone(),
            ..Progress::default()
        })
        .unwrap();
        q.set_tasks(vec![]);
        q.set_tasks(vec![t.clone()]);
        assert_eq!(q.focus(t.clone()).seeds, vec![(9, 9)]);
        let mut restart = Queue {
            cursors: serde_json::from_str::<Progress>(&json).unwrap().cursors,
            ..Queue::default()
        };
        restart.set_tasks(vec![t.clone()]);
        assert_eq!(restart.focus(t).seeds, vec![(9, 9)]);
    }

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
            float: None,
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
                    near_credit: 3.0, // crédito de 3 centavos, escrito à mão
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
    fn boosts_reward_yield_and_fade_back_to_one() {
        let stat = |n: f64, r: f64, queries: u64| Stat {
            n,
            r,
            queries,
            ..Stat::default()
        };
        let mut q = Queue::default();
        q.load_stats(vec![
            ("d:good".into(), stat(100.0, 1000.0, 100)),
            ("d:mid1".into(), stat(100.0, 100.0, 100)),
            ("d:mid2".into(), stat(100.0, 100.0, 100)),
            ("d:bad".into(), stat(100.0, 50.0, 100)),
            ("d:new".into(), stat(5.0, 0.0, 5)), // menos de 10 consultas: fora da referência
            ("r:rare".into(), stat(100.0, 5000.0, 100)), // só `d:` entra
        ]);
        // referência = mediana de r/n de good, mid1, mid2 e bad = (1 + 1) / 2 = 1
        let b = q.boosts(0);
        assert_eq!(b["d:good"], 4.0); // teto
        assert!(b["d:bad"] > 0.25 && b["d:bad"] < 1.0, "{b:?}"); // 70 / 120 ≈ 0,58
        assert!((b["d:mid1"] - 1.0).abs() < 1e-9, "{b:?}");
        assert!(!b.contains_key("r:rare"));
        // 10 meias-vidas sem consulta: tudo volta para perto de 1
        let late = q.boosts(10 * 86_400);
        assert!((late["d:good"] - 1.0).abs() < 0.2, "{late:?}");
        assert!((late["d:bad"] - 1.0).abs() < 0.2, "{late:?}");
        // notas antigas sem recompensa (instalação sobre `arm_stats` velho): nenhuma referência
        let mut empty = Queue::default();
        empty.load_stats(vec![
            ("d:a".into(), stat(100.0, 0.0, 100)),
            ("d:b".into(), stat(50.0, 0.0, 50)),
        ]);
        assert!(empty.boosts(0).is_empty());
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
    fn harvest_pick_prefers_stale_productive_target() {
        // dois alvos com a mesma taxa medida: "fresh" consultado há 30 s, "stale" há 30 min
        let now = 100_000;
        let stat = |last_at| Stat {
            n: 10.0,
            r: 600.0,
            queries: 10,
            first_at: 1,
            reward_at: now - 60,
            updated_at: now,
            exposure: 6000.0,
            last_at,
            ..Stat::default()
        };
        let queue = |policy| {
            let mut q = Queue {
                policy,
                ..Queue::default()
            };
            q.set_tasks(vec![task("fresh", 1.0), task("stale", 1.0)]);
            q.load_stats(vec![
                ("fresh".into(), stat(now - 30)),
                ("stale".into(), stat(now - 1800)),
            ]);
            q
        };
        // UCB1 empata e desempata pelo id; a colheita vai para o estoque maior
        let mut q = queue(Policy::Ucb);
        assert_eq!(q.next(now).unwrap().id, "fresh");
        assert_eq!(q.shadow, (0, 0));
        let mut q = queue(Policy::Harvest);
        assert_eq!(q.next(now).unwrap().id, "stale");
        assert_eq!(q.shadow, (1, 0));
        // em sombra o UCB1 escolhe e a discordância fica contada
        let mut q = queue(Policy::default());
        assert_eq!(q.next(now).unwrap().id, "fresh");
        assert_eq!(q.shadow, (1, 0));
        // o resultado da consulta soma exposição e marca a hora
        q.reward("fresh", now + 10, Found::default());
        let s = &q.stats["fresh"];
        assert_eq!(s.last_at, now + 10);
        assert!(s.exposure > 6000.0 * 0.99 && s.exposure < 6000.0 + 40.0);
        // alvo escolhido sem resultado medido não prende a colheita: perde para o que tem estoque
        let mut q = queue(Policy::Harvest);
        q.load_stats(vec![(
            "stale".into(),
            Stat {
                exposure: 0.0,
                last_at: 0,
                ..stat(0)
            },
        )]);
        assert_eq!(q.next(now).unwrap().id, "fresh");
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
