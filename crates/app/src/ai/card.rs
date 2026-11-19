//! Element cards and the budgeted model summary. Plain data in, deterministic text out: the same
//! model gives the same text (prompt caching), and the text never passes its character budget.

use super::fmt::{self, Out, Reply};
use serde_json::{json, Value as J};

/// Hard limit of one card, in characters.
pub const CARD_MAX: usize = 400;
/// Default budget of `model_summary`, in tokens.
pub const SUMMARY_TOKENS: usize = 1500;
const CHARS_PER_TOKEN: usize = 4;
const SUMMARY_MIN_CHARS: usize = 300;
const PART_MAX: usize = 80;
const SEP: &str = " \u{b7} ";
/// Room kept for the closing ` \u{b7} +NN more`.
const CUT_RESERVE: usize = 12;
const ROOMS_SHOWN: usize = 2;

/// Where an element sits: its host and, for a door or window, the distance from the host start.
#[derive(Clone, Debug, PartialEq)]
pub struct Host {
    pub key: String,
    pub along: Option<f64>,
}

/// Everything a card says, already read from the model. SI units: metres, square and cubic metres.
#[derive(Clone, Debug, Default)]
pub struct CardIn {
    pub key: String,
    pub ty: String,
    pub name: String,
    pub mark: String,
    pub level: String,
    pub nums: Vec<(String, f64)>,
    pub props: Vec<(String, String)>,
    pub host: Option<Host>,
    /// `3.04 Kitchen`, in a stable order.
    pub rooms: Vec<String>,
    /// Plan items of the AI that touch this element.
    pub ai_items: usize,
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

fn cut(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// A number with at most 3 decimals and no trailing zeros.
fn n3(x: f64) -> String {
    if !x.is_finite() {
        return "n/a".to_string();
    }
    let r = (x * 1000.0).round() / 1000.0;
    if r == 0.0 {
        "0".to_string()
    } else {
        format!("{r}")
    }
}

fn unit_of(k: &str) -> &'static str {
    match k {
        "length" | "width" | "height" | "thickness" | "sill" | "offset" | "depth"
        | "perimeter" => " m",
        "area" | "surface" => " m2",
        "volume" => " m3",
        _ => "",
    }
}

fn rank(k: &str) -> usize {
    [
        "length",
        "width",
        "height",
        "thickness",
        "sill",
        "offset",
        "area",
        "volume",
    ]
    .iter()
    .position(|x| *x == k)
    .unwrap_or(100)
}

fn size_part(nums: &[(String, f64)]) -> Option<String> {
    let mut v: Vec<&(String, f64)> = nums.iter().collect();
    v.sort_by(|a, b| {
        rank(&a.0)
            .cmp(&rank(&b.0))
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.total_cmp(&b.1))
    });
    let mut out = String::new();
    let mut shown = 0;
    for (k, x) in &v {
        let item = format!("{} {}{}", fmt::clip(k, 20), n3(*x), unit_of(k));
        let add = if out.is_empty() {
            item
        } else {
            format!(", {item}")
        };
        if chars(&out) + chars(&add) > PART_MAX {
            break;
        }
        out.push_str(&add);
        shown += 1;
    }
    if shown == 0 {
        return None;
    }
    if shown < v.len() {
        out.push_str(&format!(", +{}", v.len() - shown));
    }
    Some(out)
}

fn params_part(props: &[(String, String)]) -> Option<String> {
    if props.is_empty() {
        return None;
    }
    let mut v: Vec<&(String, String)> = props.iter().collect();
    v.sort();
    let mut out = format!("{} params:", v.len());
    let mut shown = 0;
    for (k, x) in &v {
        let item = format!("{}={}", fmt::clip(k, 24), fmt::clip(x, 24));
        let add = if shown == 0 {
            format!(" {item}")
        } else {
            format!(", {item}")
        };
        // room for the closing ` +NNN`
        if chars(&out) + chars(&add) + 5 > PART_MAX {
            break;
        }
        out.push_str(&add);
        shown += 1;
    }
    if shown < v.len() {
        out.push_str(&format!(" +{}", v.len() - shown));
    }
    Some(out)
}

fn rooms_part(rooms: &[String]) -> Option<String> {
    if rooms.is_empty() {
        return None;
    }
    let head: Vec<String> = rooms
        .iter()
        .take(ROOMS_SHOWN)
        .map(|r| fmt::clip(r, 30))
        .collect();
    let mut s = format!(
        "{} {}",
        if rooms.len() == 1 { "room" } else { "rooms" },
        head.join(", ")
    );
    if rooms.len() > ROOMS_SHOWN {
        s.push_str(&format!(", +{}", rooms.len() - ROOMS_SHOWN));
    }
    Some(s)
}

