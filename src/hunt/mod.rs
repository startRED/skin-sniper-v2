//! Caça: descobre o que vigiar dado o saldo (`catalog`), decide quem consultar a seguir dentro do ritmo do
//! limitador (`scheduler`) e transforma anúncios novos em oportunidades (`detect`). Ver docs/projeto.md, "Caça".
//!
//! `Hunt` é o estado compartilhado: o laço (`scheduler::run`) e o servidor mexem nele; nenhum lock
//! atravessa um `await`.

pub mod catalog;
pub mod detect;
pub mod orders;
pub mod patterns;
pub mod scheduler;
pub mod watch;

use crate::csfloat::{CsfloatClient, CsfloatSale};
use crate::db::{Db, now_unix};
use crate::notify::Telegram;
use crate::pricing::combo::{self, Candidate};
use crate::settings::SettingsStore;
use crate::steam::client::SteamClient;
use crate::web::api::{
    Combo, Event, Hunt as HuntView, KeyStatus, Keys, Opportunity, SaldoSource, State,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::{Notify, broadcast};

/// O que a caça precisa de fora.
pub struct HuntDeps {
    pub db: Db,
    pub steam: Arc<SteamClient>,
    pub csfloat: Arc<CsfloatClient>,
    pub events: broadcast::Sender<Event>,
    pub settings: SettingsStore,
    /// Aviso de oportunidade que passa no critério; `None` desliga (testes).
    pub telegram: Option<Arc<Telegram>>,
}

/// Oportunidade ativa em memória, com a tarefa que a achou.
pub(crate) struct Active {
    pub opp: Opportunity,
    pub task: String,
}

/// Menor anúncio por (nome, grupo raro ou nenhum).
pub(crate) type Rivals = HashMap<(String, Option<String>), i64>;

pub(crate) struct Shared {
    pub saldo: i64,
    pub source: SaldoSource,
    pub keys: Keys,
    pub queue: scheduler::Queue,
    pub disc: catalog::Discovery,
    /// Razão BRL/USD medida e quando.
    pub ratio: Option<(f64, i64)>,
    /// Saldo e hora da última seleção (para re-selecionar quando muda > 20 % ou passa 24 h).
    pub selected: Option<(i64, i64)>,
    pub active: HashMap<String, Active>,
    pub combo: Combo,
    pub message: Option<String>,
    /// Vendas do CSFloat por nome: (quando buscou, vendas). Cache de 6 h.
    pub csfloat_cache: HashMap<String, (i64, Vec<CsfloatSale>)>,
    /// Consultas seguidas que vieram vazias por tarefa (uma página vazia pode ser transitória).
    pub empty_streak: HashMap<String, u8>,
    pub last_state_pub: i64,
    /// Último (preço, `history_at`, menor concorrente) com que cada anúncio foi avaliado: muda → avalia de novo.
    pub evaluated: HashMap<String, (i64, i64, Option<i64>)>,
    /// Balanço do ciclo de 10 min (o que foi conferido e o que chegou perto).
    pub cycle: detect::Cycle,
    /// Por tarefa: menor anúncio visto na última consulta (se havia algum) e quando foi.
    pub floors: HashMap<String, (Option<i64>, i64)>,
    /// Raro, por faixa (`Task::floor_key`): quando foi vista e o menor anúncio por (nome, grupo). Concorrente de
    /// outra faixa do mesmo grupo não aparece na página consultada.
    pub rivals: HashMap<String, (i64, Rivals)>,
    /// Desconto do histórico pelas vendas reais, `[desconto, raro]` (`web::calibration::correction`).
    pub correction: [f64; 2],
    /// Topo do livro de pedidos por nome: (quando tentou, livro; `None` se a leitura falhou). `orders` atualiza; `watch` sugere.
    pub books: HashMap<String, (i64, Option<crate::steam::book::Book>)>,
    /// Última leitura de livro (0 = ainda não ligou).
    pub book_at: i64,
    /// Patterns aprendidos das vendas do CSFloat (`patterns`).
    pub learned: patterns::Learned,
    /// Última leitura do coletor de patterns (0 = ainda não ligou) e quando cada skin foi lida.
    pub learn_at: i64,
    pub learn_seen: HashMap<String, i64>,
}

pub struct Hunt {
    pub(crate) db: Db,
    pub(crate) steam: Arc<SteamClient>,
    pub(crate) csfloat: Arc<CsfloatClient>,
    pub(crate) events: broadcast::Sender<Event>,
    pub(crate) settings: SettingsStore,
    pub(crate) telegram: Option<Arc<Telegram>>,
    /// Relógio injetável para os testes.
    pub(crate) now: fn() -> i64,
    pub(crate) sh: Mutex<Shared>,
    /// Acorda o laço quando o saldo muda.
    pub(crate) wake: Notify,
}

fn no_key() -> KeyStatus {
    KeyStatus {
        set: false,
        ok: None,
        detail: None,
    }
}

impl Hunt {
    pub fn new(deps: HuntDeps) -> Arc<Self> {
        Self::with_clock(deps, now_unix)
    }

    pub(crate) fn with_clock(deps: HuntDeps, now: fn() -> i64) -> Arc<Self> {
        Arc::new(Hunt {
            db: deps.db,
            steam: deps.steam,
            csfloat: deps.csfloat,
            events: deps.events,
            settings: deps.settings,
            telegram: deps.telegram,
            now,
            wake: Notify::new(),
            sh: Mutex::new(Shared {
                saldo: 0,
                source: SaldoSource::None,
                keys: Keys {
                    steam: no_key(),
                    csfloat: no_key(),
                    telegram: no_key(),
                },
                queue: scheduler::Queue::default(),
                disc: catalog::Discovery::default(),
                ratio: None,
                selected: None,
                active: HashMap::new(),
                combo: Combo {
                    ids: vec![],
                    cost_cents: 0,
                    profit_cents: 0,
                },
                message: Some("caça ainda não começou".into()),
                csfloat_cache: HashMap::new(),
                empty_streak: HashMap::new(),
                last_state_pub: 0,
                evaluated: HashMap::new(),
                cycle: detect::Cycle::default(),
                floors: HashMap::new(),
                rivals: HashMap::new(),
                correction: [1.0; 2],
                books: HashMap::new(),
                book_at: 0,
                learned: patterns::Learned::new(),
                learn_at: 0,
                learn_seen: HashMap::new(),
            }),
        })
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Shared> {
        self.sh.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn now(&self) -> i64 {
        (self.now)()
    }

    pub fn saldo(&self) -> i64 {
        self.lock().saldo
    }

    /// Saldo novo (manual ou lido da Steam). O laço reseleciona quando a mudança passa de 20 %.
    pub fn set_saldo(&self, cents: i64, source: SaldoSource) {
        {
            let mut s = self.lock();
            s.saldo = cents.max(0);
            s.source = source;
        }
        self.wake.notify_one();
    }

    pub fn set_keys(&self, f: impl FnOnce(&mut Keys)) {
        f(&mut self.lock().keys);
        self.wake.notify_one();
    }

    /// Carrega do banco o que sobreviveu a um reinício: oportunidades ainda ativas e a razão BRL/USD.
    pub async fn load(&self) -> anyhow::Result<()> {
        let rows: Vec<(String, Option<String>)> = self
            .db
            .call(|c| {
                c.prepare("SELECT json, task FROM opportunities WHERE gone_at IS NULL")?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect()
            })
            .await?;
        let ratio = catalog::load_ratio(&self.db).await?;
        scheduler::load_stats(self).await?;
        self.refresh_correction().await?;
        scheduler::load_pace(self).await?;
        patterns::load(self).await?;
        {
            let mut s = self.lock();
            for (json, task) in rows {
                if let Ok(opp) = serde_json::from_str::<Opportunity>(&json) {
                    s.active.insert(
                        opp.id.clone(),
                        Active {
                            opp,
                            task: task.unwrap_or_default(),
                        },
                    );
                }
            }
            s.ratio = ratio;
        }
        self.recompute_combo();
        Ok(())
    }

    /// Relê as vendas registradas; chamado ao subir e a cada venda informada no painel.
    pub async fn refresh_correction(&self) -> anyhow::Result<()> {
        let c = crate::web::calibration::load_correction(&self.db).await?;
        let mut s = self.lock();
        if c != s.correction {
            tracing::info!(desconto = c[0], raro = c[1], "fator das vendas reais");
            s.correction = c;
            // Previsões mudaram: cada anúncio volta a ser avaliado na próxima consulta.
            s.evaluated.clear();
        }
        Ok(())
    }

    /// Fator das vendas reais para o tipo (raro = item de grupo).
    pub(crate) fn correction(&self, rare: bool) -> f64 {
        self.lock().correction[usize::from(rare)]
    }

    /// Melhor combinação das oportunidades ativas que cabe no saldo (lucro esperado ajustado ao risco).
    pub(crate) fn recompute_combo(&self) {
        let mut s = self.lock();
        let cands: Vec<Candidate> = s
            .active
            .values()
            .filter_map(|a| {
                let profit_cents = a.opp.ev_profit_cents.filter(|profit| *profit > 0)?;
                Some(Candidate {
                    id: a.opp.id.clone(),
                    name: a.opp.name.clone(),
                    cost_cents: a.opp.price_cents,
                    profit_cents,
                })
            })
            .collect();
        let c = combo::best_combo(&cands, s.saldo);
        s.combo = Combo {
            ids: c.ids,
            cost_cents: c.cost_cents,
            profit_cents: c.profit_cents,
        };
    }

    /// `State` completo: saldo, estado da caça, combo e chaves.
    pub fn state(&self) -> State {
        let stats = self.steam.stats();
        let s = self.lock();
        let paused = stats.paused_until;
        State {
            saldo_cents: s.saldo,
            saldo_source: s.source,
            hunt: HuntView {
                watching: s.queue.len() as u32,
                queries_per_min: f64::from(stats.queries_per_min),
                last_query_at: stats.last_query_at,
                paused_until: paused,
                message: if paused.is_some() {
                    Some("a Steam pediu calma (429); caça em pausa".into())
                } else {
                    let now = self.now();
                    s.message.clone().or_else(|| s.cycle.message(now))
                },
            },
            combo: s.combo.clone(),
            keys: s.keys.clone(),
        }
    }

    pub(crate) fn publish_state(&self) {
        self.lock().last_state_pub = self.now();
        let _ = self.events.send(Event::State(self.state()));
    }

    pub(crate) fn set_message(&self, m: Option<String>) {
        self.lock().message = m;
    }
}

#[cfg(test)]
mod tests;
