//! Room tags of the plan: fit the text into the room, cache the layout per zoom bucket.
//!
//! `fit_room_label` is pure (text measuring is injected). Priority of the lines: name, then
//! number, then area; whatever does not fit is dropped, nothing is drawn outside the room box.

use crate::editor::cw::inside;
use crate::editor::levels::RoomItem;
use ddc_geom::{v2, V2};
use std::collections::HashMap;

/// Which font a string is measured with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Title,
    Area,
}

/// Line heights and padding in screen pixels.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub title_h: f32,
    pub area_h: f32,
    pub gap: f32,
    pub pad: f32,
    /// Smallest room box side (px) that still gets a dot marker.
    pub dot_min: f32,
}

/// Result of fitting: lines to draw, or only a dot, or nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fit {
    pub title: Option<String>,
    pub area: Option<String>,
    pub dot: bool,
}

impl Fit {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.area.is_none() && !self.dot
    }
}

/// `text` cut with an ellipsis so that it is at most `max_w` wide; None when not even one
/// character plus the ellipsis fits.
pub fn elide(text: &str, max_w: f32, measure: &mut dyn FnMut(&str, Face) -> f32) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if measure(text, Face::Title) <= max_w {
        return Some(text.to_string());
    }
    let chars: Vec<char> = text.chars().collect();
    let build = |n: usize| {
        let mut s: String = chars[..n].iter().collect();
        s = s.trim_end().to_string();
        s.push('\u{2026}');
        s
    };
    // longest prefix whose elided form fits (width grows with the prefix)
    let (mut lo, mut hi) = (0usize, chars.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&build(mid), Face::Title) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    // a single letter plus an ellipsis says nothing: show the number instead
    (lo >= 2).then(|| build(lo))
}

/// Fit the tag of one room into a box of `box_w` x `box_h` px.
pub fn fit_room_label(
    number: &str,
    name: &str,
    area: &str,
    box_w: f32,
    box_h: f32,
    m: &Metrics,
    measure: &mut dyn FnMut(&str, Face) -> f32,
) -> Fit {
    let (iw, ih) = (box_w - 2.0 * m.pad, box_h - 2.0 * m.pad);
    let dot = box_w.min(box_h) >= m.dot_min;
    if iw <= 0.0 || ih < m.title_h {
        return Fit { dot, ..Fit::default() };
    }
    let (number, name) = (number.trim(), name.trim());
    let full = format!("{number} {name}").trim().to_string();
    let title = if !name.is_empty() && measure(&full, Face::Title) <= iw {
        Some(full)
    } else {
        elide(name, iw, measure).or_else(|| elide(number, iw, measure))
    };
    let Some(title) = title else {
        return Fit { dot, ..Fit::default() };
    };
    let area = (ih >= m.title_h + m.gap + m.area_h
        && !area.is_empty()
        && measure(area, Face::Area) <= iw)
        .then(|| area.to_string());
    Fit { title: Some(title), area, dot: false }
}

/// Axis-aligned box (w, h in model units) around `c` that lies inside the room
/// (outer ring minus holes); (0, 0) when nothing fits. Rotated rooms get their inscribed box.
pub fn inscribed_box(c: V2, rings: &[Vec<V2>]) -> (f64, f64) {
    // nearest boundary crossing along each axis direction (left, right, down, up)
    let mut d = [f64::INFINITY; 4];
    for r in rings {
        for k in 0..r.len() {
            let (a, b) = (r[k], r[(k + 1) % r.len()]);
            if (a.y - c.y) * (b.y - c.y) <= 0.0 && a.y != b.y {
                let x = a.x + (c.y - a.y) / (b.y - a.y) * (b.x - a.x) - c.x;
                let i = if x < 0.0 { 0 } else { 1 };
                d[i] = d[i].min(x.abs());
            }
            if (a.x - c.x) * (b.x - c.x) <= 0.0 && a.x != b.x {
                let y = a.y + (c.x - a.x) / (b.x - a.x) * (b.y - a.y) - c.y;
                let i = if y < 0.0 { 2 } else { 3 };
                d[i] = d[i].min(y.abs());
            }
        }
    }
    // 0.999: corners exactly on the boundary are ambiguous for the even-odd test
    let (mut hw, mut hh) = (d[0].min(d[1]) * 0.999, d[2].min(d[3]) * 0.999);
    if !hw.is_finite() || !hh.is_finite() {
        return (0.0, 0.0);
    }
    let Some((outer, holes)) = rings.split_first() else {
        return (0.0, 0.0);
    };
    let ok = |hw: f64, hh: f64| {
        [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].iter().all(|(sx, sy)| {
            let p = v2(c.x + sx * hw, c.y + sy * hh);
            inside(p, outer) && !holes.iter().any(|h| inside(p, h))
        })
    };
    for _ in 0..14 {
        if ok(hw, hh) {
            return (2.0 * hw, 2.0 * hh);
        }
        hw *= 0.8;
        hh *= 0.8;
    }
    (0.0, 0.0)
}

