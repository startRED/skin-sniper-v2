//! Caça: descobre o que vigiar dado o saldo (`catalog`), decide quem consultar a seguir dentro do ritmo do
//! limitador (`scheduler`) e transforma anúncios novos em oportunidades (`detect`). Ver docs/projeto.md, "Caça".
//!
//! `Hunt` é o estado compartilhado: o laço (`scheduler::run`) e o servidor mexem nele; nenhum lock
//! atravessa um `await`.

pub mod catalog;
pub mod detect;
pub mod scheduler;

use crate::csfloat::{CsfloatClient, CsfloatSale};
use crate::db::{Db, now_unix};
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
}

/// Oportunidade ativa em memória, com a tarefa que a achou.
pub(crate) struct Active {
    pub opp: Opportunity,
    pub task: String,
}

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
}

pub struct Hunt {
    pub(crate) db: Db,
    pub(crate) steam: Arc<SteamClient>,
    pub(crate) csfloat: Arc<CsfloatClient>,
    pub(crate) events: broadcast::Sender<Event>,
    pub(crate) settings: SettingsStore,
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

    /// Melhor combinação das oportunidades ativas que cabe no saldo (lucro no preço médio).
    pub(crate) fn recompute_combo(&self) {
        let mut s = self.lock();
        let cands: Vec<Candidate> = s
            .active
            .values()
            .map(|a| Candidate {
                id: a.opp.id.clone(),
                cost_cents: a.opp.price_cents,
                profit_cents: a.opp.sell.mid.profit_cents,
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
                    s.message.clone()
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
