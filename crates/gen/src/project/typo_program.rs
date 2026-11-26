// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A brief that names a building type but no rooms or outline: the default programme and plan
//! come from `data/typology_programs.json` (keywords in several languages, area shares, English
//! names the gen bundles translate), so the generator never relies on the model to list them. A type that already has a
//! preset or a base typology hands over to it. Rooms, a preset or a program of the brief win.

use super::footprint;
use super::rules::{Layer, RuleSet};
use super::Typology;
use serde_json::{json, Value as J};
use std::sync::OnceLock;

pub(super) fn data() -> &'static J {
    static D: OnceLock<J> = OnceLock::new();
    D.get_or_init(|| serde_json::from_str(include_str!("../../data/typology_programs.json")).expect("typology_programs.json is valid"))
}

/// A note template of the data in `lang` (`gen.typo_note.<field>`), else its English.
fn note(d: &J, field: &str, lang: &str) -> String {
    let en = d[field]["en"].as_str().unwrap_or_default();
    super::names::look(lang, &format!("gen.typo_note.{field}")).unwrap_or(en).to_string()
}

/// A name of the data in `lang`, translated under `scope`.
fn name_of(v: &J, lang: &str, scope: &str) -> String {
    super::names::tr(lang, scope, v["en"].as_str().unwrap_or_default())
}

/// What the type is, worded for the notes (`gen.what.<slug>`).
fn what_of(v: &J, lang: &str) -> String {
    let en = v["en"].as_str().unwrap_or_default();
    super::names::look(lang, &format!("gen.what.{}", super::names::slug(en))).unwrap_or(en).to_string()
}

fn is_cjk(s: &str) -> bool {
    s.chars().any(|c| c >= '\u{2e80}')
}

/// Lower case, every run of non-letters one space.
fn norm(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_string()
}

/// Whether the normalised text holds the keyword as a word: whole tokens (a plural `s` or a
/// compound head such as Kunstmuseum allowed, "museumsquartier" not); a `*` stem takes any ending;
/// zh/ja keywords match a whole token or end one.
fn word_in(n: &str, w: &str) -> bool {
    let (w, stem) = match w.strip_suffix('*') {
        Some(s) => (norm(s), true),
        None => (norm(w), false),
    };
    if w.is_empty() {
        return false;
    }
    if is_cjk(&w) {
        // no spaces in a run: a keyword counts when a typical suffix or the run's end follows it,
        // so a street name (博物馆路) is not a museum
        const SUFFIX: [&str; 5] = ["设计", "任务书", "项目", "楼", "馆"];
        return n.split(' ').any(|t| {
            t.match_indices(&w).any(|(i, _)| {
                let rest = &t[i + w.len()..];
                rest.is_empty() || SUFFIX.iter().any(|s| rest.starts_with(s))
            })
        });
    }
    let b = n.as_bytes();
    n.match_indices(&w).any(|(i, _)| {
        let end = i + w.len();
        let start_ok = i == 0 || b[i - 1] == b' ' || (!stem && !w.contains(' ') && w.chars().count() >= 5);
        let end_ok = stem || end == n.len() || b[end] == b' ' || (b[end] == b's' && (end + 1 == n.len() || b[end + 1] == b' '));
        start_ok && end_ok
    })
}

/// A feature that is the keyword alone (a standalone noun), plural or stem ending allowed.
fn feature_is(f: &str, w: &str) -> bool {
    let (w, stem) = match w.strip_suffix('*') {
        Some(s) => (norm(s), true),
        None => (norm(w), false),
    };
    !w.is_empty() && !f.contains(' ') && (f == w || f == format!("{w}s") || (stem && f.starts_with(&w)))
}

/// The table entry the brief names: the longest keyword found in its typology, name or preset
/// (not beside a corridor, access or street word), or in a feature that is that keyword alone.
fn entry(v: &J) -> Option<&'static J> {
    let d = data();
    let quals: Vec<String> = d["qualifiers"].as_array().into_iter().flatten().filter_map(J::as_str).map(norm).collect();
    let texts: Vec<String> = ["typology", "name", "preset"]
        .iter()
        .filter_map(|k| v[*k].as_str().map(|s| (*k, s)))
        // a base typology name is a default, not a word of the brief
        .filter(|(k, s)| !(*k == "typology" && Typology::NAMES.iter().any(|n| *n == *s)))
        .map(|(_, s)| norm(s))
        .filter(|n| !n.split(' ').any(|t| quals.iter().any(|q| q == t)))
        .collect();
    let feats: Vec<String> = v["features"].as_array().into_iter().flatten().filter_map(J::as_str).map(norm).collect();
    let mut best: Option<(usize, &J)> = None;
    for t in d["types"].as_array()? {
        for w in t["words"].as_array().into_iter().flatten().filter_map(J::as_str) {
            let hit = texts.iter().any(|n| word_in(n, w)) || feats.iter().any(|f| feature_is(f, w));
            let len = w.chars().count();
            if hit && best.map_or(true, |b| len > b.0) {
                best = Some((len, t));
            }
        }
    }
    best.map(|b| b.1)
}

/// Whether the brief asks the room of `l` tall: a feature or the name has one of the line's
/// `asked_by` words and a tall word (atrium, tall).
fn tall_asked(v: &J, l: &J) -> bool {
    let words = |k: &J| -> Vec<String> { k.as_array().into_iter().flatten().filter_map(J::as_str).map(str::to_lowercase).collect() };
    let (room, tall) = (words(&l["asked_by"]), words(&data()["tall_words"]));
    let mut texts: Vec<String> = v["features"].as_array().into_iter().flatten().filter_map(J::as_str).map(norm).collect();
    texts.extend(["name", "brief"].iter().filter_map(|k| v[*k].as_str()).map(norm));
    texts.iter().any(|t| room.iter().any(|w| word_in(t, w)) && tall.iter().any(|w| word_in(t, w)))
}

