//! Species of generated site trees. The species table is the family `Tree` of the built-in
//! library (one type per tree: form, size, seed, `Group`, `Weight`); the rule `site.tree_mix`
//! names which groups a site plants (`data/tree_mix.json`). Trees come in a seeded order
//! without repeats until the table runs out, each turned a different way, so neighbours differ.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use ddc_geom::component::Family;

// gen cannot depend on ddc-model (model sits above gen), so read the same library file.
const LIBRARY: &str = include_str!("../../../model/data/library.ddcf");

const MIXES: &str = include_str!("../../data/tree_mix.json");

struct Species {
    name: &'static str,
    group: i64,
    weight: f64,
    /// Season the type stands in leaf, the `Season` of the library type.
    season: i64,
    /// Height, crown width and clear trunk of the type, m.
    dims: (f64, f64, f64),
}

fn tree_family() -> Option<Family> {
    let src = LIBRARY
        .split("\n---")
        .map(|s| s.trim_start_matches('\n'))
        .find(|s| s.lines().any(|l| l.trim() == "family Tree"))?;
    Family::parse(src).ok()
}

fn table() -> &'static [Species] {
    static T: OnceLock<Vec<Species>> = OnceLock::new();
    T.get_or_init(|| {
        let Some(f) = tree_family() else { return vec![] };
        f.types
            .iter()
            .filter_map(|t| {
                let r = f.resolve(&t.values, &[]).ok()?;
                let num = |n: &str| r.nums.get(n).map(|q| q.v);
                Some(Species {
                    name: Box::leak(t.name.clone().into_boxed_str()),
                    group: num("Group")? as i64,
                    weight: num("Weight")?,
                    season: num("Season").unwrap_or(0.0) as i64,
                    dims: (num("Height").unwrap_or(8.0), num("Crown").unwrap_or(4.0), num("Clear").unwrap_or(2.5)),
                })
            })
            .collect()
    })
}

/// Names of the seasons, in the order of the rule's options.
pub fn season_names() -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(MIXES).unwrap_or_default();
    let mut k: Vec<(i64, String)> = v["seasons"].as_object().map(|o| o.iter().filter_map(|(n, c)| Some((c.as_i64()?, n.clone()))).collect()).unwrap_or_default();
    k.sort();
    k.into_iter().map(|x| x.1).collect()
}

/// The season the rule `site.tree_season` names.
pub fn season_name() -> String {
    let i = super::rules::rule("site.tree_season").round() as usize;
    super::rules::def("site.tree_season").and_then(|d| d.options.get(i).cloned()).unwrap_or_default()
}

/// The `Season` code of a season name and the code of types that stand in leaf all year.
fn season_codes(season: &str) -> (Option<i64>, i64) {
    let v: serde_json::Value = serde_json::from_str(MIXES).unwrap_or_default();
    (v["seasons"][season].as_i64(), v["all_year"].as_i64().unwrap_or(-1))
}

/// Names of the mixes, sorted.
pub fn mix_names() -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(MIXES).unwrap_or_default();
    let mut k: Vec<String> = v["mixes"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
    k.sort();
    k
}

/// The mix the rule `site.tree_mix` names.
pub fn mix_name() -> String {
    let i = super::rules::rule("site.tree_mix").round() as usize;
    super::rules::def("site.tree_mix").and_then(|d| d.options.get(i).cloned()).unwrap_or_default()
}

/// The `Group` numbers of the named groups a mix lists.
fn groups(mix: &str) -> Vec<i64> {
    let v: serde_json::Value = serde_json::from_str(MIXES).unwrap_or_default();
    v["mixes"][mix]
        .as_array()
        .map(|a| a.iter().filter_map(|g| g.as_str()).filter_map(|g| v["groups"][g].as_i64()).collect())
        .unwrap_or_default()
}

/// The mix for the climate words of a brief, in any of the languages of `tree_mix.json`
/// ("north", "mountain" -> conifer; "tropical" -> palms; "desert", "hot", "dry" -> arid); `None`
/// when no word is known. Words match whole (a trailing `*` in the data: as a prefix), any case;
/// a Chinese word is found anywhere in the text.
pub fn mix_for_climate(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(MIXES).unwrap_or_default();
    let low = text.to_lowercase();
    let words: Vec<&str> = low.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).collect();
    let hit = |w: &str| match w.strip_suffix('*') {
        Some(prefix) => words.iter().any(|x| x.starts_with(prefix)),
        None if w.chars().next().is_some_and(|c| c as u32 >= 0x2E80) => low.contains(w),
        None => words.iter().any(|x| *x == w),
    };
    ["tropical", "arid", "conifer"].iter().find_map(|mix| {
        let list = v["climates"][*mix].as_array()?;
        list.iter().filter_map(|w| w.as_str()).any(|w| hit(w)).then(|| (*mix).to_string())
    })
}