/// Point-in-room lookup through a uniform grid: each query tests only nearby rooms.
pub struct RoomIndex<'a> {
    rooms: &'a [RoomItem],
    cell: f64,
    grid: HashMap<(i64, i64), Vec<usize>>,
    big: Vec<usize>,
    /// Number of polygon tests done (for the complexity test).
    pub tests: std::cell::Cell<usize>,
}

impl<'a> RoomIndex<'a> {
    pub fn new(rooms: &'a [RoomItem]) -> Self {
        let boxes: Vec<(V2, V2)> = rooms
            .iter()
            .map(|r| {
                let o = r.rings.first().map(|v| v.as_slice()).unwrap_or(&[]);
                o.iter().fold((v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN)), |(lo, hi), q| {
                    (v2(lo.x.min(q.x), lo.y.min(q.y)), v2(hi.x.max(q.x), hi.y.max(q.y)))
                })
            })
            .collect();
        let ext: Vec<f64> = boxes
            .iter()
            .filter(|(lo, hi)| hi.x >= lo.x)
            .map(|(lo, hi)| (hi.x - lo.x).max(hi.y - lo.y))
            .collect();
        let cell = if ext.is_empty() { 1.0 } else { ext.iter().sum::<f64>() / ext.len() as f64 }
            .max(0.5);
        let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        let mut big = vec![];
        for (i, (lo, hi)) in boxes.iter().enumerate() {
            if hi.x < lo.x {
                continue;
            }
            let (x0, x1) = ((lo.x / cell).floor() as i64, (hi.x / cell).floor() as i64);
            let (y0, y1) = ((lo.y / cell).floor() as i64, (hi.y / cell).floor() as i64);
            if (x1 - x0 + 1) * (y1 - y0 + 1) > 64 {
                big.push(i);
                continue;
            }
            for gx in x0..=x1 {
                for gy in y0..=y1 {
                    grid.entry((gx, gy)).or_default().push(i);
                }
            }
        }
        Self { rooms, cell, grid, big, tests: std::cell::Cell::new(0) }
    }

    /// Id of the first (lowest index) room whose contour contains `p`.
    pub fn pick(&self, p: V2) -> Option<i64> {
        let key = ((p.x / self.cell).floor() as i64, (p.y / self.cell).floor() as i64);
        let near = self.grid.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
        near.iter()
            .chain(self.big.iter())
            .filter(|&&i| {
                self.tests.set(self.tests.get() + 1);
                let Some((o, hs)) = self.rooms[i].rings.split_first() else {
                    return false;
                };
                inside(p, o) && !hs.iter().any(|h| inside(p, h))
            })
            .min()
            .map(|&i| self.rooms[i].id)
    }
}

/// Tag of one room as cached: where it is and what is drawn.
#[derive(Clone, Debug)]
pub struct LabelEntry {
    pub id: i64,
    pub c0: V2,
    pub fit: Fit,
    pub title_w: f32,
    pub area_w: f32,
}

/// Zoom bucket: quarter octaves, rounded down so the fit is made at a scale not above the
/// real one (the box is never overestimated) and a wheel step does not refit every frame.
pub fn zoom_bucket(px_per_m: f64) -> i32 {
    (px_per_m.max(1e-9).log2() * 4.0 + 1e-9).floor() as i32
}

/// Scale (px per metre) that represents a bucket.
pub fn bucket_scale(b: i32) -> f64 {
    2f64.powf(f64::from(b) / 4.0)
}

/// Layout of all room tags at one zoom bucket. `centres` are (label point, area in m2).
pub fn build_layout(
    centres: &[(V2, f64)],
    rooms: &[RoomItem],
    area_text: &dyn Fn(f64) -> String,
    bucket: i32,
    m: &Metrics,
    measure: &mut dyn FnMut(&str, Face) -> f32,
) -> Vec<LabelEntry> {
    let idx = RoomIndex::new(rooms);
    let s = bucket_scale(bucket);
    let mut out = Vec::with_capacity(centres.len());
    for &(c0, area) in centres {
        let Some(id) = idx.pick(c0) else { continue };
        let Some(r) = rooms.iter().find(|r| r.id == id) else { continue };
        let (w, h) = inscribed_box(c0, &r.rings);
        let area_s = area_text(area);
        let fit = fit_room_label(
            &r.number,
            &r.name,
            &area_s,
            (w * s) as f32,
            (h * s) as f32,
            m,
            measure,
        );
        if fit.is_empty() {
            continue;
        }
        let tw = fit.title.as_deref().map_or(0.0, |t| measure(t, Face::Title));
        let aw = fit.area.as_deref().map_or(0.0, |t| measure(t, Face::Area));
        out.push(LabelEntry { id, c0, fit, title_w: tw, area_w: aw });
    }
    out
}

