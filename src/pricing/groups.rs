//! Grupo raro de um anúncio: tier de pattern (guias CSPatterns / blue gem), faixa de fade do topo
//! ou fase Doppler. Tabelas embutidas de `assets/patterns/` (ver LICENSES.md); sem rede.
//!
//! Só vale caçar o topo: o prêmio de fade só paga no topo do rank e, nas vendas pagas, tier fraco não
//! garantiu prêmio (docs/conhecimento.md 5.6). Por isso `seed_ranges` devolve só o tier 1 e o topo do fade,
//! enquanto `group_of` ainda reconhece os tiers menores (para a evidência ser honesta).

use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::sync::{OnceLock, RwLock};

const CSPATTERNS: &str = include_str!("../../assets/patterns/cspatterns.json");
const BLUEGEM: &str = include_str!("../../assets/patterns/bluegem.json");
const FADE: &str = include_str!("../../assets/patterns/fade.json");
const PHASES: &str = include_str!("../../assets/patterns/phases.json");
const FLOATS: &str = include_str!("../../assets/patterns/floats.json");
const RESEARCHED: &str = include_str!("../../assets/patterns/researched.json");

/// Float de elite: a fração do desgaste (a partir do menor float possível da skin) que conta como "float
/// baixo". Em 06/11, nas vendas do CSFloat de 157 skins, os 10 % melhores de cada desgaste saíram de 1,3× a 2,8×
/// a mediana em várias delas (XM1014 Zombie Offensive FN, USP-S Tropical Breeze FN, M4A1-S Solitude MW).
/// Hipótese de recorte, não calibração: o prêmio de cada skin vem das vendas da mesma faixa (n ≥ 3).
const FLOAT_ELITE_SHARE: f64 = 0.10;
/// Faixa aprendida: vendas mínimas do nome, vendas vizinhas por janela, prêmio mínimo da janela (1,5× deixa 30 %
/// depois da taxa de 15 %) e teto da faixa em parte do desgaste. A faixa é também o filtro da consulta, que traz
/// os mais baratos primeiro: faixa larga enche a página de float mediano no piso e esconde o float de ponta. Na
/// Steam só há prova de saída para os 10 % melhores (37 % dos anúncios a 1,5–2× o comum sumiram, 36 nomes).
const FLOAT_LEARN_MIN_SALES: usize = 20;
const FLOAT_WINDOW: usize = 5;
const FLOAT_MIN_PREMIUM: f64 = 1.5;
const FLOAT_MAX_SHARE: f64 = 0.25;
/// Teto do prêmio de float no preço de venda na Steam. As vendas do CSFloat pagam 3–8× em float baixo, mas na
/// Steam (anúncios de float bom vistos de 05 a 11/11/2026) some sem voltar 36 % do que pede 1,15–1,5× o preço
/// comum, 16 % do que pede 1,5–2× e 5 % do que pede 3× ou mais.
/// Anúncio comum some 6 %, 3 % e 3 % nas mesmas faixas: o prêmio de float fecha na Steam, mas não no tamanho do CSFloat.
// shortcut: 2× é hipótese (último degrau em que some bem mais que o comum); acertar com venda de float fechada
// na Steam. O lucro esperado da avaliação ainda pesa a venda rápida, que não leva prêmio nenhum.
pub const FLOAT_STEAM_MAX_MULTIPLE: f64 = 2.0;

/// Fade: só entra quem tem ao menos este % E está nas primeiras posições do rank da tabela.
const FADE_MIN_PCT: f64 = 99.0;
const FADE_TOP_RANK: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// Chave estável (agrupa vendas do CSFloat): "cs:Glock-18 | Moonrise:t1", "phase:Doppler:Ruby"...
    pub key: String,
    /// Texto para leigo: "T1 Center Star", "Ruby", "Fade 99%+".
    pub label: String,
}