/// Height, crown width and clear trunk of a library tree type, m.
pub fn dims(ty: &str) -> Option<(f64, f64, f64)> {
    table().iter().find(|s| s.name == ty).map(|s| s.dims)
}

/// Text as lower case with Russian yo folded to ye (U+0451 to U+0435), for matching words of a brief.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase().replace('\u{0451}', "\u{0435}").split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).map(String::from).collect()
}

/// Bits of the species the brief names (`site.tree_species`): a word of a species is a prefix of
/// a word of the text, so "oaks", the Russian instrumental plural and "Eichen" all name their oak.
pub fn species_mask(text: &str) -> u32 {
    let v: serde_json::Value = serde_json::from_str(MIXES).unwrap_or_default();
    let w = words(text);
    let mut mask = 0;
    for (i, s) in v["species"].as_array().map_or(&[][..], Vec::as_slice).iter().enumerate() {
        let hit = s["words"].as_array().map_or(&[][..], Vec::as_slice).iter().filter_map(|x| x.as_str()).any(|k| {
            let k = k.replace('\u{0451}', "\u{0435}");
            w.iter().any(|x| x.starts_with(&k))
        });
        if hit {
            mask |= 1 << i;
        }
    }
    mask
}

/// Whether the brief asks for a hedge.
pub fn wants_hedge(text: &str) -> bool {
    let v: serde_json::Value = serde_json::from_str(MIXES).unwrap_or_default();
    let w = words(text);
    v["hedge_words"].as_array().map_or(&[][..], Vec::as_slice).iter().filter_map(|x| x.as_str()).any(|k| w.iter().any(|x| x.starts_with(k)))
}

/// The side a brief puts the hedge on, and the text without the compass word it used (so that
/// "the north edge" does not also make a northern climate): "street" for the street side, else a
/// compass side, from the words round the hedge word (English, Russian, German).
pub fn hedge_edge(text: &str) -> Option<(&'static str, String)> {
    let v: serde_json::Value = serde_json::from_str(MIXES).unwrap_or_default();
    let w = words(text);
    let hedge = |x: &String| v["hedge_words"].as_array().map_or(&[][..], Vec::as_slice).iter().filter_map(|k| k.as_str()).any(|k| x.starts_with(k));
    let at = w.iter().position(hedge)?;
    let (lo, hi) = (at.saturating_sub(3), (at + 6).min(w.len()));
    for i in lo..hi {
        for (edge, stems) in v["edges"].as_object()?.iter() {
            if stems.as_array().map_or(&[][..], Vec::as_slice).iter().filter_map(|s| s.as_str()).any(|s| w[i].starts_with(s)) {
                let rest: Vec<&str> = w.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, x)| x.as_str()).collect();
                let edge: &'static str = ["street", "south", "east", "north", "west"].into_iter().find(|e| *e == edge.as_str())?;
                return Some((edge, rest.join(" ")));
            }
        }
    }
    None
}

/// Names of the species of the rule `site.tree_species`, in bit order.
fn species_names(mask: u32) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(MIXES).unwrap_or_default();
    v["species"].as_array().map_or(&[][..], Vec::as_slice).iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).filter_map(|(_, s)| s["name"].as_str().map(String::from)).collect()
}