/// The card line. Parts go in priority order; the ones that do not fit are counted, not cut in
/// half. Never longer than `max` characters.
pub fn card_text(c: &CardIn, max: usize) -> String {
    let mut head = fmt::clip(&c.key, 40);
    if !c.ty.is_empty() {
        head.push(' ');
        head.push_str(&fmt::clip(&c.ty, 40));
    }
    let mut parts: Vec<String> = vec![head];
    parts.extend(size_part(&c.nums));
    if !c.level.is_empty() {
        parts.push(format!("level {}", fmt::clip(&c.level, 30)));
    }
    if let Some(h) = &c.host {
        parts.push(match h.along {
            Some(a) => format!("in {} @{} m from start", fmt::clip(&h.key, 30), n3(a)),
            None => format!("in {}", fmt::clip(&h.key, 30)),
        });
    }
    parts.extend(rooms_part(&c.rooms));
    if c.ai_items > 0 {
        parts.push(format!("changed by AI in {} plan item(s)", c.ai_items));
    }
    if !c.name.is_empty() {
        parts.push(format!("name {}", fmt::clip(&c.name, 40)));
    }
    if !c.mark.is_empty() {
        parts.push(format!("mark {}", fmt::clip(&c.mark, 30)));
    }
    parts.extend(params_part(&c.props));

    let mut out = String::new();
    let mut taken = 0;
    for (i, p) in parts.iter().enumerate() {
        let add = if out.is_empty() {
            p.clone()
        } else {
            format!("{SEP}{p}")
        };
        let reserve = if i + 1 == parts.len() { 0 } else { CUT_RESERVE };
        if chars(&out) + chars(&add) + reserve > max {
            break;
        }
        out.push_str(&add);
        taken += 1;
    }
    if taken < parts.len() {
        out.push_str(&format!("{SEP}+{} more", parts.len() - taken));
    }
    cut(&out, max)
}

/// The tool reply: the card as one text field.
pub fn card_reply(c: &CardIn) -> Reply {
    fmt::line("card", &card_text(c, CARD_MAX))
}

/// Is `p` inside the outer ring and outside the holes, or within `tol` of the outer ring?
pub fn near_region(rings: &[Vec<(f64, f64)>], p: (f64, f64), tol: f64) -> bool {
    let Some((outer, holes)) = rings.split_first() else {
        return false;
    };
    if outer.len() < 3 {
        return false;
    }
    let edge_dist = |ring: &[(f64, f64)]| {
        (0..ring.len())
            .map(|i| seg_dist(p, ring[i], ring[(i + 1) % ring.len()]))
            .fold(f64::INFINITY, f64::min)
    };
    if edge_dist(outer.as_slice()) <= tol {
        return true;
    }
    inside(outer, p) && !holes.iter().any(|h| h.len() >= 3 && inside(h, p))
}

fn inside(ring: &[(f64, f64)], p: (f64, f64)) -> bool {
    let mut odd = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[j]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            odd = !odd;
        }
        j = i;
    }
    odd
}

fn seg_dist(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
    };
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// Characters of a summary for a budget in tokens, never above what a reply may hold.
pub fn budget_chars(tokens: usize, reply_max: usize) -> usize {
    tokens
        .saturating_mul(CHARS_PER_TOKEN)
        .max(SUMMARY_MIN_CHARS)
        .min(reply_max.max(SUMMARY_MIN_CHARS))
}

/// The model as the summary sees it. Order of the vectors does not matter: `summary` sorts.
#[derive(Clone, Debug, Default)]
pub struct SummaryIn {
    /// id, name, elevation in metres, element count.
    pub levels: Vec<(i64, String, f64, usize)>,
    pub cats: Vec<(String, usize)>,
    /// category, type name, element count.
    pub types: Vec<(String, String, usize)>,
    /// ref, label, area in square metres.
    pub rooms: Vec<(String, String, f64)>,
    /// min x y z, max x y z in metres.
    pub bounds: Option<[f64; 6]>,
}

