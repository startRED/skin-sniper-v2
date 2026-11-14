//! Segunda fonte do prêmio: tabela externa de preço por grupo de seed, gerada fora do app
//! (`research/cs2pattern-prices.json`). Dois usos em `patterns`: entra como mais um nível do encolhimento do
//! prêmio e, se ela mostra bem menos prêmio que as vendas daqui (grupo medido a 1,00× lá), o seed é pulado
//! (`rarity::diverges`).
//!
//! Formato esperado (hipótese, o arquivo ainda não existia quando isto foi escrito; ajustar `parse` ao real):
//! `{"skins": {"Glock-18 | Moonrise": [{"seeds": [59, 694], "multiple": 1.0}]}}`, `multiple` = preço do grupo ÷
//! preço comum da skin. Arquivo ausente ou ilegível = sem segunda fonte, nunca erro nem dado inventado.

use serde::Deserialize;
use std::collections::HashMap;

/// Onde a pesquisa grava; `SKIN_SNIPER_EXTERNAL_PRICES` troca.
const DEFAULT_PATH: &str = r"E:\Documentos\ProjetosIA\Skin Sniper\research\cs2pattern-prices.json";

#[derive(Deserialize)]
struct File {
    skins: HashMap<String, Vec<Group>>,
}
#[derive(Deserialize)]
struct Group {
    seeds: Vec<u16>,
    multiple: f64,
}

#[derive(Debug, Default, Clone)]
pub struct External(HashMap<String, Vec<(Vec<u16>, f64)>>);

impl External {
    pub fn parse(json: &str) -> serde_json::Result<External> {
        let f: File = serde_json::from_str(json)?;
        Ok(External(
            f.skins
                .into_iter()
                .map(|(k, v)| {
                    let v = v
                        .into_iter()
                        .filter(|g| g.multiple.is_finite() && g.multiple > 0.0)
                        .map(|g| (g.seeds, g.multiple))
                        .collect();
                    (k, v)
                })
                .collect(),
        ))
    }

    /// Lê o arquivo; vazio se não existe (o normal enquanto a pesquisa não termina) ou não parseia (avisa).
    pub fn read() -> External {
        let path =
            std::env::var("SKIN_SNIPER_EXTERNAL_PRICES").unwrap_or_else(|_| DEFAULT_PATH.into());
        let Ok(text) = std::fs::read_to_string(&path) else {
            return External::default();
        };
        External::parse(&text).unwrap_or_else(|e| {
            tracing::warn!("tabela externa de prêmio ilegível em {path}: {e}");
            External::default()
        })
    }

    /// Prêmio (× o comum) que a tabela dá ao seed da skin.
    pub fn multiple(&self, base: &str, seed: u16) -> Option<f64> {
        self.0
            .get(base)?
            .iter()
            .find(|(seeds, _)| seeds.contains(&seed))
            .map(|(_, m)| *m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_expected_shape_and_ignores_bad_rows() {
        let e = External::parse(
            r#"{"skins": {"Glock-18 | Moonrise": [{"seeds": [59, 694], "multiple": 1.0},
                {"seeds": [7], "multiple": -2}, {"seeds": [8], "multiple": 0}]}}"#,
        )
        .unwrap();
        assert_eq!(e.multiple("Glock-18 | Moonrise", 694), Some(1.0));
        assert_eq!(e.multiple("Glock-18 | Moonrise", 7), None);
        assert_eq!(e.multiple("Glock-18 | Moonrise", 1), None);
        assert_eq!(e.multiple("AK-47 | Redline", 59), None);
        assert!(External::parse("{}").is_err() && External::parse("lixo").is_err());
    }
}
