//! Precificação: funções puras (sem rede nem banco). Ver docs/projeto.md, "Precificação".
#![allow(dead_code)]

pub mod combo;
pub mod fees;
pub mod groups;
pub mod premium;
pub mod sell;

/// `1234` → `R$ 12,34`; negativo → `-R$ 1,20`.
pub fn brl(cents: i64) -> String {
    let c = cents.unsigned_abs();
    format!(
        "{}R$ {},{:02}",
        if cents < 0 { "-" } else { "" },
        c / 100,
        c % 100
    )
}
