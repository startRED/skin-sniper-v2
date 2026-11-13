//! Grupo raro de um anúncio: pattern (seed) ou fase Doppler. Tabelas embutidas de `assets/patterns/` (ver
//! LICENSES.md); sem rede. Float não é mais motivo de raro (12/11/2026: só pattern).
//!
//! As tabelas são o catálogo de candidatos: tiers de guia, % de azul por seed (`blue.json`), % do fade e grupos
//! nomeados curados (`named.json`). Quem decide se um seed é raro são as vendas: `hunt::patterns` mede cada
//! skin e publica os seeds que pagam (`set_paying`). Skin medida: só os seeds pagantes têm grupo. Skin ainda sem
//! medida (instalação nova, teste): vale a tabela (tier 1, topo do fade, grupo nomeado).

use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::sync::{OnceLock, RwLock};

const CSPATTERNS: &str = include_str!("../../assets/patterns/cspatterns.json");
const BLUEGEM: &str = include_str!("../../assets/patterns/bluegem.json");
const FADE: &str = include_str!("../../assets/patterns/fade.json");
const PHASES: &str = include_str!("../../assets/patterns/phases.json");
const FLOATS: &str = include_str!("../../assets/patterns/floats.json");
const NAMED: &str = include_str!("../../assets/patterns/named.json");
const BLUE: &str = include_str!("../../assets/patterns/blue.json");

/// Float de elite: a fração do desgaste (a partir do menor float possível da skin) cujas vendas o float pode
/// explicar (1,3× a 2,8× a mediana em várias skins, 06/11/2026). Só serve para essas vendas não ensinarem seed.
const FLOAT_ELITE_SHARE: f64 = 0.10;

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
struct NamedFile {
    skins: HashMap<String, Vec<NamedGroup>>,
}
#[derive(Deserialize)]
struct NamedGroup {
    label: String,
    seeds: Vec<u16>,
    /// O que se vê na arma e quem paga por isso, para leigo.
    #[serde(default)]
    why: Option<String>,
}
#[derive(Deserialize)]
struct BluePctFile {
    /// Skin → % de azul da face visível por seed (0–1000).
    skins: HashMap<String, Vec<f64>>,
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
    named: NamedFile,
    blue_pct: BluePctFile,
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
            named: load(NAMED),
            blue_pct: load(BLUE),
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

/// Desgaste do nome como número (0 = Factory New … 4 = Battle-Scarred). Nome sem desgaste conta como o melhor.
pub fn wear_index(market_hash_name: &str) -> u8 {
    const WEARS: [&str; 5] = [
        "(Factory New)",
        "(Minimal Wear)",
        "(Field-Tested)",
        "(Well-Worn)",
        "(Battle-Scarred)",
    ];
    WEARS
        .iter()
        .position(|w| market_hash_name.ends_with(w))
        .unwrap_or(0) as u8
}

/// Faixa de float de elite `(de, até)` do nome: os `FLOAT_ELITE_SHARE` melhores do desgaste, a partir do menor
/// float que a skin tem. `None` sem desgaste no nome, sem tabela de float ou se a skin não chega no desgaste.
pub fn float_band(market_hash_name: &str) -> Option<(f64, f64)> {
    let (_, lo, hi) = wear_range(market_hash_name)?;
    let (min, max) = *tables().floats.skins.get(split_name(market_hash_name).0)?;
    let (lo, hi) = (lo.max(min), hi.min(max));
    (lo < hi).then_some((lo, lo + (hi - lo) * FLOAT_ELITE_SHARE))
}

/// `(seed, desgaste)` que as vendas provaram pagar prêmio, por skin (`hunt::patterns::load`). Skin ausente = sem
/// medida.
type Paying = HashMap<String, BTreeSet<(u16, u8)>>;
#[cfg(not(test))]
static PAYING: RwLock<Option<Paying>> = RwLock::new(None);
// Em teste a tabela é por thread: cada teste roda na sua e não vaza para os vizinhos.
#[cfg(test)]
thread_local!(static PAYING: RwLock<Option<Paying>> = const { RwLock::new(None) });

fn with_paying<R>(f: impl FnOnce(&RwLock<Option<Paying>>) -> R) -> R {
    #[cfg(not(test))]
    return f(&PAYING);
    #[cfg(test)]
    PAYING.with(f)
}

pub fn set_paying(paying: Paying) {
    with_paying(|p| {
        if let Ok(mut p) = p.write() {
            *p = Some(paying);
        }
    });
}

/// `Some(paga?)` se as vendas da skin já foram medidas; `None` = sem medida, vale a tabela.
fn paying(base: &str, seed: u16, wear: u8) -> Option<bool> {
    with_paying(|p| Some(p.read().ok()?.as_ref()?.get(base)?.contains(&(seed, wear))))
}

/// Nota do seed na skin, maior = mais raro: % de azul (`blue.json`) ou % do fade. `None` = skin sem nota.
/// Nas vendas de 13/09 a 12/11/2026 a nota explica o prêmio: Desert Eagle Heat Treated abaixo de 40 % de azul
/// sai a 0,95× o comum, 50–55 % a 4,3×, 70–80 % a 34×; SSG 08 Acid Fade paga de 99 % para cima.
pub fn score(base: &str, seed: u16) -> Option<f64> {
    let t = tables();
    if let Some(pct) = t.blue_pct.skins.get(base) {
        return pct.get(usize::from(seed)).copied();
    }
    let (_, weapon, pattern) = split_name(base);
    let table = t.fade.finishes.get(pattern)?.get(weapon)?;
    *table.pct.get(usize::from(seed))?
}

/// Seeds do mesmo grupo de catálogo do seed (grupo nomeado, tier do guia, tier de blue gem), ele incluído:
/// de onde vem a prova enquanto o seed tem menos de 3 vendas próprias.
pub fn siblings(base: &str, seed: u16) -> Option<&'static [u16]> {
    let t = tables();
    let (_, weapon, pattern) = split_name(base);
    let named = t
        .named
        .skins
        .get(base)
        .into_iter()
        .flatten()
        .map(|g| &g.seeds);
    let cs =
        t.cs.skins
            .get(base)
            .into_iter()
            .flat_map(|s| s.tiers.values());
    let blue = t
        .blue
        .weapons
        .get(weapon)
        .filter(|_| pattern == "Case Hardened")
        .into_iter()
        .flat_map(HashMap::values);
    named
        .chain(cs)
        .chain(blue)
        .find(|seeds| seeds.contains(&seed))
        .map(Vec::as_slice)
}

