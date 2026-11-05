//! Pedidos de compra sugeridos (decisão do Erick: só sugerir; quem abre o pedido na Steam é ele). Uma consulta
//! ao livro a cada `GAP`, sempre do alvo comum com o livro mais velho; `watch::list` sugere 1 centavo acima do
//! maior pedido quando esse preço ainda dá lucro.

use super::Hunt;
use crate::steam::book;

/// shortcut: 60 s é ~1 em 10 consultas no ritmo de 6 s; subir se os pedidos renderem mais que a caça.
const GAP: i64 = 60;
/// Livro mais novo que isso não é lido de novo.
const TTL: i64 = 30 * 60;
/// Livro mais velho que isso não vira sugestão: o topo da fila já pode ter mudado.
pub const STALE: i64 = 60 * 60;

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
        name
    };
    // Falha também entra (sem livro): o alvo vai para o fim da fila e a sugestão velha some.
    let b = book::fetch_book(&h.steam, &name)
        .await
        .inspect_err(|e| tracing::warn!("livro de pedidos de {name} falhou: {e}"))
        .ok();
    h.lock().books.insert(name, (now, b));
    true
}