#[derive(Deserialize)]
struct CsFile {
    skins: HashMap<String, CsSkin>,
}
#[derive(Deserialize)]
struct CsSkin {
    tiers: HashMap<String, Vec<u16>>,
}
#[derive(Deserialize)]
struct BlueFile {
    weapons: HashMap<String, HashMap<String, Vec<u16>>>,
}
#[derive(Deserialize)]
struct FadeFile {
    finishes: HashMap<String, HashMap<String, FadeTable>>,
}
#[derive(Deserialize)]
struct FadeTable {
    pct: Vec<Option<f64>>,
    rank: Vec<Option<u32>>,
}
#[derive(Deserialize)]
struct FloatsFile {
    /// "Glock-18 | Moonrise" → [menor float, maior float].
    skins: HashMap<String, (f64, f64)>,
}
#[derive(Deserialize)]
struct ResearchedFile {
    skins: HashMap<String, HashMap<u16, ResearchedSeed>>,
}
#[derive(Deserialize)]
struct ResearchedSeed {
    label: String,
}
#[derive(Deserialize)]
struct PhasesFile {
    phases: HashMap<u32, Phase>,
}
#[derive(Deserialize)]
struct Phase {
    pattern: String,
    phase: String,
}

struct Tables {
    cs: CsFile,
    blue: BlueFile,
    fade: FadeFile,
    phases: PhasesFile,
    floats: FloatsFile,
    researched: ResearchedFile,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        // Arquivos embutidos em tempo de compilação: se não parseiam, o teste `tables_parse` pega.
        fn load<T: serde::de::DeserializeOwned>(s: &str) -> T {
            serde_json::from_str(s)
                .unwrap_or_else(|e| panic!("tabela de pattern embutida inválida: {e}"))
        }
        Tables {
            cs: load(CSPATTERNS),
            blue: load(BLUEGEM),
            fade: load(FADE),
            phases: load(PHASES),
            floats: load(FLOATS),
            researched: load(RESEARCHED),
        }
    })
}

/// "★ StatTrak™ Karambit | Fade (Factory New)" → ("Karambit | Fade", "Karambit", "Fade").
fn split_name(market_hash_name: &str) -> (&str, &str, &str) {
    let mut s = market_hash_name.trim();
    s = s.strip_prefix('★').unwrap_or(s).trim_start();
    s = s.strip_prefix("StatTrak™ ").unwrap_or(s);
    s = s.strip_prefix("Souvenir ").unwrap_or(s);
    if let Some(i) = s.rfind(" (")
        && s.ends_with(')')
    {
        s = &s[..i];
    }
    match s.split_once(" | ") {
        Some((w, p)) => (s, w, p),
        None => (s, s, ""),
    }
}

/// Menor tier (1 = melhor) em que a seed aparece.
fn tier_of(tiers: &HashMap<String, Vec<u16>>, seed: u16) -> Option<u32> {
    tiers
        .iter()
        .filter(|(_, seeds)| seeds.contains(&seed))
        .filter_map(|(t, _)| t.parse::<u32>().ok())
        .min()
}

/// Seed no topo do fade? (% alto E posição alta no rank da tabela)
fn fade_top(t: &FadeTable, seed: usize) -> bool {
    matches!(t.pct.get(seed), Some(Some(p)) if *p >= FADE_MIN_PCT)
        && matches!(t.rank.get(seed), Some(Some(r)) if *r <= FADE_TOP_RANK)
}

fn cs_label(pattern: &str, tier: u32) -> String {
    // Nomes que a comunidade usa para o tier 1 de cada skin com tabela.
    match (pattern, tier) {
        ("Moonrise", 1) => "T1 Center Star".into(),
        ("High Beam", 1) => "T1 Full Beam".into(),
        _ => format!("T{tier} {pattern}"),
    }
}

/// Limites de float do desgaste no nome: "(Field-Tested)" → (0,15, 0,38).
fn wear_range(market_hash_name: &str) -> Option<(&'static str, f64, f64)> {
    [
        ("Factory New", 0.0, 0.07),
        ("Minimal Wear", 0.07, 0.15),
        ("Field-Tested", 0.15, 0.38),
        ("Well-Worn", 0.38, 0.45),
        ("Battle-Scarred", 0.45, 1.0),
    ]
    .into_iter()
    .find(|(w, _, _)| market_hash_name.ends_with(&format!("({w})")))
}