/// A rule as the brief's own rules set it.
fn rule_of(v: &J, id: &str) -> f64 {
    let mut rs = RuleSet::new();
    if !v["rules"].is_null() {
        let _ = rs.apply_patch(Layer::Brief, &v["rules"]);
    }
    rs.get(id)
}

/// Whether the brief is of a table type and has no say over its rooms.
pub(crate) fn asked(v: &J) -> bool {
    if ["rooms", "ground_rooms", "program", "apartments"].iter().any(|k| !v[*k].is_null()) {
        return false;
    }
    // a preset of the brief, or a typology that is a data preset, keeps its own programme
    let named = |k: &str| v[k].as_str().is_some_and(|s| super::presets::find(s).is_some());
    if named("preset") || named("typology") || v["preset"].is_object() {
        return false;
    }
    entry(v).is_some()
}

/// Metres of each room of a line so that the floor adds up to `target`, every room within its
/// own limits: rooms at a limit stay, the others share the rest, until it settles.
pub(crate) fn sized(lines: &[&J], area: f64) -> Vec<f64> {
    let share = |l: &J| l["share"].as_f64().unwrap_or(0.0);
    let n = |l: &J| l["count"].as_f64().unwrap_or(1.0).max(1.0);
    let lim = |l: &J| (l["min"].as_f64().unwrap_or(4.0), l["max"].as_f64().unwrap_or(1e4));
    let target: f64 = lines.iter().map(|l| share(l) * area).sum();
    let mut fixed: Vec<Option<f64>> = vec![None; lines.len()];
    let mut each: Vec<f64> = lines.iter().map(|l| (share(l) * area / n(l)).clamp(lim(l).0, lim(l).1)).collect();
    for _ in 0..12 {
        let held: f64 = lines.iter().zip(&fixed).filter_map(|(l, f)| f.map(|x| x * n(l))).sum();
        let free: f64 = lines.iter().zip(&fixed).filter(|(_, f)| f.is_none()).map(|(l, _)| share(l) * area).sum();
        if free <= 0.0 {
            break;
        }
        let k = ((target - held) / free).max(0.0);
        let mut moved = false;
        for (i, l) in lines.iter().enumerate() {
            if fixed[i].is_some() {
                continue;
            }
            let want = share(l) * area / n(l) * k;
            let c = want.clamp(lim(l).0, lim(l).1);
            each[i] = c;
            if (c - want).abs() > 1e-9 {
                fixed[i] = Some(c);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    each
}

/// The brief with the type's storeys, outline and rooms written in (or its preset named), and
/// what the reply says was filled; `None` when it is not a table brief or nothing changes.
pub(crate) fn fill(v: &J) -> Option<(J, Vec<String>)> {
    if !asked(v) {
        return None;
    }
    let (d, t) = (data(), entry(v)?);
    let lang = v["lang"].as_str().map_or("en", super::names::locale);
    let scope = format!("type.{}", t["id"].as_str().unwrap_or_default());
    let what = what_of(&t["what"], lang);
    let mut out = v.clone();
    let mut notes = vec![];
    // what the type is (residential, one household, flats) when it says so and the brief does not
    if v["traits"].is_null() && t["traits"].is_object() {
        out["traits"] = t["traits"].clone();
    }
    let delegated = t["preset"].is_string() || t["typology"].is_string();
    if let Some(p) = t["preset"].as_str() {
        out["preset"] = json!(p);
        notes.push(note(d, "delegate_note", lang).replace("{what}", &what).replace("{via}", &format!("preset {p}")));
    } else if let Some(b) = t["typology"].as_str() {
        if v["typology"].as_str() != Some(b) {
            out["typology"] = json!(b);
            notes.push(note(d, "delegate_note", lang).replace("{what}", &what).replace("{via}", &format!("base typology {b}")));
        }
    } else {
        if !v["typology"].as_str().is_some_and(|x| Typology::NAMES.contains(&x)) {
            out["typology"] = json!(Typology::Office.name());
        }
        notes.push(note(d, "note", lang).replace("{what}", &what));
    }
    // a preset or base typology brings its own name, storeys and heights
    if !delegated {
        if v["name"].is_null() {
            out["name"] = json!(name_of(&t["name"], lang, &scope));
        }
        for k in ["floors", "floor_height"] {
            if v[k].is_null() {
                out[k] = t[k].clone();
            }
        }
        // a tall space (nave, hall, bay): a one-storey building raises its storey to it, within
        // the rule; a taller building keeps its storeys and says so
        let tall_of = |l: &J| l["tall"].as_f64().or_else(|| l["tall_asked"].as_f64().filter(|_| tall_asked(v, l)));
        let tall = t["lines"].as_array().into_iter().flatten().filter_map(tall_of).fold(0.0, f64::max);
        // the tall level is the ground one: that is where the tall rooms stand
        let ground_tall = t["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|l| l["floor"].as_str() == Some("ground"))
            .filter_map(tall_of)
            .fold(0.0, f64::max);
        let storeys = out["floors"].as_u64().unwrap_or(1);
        if tall > 0.0 && storeys == 1 {
            let mut rs = RuleSet::new();
            if !v["rules"].is_null() {
                let _ = rs.apply_patch(Layer::Brief, &v["rules"]);
            }
            let cap = rs.get("typology.tall_space_max");
            if v["floor_height"].is_null() {
                let h = tall.min(cap).max(t["floor_height"].as_f64().unwrap_or(0.0));
                out["floor_height"] = json!(h);
                notes.push(note(d, "raise_note", lang).replace("{what}", &what).replace("{h}", &format!("{h:.1}")).replace("{cap}", &format!("{cap:.0}")));
            }
        } else if ground_tall > 0.0 && v["storey_heights"].is_null() {
            // several storeys: the ground level is as tall as its tall room, the others normal
            let mut rs = RuleSet::new();
            if !v["rules"].is_null() {
                let _ = rs.apply_patch(Layer::Brief, &v["rules"]);
            }
            let cap = rs.get("typology.tall_space_max");
            let upper = t["upper_height"].as_f64().or(d["upper_height"].as_f64()).unwrap_or(4.0);
            let h = ground_tall.min(cap);
            if v["floor_height"].is_null() {
                out["floor_height"] = json!(upper);
            }
            out["storey_heights"] = json!([h]);
            notes.push(note(d, "levels_note", lang).replace("{h}", &format!("{h:.1}")).replace("{n}", &storeys.to_string()));
        } else if tall > 0.0 {
            let f = out["floor_height"].as_f64().unwrap_or(0.0);
            notes.push(note(d, "tall_note", lang).replace("{h}", &format!("{tall:.1}")).replace("{n}", &storeys.to_string()).replace("{f}", &format!("{f:.1}")));
        }
    }
    let fp = &v["footprint"];
    let shaped = footprint::KEYS.iter().any(|k| !fp[*k].is_null());
    let fnum = |k: &str| fp[k].as_f64().filter(|x| x.is_finite() && *x > 0.0);
    let (asked_area, w0, d0) = (fnum("area"), fnum("width"), fnum("depth"));
    let area = match (w0, d0, asked_area) {
        (Some(w), Some(dp), _) => w * dp,
        (_, _, Some(a)) => a,
        _ => t["floor_area"].as_f64().unwrap_or(2800.0),
    };
    // `get_mut`, not `out["footprint"]`: indexing a JSON object for writing inserts the key,
    // and a brief nothing changed in must compare equal to the one asked
    if let Some(o) = out.get_mut("footprint").and_then(J::as_object_mut) {
        o.remove("area");
    }
    // a preset sizes its own plan unless the brief gave an area
    if !shaped && (w0.is_none() || d0.is_none()) && (!delegated || asked_area.is_some()) {
        let half = |x: f64| (x * 2.0).round() / 2.0;
        let (w, dp) = match (w0, d0) {
            (Some(w), None) => (w, half(area / w)),
            (None, Some(dp)) => (half(area / dp), dp),
            _ => {
                let w = half((area * t["aspect"].as_f64().unwrap_or(1.75)).sqrt());
                (w, half(area / w))
            }
        };
        if !out["footprint"].is_object() {
            out["footprint"] = json!({});
        }
        out["footprint"]["width"] = json!(w);
        out["footprint"]["depth"] = json!(dp);
        let s = note(d, "outline_note", lang);
        notes.push(s.replace("{w}", &format!("{w:.1}")).replace("{d}", &format!("{dp:.1}")).replace("{a}", &format!("{area:.0}")));
    }
    if !delegated {
        let floors = out["floors"].as_u64().unwrap_or(1);
        let floor = |which: &str| -> Vec<J> {
            let lines: Vec<&J> = t["lines"].as_array().into_iter().flatten().filter(|l| l["floor"] == which).collect();
            // the rooms keep `typology.net_share` of the floor at most: walls, corridor and
            // core take the rest, so the outline always has room for the programme
            let shares: f64 = lines.iter().map(|l| l["share"].as_f64().unwrap_or(0.0)).sum();
            let net = rule_of(v, "typology.net_share");
            let each = sized(&lines, area * if shares > net { net / shares } else { 1.0 });
            lines
                .iter()
                .zip(each)
                .map(|(l, a)| json!({"kind": l["kind"], "area": (a * 10.0).round() / 10.0, "count": l["count"], "name": name_of(&l["name"], lang, &scope)}))
                .collect()
        };
        // one storey takes rooms alone
        if floors > 1 {
            out["ground_rooms"] = J::Array(floor("ground"));
            let upper = floor("typical");
            out["rooms"] = J::Array(if upper.is_empty() { floor("ground") } else { upper });
        } else {
            out["rooms"] = J::Array(floor("ground"));
        }
    }
    (out != *v).then_some((out, notes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{build_project, parse_spec, RoomKind, RoomReq};

    /// A Russian sample text from the test data file.
    fn ru(key: &str) -> String {
        let m: J = serde_json::from_str(include_str!("../../tests/data/ru_samples.json")).unwrap();
        m[key].as_str().unwrap_or_else(|| panic!("no ru sample {key}")).to_string()
    }

    /// The English words followed by the Russian samples under `keys`.
    fn langs(en: &[&str], keys: &[&str]) -> Vec<String> {
        en.iter().map(|s| s.to_string()).chain(keys.iter().map(|k| ru(k))).collect()
    }

    fn count(rooms: &[RoomReq], k: RoomKind) -> u32 {
        rooms.iter().filter(|r| r.kind == k).map(|r| r.count).sum()
    }
    fn total(rooms: &[RoomReq]) -> f64 {
        rooms.iter().map(|r| r.area * r.count as f64).sum()
    }
    fn spec(name: &str) -> crate::project::ProjectSpec {
        parse_spec(&json!({"typology": "office", "name": name})).unwrap_or_else(|e| panic!("{name}: {e:?}"))
    }
    fn said(s: &crate::project::ProjectSpec, w: &str) -> bool {
        s.notes.iter().any(|n| n.contains(w))
    }

    #[test]
    fn a_museum_in_any_language_without_rooms_gets_the_programme() {
        for w in langs(&["museum", "Museum", "Kunstmuseum", "museo", "博物馆", "城市博物馆", "Art Gallery"], &["museum", "museum_gen"]) {
            let w = w.as_str();
            let s = spec(w);
            assert!(count(&s.rooms, RoomKind::Hall) >= 4, "{w}: galleries {}", count(&s.rooms, RoomKind::Hall));
            let g = &s.ground_rooms;
            assert_eq!(count(g, RoomKind::Hall), 5, "{w}");
            assert_eq!(count(g, RoomKind::Lobby), 2, "{w}: atrium and foyer");
            for k in [RoomKind::Dining, RoomKind::Retail, RoomKind::Workshop, RoomKind::Storage] {
                assert_eq!(count(g, k), 1, "{w}: {k:?}");
            }
            assert_eq!(count(g, RoomKind::Wc), 2, "{w}");
            assert_eq!(s.floors, 3, "{w}");
            assert!(said(&s, "museum programme"), "{w}: {:?}", s.notes);
        }
    }

    #[test]
    fn the_typology_word_is_read_and_the_outline_comes_from_the_area() {
        let s = parse_spec(&json!({"typology": "museum"})).unwrap_or_else(|e| panic!("{e:?}"));
        let (w, dp) = (s.width.unwrap(), s.depth.unwrap());
        assert!((w * dp - 2800.0).abs() / 2800.0 < 0.05, "{w} x {dp}");
        let s = parse_spec(&json!({"typology": "museum", "footprint": {"area": 3600.0}})).unwrap();
        assert!((s.width.unwrap() * s.depth.unwrap() - 3600.0).abs() / 3600.0 < 0.05);
        let s = parse_spec(&json!({"typology": "museum", "footprint": {"width": 60.0}})).unwrap();
        assert_eq!(s.width, Some(60.0));
        assert!((s.depth.unwrap() - 2800.0 / 60.0).abs() <= 0.5);
    }

    #[test]
    fn the_programme_follows_the_floor_area_and_the_brief_wins() {
        let area = |v: J| total(&parse_spec(&v).unwrap().ground_rooms);
        let small = area(json!({"typology": "museum", "footprint": {"width": 40.0, "depth": 30.0}}));
        let big = area(json!({"typology": "museum", "footprint": {"width": 80.0, "depth": 50.0}}));
        assert!(big > 2.0 * small, "{small} vs {big}");
        // rooms asked: nothing is filled
        let s = parse_spec(&json!({"typology": "office", "name": "Museum", "rooms": [{"kind": "office", "area": 30, "count": 4}]})).unwrap();
        assert_eq!(count(&s.rooms, RoomKind::Hall), 0);
        assert!(!said(&s, "museum programme"));
        // an office called anything else is not a table type
        assert!(!asked(&json!({"typology": "office", "name": "Head office"})));
    }

    #[test]
    fn keywords_match_whole_words_not_parts_of_other_words() {
        for name in ["Museumsquartier", "Gallery access corridor", "Template office", "Schoolbus depot"] {
            assert!(!asked(&json!({"typology": "office", "name": name})), "{name} must not trigger");
        }
        assert!(!asked(&json!({"typology": "office", "name": "Museum quarter"})), "a qualifier word");
        assert!(asked(&json!({"typology": "office", "name": "Museums"})));
        assert!(asked(&json!({"typology": "office", "name": "Stadtbibliothek"})), "compound head");
        assert!(!asked(&json!({"typology": "office", "name": "Template"})), "temple is not a word of template");
        assert!(asked(&json!({"typology": "office", "name": "Buddhist temple"})));
        // zh: the whole token or a compound ending in it
        assert!(asked(&json!({"typology": "office", "name": "市立图书馆"})));
        assert!(!asked(&json!({"typology": "office", "name": "图书馆入口"})));
    }

    #[test]
    fn chinese_keywords_need_a_suffix_or_the_end_of_the_run() {
        assert!(asked(&json!({"name": "博物馆设计任务书"})));
        assert!(asked(&json!({"name": "博物馆"})));
        assert!(!asked(&json!({"name": "博物馆路"})));
    }

    #[test]
    fn features_alone_trigger_only_as_a_standalone_noun() {
        assert!(!asked(&json!({"typology": "office", "features": ["gallery access corridor"]})));
        assert!(!asked(&json!({"typology": "office", "features": ["museum entrance", "glazed gallery walkway"]})));
        assert!(asked(&json!({"typology": "office", "features": ["museum"]})));
        assert!(asked(&json!({"typology": "office", "features": [ru("museum_cap")]})));
        let s = parse_spec(&json!({"typology": "office", "features": ["library"]})).unwrap();
        assert!(count(&s.rooms, RoomKind::Hall) >= 2);
    }

    #[test]
    fn the_shares_add_up_to_the_floor_area_after_the_limits() {
        let d = data();
        for t in d["types"].as_array().unwrap().iter().filter(|t| t["lines"].is_array()) {
            for which in ["ground", "typical"] {
                let lines: Vec<&J> = t["lines"].as_array().unwrap().iter().filter(|l| l["floor"] == which).collect();
                if lines.is_empty() {
                    continue;
                }
                let area = t["floor_area"].as_f64().unwrap();
                let target: f64 = lines.iter().map(|l| l["share"].as_f64().unwrap() * area).sum();
                let each = sized(&lines, area);
                let got: f64 = lines.iter().zip(&each).map(|(l, a)| a * l["count"].as_f64().unwrap()).sum();
                assert!((got - target).abs() / target < 0.005, "{} {which}: {got} vs {target}", t["id"]);
                for (l, a) in lines.iter().zip(&each) {
                    assert!(*a >= l["min"].as_f64().unwrap() - 1e-9 && *a <= l["max"].as_f64().unwrap() + 1e-9, "{} {}", t["id"], l["kind"]);
                }
            }
        }
        // a clamp that bites: the remainder moves to the rooms that can take it
        let big = json!({"share": 0.5, "count": 1, "min": 10, "max": 100});
        let free = json!({"share": 0.5, "count": 1, "min": 10, "max": 1000});
        let each = sized(&[&big, &free], 1000.0);
        assert_eq!(each[0], 100.0);
        assert!((each[0] + each[1] - 1000.0).abs() < 1e-6, "{each:?}");
    }

    #[test]
    fn the_default_museum_builds_with_galleries_atrium_and_the_russian_names() {
        let s = parse_spec(&json!({"typology": "museum", "lang": "ru"})).unwrap_or_else(|e| panic!("{e:?}"));
        assert!(s.ground_rooms.iter().any(|r| r.name == ru("atrium_cap")));
        assert!(said(&s, &ru("museum_programme")));
        let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        let rooms: Vec<_> = p.blocks().flat_map(|(_, st)| &st.rooms).collect();
        assert!(rooms.len() >= 12, "{} rooms", rooms.len());
        assert!(rooms.iter().filter(|r| r.kind == RoomKind::Hall).count() >= 4);
    }

    fn height_of(name: &str, extra: J) -> Result<f64, String> {
        let mut v = json!({"typology": "office", "name": name});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        parse_spec(&v).map(|s| s.floor_height).map_err(|e| format!("{e:?}"))
    }

    #[test]
    fn a_temple_raises_its_one_storey_to_the_height_of_the_nave() {
        for w in langs(&["church", "Kirche", "教堂"], &["church"]) {
            let w = w.as_str();
            assert_eq!(height_of(w, json!({})), Ok(12.0), "{w}");
        }
        let s = parse_spec(&json!({"typology": "office", "name": "church"})).unwrap();
        assert!(said(&s, "typology.tall_space_max"));
        let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(p.storeys.len(), 1);
        assert_eq!(p.storeys[0].height, 12.0, "a real tall storey, not a 6 m one");
    }

    #[test]
    fn a_sports_hall_stands_as_high_as_its_court_needs() {
        for w in ["sports hall", "Sporthalle", "体育馆"] {
            assert_eq!(height_of(w, json!({})), Ok(9.0), "{w}");
        }
    }

    #[test]
    fn a_theatre_auditorium_and_stage_take_a_tall_storey() {
        for w in langs(&["theatre", "Konzerthaus"], &["theatre"]) {
            let w = w.as_str();
            assert_eq!(height_of(w, json!({})), Ok(14.0), "{w}");
        }
    }

    #[test]
    fn a_fire_station_apparatus_bay_is_taller_than_a_floor() {
        for w in langs(&["fire station", "Feuerwache"], &["fire_station"]) {
            let w = w.as_str();
            assert_eq!(height_of(w, json!({})), Ok(7.5), "{w}");
        }
        let s = spec("fire station");
        assert_eq!(s.floors, 1);
        assert_eq!(count(&s.rooms, RoomKind::Bedroom), 6, "the dormitories share the one storey");
    }

    fn heights(v: J) -> (Vec<f64>, f64) {
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{v}: {e:?}"));
        (s.storey_heights.clone(), s.floor_height)
    }

    #[test]
    fn a_three_storey_theatre_has_a_tall_auditorium_level_and_normal_ones() {
        let v = json!({"typology": "office", "name": "theatre", "floors": 3});
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(s.storey_heights, vec![14.0, 4.0, 4.0]);
        assert_eq!(s.building_height(), 22.0);
        let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        let z: Vec<f64> = p.storeys.iter().map(|st| st.elevation).collect();
        assert_eq!(z, vec![0.0, 14.0, 18.0]);
        assert!(said(&s, "14.0 m high"), "{:?}", s.notes);
    }

    #[test]
    fn the_tall_level_follows_the_rule_and_the_brief_heights() {
        let (h, f) = heights(json!({"typology": "office", "name": "church", "floors": 2, "rules": {"typology.tall_space_max": 9.0}}));
        assert_eq!((h, f), (vec![9.0, 4.0], 4.0));
        // heights of the brief are kept
        let (h, _) = heights(json!({"typology": "office", "name": "theatre", "floors": 2, "storey_heights": [10.0, 3.5]}));
        assert_eq!(h, vec![10.0, 3.5]);
        let (h, f) = heights(json!({"typology": "office", "name": "theatre", "floors": 2, "floor_height": 3.6}));
        assert_eq!((h, f), (vec![14.0, 3.6], 3.6));
        // one storey is a tall storey, not a list
        assert!(heights(json!({"typology": "office", "name": "theatre"})).0.is_empty());
    }

    #[test]
    fn a_museum_with_a_tall_atrium_raises_its_atrium_level() {
        let plain = parse_spec(&json!({"typology": "office", "name": "museum"})).unwrap_or_else(|e| panic!("{e:?}"));
        assert!(plain.storey_heights.is_empty());
        assert_eq!(plain.floor_height, 5.0);
        for f in langs(&["tall atrium", "Atrium, double height", "atrium: high"], &["tall_atrium"]) {
            let (h, _) = heights(json!({"typology": "office", "name": "museum", "features": [f]}));
            assert_eq!(h, vec![12.0, 4.0, 4.0], "{f}");
        }
        // an atrium alone, or a tall something else, changes nothing
        for f in ["atrium", "tall tower"] {
            assert!(heights(json!({"typology": "office", "name": "museum", "features": [f]})).0.is_empty(), "{f}");
        }
        let s = parse_spec(&json!({"typology": "office", "name": "museum", "features": ["tall atrium"]})).unwrap();
        let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(p.storeys[0].height, 12.0);
        assert_eq!(p.storeys[1].elevation, 12.0);
    }

    #[test]
    fn the_tall_space_follows_the_rule_and_the_brief() {
        // the rule caps the storey
        assert_eq!(height_of("theatre", json!({"rules": {"typology.tall_space_max": 10.0}})), Ok(10.0));
        // a floor height of the brief is kept
        assert_eq!(height_of("church", json!({"floor_height": 5.0})), Ok(5.0));
        // a one-storey brief may ask up to the rule, no more
        assert_eq!(height_of("church", json!({"floor_height": 15.0, "floors": 1})), Ok(15.0));
        assert!(height_of("church", json!({"floor_height": 16.0, "floors": 1})).is_err());
        // a building of several storeys keeps the 6 m cap on its normal storeys
        assert!(height_of("church", json!({"floor_height": 8.0, "floors": 2})).is_err());
        let s = parse_spec(&json!({"typology": "office", "name": "theatre", "floors": 2})).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(s.floor_height, 4.0);
        assert_eq!(s.storey_heights, vec![14.0, 4.0]);
        assert!(count(&s.ground_rooms, RoomKind::Hall) >= 2 && count(&s.rooms, RoomKind::Hall) >= 2);
    }

    #[test]
    fn a_library_gets_reading_halls_stacks_and_study_rooms() {
        for w in langs(&["library", "Bibliothek", "biblioteca", "图书馆"], &["library"]) {
            let w = w.as_str();
            let s = spec(w);
            assert_eq!(s.floors, 2, "{w}");
            assert_eq!(count(&s.ground_rooms, RoomKind::Hall), 2, "{w}");
            assert_eq!(count(&s.ground_rooms, RoomKind::Storage), 2, "{w}: stacks");
            assert_eq!(count(&s.rooms, RoomKind::Meeting), 4, "{w}: study rooms");
            assert!(said(&s, "library programme"), "{w}");
        }
    }

    #[test]
    fn a_theatre_gets_foyer_auditorium_stage_and_dressing_rooms() {
        for w in langs(&["theatre", "Theater", "concert hall", "teatro", "Konzerthaus", "剧院"], &["theatre"]) {
            let w = w.as_str();
            let s = spec(w);
            let g = &s.rooms;
            assert_eq!(s.floors, 1, "{w}: the auditorium and stage need one tall storey");
            assert_eq!(count(g, RoomKind::Hall), 2, "{w}: auditorium and stage");
            assert_eq!(count(g, RoomKind::Lobby), 1, "{w}: foyer");
            assert_eq!(count(g, RoomKind::Staff), 4, "{w}: dressing rooms");
            assert!(said(&s, "theatre programme"), "{w}");
        }
    }

    #[test]
    fn a_church_is_one_storey_with_a_nave_narthex_and_vestry() {
        for w in langs(&["church", "Kirche", "iglesia", "教堂", "chapel"], &["church"]) {
            let w = w.as_str();
            let s = spec(w);
            assert_eq!(s.floors, 1, "{w}");
            assert!(s.ground_rooms.is_empty(), "{w}: one storey takes rooms alone");
            assert_eq!(count(&s.rooms, RoomKind::Hall), 3, "{w}: nave, sanctuary, community hall");
            assert_eq!(count(&s.rooms, RoomKind::Lobby), 1, "{w}: narthex");
            assert_eq!(count(&s.rooms, RoomKind::Storage), 1, "{w}: vestry");
        }
    }

    #[test]
    fn a_sports_hall_is_one_storey_with_a_court_and_changing_rooms() {
        for w in langs(&["sports hall", "Sporthalle", "polideportivo", "体育馆"], &["sports_hall"]) {
            let w = w.as_str();
            let s = spec(w);
            assert_eq!(s.floors, 1, "{w}");
            assert_eq!(count(&s.rooms, RoomKind::Gym), 2, "{w}: court and fitness room");
            assert_eq!(count(&s.rooms, RoomKind::Staff), 2, "{w}: changing rooms");
            let court = s.rooms.iter().find(|r| r.kind == RoomKind::Gym).unwrap();
            assert!(court.area >= 300.0, "{w}: court {}", court.area);
        }
    }

    #[test]
    fn a_fire_station_gets_an_appliance_bay_dormitories_and_a_watch_room() {
        for w in langs(&["fire station", "Feuerwache", "estación de bomberos", "消防站"], &["fire_station"]) {
            let w = w.as_str();
            let s = spec(w);
            assert_eq!(s.floors, 1, "{w}");
            assert_eq!(count(&s.rooms, RoomKind::Workshop), 2, "{w}: apparatus bay and workshop");
            assert_eq!(count(&s.rooms, RoomKind::Bedroom), 6, "{w}: dormitories");
            assert!(s.rooms.iter().any(|r| r.area >= 300.0), "{w}: the bay is large");
        }
    }

    #[test]
    fn hotel_clinic_and_warehouse_hand_over_to_their_presets() {
        let (ru_hotel, ru_warehouse) = (ru("hotel"), ru("warehouse"));
        for (w, preset, kind) in [("hotel", "hotel", RoomKind::Bedroom), (ru_hotel.as_str(), "hotel", RoomKind::Bedroom), ("Klinik", "clinic", RoomKind::Office), (ru_warehouse.as_str(), "warehouse",RoomKind::Storage)] {
            let v = json!({"typology": "office", "name": w});
            let (filled, notes) = fill(&v).unwrap_or_else(|| panic!("{w}"));
            assert_eq!(filled["preset"], preset, "{w}");
            assert!(filled["rooms"].is_null() && filled["ground_rooms"].is_null(), "{w}: the preset's own programme, not a copy");
            assert!(notes.iter().any(|n| n.contains("preset")), "{w}: {notes:?}");
            let s = parse_spec(&v).unwrap_or_else(|e| panic!("{w}: {e:?}"));
            assert!(count(&s.rooms, kind) + count(&s.ground_rooms, kind) > 0, "{w}");
        }
    }

    #[test]
    fn school_and_office_hand_over_to_their_base_typologies() {
        for w in langs(&["school", "Schule", "学校"], &["school"]) {
            let w = w.as_str();
            let (filled, _) = fill(&json!({"typology": "office", "name": w})).unwrap_or_else(|| panic!("{w}"));
            assert_eq!(filled["typology"], "school", "{w}");
            assert!(filled["rooms"].is_null(), "{w}");
            parse_spec(&json!({"typology": "office", "name": w})).unwrap_or_else(|e| panic!("{w}: {e:?}"));
        }
        for w in langs(&["office building", "Bürogebäude", "办公楼"], &["office_building"]) {
            let w = w.as_str();
            assert!(asked(&json!({"typology": "apartment_section", "name": w})), "{w}");
            let (filled, _) = fill(&json!({"typology": "apartment_section", "name": w})).unwrap();
            assert_eq!(filled["typology"], "office", "{w}");
        }
        // nothing to hand over when the typology already is the base
        assert!(fill(&json!({"typology": "office", "name": "Office building"})).is_none());
    }

    /// Filling never writes a key the brief did not have unless it gives it a value.
    #[test]
    fn a_filled_brief_gains_no_empty_keys() {
        for v in [json!({"typology": "office", "name": "Office building"}), json!({"typology": "office", "name": "church"})] {
            let Some((out, _)) = fill(&v) else { continue };
            for (k, x) in out.as_object().unwrap() {
                assert!(!x.is_null() || !v[k].is_null(), "{v}: {k} written as null");
            }
        }
    }

    #[test]
    fn a_column_grows_with_the_storey_until_it_is_slender_enough() {
        use crate::project::build::column;
        let lim = crate::project::rules::rule("structure.column_slenderness_max");
        let base = crate::project::rules::rule("grid.column");
        assert_eq!(column(6.0).0, base.max(6.0 / lim), "a 6 m storey keeps the grid column when it is slender enough");
        for h in [3.0, 6.0, 9.0, 12.0, 14.0, 15.0] {
            let (b, d) = column(h);
            assert!(b >= base && b == d, "{h}");
            assert!(h / b <= lim + 1e-9, "{h} m on {b} m is slenderer than {lim}");
        }
        assert!(column(14.0).0 > column(6.0).0, "a 14 m storey needs a thicker column than a 6 m one");
    }

    #[test]
    fn columns_of_a_tall_frame_building_meet_the_slenderness_rule() {
        let lim = crate::project::rules::rule("structure.column_slenderness_max");
        let mut side = vec![];
        for h in [6.0, 14.0] {
            let v = json!({"typology": "office", "name": "Office", "floors": 1, "floor_height": h, "design": {"structure": "frame"}});
            let p = build_project(&parse_spec(&v).unwrap_or_else(|e| panic!("{h}: {e:?}"))).unwrap_or_else(|e| panic!("{h}: {e}"));
            let st = &p.storeys[0];
            assert_eq!(st.height, h);
            assert!(!st.layout.columns.is_empty(), "a frame office of {h} m has columns");
            for c in &st.layout.columns {
                assert!(st.height / c.section.0 <= lim + 1e-9, "{h} m storey on a {} m column", c.section.0);
            }
            side.push(st.layout.columns[0].section.0);
        }
        assert!(side[1] > side[0], "the 14 m columns are thicker than the 6 m ones: {side:?}");
    }

    #[test]
    fn tall_halls_either_have_columns_within_the_rule_or_bear_on_walls() {
        let lim = crate::project::rules::rule("structure.column_slenderness_max");
        for (name, h) in [("theatre", 14.0), ("sports hall", 9.0)] {
            let s = spec(name);
            let p = build_project(&s).unwrap_or_else(|e| panic!("{name}: {e}"));
            let st = &p.storeys[0];
            assert_eq!(st.height, h);
            if s.design.structure.is_frame() {
                assert!(!st.layout.columns.is_empty(), "{name}: a frame hall without columns");
            }
            for c in &st.layout.columns {
                assert!(st.height / c.section.0 <= lim + 1e-9, "{name}: {} m column", c.section.0);
            }
        }
    }

    /// Whether a service point lies in the envelope of the storey it serves: between its floor
    /// and ceiling, and on the top storey also in the roof void, up to the underside of the
    /// roof solid over that plan point (the hip model the plant layout uses).
    fn in_envelope(p: &crate::project::GProject, q: (f64, f64, f64)) -> bool {
        let Some(top) = p.storeys.last() else { return false };
        let base = p.storeys.first().map_or(0.0, |s| s.elevation);
        if !q.2.is_finite() || q.2 < base - 1e-6 {
            return false;
        }
        if q.2 <= top.elevation + top.height + 1e-6 {
            return true;
        }
        let r = &p.roof;
        if r.slope <= crate::project::FLAT_FALL_DEG + 1e-9 {
            // a flat roof: only the roof equipment zone of a unit, the generator's own
            return crate::project::hvac::roof_plant_zones(p)
                .iter()
                .any(|(b, z)| q.0 >= b.0 - 1e-6 && q.0 <= b.2 + 1e-6 && q.1 >= b.1 - 1e-6 && q.1 <= b.3 + 1e-6 && q.2 <= z + 1e-6);
        }
        let (xs, ys) = (r.ring.iter().map(|v| v.0), r.ring.iter().map(|v| v.1));
        let b = (xs.clone().fold(f64::MAX, f64::min), ys.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max), ys.fold(f64::MIN, f64::max));
        let e = (q.0 - b.0).min(b.2 - q.0).min(q.1 - b.1).min(b.3 - q.1);
        if e < 0.0 {
            return false;
        }
        let a = r.slope.to_radians();
        let under = r.elevation + r.lift + e * a.tan() - crate::project::roof_thickness(r.slope) / a.cos();
        q.2 <= under + 1e-6
    }

    /// Ducts and fire runs of a 12 m hall stay in its envelope: inside the storey, or in the
    /// roof void above the top storey up to the underside of the roof solid, never through it.
    /// The roof void is part of the top storey's envelope (a unit may stand there under a
    /// pitched roof); this is the rule as specified, not a looser bound. Over a flat roof a
    /// riser to the rooftop unit is roof equipment: it may leave the storey only inside that
    /// unit's footprint and connection legs, up to the top of the plant layers, the zone the
    /// generator itself places the plant in (`hvac::roof_plant_zones`).
    #[test]
    fn a_twelve_metre_hall_keeps_its_ducts_and_standpipes_inside_the_storey() {
        let s = parse_spec(&json!({"typology": "office", "name": "church", "services": true})).unwrap_or_else(|e| panic!("{e:?}"));
        let mut p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        let h = p.storeys[0].height;
        assert_eq!(h, 12.0);
        for d in &p.hvac.ducts {
            assert!(d.points.iter().all(|&q| in_envelope(&p, q)), "duct leaves the storey: {:?}", d.points);
        }
        for r in p.fire.runs.iter().chain(p.fire.standpipes.iter().flat_map(|s| s.runs.iter())) {
            assert!(r.run.points.iter().all(|&q| in_envelope(&p, q)), "fire run leaves the storey: {:?}", r.run.points);
        }
        // over a flat roof a riser to the unit is roof equipment: inside the unit's footprint
        // and connection legs, up to the top of the plant layers; anything past it fails
        let zones = crate::project::hvac::roof_plant_zones(&p);
        assert!(!zones.is_empty(), "the hall has a rooftop unit");
        let (b, z) = zones[0];
        let mid = ((b.0 + b.2) / 2.0, (b.1 + b.3) / 2.0);
        assert!(in_envelope(&p, (mid.0, mid.1, z)), "the top of the plant layers is the unit's own");
        assert!(!in_envelope(&p, (mid.0, mid.1, z + 0.05)), "a duct over the plant layers passes");
        assert!(!in_envelope(&p, (b.2 + 0.05, mid.1, h + 0.5)), "a duct off the unit and its legs passes");
        assert!(!in_envelope(&p, (mid.0, b.3 + 0.05, h + 0.5)), "a duct off the unit and its legs passes");
        assert!(!in_envelope(&p, (b.2 + 0.05, mid.1, h + 1.0)), "a duct over a flat roof's storey passes");
        // under a pitched roof the void is the top storey's: a duct pushed through the roof
        // solid fails, a point below the floor too
        let q = p.hvac.ducts.iter().flat_map(|d| d.points.iter()).copied().next().expect("the hall has ducts");
        p.roof.slope = 30.0;
        let r = &p.roof;
        let (xs, ys) = (r.ring.iter().map(|v| v.0), r.ring.iter().map(|v| v.1));
        let b = (xs.clone().fold(f64::MAX, f64::min), ys.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max), ys.fold(f64::MIN, f64::max));
        let e = (q.0 - b.0).min(b.2 - q.0).min(q.1 - b.1).min(b.3 - q.1).max(0.0);
        let a = r.slope.to_radians();
        // the underside of the solid passes, a point just into the solid does not
        let under = r.elevation + r.lift + e * a.tan() - crate::project::roof_thickness(r.slope) / a.cos();
        assert!(in_envelope(&p, (q.0, q.1, under.max(h))), "the roof void is the top storey's own");
        assert!(!in_envelope(&p, (q.0, q.1, under.max(h) + 0.05)), "a duct pushed into the roof solid passes");
        assert!(!in_envelope(&p, (q.0, q.1, -1.0)), "a duct under the floor passes");
    }

    #[test]
    fn a_twelve_metre_temple_needs_no_standpipe_and_one_forced_stays_inside() {
        // standpipes start at the floor height of the top storey, not at the height of one tall storey
        let v = json!({"typology": "office", "name": "church", "services": true});
        let p = build_project(&parse_spec(&v).unwrap_or_else(|e| panic!("{e:?}"))).unwrap_or_else(|e| panic!("{e}"));
        assert!(p.fire.standpipes.is_empty(), "a one-storey hall has no standpipe");
        let v = json!({"typology": "office", "name": "church", "services": true, "rules": {"fire.standpipe_height": 0.0}});
        let p = build_project(&parse_spec(&v).unwrap_or_else(|e| panic!("{e:?}"))).unwrap_or_else(|e| panic!("{e}"));
        let h = p.storeys[0].height;
        for r in p.fire.standpipes.iter().flat_map(|s| s.runs.iter()) {
            assert!(r.run.points.iter().all(|q| q.2.is_finite() && q.2 >= -1e-6 && q.2 <= h + 1e-6), "{:?}", r.run.points);
        }
    }

    /// The filled rooms keep within `typology.net_share` of the outline, whatever the table's shares.
    #[test]
    fn a_filled_programme_keeps_within_the_net_share_of_its_outline() {
        let s = parse_spec(&json!({"typology": "office", "name": "church"})).unwrap();
        let (w, d) = (s.width.unwrap(), s.depth.unwrap());
        let net = s.rules.get("typology.net_share");
        assert!(total(&s.rooms) <= net * w * d * 1.01, "{} m2 of rooms on {w} x {d}", total(&s.rooms));
    }

    /// A room past what one band holds turns a given outline into the deep plan, the room in the bay.
    #[test]
    fn a_dominant_room_turns_the_plan_deep() {
        let v = json!({"typology": "office", "floors": 1, "footprint": {"width": 36.0, "depth": 25.0},
            "rooms": [{"kind": "hall", "area": 500, "count": 1}, {"kind": "office", "area": 20, "count": 2}]});
        let s = parse_spec(&v).unwrap();
        assert!(s.hall_depth.is_some(), "a 500 m2 hall in a 36 x 25 m plan takes the open bay");
        build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        let small = parse_spec(&json!({"typology": "office", "floors": 1, "footprint": {"width": 36.0, "depth": 25.0},
            "rooms": [{"kind": "office", "area": 60, "count": 6}]})).unwrap();
        assert!(small.hall_depth.is_none());
    }
}