/// Levels, categories, rooms and types within `max` characters. What does not fit is counted:
/// `levels_more`, `rooms_more`, `types_more`, and fields that do not fit at all are in `omitted`.
pub fn summary(inp: &SummaryIn, max: usize) -> Reply {
    let mut levels = inp.levels.clone();
    levels.sort_by(|a, b| a.2.total_cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    let mut cats = inp.cats.clone();
    cats.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut types = inp.types.clone();
    types.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    let mut rooms = inp.rooms.clone();
    rooms.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

    let total: usize = cats.iter().map(|c| c.1).sum();
    let mut o = Out::new(max);
    o.put_own("units", &json!("SI: m, m2, m3"));
    o.put_own("elements", &json!(total));
    let mut by_cat = serde_json::Map::new();
    for (k, n) in &cats {
        by_cat.insert(k.clone(), json!(n));
    }
    o.put("cats", &J::Object(by_cat));
    if let Some(b) = inp.bounds {
        o.put_own(
            "bounds",
            &json!({
                "min": [fmt::num(b[0]), fmt::num(b[1]), fmt::num(b[2])],
                "max": [fmt::num(b[3]), fmt::num(b[4]), fmt::num(b[5])],
            }),
        );
    }
    o.put_own(
        "cols",
        &json!({
            "levels": ["id", "name", "elev_m", "n"],
            "rooms": ["ref", "label", "area_m2"],
            "types": ["cat", "type", "n"],
        }),
    );
    let level_items: Vec<J> = levels
        .iter()
        .map(|l| json!([l.0, l.1, fmt::num(l.2), l.3]))
        .collect();
    // levels, rooms and types share what is left (3 : 3 : 4); unused room flows to the next list
    o.list_capped("levels", &level_items, o.left() * 3 / 10);
    if !rooms.is_empty() {
        let area: f64 = rooms.iter().map(|r| r.2).sum();
        o.put_own("room_area_m2", &fmt::num(area));
    }
    let room_items: Vec<J> = rooms
        .iter()
        .map(|r| json!([r.0, r.1, fmt::num(r.2)]))
        .collect();
    o.list_capped("rooms", &room_items, o.left() * 3 / 7);
    let type_items: Vec<J> = types.iter().map(|t| json!([t.0, t.1, t.2])).collect();
    o.list("types", &type_items);
    o.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn door() -> CardIn {
        CardIn {
            key: "door:55".into(),
            ty: "Single flush".into(),
            level: "L3".into(),
            nums: vec![("height".into(), 2.1), ("width".into(), 0.9)],
            host: Some(Host {
                key: "wall:12".into(),
                along: Some(1.2),
            }),
            rooms: vec!["3.04 Kitchen".into()],
            ai_items: 1,
            ..Default::default()
        }
    }

    fn wall_with(n: usize) -> CardIn {
        CardIn {
            key: "wall:7".into(),
            ty: "Generic 200 ".repeat(8),
            name: "n".repeat(300),
            mark: "m".repeat(300),
            level: "Level ".repeat(30),
            nums: (0..n).map(|i| (format!("dim{i}"), i as f64 * 1.2345)).collect(),
            props: (0..n)
                .map(|i| (format!("param_{i:03}"), "value ".repeat(20)))
                .collect(),
            rooms: (0..n).map(|i| format!("{i} Room")).collect(),
            ai_items: 3,
            ..Default::default()
        }
    }

    #[test]
    fn card_under_400_chars() {
        for n in [0usize, 1, 5, 200, 2000] {
            let t = card_text(&wall_with(n), CARD_MAX);
            assert!(chars(&t) <= CARD_MAX, "{n}: {}", chars(&t));
            assert!(t.starts_with("wall:7"), "{t}");
        }
        for max in [10usize, 40, 100, 400] {
            assert!(chars(&card_text(&wall_with(200), max)) <= max);
        }
    }

    #[test]
    fn card_never_exceeds_the_budget_for_a_wall_with_200_params() {
        let c = wall_with(200);
        let t = card_text(&c, CARD_MAX);
        assert!(chars(&t) <= CARD_MAX, "{}", chars(&t));
        let r = card_reply(&c);
        let v: J = serde_json::from_str(&r.text).unwrap();
        assert!(chars(v["card"].as_str().unwrap()) <= CARD_MAX);
        assert_eq!(v["untrusted"], "data");
        let short = card_text(&c, 200);
        assert!(chars(&short) <= 200 && short.contains(" more"), "{short}");
    }

    #[test]
    fn card_lists_host_and_room() {
        let t = card_text(&door(), CARD_MAX);
        assert!(t.contains("in wall:12 @1.2 m from start"), "{t}");
        assert!(t.contains("room 3.04 Kitchen"), "{t}");
        assert!(t.contains("changed by AI in 1 plan item(s)"), "{t}");
    }

    #[test]
    fn card_names_units_and_ids() {
        let t = card_text(&door(), CARD_MAX);
        assert!(t.starts_with("door:55 Single flush"), "{t}");
        assert!(t.contains("width 0.9 m, height 2.1 m"), "{t}");
        let a = CardIn {
            key: "floor:2".into(),
            nums: vec![("volume".into(), 3.0), ("area".into(), 12.5)],
            ..Default::default()
        };
        assert!(card_text(&a, CARD_MAX).contains("area 12.5 m2, volume 3 m3"));
    }

    #[test]
    fn card_is_deterministic() {
        let a = card_text(&wall_with(50), CARD_MAX);
        let mut c = wall_with(50);
        c.props.reverse();
        c.nums.reverse();
        assert_eq!(a, card_text(&c, CARD_MAX));
        assert_eq!(card_text(&door(), CARD_MAX), card_text(&door(), CARD_MAX));
    }

    #[test]
    fn card_text_from_the_model_is_cleaned() {
        let mut c = door();
        c.name = "a\u{202e}b\nc".into();
        let t = card_text(&c, CARD_MAX);
        assert!(t.contains("name abc"), "{t}");
        assert!(!t.contains('\n'));
    }

    #[test]
    fn a_door_between_two_rooms_lists_both() {
        let sq = |x0: f64| {
            vec![vec![
                (x0, 0.0),
                (x0 + 4.0, 0.0),
                (x0 + 4.0, 4.0),
                (x0, 4.0),
            ]]
        };
        let p = (4.0, 2.0);
        assert!(near_region(&sq(0.0), p, 0.3));
        assert!(near_region(&sq(4.0), p, 0.3));
        assert!(!near_region(&sq(10.0), p, 0.3));
        assert!(near_region(&sq(0.0), (2.0, 2.0), 0.0));
        let mut holed = sq(0.0);
        holed.push(vec![(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)]);
        assert!(!near_region(&holed, (2.0, 2.0), 0.1));
        assert!(!near_region(&[], p, 1.0));
    }

    fn big() -> SummaryIn {
        SummaryIn {
            levels: (0..60)
                .map(|i| (i, format!("Level {i}"), i as f64 * 3.0, 100 + i as usize))
                .collect(),
            cats: ["wall", "door", "window", "floor", "room"]
                .iter()
                .enumerate()
                .map(|(i, c)| (c.to_string(), 1000 - i * 7))
                .collect(),
            types: (0..500)
                .map(|i| ("wall".to_string(), format!("Type {i}"), 500 - i % 50))
                .collect(),
            rooms: (0..300)
                .map(|i| (format!("room:{i}"), format!("{i}.01 Office"), 12.5))
                .collect(),
            bounds: Some([0.0, 0.0, 0.0, 40.0, 25.0, 12.0]),
        }
    }

    #[test]
    fn summary_respects_budget() {
        for max in [300usize, 500, 1000, 1500, 6000, 24000] {
            let r = summary(&big(), max);
            assert!(chars(&r.text) <= max, "{max}: {}", chars(&r.text));
            let v: J = serde_json::from_str(&r.text).unwrap();
            assert_eq!(v["untrusted"], "data");
            if (1500..24000).contains(&max) {
                assert!(v["types_more"].as_u64().is_some(), "{max}: {}", r.text);
                assert!(v["rooms_more"].as_u64().is_some(), "{max}");
            }
        }
        let tiny = summary(&SummaryIn::default(), 300);
        assert!(serde_json::from_str::<J>(&tiny.text).is_ok());
        assert_eq!(budget_chars(1500, 100_000), 6000);
        assert_eq!(budget_chars(1, 100_000), SUMMARY_MIN_CHARS);
        assert_eq!(budget_chars(100_000, 8000), 8000);
    }

    #[test]
    fn summary_says_what_it_cut_and_keeps_the_header() {
        let r = summary(&big(), 1500);
        let v: J = serde_json::from_str(&r.text).unwrap();
        assert_eq!(v["units"], "SI: m, m2, m3");
        assert_eq!(v["elements"], 1000 + 993 + 986 + 979 + 972);
        assert_eq!(v["levels"][0][1], "Level 0");
        let shown = v["rooms"].as_array().map_or(0, Vec::len);
        let more = v["rooms_more"].as_u64().unwrap_or(0) as usize;
        assert!(shown + more == 300 || v["omitted"].is_array(), "{}", r.text);
    }

    #[test]
    fn summary_is_deterministic() {
        let a = summary(&big(), 3000).text;
        let mut r = big();
        r.levels.reverse();
        r.cats.reverse();
        r.types.reverse();
        r.rooms.reverse();
        assert_eq!(a, summary(&r, 3000).text);
        assert_eq!(a, summary(&big(), 3000).text);
    }
}