/// Todo seed de catálogo da skin (grupos nomeados e tiers): candidatos a raro mesmo sem venda própria.
pub fn catalog_seeds(base: &str) -> BTreeSet<u16> {
    (0..=1000)
        .filter(|&s| siblings(base, s).is_some())
        .collect()
}

/// Grupo raro do anúncio, ou `None` se o seed não paga prêmio (skin medida) / não está nas tabelas (sem medida).
/// `seed` e `paint_index` vêm das propriedades da listagem. O float não cria grupo.
pub fn group_of(
    market_hash_name: &str,
    seed: Option<u32>,
    paint_index: Option<u32>,
    _float: Option<f64>,
) -> Option<Group> {
    pattern_group(market_hash_name, seed, paint_index)
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
    let proven = paying(base, seed, wear_index(market_hash_name));
    if proven == Some(false) {
        return None;
    }
    let proven = proven == Some(true);
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
    let named = t.named.skins.get(base).into_iter().flatten();
    let in_named = named.clone().any(|g| g.seeds.contains(&seed));
    let fade = t.fade.finishes.get(pattern).and_then(|f| f.get(weapon));
    if let Some(table) = fade
        && !in_named
        && (proven || fade_top(table, seed.into()))
    {
        // Um grupo por ponto de %: 13/11 o #188 (81 %, "Black Pearl") e o #172 (98,9 %) saíam juntos como
        // "99%+", e o anúncio mais barato de um derrubava o preço de revenda do outro.
        let band = score(base, seed).map_or(99, |p| (p.floor() as u32).min(99));
        return Some(if band >= 99 {
            Group {
                key: format!("fade:{pattern}:{weapon}"),
                label: format!("{pattern} 99%+"),
            }
        } else {
            Group {
                key: format!("fade:{pattern}:{weapon}:{band}"),
                label: format!("{pattern} {band}%"),
            }
        });
    }
    let label = match named.clone().find(|g| g.seeds.contains(&seed)) {
        Some(g) => format!("{} #{seed}", g.label),
        None if !proven => return None,
        None => match score(base, seed) {
            Some(pct) if fade.is_some() => format!("{pattern} {pct:.0}% #{seed}"),
            Some(pct) => format!("Blue {pct:.0}% #{seed}"),
            None => format!("Pattern #{seed}"),
        },
    };
    Some(Group {
        key: format!("learned:{base}:{seed}"),
        label,
    })
}