/// Faixa de float aprendida por nome (`set_float_bands`): `Some((até, prêmio))`, ou `None` quando as vendas
/// mostram que float baixo não paga prêmio nesse nome. Nome ausente = sem medida, vale a faixa fixa.
/// `(até, prêmio, parte das vendas do nome que caiu na faixa)`.
pub type Band = Option<(f64, f64, f64)>;
static LEARNED_BANDS: RwLock<Option<HashMap<String, Band>>> = RwLock::new(None);

pub fn set_float_bands(bands: HashMap<String, Band>) {
    if let Ok(mut b) = LEARNED_BANDS.write() {
        *b = Some(bands);
    }
}

fn learned_band(market_hash_name: &str) -> Option<Band> {
    LEARNED_BANDS
        .read()
        .ok()?
        .as_ref()?
        .get(market_hash_name)
        .copied()
}

/// Limites de float do nome: o desgaste cortado pelo float que a skin tem.
fn wear_span(market_hash_name: &str) -> Option<(f64, f64)> {
    let (_, lo, hi) = wear_range(market_hash_name)?;
    let (min, max) = *tables().floats.skins.get(split_name(market_hash_name).0)?;
    let (lo, hi) = (lo.max(min), hi.min(max));
    (lo < hi).then_some((lo, hi))
}

/// Faixa de float de elite `(de, até)` do item, começando no menor float que a skin tem: até onde as vendas do
/// CSFloat pagaram prêmio (`learn_float_band`) ou, sem medida, os `FLOAT_ELITE_SHARE` melhores do desgaste.
/// `None` sem desgaste no nome, sem tabela de float, se a skin não chega no desgaste ou se o prêmio medido é nulo.
pub fn float_band(market_hash_name: &str) -> Option<(f64, f64)> {
    let (lo, hi) = wear_span(market_hash_name)?;
    match learned_band(market_hash_name) {
        Some(band) => band.map(|(edge, _, _)| (lo, edge)),
        None => Some((lo, lo + (hi - lo) * FLOAT_ELITE_SHARE)),
    }
}

/// `(múltiplo mediano das vendas dentro da faixa aprendida, parte das vendas do nome que caiu nela)`;
/// `None` sem medida ou sem prêmio.
pub fn float_premium(market_hash_name: &str) -> Option<(f64, f64)> {
    learned_band(market_hash_name)?.map(|(_, premium, share)| (premium, share))
}