/// Cache key: model state, level, zoom bucket and the area text format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutKey {
    pub model: (usize, u64, u64, i64),
    pub bucket: i32,
    pub fmt: String,
}

/// One-slot cache of the label layout.
#[derive(Default)]
pub struct LayoutCache {
    key: Option<LayoutKey>,
    entries: Vec<LabelEntry>,
    /// How many times the layout was rebuilt.
    pub builds: usize,
}

impl LayoutCache {
    pub fn get(
        &mut self,
        key: LayoutKey,
        build: impl FnOnce() -> Vec<LabelEntry>,
    ) -> &[LabelEntry] {
        if self.key.as_ref() != Some(&key) {
            self.entries = build();
            self.key = Some(key);
            self.builds += 1;
        }
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Metrics = Metrics { title_h: 14.0, area_h: 12.0, gap: 4.0, pad: 3.0, dot_min: 8.0 };

    // 6 px per character, whatever the face
    fn mw(s: &str, _f: Face) -> f32 {
        s.chars().count() as f32 * 6.0
    }

    fn fit(num: &str, name: &str, area: &str, w: f32, h: f32) -> Fit {
        fit_room_label(num, name, area, w, h, &M, &mut mw)
    }

    #[test]
    fn big_room_shows_all_lines() {
        let f = fit("101", "Office", "24.0 m2", 200.0, 100.0);
        assert_eq!(f.title.as_deref(), Some("101 Office"));
        assert_eq!(f.area.as_deref(), Some("24.0 m2"));
        assert!(!f.dot);
    }

    #[test]
    fn small_room_keeps_name_only() {
        // one title line fits, the area line does not
        let f = fit("101", "Office", "24.0 m2", 120.0, 22.0);
        assert!(f.title.is_some());
        assert_eq!(f.area, None);
    }

    #[test]
    fn narrow_room_drops_number_before_name() {
        let f = fit("101", "Office", "24.0 m2", 44.0, 60.0);
        assert_eq!(f.title.as_deref(), Some("Office"));
        assert_eq!(f.area, None); // 42 px of area text do not fit in 38 px
    }

    #[test]
    fn tiny_room_is_dot_or_nothing() {
        let f = fit("101", "Office", "24.0 m2", 12.0, 12.0);
        assert!(f.title.is_none() && f.area.is_none() && f.dot);
        let g = fit("101", "Office", "24.0 m2", 6.0, 6.0);
        assert!(g.is_empty());
    }

    #[test]
    fn long_name_is_elided_inside_width() {
        let name = "A very long conference room name";
        let f = fit("", name, "9 m2", 100.0, 40.0);
        let t = f.title.unwrap();
        assert!(t.ends_with('\u{2026}'), "{t}");
        assert!(mw(&t, Face::Title) <= 94.0, "{t}");
        // the longest prefix that fits is used: one more char would overflow
        assert!(t.chars().count() >= 14, "{t}");
    }

    #[test]
    fn name_that_cannot_fit_falls_back_to_number() {
        let f = fit("12", "Conference", "", 22.0, 30.0);
        assert_eq!(f.title.as_deref(), Some("12"));
    }

    #[test]
    fn elide_edge_cases() {
        assert_eq!(elide("", 100.0, &mut mw), None);
        assert_eq!(elide("abc", 100.0, &mut mw), Some("abc".into()));
        assert_eq!(elide("abcdef", 5.0, &mut mw), None);
        assert_eq!(elide("abcdef", 12.0, &mut mw), None);
        assert_eq!(elide("abcdef", 18.0, &mut mw), Some("ab\u{2026}".into()));
    }

    #[test]
    fn zoom_change_changes_fit() {
        let near = fit("", "Office", "", 4.0 * 100.0, 3.0 * 100.0);
        let far = fit("", "Office", "", 4.0 * 5.0, 3.0 * 5.0);
        assert!(near.title.is_some() && near.area.is_none());
        assert!(far.title.is_none() && far.dot);
    }

    #[test]
    fn buckets_are_quarter_octaves() {
        assert_eq!(zoom_bucket(1.0), 0);
        assert_eq!(zoom_bucket(2.0), 4);
        assert_eq!(zoom_bucket(bucket_scale(7)), 7);
        assert_eq!(zoom_bucket(100.0), zoom_bucket(101.0));
        assert_ne!(zoom_bucket(100.0), zoom_bucket(150.0));
    }

    fn sq(x: f64, y: f64, w: f64, h: f64) -> Vec<V2> {
        vec![v2(x, y), v2(x + w, y), v2(x + w, y + h), v2(x, y + h)]
    }

    #[test]
    fn inscribed_box_of_rectangle_and_rotated_room() {
        let (w, h) = inscribed_box(v2(5.0, 2.0), &[sq(0.0, 0.0, 10.0, 4.0)]);
        assert!((w - 10.0).abs() < 0.05 && (h - 4.0).abs() < 0.05, "{w} {h}");
        // diamond |x|+|y| <= 5: box corners must stay inside the diamond
        let d = vec![v2(5.0, 0.0), v2(0.0, 5.0), v2(-5.0, 0.0), v2(0.0, -5.0)];
        let (w, h) = inscribed_box(v2(0.0, 0.0), &[d]);
        assert!(w > 0.0 && w / 2.0 + h / 2.0 <= 5.0 + 1e-9, "{w} {h}");
        // a hole at the centre leaves no box
        let hole = sq(-1.0, -1.0, 2.0, 2.0);
        assert_eq!(inscribed_box(v2(0.0, 0.0), &[sq(-5.0, -5.0, 10.0, 10.0), hole]), (0.0, 0.0));
    }

    fn item(id: i64, x: f64, y: f64, w: f64, h: f64) -> RoomItem {
        RoomItem {
            id,
            name: format!("Room {id}"),
            number: id.to_string(),
            rings: vec![sq(x, y, w, h)],
        }
    }

    fn row(n: usize) -> (Vec<RoomItem>, Vec<(V2, f64)>) {
        // ids of one digit count: a longer tag text costs more measures, not more rooms
        let rooms: Vec<RoomItem> =
            (0..n).map(|i| item(i as i64 + 10_000, i as f64 * 6.0, 0.0, 5.0, 4.0)).collect();
        let cs = (0..n).map(|i| (v2(i as f64 * 6.0 + 2.5, 2.0), 20.0)).collect();
        (rooms, cs)
    }

    #[test]
    fn layout_is_linear_in_room_count() {
        let count = |n: usize| {
            let (rooms, cs) = row(n);
            let mut calls = 0usize;
            let mut meas = |s: &str, f: Face| {
                calls += 1;
                mw(s, f)
            };
            let out = build_layout(&cs, &rooms, &|a| format!("{a:.0} m2"), 16, &M, &mut meas);
            assert_eq!(out.len(), n);
            let tests = RoomIndex::new(&rooms);
            for (c, _) in &cs {
                tests.pick(*c);
            }
            (calls, tests.tests.get())
        };
        let (c1, t1) = count(200);
        let (c2, t2) = count(2000);
        assert!(c2 <= c1 * 10 + 10, "{c1} {c2}");
        assert!(t2 <= t1 * 10 + 10, "{t1} {t2}");
        // polygon tests per query stay a small constant, not O(rooms)
        assert!(t2 <= 2000 * 4, "{t2}");
    }

    #[test]
    fn index_picks_lowest_room_and_misses_outside() {
        let rooms = vec![item(7, 0.0, 0.0, 5.0, 4.0), item(9, 100.0, 0.0, 5.0, 4.0)];
        let idx = RoomIndex::new(&rooms);
        assert_eq!(idx.pick(v2(2.0, 2.0)), Some(7));
        assert_eq!(idx.pick(v2(102.0, 2.0)), Some(9));
        assert_eq!(idx.pick(v2(50.0, 2.0)), None);
    }

    #[test]
    fn cache_hits_and_misses_on_revision_and_zoom_bucket() {
        let mut c = LayoutCache::default();
        let key = |rev: u64, b: i32| LayoutKey { model: (1, 0, rev, 0), bucket: b, fmt: "m2".into() };
        let mut built = 0;
        for (rev, b) in [(1, 5), (1, 5), (1, 5), (2, 5), (2, 5), (2, 6), (2, 6), (1, 5)] {
            c.get(key(rev, b), || {
                built += 1;
                vec![]
            });
        }
        // misses: first, rev change, bucket change, back to the old key
        assert_eq!((built, c.builds), (4, 4));
    }

    #[test]
    fn cache_misses_on_area_format_change() {
        let mut c = LayoutCache::default();
        let k = |f: &str| LayoutKey { model: (1, 0, 1, 0), bucket: 0, fmt: f.into() };
        c.get(k("a"), Vec::new);
        c.get(k("b"), Vec::new);
        c.get(k("b"), Vec::new);
        assert_eq!(c.builds, 2);
    }

    #[test]
    fn cyrillic_name_elides_without_breaking_chars() {
        let name = "\u{41a}\u{43e}\u{43d}\u{444}\u{435}\u{440}\u{435}\u{43d}\u{446}-\u{437}\u{430}\u{43b}";
        let f = fit("", name, "", 60.0, 30.0);
        let t = f.title.unwrap();
        assert!(t.ends_with('\u{2026}') && mw(&t, Face::Title) <= 54.0, "{t}");
    }
}