/// FNV-1a of a type's name without its season: the same key for a species' type in any season.
fn name_key(ty: &str) -> u64 {
    let base = [" Autumn", " Bare", " Spring"].iter().fold(ty, |s, t| s.strip_suffix(t).unwrap_or(s));
    base.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Seeded unit number in (0, 1): placement jitter that a site keeps.
pub fn unit(seed: u64, k: u64) -> f64 {
    hash(seed, k)
}

/// Species of a library type: the name without its variant letter and its season.
pub fn species_of(ty: &str) -> &str {
    let mut s = ty;
    for tail in [" Autumn", " Bare", " Spring"] {
        s = s.strip_suffix(tail).unwrap_or(s);
    }
    match s.rsplit_once(' ') {
        Some((head, v)) if v.len() == 1 => head,
        _ => s,
    }
}

/// Places of trees round a closed ring of length `perim` (m along it), one per crown width in
/// `crowns`: groups of one to three whose crowns nearly touch, the groups apart by uneven gaps,
/// as trees are planted in a park. `None` when the groups do not fit the ring.
pub fn grouped(seed: u64, crowns: &[f64], perim: f64) -> Option<Vec<f64>> {
    let n = crowns.len();
    if n == 0 {
        return Some(vec![]);
    }
    let (mut sizes, mut left, mut g) = (vec![], n, 0u64);
    while left > 0 {
        let h = hash(seed ^ 0x6A0C, g);
        let s = if h < 0.3 { 1 } else if h < 0.65 { 2 } else { 3 };
        sizes.push(s.min(left));
        left -= s.min(left);
        g += 1;
    }
    let m = sizes.len();
    let first: Vec<usize> = sizes.iter().scan(0, |a, s| { let f = *a; *a += s; Some(f) }).collect();
    // inside a group the crowns stand side by side, a little apart
    let step = |i: usize| 0.5 * (crowns[i - 1] + crowns[i]) * GROUP_STEP;
    let mut inner = 0.0;
    for gi in 0..m {
        for k in 1..sizes[gi] {
            inner += step(first[gi] + k);
        }
    }
    // between groups at least half again the crowns, the rest of the ring shared unevenly
    let base: Vec<f64> = (0..m)
        .map(|gi| 0.5 * (crowns[first[gi] + sizes[gi] - 1] + crowns[first[(gi + 1) % m]]) * GROUP_GAP)
        .collect();
    let spare = perim - inner - base.iter().sum::<f64>();
    if spare < 0.0 {
        return None;
    }
    let w: Vec<f64> = (0..m).map(|gi| 0.3 + hash(seed ^ 0x6A9, gi as u64)).collect();
    let ws: f64 = w.iter().sum();
    let mut t = perim * hash(seed ^ 0x57A27, 0);
    let mut out = Vec::with_capacity(n);
    for gi in 0..m {
        for k in 0..sizes[gi] {
            if k > 0 {
                t += step(first[gi] + k);
            }
            out.push(t.rem_euclid(perim));
        }
        t += base[gi] + spare * w[gi] / ws;
    }
    Some(out)
}

/// Distance of neighbours in a group, in crown widths: the crowns all but touch.
pub const GROUP_STEP: f64 = 1.05;
/// Least distance of neighbouring groups, in crown widths.
pub const GROUP_GAP: f64 = 1.6;

/// Size factor of tree `k` of a site: 1 +- `jitter`, seeded, so a site keeps its trees.
pub fn scale(seed: u64, k: usize, jitter: f64) -> f64 {
    1.0 + jitter * (2.0 * hash(seed ^ 0x5CA1E, k as u64) - 1.0)
}

fn hash(a: u64, b: u64) -> f64 {
    let mut z = a ^ b.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    ((z >> 11) as f64 + 0.5) / (1u64 << 53) as f64
}

/// `n` trees of the mix in a season as (library type, turn in radians): the types of the season
/// and the evergreens; one species never follows itself while the mix has another. A mix or
/// season without a type gives none.
pub fn sequence(mix: &str, season: &str, seed: u64, n: usize) -> Vec<(&'static str, f64)> {
    let ok = groups(mix);
    let (code, any) = season_codes(season);
    let Some(code) = code else { return vec![] };
    // species named by the brief replace the mix's groups
    let named = species_names(super::rules::rule("site.tree_species").round() as u32);
    let pool: Vec<&Species> = table()
        .iter()
        .filter(|s| s.weight > 0.0 && (s.season == code || s.season == any))
        .filter(|s| if named.is_empty() { ok.contains(&s.group) } else { named.iter().any(|n| s.name.starts_with(&format!("{n} "))) })
        .collect();
    // named species with no type in this season: the mix (the climate's, or the rule's) decides
    let pool: Vec<&Species> = if pool.is_empty() && !named.is_empty() {
        table().iter().filter(|s| s.weight > 0.0 && ok.contains(&s.group) && (s.season == code || s.season == any)).collect()
    } else {
        pool
    };
    if pool.is_empty() {
        return vec![];
    }
    let mut out: Vec<(&'static str, f64)> = Vec::with_capacity(n);
    let mut round = 0u64;
    while out.len() < n {
        // weighted draw without replacement: the smaller -ln(u) / weight, the earlier; u is drawn
        // by the type's name without its season, so every season draws the same trees
        let mut order: BTreeMap<u64, &Species> = BTreeMap::new();
        for s in &pool {
            let key = -hash(seed.wrapping_add(round), name_key(s.name)).ln() / s.weight;
            order.insert(key.to_bits(), s);
        }
        // the draw in order, each time the first type of another species than the last one; a
        // type left over goes where neither neighbour is of its species
        let mut left: Vec<&Species> = order.into_values().collect();
        let mut list: Vec<&Species> = Vec::with_capacity(left.len());
        let mut prev = out.last().map(|(t, _)| species_of(t));
        while !left.is_empty() {
            let j = left.iter().position(|s| Some(species_of(s.name)) != prev).unwrap_or(0);
            let s = left.remove(j);
            let sp = species_of(s.name);
            if Some(sp) == prev {
                let at = (1..list.len()).find(|&i| species_of(list[i - 1].name) != sp && species_of(list[i].name) != sp);
                if let Some(i) = at {
                    list.insert(i, s);
                    continue;
                }
            }
            list.push(s);
            prev = Some(sp);
        }
        for s in list {
            if out.len() < n {
                let turn = hash(seed ^ 0x5EED, out.len() as u64) * std::f64::consts::TAU;
                out.push((s.name, turn));
            }
        }
        round += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_species_never_follows_itself() {
        for mix in mix_names() {
            for seed in [1u64, 77, 4242] {
                let s = sequence(&mix, "summer", seed, 60);
                let pool: std::collections::BTreeSet<_> = s.iter().map(|x| species_of(x.0)).collect();
                if pool.len() < 2 {
                    continue;
                }
                for w in s.windows(2) {
                    assert_ne!(species_of(w[0].0), species_of(w[1].0), "{mix} {seed}: {:?}", s.iter().map(|x| x.0).collect::<Vec<_>>());
                }
            }
        }
        assert_eq!(species_of("Oak A Autumn"), "Oak");
        assert_eq!(species_of("Scots Pine B"), "Scots Pine");
        assert_eq!(species_of("Lilac"), "Lilac");
    }

    /// Groups: neighbours in a group stand a crown apart, the gaps between groups are wider and
    /// uneven, every place lies on the ring and the seed keeps them.
    #[test]
    fn ring_trees_stand_in_groups_with_uneven_gaps() {
        let crowns: Vec<f64> = (0..12).map(|k| 4.0 + (k % 4) as f64 * 1.5).collect();
        let perim = 260.0;
        let t = grouped(9, &crowns, perim).unwrap();
        assert_eq!(t, grouped(9, &crowns, perim).unwrap());
        assert_ne!(t, grouped(10, &crowns, perim).unwrap());
        assert!(t.iter().all(|x| (0.0..perim).contains(x)));
        let gaps: Vec<f64> = (0..t.len()).map(|i| (t[(i + 1) % t.len()] - t[i]).rem_euclid(perim)).collect();
        let tight = gaps.iter().zip(0..).filter(|(g, i)| **g < 0.5 * (crowns[*i] + crowns[(*i + 1) % 12]) * 1.2).count();
        assert!(tight >= 3, "groups: {gaps:?}");
        for (i, g) in gaps.iter().enumerate() {
            assert!(*g + 1e-9 >= 0.5 * (crowns[i] + crowns[(i + 1) % 12]) * GROUP_STEP, "crowns meet: {gaps:?}");
        }
        let wide: Vec<f64> = gaps.iter().copied().filter(|g| *g > 8.0).collect();
        let mean = wide.iter().sum::<f64>() / wide.len() as f64;
        let sd = (wide.iter().map(|g| (g - mean).powi(2)).sum::<f64>() / wide.len() as f64).sqrt();
        assert!(sd / mean > 0.15, "even gaps: {wide:?}");
        assert!(grouped(9, &crowns, 40.0).is_none());
    }

    #[test]
    fn climate_words_work_in_five_languages() {
        let cases = [
            ("en", "hot", "arid"), ("en", "dry", "arid"), ("en", "forest", "conifer"), ("en", "cold", "conifer"), ("en", "tropical resort", "tropical"),
            ("de", "heißes Klima", "arid"), ("de", "trocken", "arid"), ("de", "Wald", "conifer"), ("de", "kaltes Gebirge", "conifer"), ("de", "tropische Insel", "tropical"),
            ("es", "clima caluroso", "arid"), ("es", "seco", "arid"), ("es", "bosque", "conifer"), ("es", "frío", "conifer"), ("es", "isla tropical", "tropical"),
            ("zh", "炎热干燥的沙漠", "arid"), ("zh", "干旱", "arid"), ("zh", "森林", "conifer"), ("zh", "寒冷的北方", "conifer"), ("zh", "热带海岛", "tropical"),
        ];
        for (lang, text, mix) in cases {
            assert_eq!(mix_for_climate(text).as_deref(), Some(mix), "{lang}: {text}");
        }
        let ru: serde_json::Value = serde_json::from_str(include_str!("../../tests/data/ru_samples.json")).unwrap();
        for c in ru["climate"].as_array().unwrap() {
            let (text, mix) = (c[0].as_str().unwrap(), c[1].as_str().unwrap());
            assert_eq!(mix_for_climate(text).as_deref(), Some(mix), "ru: {text}");
        }
        assert_eq!(mix_for_climate("temperate"), None);
        assert_eq!(mix_for_climate("温带"), None);
    }

    #[test]
    fn every_mix_has_types_and_is_deterministic() {
        assert!(table().len() >= 150, "{} types", table().len());
        for m in mix_names() {
            let a = sequence(&m, "summer", 7, 12);
            assert_eq!(a.len(), 12, "{m}");
            assert_eq!(a, sequence(&m, "summer", 7, 12), "{m}");
        }
    }

    #[test]
    fn the_rule_options_are_the_mixes() {
        let mut o = super::super::rules::def("site.tree_mix").unwrap().options.clone();
        o.sort();
        assert_eq!(o, mix_names());
        assert!(mix_names().contains(&mix_name()));
    }

    #[test]
    fn neighbours_differ_and_seeds_reshuffle() {
        let a = sequence("mixed", "summer", 1, 10);
        let names: std::collections::BTreeSet<_> = a.iter().map(|x| x.0).collect();
        assert_eq!(names.len(), 10);
        assert_ne!(a, sequence("mixed", "summer", 2, 10));
        let turns: std::collections::BTreeSet<u64> = a.iter().map(|x| x.1.to_bits()).collect();
        assert_eq!(turns.len(), a.len(), "every tree has its own turn");
        assert!(a.iter().all(|x| (0.0..std::f64::consts::TAU).contains(&x.1)));
    }

    #[test]
    fn every_group_a_mix_names_exists_in_the_table() {
        let v: serde_json::Value = serde_json::from_str(MIXES).unwrap();
        for (g, n) in v["groups"].as_object().unwrap() {
            let n = n.as_i64().unwrap();
            assert!(table().iter().any(|s| s.group == n && s.weight > 0.0), "group {g} has no type");
        }
        for m in mix_names() {
            for g in v["mixes"][&m].as_array().unwrap() {
                assert!(v["groups"][g.as_str().unwrap()].is_i64(), "{m}: unknown group {g}");
            }
        }
    }

    #[test]
    fn the_rule_options_are_the_seasons() {
        let mut o = super::super::rules::def("site.tree_season").unwrap().options.clone();
        o.sort();
        let mut n = season_names();
        n.sort();
        assert_eq!(o, n);
        assert!(season_names().contains(&season_name()));
    }

    #[test]
    fn a_season_picks_its_own_types_and_the_evergreens() {
        let names = |se: &str| -> Vec<&'static str> { sequence("mixed", se, 5, 60).into_iter().map(|x| x.0).collect() };
        let fall = names("autumn");
        assert!(fall.iter().any(|n| n.ends_with(" Autumn")), "{fall:?}");
        assert!(!fall.iter().any(|n| n.ends_with(" Bare") || n.ends_with(" Spring")), "{fall:?}");
        let bare = names("winter");
        assert!(bare.iter().any(|n| n.ends_with(" Bare")), "{bare:?}");
        assert!(bare.iter().any(|n| n.starts_with("Spruce") || n.starts_with("Fir") || n.starts_with("Scots")), "evergreens stay: {bare:?}");
        assert!(!names("summer").iter().any(|n| n.ends_with(" Bare") || n.ends_with(" Autumn") || n.ends_with(" Spring")));
        assert!(names("spring").iter().any(|n| n.ends_with(" Spring")));
        assert!(sequence("mixed", "monsoon", 5, 3).is_empty());
    }

    #[test]
    fn seasons_of_one_seed_keep_the_same_species_shapes() {
        // a deciduous type of every season has the base type's seed, so its tree is the same
        let f = tree_family().unwrap();
        let seed = |n: &str| f.family_type(n).map(|t| f.resolve(&t.values, &[]).unwrap().nums["Seed"].v);
        for sp in ["Oak A", "Birch B", "Cherry A"] {
            for suf in [" Autumn", " Bare", " Spring"] {
                assert_eq!(seed(sp), seed(&format!("{sp}{suf}")), "{sp}{suf}");
            }
        }
    }

    #[test]
    fn arid_mix_has_palms_and_conifer_mix_has_none() {
        let palm = |m: &str| sequence(m, "summer", 3, 40).iter().any(|x| x.0.contains("Palm"));
        assert!(palm("arid"));
        assert!(!palm("conifer") && !palm("mixed"));
    }
}