/// Explicação do grupo nomeado do seed (`named.json`), se a pesquisa escreveu uma.
pub fn named_why(market_hash_name: &str, seed: u16) -> Option<&'static str> {
    let base = skin_base(market_hash_name);
    tables()
        .named
        .skins
        .get(base)?
        .iter()
        .find(|g| g.seeds.contains(&seed))?
        .why
        .as_deref()
}

/// Seeds de tabela que valem caçar enquanto a skin não tem medida (tier 1 do guia, blue gem tier 1, topo do
/// fade, grupos nomeados), juntando seeds consecutivas em faixas `(de, até)` inclusivas, ordenadas. Vazio se a
/// skin não tem tabela. Fase Doppler não entra: não se filtra por seed.
pub fn seed_ranges(market_hash_name: &str) -> Vec<(u16, u16)> {
    let t = tables();
    let (base, weapon, pattern) = split_name(market_hash_name);
    let mut seeds: BTreeSet<u16> = BTreeSet::new();
    for g in t.named.skins.get(base).into_iter().flatten() {
        seeds.extend(&g.seeds);
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

/// Skins (sem desgaste) de todas as tabelas que o mercado de armas pode ter: cspatterns, blue gem, fade, % de
/// azul e grupos nomeados.
/// Faca fica de fora (a varredura do catálogo descarta ★) e fase Doppler também (só facas).
pub fn table_skins() -> Vec<String> {
    let t = tables();
    let gun = |w: &str| {
        !(w.contains("Knife") || w.contains("Bayonet") || w.contains("Daggers") || w == "Karambit")
    };
    let mut out: BTreeSet<String> = t.cs.skins.keys().cloned().collect();
    out.extend(t.named.skins.keys().cloned());
    out.extend(t.blue_pct.skins.keys().cloned());
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
        // todo nome tem seed de catálogo ou nota por seed: é isso que vira tarefa quando a venda prova
        assert!(v.iter().all(|n| {
            !seed_ranges(&format!("{n} (Field-Tested)")).is_empty() || score(n, 1).is_some()
        }));
    }

    const MOON: &str = "Glock-18 | Moonrise (Field-Tested)";

    fn count(r: &[(u16, u16)]) -> usize {
        r.iter().map(|&(a, b)| (b - a + 1) as usize).sum()
    }

    #[test]
    fn measured_skin_keeps_only_the_seeds_that_pay() {
        let base = "Glock-18 | High Beam";
        let name = "Glock-18 | High Beam (Factory New)";
        assert_eq!(
            group_of(name, Some(302), None, None).unwrap().label,
            "T1 Full Beam"
        );
        assert_eq!(group_of(name, Some(7), None, None), None);
        set_paying(HashMap::from([(
            base.to_string(),
            BTreeSet::from([(7, 0), (511, 0)]),
        )]));
        // medida: o T1 que não paga some, o seed fora da tabela que paga entra, o T1 que paga fica
        assert_eq!(group_of(name, Some(302), None, None), None);
        let g = group_of(name, Some(7), None, None).unwrap();
        assert_eq!(
            (g.key.as_str(), g.label.as_str()),
            ("learned:Glock-18 | High Beam:7", "Pattern #7")
        );
        assert_eq!(
            group_of(name, Some(511), None, None).unwrap().label,
            "T1 Full Beam"
        );
        // o seed provado só no Factory New não é raro no Battle-Scarred
        let worn = "Glock-18 | High Beam (Battle-Scarred)";
        assert_eq!(group_of(worn, Some(7), None, None), None);
        assert_eq!((wear_index(worn), wear_index("Sealed Terminal")), (4, 0));
        // outra skin continua sem medida: vale a tabela
        assert!(group_of(MOON, Some(59), None, None).is_some());
        // skin com nota: fade fora do topo entra quando paga, e o topo que não paga sai
        let ssg = "SSG 08 | Acid Fade (Factory New)";
        set_paying(HashMap::from([(
            "SSG 08 | Acid Fade".to_string(),
            BTreeSet::from([(872, 0)]),
        )]));
        // 872 tem 98,85 %: grupo e rótulo do próprio ponto de %, não "99%+"
        let g = group_of(ssg, Some(872), None, None).unwrap();
        assert_eq!(g.key, "fade:Acid Fade:SSG 08:98");
        assert_eq!(g.label, "Acid Fade 98%");
        assert_eq!(group_of(ssg, Some(576), None, None), None);
    }

    #[test]
    fn score_is_blue_or_fade_percent_and_siblings_come_from_the_catalog() {
        let deagle = "Desert Eagle | Heat Treated";
        // 490 é dos mais azuis da Desert Eagle; 1 é comum
        assert!(
            score(deagle, 490) > score(deagle, 1),
            "{:?}",
            score(deagle, 490)
        );
        assert_eq!(score("SSG 08 | Acid Fade", 576), Some(100.0));
        assert_eq!(score("Glock-18 | Moonrise", 59), None);
        assert_eq!(score(deagle, 1001), None);
        assert_eq!(siblings("AWP | PAW", 41).unwrap(), [41, 350]);
        assert!(siblings("Glock-18 | Moonrise", 59).unwrap().contains(&706));
        assert_eq!(siblings("AWP | PAW", 1), None);
        assert!(catalog_seeds("AWP | PAW").contains(&420));
        // um seed aparece em um grupo só por skin
        for (base, groups) in &tables().named.skins {
            let all: Vec<u16> = groups.iter().flat_map(|g| g.seeds.clone()).collect();
            let unique: BTreeSet<u16> = all.iter().copied().collect();
            assert_eq!(all.len(), unique.len(), "{base}");
            assert!(all.iter().all(|s| *s <= 1000), "{base}");
        }
    }

    #[test]
    fn tables_parse() {
        let t = tables();
        assert_eq!(t.cs.skins.len(), 4);
        assert!(t.blue.weapons.contains_key("AK-47"));
        assert!(t.phases.phases.contains_key(&415));
        assert_eq!(t.fade.finishes["Fade"]["Glock-18"].pct.len(), 1001);
        assert!(t.blue_pct.skins.values().all(|v| v.len() == 1001));
        assert_eq!(t.blue_pct.skins.len(), 5);
    }

    #[test]
    fn named_seeds_are_separate_and_discoverable() {
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
    fn float_is_no_longer_a_rare_group() {
        // Moonrise vai de 0 a 0,65: FT é 0,15–0,38 e a elite são os 10 % do começo
        let (lo, hi) = float_band(MOON).unwrap();
        assert!(
            (lo - 0.15).abs() < 1e-9 && (hi - 0.173).abs() < 1e-9,
            "{lo} {hi}"
        );
        assert_eq!(group_of(MOON, Some(1), None, Some(0.152)), None);
        assert_eq!(
            group_of("AWP | Asiimov (Battle-Scarred)", None, None, Some(0.99)),
            None
        );
        assert_eq!(
            group_of(MOON, Some(59), None, Some(0.152)).unwrap().label,
            "T1 Center Star"
        );
        // a skin começa acima do começo do desgaste: a faixa começa no menor float dela
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
