//! Pedidos de compra sugeridos (decisão do Erick: só sugerir; quem abre o pedido na Steam é ele). Uma consulta
//! ao livro a cada `GAP`, sempre do alvo comum com o livro mais velho; `watch::list` sugere 1 centavo acima do
//! maior pedido quando esse preço ainda dá lucro.

use super::Hunt;
use crate::steam::book::{self, Book};
use rusqlite::params;

/// shortcut: 60 s é ~1 em 10 consultas no ritmo de 6 s; subir se os pedidos renderem mais que a caça.
pub(super) const GAP: i64 = 60;
/// Livro mais novo que isso não é lido de novo.
pub(super) const TTL: i64 = 30 * 60;
/// Livro mais velho que isso não vira sugestão: o topo da fila já pode ter mudado.
pub const STALE: i64 = 60 * 60;
/// Livro lido sem mudança é registrado de novo depois disso: o professor precisa saber o menor anúncio de cada dia.
const LOG_HEARTBEAT: i64 = 12 * 3600;

/// `true` = gastou a vaga desta volta com um livro (deu certo ou não).
pub async fn maybe_step(h: &Hunt) -> bool {
    let now = h.now();
    let name = {
        let mut s = h.lock();
        // o primeiro livro só depois de `GAP` ligado: a caça começa antes
        if s.book_at == 0 {
            s.book_at = now;
        }
        if now - s.book_at < GAP {
            return false;
        }
        // o livro que o professor pede para rotular uma compra virtual (dias 7–9) vem antes
        if let Some(name) = s.label_books.pop() {
            s.book_at = now;
            (name, true)
        } else {
            let pick = s
                .queue
                .tasks()
                .into_iter()
                .filter(|t| !t.rare())
                .map(|t| (s.books.get(&t.name).map_or(0, |b| b.0), t.name))
                .filter(|(at, _)| now - at >= TTL)
                .min();
            let Some((_, name)) = pick else {
                return false;
            };
            s.book_at = now;
            (name, false)
        }
    };
    let (name, force) = name;
    // Falha também entra (sem livro): o alvo vai para o fim da fila e a sugestão velha some.
    let b = book::fetch_book(&h.steam, &name)
        .await
        .inspect_err(|e| tracing::warn!("livro de pedidos de {name} falhou: {e}"))
        .ok();
    remember_book(h, &name, now, b, force).await;
    true
}

/// Guarda a leitura em `Shared::books` (o que os pedidos e o teto usam) e, quando o topo mudou (maior pedido,
/// número de anúncios ou os degraus de venda), no registro do livro (`book_snapshots`); sem mudança, registra de
/// novo a cada `LOG_HEARTBEAT` ou quando `force` (leitura pedida pelo professor). Devolve a leitura anterior.
pub(crate) async fn remember_book(
    h: &Hunt,
    name: &str,
    now: i64,
    b: Option<Book>,
    force: bool,
) -> Option<Book> {
    let before = h
        .lock()
        .books
        .insert(name.to_string(), (now, b.clone()))
        .and_then(|(_, b)| b);
    let Some(b) = b else {
        return before;
    };
    super::ledger::count_hour(h, now, "books", 1).await;
    let same = before.as_ref().is_some_and(|p| {
        (p.max_buy_cents, p.sell_orders, &p.sell_levels)
            == (b.max_buy_cents, b.sell_orders, &b.sell_levels)
    });
    let stale = h
        .lock()
        .book_logged
        .get(name)
        .is_none_or(|&at| now - at >= LOG_HEARTBEAT);
    if !same || force || stale {
        h.lock().book_logged.insert(name.to_string(), now);
        let (n, levels) = (
            name.to_string(),
            serde_json::to_string(&b.sell_levels).unwrap_or_default(),
        );
        let row = (b.max_buy_cents, b.buy_orders, b.sell_orders);
        if let Err(error) = h
            .db
            .call(move |c| {
                c.execute(
                    "INSERT OR REPLACE INTO book_snapshots(name, at, max_buy_cents, buy_orders, sell_orders, sell_levels)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![n, now, row.0, row.1, row.2, levels],
                )
            })
            .await
        {
            tracing::warn!(%error, "registro do livro falhou");
        }
    }
    before
}