/// Aprende a faixa de float de um nome pelas vendas limpas `(float, múltiplo)` do CSFloat: anda do melhor float
/// para o pior em janelas de `FLOAT_WINDOW` vendas vizinhas e para na primeira janela em que duas delas saíram
/// abaixo de `FLOAT_MIN_PREMIUM` × o múltiplo mediano do nome. Devolve a faixa (`Band`),
/// `Some(None)` se nem a primeira janela paga e `None` com vendas de menos para medir.
// shortcut: janela, 1,5× e teto de um quarto do desgaste são hipótese sobre 6 dias de vendas (05–11/11/2026);
// rever quando houver achado de float vendido de verdade.
pub fn learn_float_band(market_hash_name: &str, sales: &[(f64, f64)]) -> Option<Band> {
    let (lo, hi) = wear_span(market_hash_name)?;
    let mut by_float: Vec<(f64, f64)> = sales
        .iter()
        .copied()
        .filter(|(f, m)| f.is_finite() && m.is_finite() && *m > 0.0)
        .collect();
    if by_float.len() < FLOAT_LEARN_MIN_SALES {
        return None;
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let ordinary = median(&mut by_float.iter().map(|s| s.1).collect());
    by_float.sort_by(|a, b| a.0.total_cmp(&b.0));
    let pays = |w: &[(f64, f64)]| {
        let mut m: Vec<f64> = w.iter().map(|s| s.1).collect();
        m.sort_by(f64::total_cmp);
        m[1] >= FLOAT_MIN_PREMIUM * ordinary
    };
    let paid = by_float
        .windows(FLOAT_WINDOW)
        .take_while(|w| pays(w))
        .count();
    if paid == 0 {
        return Some(None);
    }
    let edge = by_float[paid + FLOAT_WINDOW - 2]
        .0
        .min(lo + (hi - lo) * FLOAT_MAX_SHARE);
    let mut inside: Vec<f64> = by_float
        .iter()
        .filter(|s| s.0 <= edge)
        .map(|s| s.1)
        .collect();
    let share = inside.len() as f64 / by_float.len() as f64;
    Some((edge > lo && !inside.is_empty()).then(|| (edge, median(&mut inside), share)))
}

/// Faixa de desgaste alto de interesse; a aparência real ainda depende da inspeção.
pub fn high_float_band(market_hash_name: &str) -> Option<(f64, f64)> {
    (split_name(market_hash_name).0 == "AWP | Asiimov"
        && wear_range(market_hash_name).is_some_and(|(wear, _, _)| wear == "Battle-Scarred"))
    .then_some((0.96, 1.0))
}

/// Grupo raro do anúncio, ou `None` se não há tabela para a skin / a seed não está nela e o float não é de elite.
/// `seed`, `paint_index` e `float` vêm das propriedades da listagem. Pattern vence float.
pub fn group_of(
    market_hash_name: &str,
    seed: Option<u32>,
    paint_index: Option<u32>,
    float: Option<f64>,
) -> Option<Group> {
    pattern_group(market_hash_name, seed, paint_index)
        .or_else(|| {
            let (lo, hi) = high_float_band(market_hash_name)?;
            float.filter(|f| (lo..=hi).contains(f))?;
            Some(Group {
                key: "highfloat:AWP | Asiimov:Battle-Scarred".into(),
                label: "Blackiimov ≥ 0.96".into(),
            })
        })
        .or_else(|| {
            let (lo, hi) = float_band(market_hash_name)?;
            float.filter(|f| (lo..=hi).contains(f))?;
            let (base, _, _) = split_name(market_hash_name);
            let (wear, _, _) = wear_range(market_hash_name)?;
            Some(Group {
                key: format!("float:{base}:{wear}"),
                label: format!("Float ≤ {hi:.3}"),
            })
        })
}

fn pattern_group(
    market_hash_name: &str,
    seed: Option<u32>,
    paint_index: Option<u32>,
) -> Option<Group> {
    let t = tables();
    let (base, weapon, pattern) = split_name(market_hash_name);

    if let Some(ph) = paint_index.and_then(|i| t.phases.phases.get(&i))
        && ph.pattern == pattern
    {
        let label = match ph.phase.as_str() {
            "Ruby" | "Sapphire" | "Black Pearl" | "Emerald" => ph.phase.clone(),
            other => format!("{pattern} {other}"),
        };
        return Some(Group {
            key: format!("phase:{}:{}", ph.pattern, ph.phase),
            label,
        });
    }

    let seed = u16::try_from(seed?).ok()?;
    if let Some(tier) = t.cs.skins.get(base).and_then(|s| tier_of(&s.tiers, seed)) {
        return Some(Group {
            key: format!("cs:{base}:t{tier}"),
            label: cs_label(pattern, tier),
        });
    }
    if pattern == "Case Hardened"
        && let Some(tier) = t.blue.weapons.get(weapon).and_then(|w| tier_of(w, seed))
    {
        return Some(Group {
            key: format!("bluegem:{weapon}:t{tier}"),
            label: format!("Blue Gem T{tier}"),
        });
    }
    if let Some(table) = t.fade.finishes.get(pattern).and_then(|f| f.get(weapon))
        && fade_top(table, seed.into())
    {
        return Some(Group {
            key: format!("fade:{pattern}:{weapon}"),
            label: format!("{pattern} 99%+"),
        });
    }
    if let Some(entry) = t.researched.skins.get(base).and_then(|s| s.get(&seed)) {
        return Some(Group {
            key: format!("learned:{base}:{seed}"),
            label: entry.label.clone(),
        });
    }
    None
}

/// Seeds que valem caçar para a skin (tier 1 do guia, blue gem tier 1, topo do fade), juntando seeds
/// consecutivas em faixas `(de, até)` inclusivas, ordenadas. Vazio se a skin não tem tabela.
/// Fase Doppler não entra: não se filtra por seed.
pub fn seed_ranges(market_hash_name: &str) -> Vec<(u16, u16)> {
    let t = tables();
    let (base, weapon, pattern) = split_name(market_hash_name);
    let mut seeds: BTreeSet<u16> = BTreeSet::new();
    if let Some(s) = t.researched.skins.get(base) {
        seeds.extend(s.keys());
    }
    if let Some(s) = t.cs.skins.get(base).and_then(|s| s.tiers.get("1")) {
        seeds.extend(s);
    }
    if pattern == "Case Hardened"
        && let Some(s) = t.blue.weapons.get(weapon).and_then(|w| w.get("1"))
    {
        seeds.extend(s);
    }
    if let Some(table) = t.fade.finishes.get(pattern).and_then(|f| f.get(weapon)) {
        seeds.extend(
            (0..table.pct.len())
                .filter(|&i| fade_top(table, i))
                .map(|i| i as u16),
        );
    }
    ranges(seeds)
}

/// Seeds em faixas `(de, até)` inclusivas, juntando as consecutivas, ordenadas.
pub fn ranges(seeds: impl IntoIterator<Item = u16>) -> Vec<(u16, u16)> {
    let seeds: BTreeSet<u16> = seeds.into_iter().collect();
    let mut out: Vec<(u16, u16)> = Vec::new();
    for s in seeds {
        match out.last_mut() {
            Some(r) if r.1 + 1 == s => r.1 = s,
            _ => out.push((s, s)),
        }
    }
    out
}

/// "StatTrak™ Glock-18 | Moonrise (Field-Tested)" → "Glock-18 | Moonrise".
pub fn skin_base(market_hash_name: &str) -> &str {
    split_name(market_hash_name).0
}

/// Skins (sem desgaste) de todas as tabelas que o mercado de armas pode ter: cspatterns, blue gem e fade.
/// Faca fica de fora (a varredura do catálogo descarta ★) e fase Doppler também (só facas).
pub fn table_skins() -> Vec<String> {
    let t = tables();
    let gun = |w: &str| {
        !(w.contains("Knife") || w.contains("Bayonet") || w.contains("Daggers") || w == "Karambit")
    };
    let mut out: BTreeSet<String> = t.cs.skins.keys().cloned().collect();
    out.extend(t.researched.skins.keys().cloned());
    out.extend(
        t.blue
            .weapons
            .keys()
            .filter(|w| gun(w))
            .map(|w| format!("{w} | Case Hardened")),
    );
    for (finish, by_weapon) in &t.fade.finishes {
        out.extend(
            by_weapon
                .keys()
                .filter(|w| gun(w))
                .map(|w| format!("{w} | {finish}")),
        );
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_skins_cover_the_tables_without_knives() {
        let v = table_skins();
        for want in [
            "Glock-18 | Moonrise",
            "Glock-18 | High Beam",
            "Desert Eagle | Heat Treated",
            "Five-SeveN | Case Hardened",
            "AK-47 | Case Hardened",
            "Glock-18 | Fade",
            "SSG 08 | Acid Fade",
        ] {
            assert!(v.iter().any(|n| n == want), "faltou {want}");
        }
        assert!(
            v.iter()
                .all(|n| !n.contains("Knife") && !n.contains("Bayonet"))
        );
        // todo nome tem consulta de seed: é isso que vira tarefa
        assert!(
            v.iter()
                .all(|n| !seed_ranges(&format!("{n} (Field-Tested)")).is_empty())
        );
    }

    const MOON: &str = "Glock-18 | Moonrise (Field-Tested)";

    fn count(r: &[(u16, u16)]) -> usize {
        r.iter().map(|&(a, b)| (b - a + 1) as usize).sum()
    }

    #[test]
    fn float_band_is_learned_from_sales_and_stops_where_the_premium_ends() {
        // FN vai de 0 a 0,07 (teto da faixa: 0,0175): 8 vendas de float baixo a 3×, 22 comuns a 1×
        let name = "Desert Eagle | Serpent Strike (Factory New)";
        let mut sales: Vec<(f64, f64)> = (0..8).map(|i| (0.001 * f64::from(i), 3.0)).collect();
        sales.extend((0..22).map(|i| (0.01 + 0.002 * f64::from(i), 1.0)));
        let (edge, premium, share) = learn_float_band(name, &sales).unwrap().unwrap();
        // a última janela que paga tem 4 vendas caras e 1 comum: a faixa termina nessa comum
        assert!(
            (edge - 0.01).abs() < 1e-9 && (premium - 3.0).abs() < 1e-9,
            "{edge} {premium}"
        );
        assert!((share - 0.3).abs() < 1e-9, "9 das 30 vendas: {share}");
        // sem prêmio no float baixo: nome medido, sem faixa
        let flat: Vec<(f64, f64)> = (0..30).map(|i| (0.002 * f64::from(i), 1.0)).collect();
        assert_eq!(learn_float_band(name, &flat), Some(None));
        // poucas vendas: sem medida, vale a faixa fixa
        assert_eq!(learn_float_band(name, &sales[..10]), None);
        // nome de teste só deste caso: a tabela aprendida é global
        let other = "Desert Eagle | Serpent Strike (Minimal Wear)";
        set_float_bands(HashMap::from([(other.to_string(), Some((0.11, 2.5, 0.2)))]));
        assert_eq!(float_band(other), Some((0.07, 0.11)));
        assert_eq!(float_premium(other), Some((2.5, 0.2)));
        assert_eq!(
            group_of(other, None, None, Some(0.10)).unwrap().label,
            "Float ≤ 0.110"
        );
        assert!(float_premium(name).is_none() && float_band(name).is_some());
    }

    #[test]
    fn tables_parse() {
        let t = tables();
        assert_eq!(t.cs.skins.len(), 4);
        assert!(t.blue.weapons.contains_key("AK-47"));
        assert!(t.phases.phases.contains_key(&415));
        assert_eq!(t.fade.finishes["Fade"]["Glock-18"].pct.len(), 1001);
    }

    #[test]
    fn researched_seeds_are_separate_and_discoverable() {
        for (base, seeds) in [
            ("Galil AR | Sandstorm", &[449, 555, 583, 761][..]),
            ("Five-SeveN | Kami", &[590, 662, 909][..]),
            ("AWP | PAW", &[41, 350, 420][..]),
            ("XM1014 | Seasons", &[26, 955][..]),
        ] {
            let name = format!("{base} (Field-Tested)");
            assert!(table_skins().contains(&base.to_string()));
            let ranges = seed_ranges(&name);
            let mut keys = BTreeSet::new();
            for &seed in seeds {
                assert!(ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&seed)));
                let group = group_of(&name, Some(u32::from(seed)), None, None).unwrap();
                assert_eq!(group.key, format!("learned:{base}:{seed}"));
                assert!(group.label.ends_with(&format!("#{seed}")));
                assert!(keys.insert(group.key));
            }
            assert_eq!(group_of(&name, Some(1), None, None), None);
        }
    }

    #[test]
    fn blackiimov_only_in_high_battle_scarred_range() {
        let name = "AWP | Asiimov (Battle-Scarred)";
        assert_eq!(high_float_band(name), Some((0.96, 1.0)));
        assert_eq!(group_of(name, None, None, Some(0.959999)), None);
        assert_eq!(
            group_of(name, None, None, Some(0.96)).unwrap().key,
            "highfloat:AWP | Asiimov:Battle-Scarred"
        );
        assert!(group_of(name, None, None, Some(1.0)).is_some());
        assert_eq!(group_of(name, None, None, Some(1.01)), None);
        assert_eq!(high_float_band("AWP | Asiimov (Field-Tested)"), None);
        assert_eq!(high_float_band("AWP | PAW (Battle-Scarred)"), None);
        assert_eq!(float_band(name).unwrap().0, 0.45);
    }

    #[test]
    fn elite_float_band_and_group() {
        // Moonrise vai de 0 a 0,65: FT é 0,15–0,38 e a elite são os 10 % do começo
        let (lo, hi) = float_band(MOON).unwrap();
        assert!(
            (lo - 0.15).abs() < 1e-9 && (hi - 0.173).abs() < 1e-9,
            "{lo} {hi}"
        );
        let g = group_of(MOON, Some(1), None, Some(0.152)).unwrap();
        assert_eq!(
            (g.key.as_str(), g.label.as_str()),
            ("float:Glock-18 | Moonrise:Field-Tested", "Float ≤ 0.173")
        );
        // StatTrak cai no mesmo grupo (as vendas do CSFloat são por nome, então o prêmio é medido à parte)
        let st = "StatTrak™ Glock-18 | Moonrise (Field-Tested)";
        assert_eq!(group_of(st, None, None, Some(0.16)).unwrap().key, g.key);
        assert_eq!(group_of(MOON, Some(1), None, Some(0.2)), None);
        assert_eq!(group_of(MOON, Some(1), None, None), None);
        // pattern vence float
        assert_eq!(
            group_of(MOON, Some(59), None, Some(0.152)).unwrap().label,
            "T1 Center Star"
        );
        // a skin começa acima do começo do desgaste: a faixa começa no menor float dela
        let (min, max) = tables().floats.skins["AK-47 | Slate"];
        assert_eq!((min, max), (0.0, 1.0));
        let (lo, _) = float_band("AWP | Asiimov (Field-Tested)").unwrap();
        assert!(lo >= 0.18, "Asiimov começa em 0,18: {lo}");
        assert_eq!(float_band("AWP | Asiimov (Factory New)"), None);
        assert_eq!(float_band("Sealed Terminal"), None);
    }

    #[test]
    fn split_name_variants() {
        assert_eq!(
            split_name("★ StatTrak™ Karambit | Fade (Factory New)"),
            ("Karambit | Fade", "Karambit", "Fade")
        );
        assert_eq!(
            split_name("Souvenir AWP | Dragon Lore (Battle-Scarred)").0,
            "AWP | Dragon Lore"
        );
        assert_eq!(
            split_name("Sealed Terminal"),
            ("Sealed Terminal", "Sealed Terminal", "")
        );
    }

    /// Lista da pesquisa-web.md 5 (consenso de 3 guias) contra a tabela embutida.
    #[test]
    fn moonrise_tiers_vs_research() {
        let tiers = &tables().cs.skins["Glock-18 | Moonrise"].tiers;
        let sorted = |k: &str| {
            let mut v = tiers[k].clone();
            v.sort();
            v
        };
        let t1 = [58, 59, 66, 90, 102, 224, 601, 628, 694, 706, 837, 864];
        let t2 = [
            19, 109, 142, 165, 194, 237, 302, 403, 475, 487, 509, 800, 958,
        ];
        assert_eq!(sorted("1"), t1);
        assert_eq!(sorted("2"), t2);
        // a pesquisa separa T3 (13) e T4 (6); a tabela do projeto antigo junta os dois em T3 (19)
        let mut t34 = vec![
            31, 38, 95, 121, 281, 355, 385, 484, 617, 630, 667, 737, 913, 137, 268, 448, 744, 769,
            986,
        ];
        t34.sort();
        assert_eq!(sorted("3"), t34);
        // Divergência registrada: um guia antigo põe 958 em T1. Tabela e consenso dizem T2; seguimos o consenso.
        assert!(!tiers["1"].contains(&958) && tiers["2"].contains(&958));
        assert_eq!(
            group_of(MOON, Some(958), Some(0), None).unwrap().label,
            "T2 Moonrise"
        );
    }

    #[test]
    fn moonrise_groups_and_ranges() {
        let g = group_of(MOON, Some(59), None, None).unwrap();
        assert_eq!(
            (g.key.as_str(), g.label.as_str()),
            ("cs:Glock-18 | Moonrise:t1", "T1 Center Star")
        );
        // vale para qualquer desgaste
        assert!(group_of("Glock-18 | Moonrise (Battle-Scarred)", Some(59), None, None).is_some());
        assert_eq!(group_of(MOON, Some(1), None, None), None); // seed fora das tabelas
        assert_eq!(group_of(MOON, None, None, None), None);
        assert_eq!(group_of(MOON, Some(70_000), None, None), None);
        assert_eq!(
            group_of("AK-47 | Redline (Field-Tested)", Some(59), None, None),
            None
        );
        assert_eq!(
            seed_ranges(MOON),
            [
                (58, 59),
                (66, 66),
                (90, 90),
                (102, 102),
                (224, 224),
                (601, 601),
                (628, 628),
                (694, 694),
                (706, 706),
                (837, 837),
                (864, 864)
            ]
        );
    }

    #[test]
    fn doppler_phase_by_paint_index() {
        let n = "★ Karambit | Doppler (Factory New)";
        assert_eq!(group_of(n, Some(1), Some(415), None).unwrap().label, "Ruby");
        assert_eq!(
            group_of(n, Some(1), Some(416), None).unwrap().label,
            "Sapphire"
        );
        assert_eq!(
            group_of(n, None, Some(419), None).unwrap().label,
            "Doppler Phase 2"
        );
        assert_eq!(group_of(n, Some(1), Some(1), None), None);
        // 568 é Emerald de Gamma Doppler, não de Doppler
        assert_eq!(group_of(n, None, Some(568), None), None);
        assert_eq!(
            group_of(
                "★ Karambit | Gamma Doppler (Factory New)",
                None,
                Some(568),
                None
            )
            .unwrap()
            .label,
            "Emerald"
        );
        assert!(seed_ranges(n).is_empty());
    }

    #[test]
    fn fade_only_the_top() {
        // Glock Fade: seed 763 = 100 % (rank 1); 412 = 80 % (rank 1, mas é o extremo ruim)
        let n = "Glock-18 | Fade (Factory New)";
        let g = group_of(n, Some(763), Some(0), None).unwrap();
        assert_eq!(
            (g.key.as_str(), g.label.as_str()),
            ("fade:Fade:Glock-18", "Fade 99%+")
        );
        assert_eq!(group_of(n, Some(412), None, None), None);
        assert_eq!(group_of(n, Some(10), None, None), None);
        let r = seed_ranges(n);
        assert!(r.iter().any(|&(a, b)| a <= 763 && 763 <= b));
        assert!(count(&r) <= 5);
        // Acid Fade da SSG 08; o flip real da seed 872 (98,85 %, rank 21) fica de fora do topo
        let ssg = "SSG 08 | Acid Fade (Factory New)";
        assert_eq!(
            group_of(ssg, Some(576), None, None).unwrap().label,
            "Acid Fade 99%+"
        );
        assert_eq!(group_of(ssg, Some(872), None, None), None);
        // faca com ★ e StatTrak (a tabela da Karambit tem 412 no topo, o inverso da Glock)
        assert!(
            group_of(
                "★ StatTrak™ Karambit | Fade (Factory New)",
                Some(412),
                None,
                None
            )
            .is_some()
        );
    }

    #[test]
    fn blue_gem_and_heat_treated() {
        let ak = "AK-47 | Case Hardened (Field-Tested)";
        let t1 = tables().blue.weapons["AK-47"]["1"].clone();
        let g = group_of(ak, Some(t1[0].into()), None, None).unwrap();
        assert_eq!(g.label, "Blue Gem T1");
        assert_eq!(count(&seed_ranges(ak)), t1.len());
        // Case Hardened sem tabela da arma: nada
        assert_eq!(
            group_of("MP9 | Case Hardened (Field-Tested)", Some(1), None, None),
            None
        );
        let ht = "Five-SeveN | Heat Treated (Field-Tested)";
        // o guia aponta 904 como "best" da Five-SeveN HT (ask 47×), mas 904 não está em nenhum tier da tabela
        assert_eq!(group_of(ht, Some(904), None, None), None);
        assert_eq!(
            group_of(ht, Some(48), None, None).unwrap().label,
            "T1 Heat Treated"
        );
        assert_eq!(
            group_of(ht, Some(7), None, None).unwrap().label,
            "T4 Heat Treated"
        );
        assert!(!seed_ranges(ht).is_empty());
    }
}
