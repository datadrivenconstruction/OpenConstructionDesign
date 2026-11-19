// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Curtain wall editing on top of the `curtain` grid: add/remove a grid-line segment,
//! pin a line (re-spacing does not move it), type of an individual panel (solid, glass,
//! empty, door, window), panel merging (via removed segments), mullion type on each
//! segment, corner mullions, tilt (wall inclined from vertical) and an arc axis.
//! Areas are exact: cell = chord x height between mullion axes, tilt divides by cos.

use std::collections::{BTreeMap, BTreeSet};

use crate::curtain::{layout_grid, Axis, Grid, Layout, Section};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PanelType {
    #[default]
    Glazed,
    Solid,
    Empty,
    Door,
    Window,
}

impl PanelType {
    pub fn as_str(self) -> &'static str {
        match self {
            PanelType::Glazed => "glazed",
            PanelType::Solid => "solid",
            PanelType::Empty => "empty",
            PanelType::Door => "door",
            PanelType::Window => "window",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "glazed" => PanelType::Glazed,
            "solid" => PanelType::Solid,
            "empty" => PanelType::Empty,
            "door" => PanelType::Door,
            "window" => PanelType::Window,
            _ => return None,
        })
    }
}

/// Grid-line segment: vertical line `line` in row `seg`, or horizontal in a column.
pub type Seg = (bool, usize, usize);

#[derive(Clone, Debug, PartialEq)]
pub struct CurtainEdit {
    pub u: Vec<f64>,
    pub v: Vec<f64>,
    pub pin_u: Vec<bool>,
    pub pin_v: Vec<bool>,
    pub removed: BTreeSet<Seg>,
    /// Panel type by its first (bottom-left) cell.
    pub panel: BTreeMap<(usize, usize), PanelType>,
    pub mullion: BTreeMap<Seg, Section>,
    pub interior: Section,
    pub border: Section,
    /// Corner mullions at the start and end of the axis (junction with a neighboring curtain wall).
    pub corner: [Option<Section>; 2],
    /// Plane tilt from vertical, rad.
    pub tilt: f64,
}

/// Panel after merging: cells (i, j), type, area.
#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    pub cells: Vec<(usize, usize)>,
    pub kind: PanelType,
    pub area: f64,
}

impl CurtainEdit {
    pub fn new(u: Vec<f64>, v: Vec<f64>, interior: Section, border: Section) -> Self {
        let (nu, nv) = (u.len(), v.len());
        let pins = |n: usize| (0..n).map(|k| k == 0 || k + 1 == n).collect();
        CurtainEdit {
            pin_u: pins(nu),
            pin_v: pins(nv),
            u,
            v,
            removed: BTreeSet::new(),
            panel: BTreeMap::new(),
            mullion: BTreeMap::new(),
            interior,
            border,
            corner: [None, None],
            tilt: 0.0,
        }
    }
    fn cols(&self) -> usize {
        self.u.len() - 1
    }
    fn rows(&self) -> usize {
        self.v.len() - 1
    }
    fn lines(&self, vertical: bool) -> &Vec<f64> {
        if vertical {
            &self.u
        } else {
            &self.v
        }
    }
    fn segs_of(&self, vertical: bool) -> usize {
        if vertical {
            self.rows()
        } else {
            self.cols()
        }
    }
    fn is_border(&self, s: Seg) -> bool {
        s.1 == 0 || s.1 + 1 == self.lines(s.0).len()
    }
    fn check(&self, s: Seg) -> Result<(), String> {
        if s.1 >= self.lines(s.0).len() || s.2 >= self.segs_of(s.0) {
            return Err(format!("no segment {s:?}"));
        }
        Ok(())
    }

    /// New line over the full length; segment indices after it shift. Returns its index.
    pub fn add_line(&mut self, vertical: bool, at: f64) -> Result<usize, String> {
        let l = self.lines(vertical);
        let k = l.partition_point(|&x| x < at);
        if k == 0 || k == l.len() || (l[k] - at).abs() < 1e-9 || (l[k - 1] - at).abs() < 1e-9 {
            return Err(format!("grid line at {at} is outside or duplicates a line"));
        }
        let shift_line = |s: Seg| {
            if s.0 == vertical && s.1 >= k {
                (s.0, s.1 + 1, s.2)
            } else {
                s
            }
        };
        // a perpendicular segment cut by the new line is split in two
        let split = |s: Seg| -> Vec<Seg> {
            if s.0 != vertical && s.2 == k - 1 {
                vec![(s.0, s.1, k - 1), (s.0, s.1, k)]
            } else if s.0 != vertical && s.2 >= k {
                vec![(s.0, s.1, s.2 + 1)]
            } else {
                vec![shift_line(s)]
            }
        };
        self.removed = self.removed.iter().flat_map(|&s| split(s)).collect();
        self.mullion = self
            .mullion
            .iter()
            .flat_map(|(&s, &m)| split(s).into_iter().map(move |x| (x, m)))
            .collect();
        self.panel = self
            .panel
            .iter()
            .map(|(&(i, j), &t)| {
                let c = if vertical {
                    (i + (i >= k) as usize, j)
                } else {
                    (i, j + (j >= k) as usize)
                };
                (c, t)
            })
            .collect();
        // both halves of the split panel keep its type
        let copies: Vec<((usize, usize), PanelType)> = self
            .panel
            .iter()
            .filter(|(&(i, j), _)| if vertical { i == k - 1 } else { j == k - 1 })
            .map(|(&(i, j), &t)| (if vertical { (k, j) } else { (i, k) }, t))
            .collect();
        for (c, t) in copies {
            self.panel.entry(c).or_insert(t);
        }
        if vertical {
            self.u.insert(k, at);
            self.pin_u.insert(k, false);
        } else {
            self.v.insert(k, at);
            self.pin_v.insert(k, false);
        }
        Ok(k)
    }

    /// Remove an interior line entirely: cells on both sides merge, the type comes from the left/lower one.
    pub fn remove_line(&mut self, vertical: bool, k: usize) -> Result<(), String> {
        let n = self.lines(vertical).len();
        if k == 0 || k + 1 >= n {
            return Err(format!("line {k} is a border or missing"));
        }
        let map = |s: Seg| -> Option<Seg> {
            if s.0 == vertical {
                match s.1.cmp(&k) {
                    std::cmp::Ordering::Equal => None,
                    std::cmp::Ordering::Greater => Some((s.0, s.1 - 1, s.2)),
                    std::cmp::Ordering::Less => Some(s),
                }
            } else {
                match s.2.cmp(&k) {
                    std::cmp::Ordering::Equal => None,
                    std::cmp::Ordering::Greater => Some((s.0, s.1, s.2 - 1)),
                    std::cmp::Ordering::Less => Some(s),
                }
            }
        };
        self.removed = self.removed.iter().filter_map(|&s| map(s)).collect();
        self.mullion = self
            .mullion
            .iter()
            .filter_map(|(&s, &m)| map(s).map(|x| (x, m)))
            .collect();
        self.panel = self
            .panel
            .iter()
            .filter_map(|(&(i, j), &t)| {
                let c = if vertical { i } else { j };
                let c2 = match c.cmp(&k) {
                    std::cmp::Ordering::Equal => return None,
                    std::cmp::Ordering::Greater => c - 1,
                    std::cmp::Ordering::Less => c,
                };
                Some((if vertical { (c2, j) } else { (i, c2) }, t))
            })
            .collect();
        if vertical {
            self.u.remove(k);
            self.pin_u.remove(k);
        } else {
            self.v.remove(k);
            self.pin_v.remove(k);
        }
        Ok(())
    }

    /// Move an interior line (strictly between its neighbors); the line becomes pinned.
    pub fn move_line(&mut self, vertical: bool, k: usize, at: f64) -> Result<(), String> {
        let l = self.lines(vertical);
        if k == 0 || k + 1 >= l.len() || !(l[k - 1] + 1e-6 < at && at < l[k + 1] - 1e-6) {
            return Err(format!("line {k} cannot move to {at}"));
        }
        if vertical {
            self.u[k] = at;
            self.pin_u[k] = true;
        } else {
            self.v[k] = at;
            self.pin_v[k] = true;
        }
        Ok(())
    }

    /// Tilt from vertical, rad: |tilt| < 60°.
    pub fn set_tilt(&mut self, tilt: f64) -> Result<(), String> {
        if !(tilt.is_finite() && tilt.abs() < 60f64.to_radians()) {
            return Err("tilt must be within ±60°".into());
        }
        self.tilt = tilt;
        Ok(())
    }

    /// Edits on a wall of a new size: pinned lines stay in place (removed if outside the wall),
    /// the rest are re-spaced evenly between pinned ones.
    pub fn fit(&self, len: f64, height: f64) -> CurtainEdit {
        let mut e = self.clone();
        for vertical in [true, false] {
            let target = if vertical { len } else { height };
            let cur = *e.lines(vertical).last().unwrap_or(&target);
            if (cur - target).abs() < 1e-9 || target <= 0.0 {
                continue;
            }
            loop {
                let l = e.lines(vertical);
                let pins = if vertical { &e.pin_u } else { &e.pin_v };
                let n = l.len();
                let off = (1..n.saturating_sub(1)).find(|&k| pins[k] && l[k] >= target - 1e-6);
                match off {
                    Some(k) => {
                        let _ = e.remove_line(vertical, k);
                    }
                    None => break,
                }
            }
            let scale = target / cur;
            let (l, pins) = if vertical {
                (&mut e.u, &e.pin_u)
            } else {
                (&mut e.v, &e.pin_v)
            };
            let n = l.len();
            for k in 1..n {
                if k + 1 == n {
                    l[k] = target;
                } else if !pins[k] {
                    l[k] *= scale;
                }
            }
            e.even_out(vertical);
        }
        e
    }

    /// Panel root for each cell (one merge, one pass).
    fn roots(&self) -> BTreeMap<(usize, usize), (usize, usize)> {
        let mut m = BTreeMap::new();
        for g in self.panels_raw() {
            for &c in &g {
                m.insert(c, g[0]);
            }
        }
        m
    }

    /// Remove a segment of an interior line (adjacent cells merge into a panel).
    pub fn remove_segment(&mut self, s: Seg) -> Result<(), String> {
        self.check(s)?;
        if self.is_border(s) {
            return Err("border segments cannot be removed".into());
        }
        self.removed.insert(s);
        Ok(())
    }

    /// Restore a segment. A panel it cuts is split; both parts keep the type.
    pub fn add_segment(&mut self, s: Seg) -> Result<(), String> {
        self.check(s)?;
        let before = self.panels_raw();
        if !self.removed.remove(&s) {
            return Err(format!("segment {s:?} is present"));
        }
        for p in self.panels_raw() {
            let old = before.iter().find(|b| b.contains(&p[0])).map(|b| b[0]);
            if let Some(t) = old.and_then(|o| self.panel.get(&o).copied()) {
                self.panel.insert(p[0], t);
            }
        }
        Ok(())
    }

    pub fn pin(&mut self, vertical: bool, k: usize, on: bool) -> Result<(), String> {
        let n = self.lines(vertical).len();
        if k == 0 || k + 1 >= n {
            return if k < n {
                Ok(())
            } else {
                Err(format!("no line {k}"))
            };
        }
        if vertical {
            self.pin_u[k] = on;
        } else {
            self.pin_v[k] = on;
        }
        Ok(())
    }

    /// Even spacing: unpinned lines are distributed equally between pinned ones.
    pub fn even_out(&mut self, vertical: bool) {
        let (l, p) = if vertical {
            (&mut self.u, &self.pin_u)
        } else {
            (&mut self.v, &self.pin_v)
        };
        let fixed: Vec<usize> = (0..l.len()).filter(|&k| p[k]).collect();
        for w in fixed.windows(2) {
            let (a, b) = (w[0], w[1]);
            let n = (b - a) as f64;
            for k in a + 1..b {
                l[k] = l[a] + (l[b] - l[a]) * (k - a) as f64 / n;
            }
        }
    }

    /// Merge the panels of two adjacent cells: removes their shared segment.
    pub fn merge(&mut self, a: (usize, usize), b: (usize, usize)) -> Result<(), String> {
        let s = match (b.0 as i64 - a.0 as i64, b.1 as i64 - a.1 as i64) {
            (1, 0) => (true, b.0, a.1),
            (-1, 0) => (true, a.0, a.1),
            (0, 1) => (false, b.1, a.0),
            (0, -1) => (false, a.1, a.0),
            _ => return Err("cells are not adjacent".into()),
        };
        let keep = self.kind_of(a);
        self.remove_segment(s)?;
        let g = self
            .panels_raw()
            .into_iter()
            .find(|g| g.contains(&a))
            .unwrap_or_default();
        self.panel.retain(|c, _| !g.contains(c));
        self.panel.insert(g[0], keep);
        Ok(())
    }

    /// Cells grouped by panels; the first is the minimal (i, j).
    fn panels_raw(&self) -> Vec<Vec<(usize, usize)>> {
        let (nu, nv) = (self.cols(), self.rows());
        let mut parent: Vec<usize> = (0..nu * nv).collect();
        fn find(p: &mut [usize], x: usize) -> usize {
            let mut r = x;
            while p[r] != r {
                r = p[r];
            }
            p[x] = r;
            r
        }
        let id = |i: usize, j: usize| i * nv + j;
        for &(vert, k, s) in &self.removed {
            let (x, y) = if vert {
                (id(k - 1, s), id(k, s))
            } else {
                (id(s, k - 1), id(s, k))
            };
            let (rx, ry) = (find(&mut parent, x), find(&mut parent, y));
            parent[rx.max(ry)] = rx.min(ry);
        }
        let mut groups: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
        for i in 0..nu {
            for j in 0..nv {
                let r = find(&mut parent, id(i, j));
                groups.entry(r).or_default().push((i, j));
            }
        }
        groups.into_values().collect()
    }

    fn root(&self, c: (usize, usize)) -> (usize, usize) {
        self.panels_raw()
            .into_iter()
            .find(|g| g.contains(&c))
            .map_or(c, |g| g[0])
    }

    pub fn kind_of(&self, c: (usize, usize)) -> PanelType {
        self.panel.get(&self.root(c)).copied().unwrap_or_default()
    }

    /// Type of the panel containing the cell. A door is allowed only on a panel touching the bottom.
    pub fn set_panel(&mut self, c: (usize, usize), t: PanelType) -> Result<(), String> {
        let g = self
            .panels_raw()
            .into_iter()
            .find(|g| g.contains(&c))
            .ok_or_else(|| format!("no cell {c:?}"))?;
        if t == PanelType::Door && !g.iter().any(|x| x.1 == 0) {
            return Err("door panel must reach the bottom of the wall".into());
        }
        self.panel.insert(g[0], t);
        Ok(())
    }

    pub fn set_mullion(&mut self, s: Seg, m: Section) -> Result<(), String> {
        self.check(s)?;
        self.mullion.insert(s, m);
        Ok(())
    }

    pub fn section(&self, s: Seg) -> Section {
        self.section_in(s, &self.roots())
    }

    fn section_in(&self, s: Seg, roots: &BTreeMap<(usize, usize), (usize, usize)>) -> Section {
        if self.removed.contains(&s) {
            return Section::None;
        }
        // no bottom mullion under a door
        let kind = |c: (usize, usize)| {
            let r = roots.get(&c).copied().unwrap_or(c);
            self.panel.get(&r).copied().unwrap_or_default()
        };
        if !s.0 && s.1 == 0 && kind((s.2, 0)) == PanelType::Door {
            return Section::None;
        }
        self.mullion
            .get(&s)
            .copied()
            .unwrap_or(if self.is_border(s) {
                self.border
            } else {
                self.interior
            })
    }

    /// Grid for the layout `curtain::layout_grid`: each segment with its own section.
    pub fn grid(&self) -> Grid {
        let roots = self.roots();
        let mut segs = vec![];
        for vert in [true, false] {
            for k in 0..self.lines(vert).len() {
                for s in 0..self.segs_of(vert) {
                    segs.push((vert, k, s, self.section_in((vert, k, s), &roots)));
                }
            }
        }
        Grid {
            su: vec![self.interior; self.u.len()],
            sv: vec![self.interior; self.v.len()],
            segs,
            top: vec![],
        }
    }

    pub fn layout(&self, axis: &Axis, z0: f64) -> Layout {
        layout_grid(axis, z0, &self.u, &self.v, &self.grid())
    }

    /// Panel type of each cell (cells of a merged panel take the type of its root).
    pub fn cell_kinds(&self) -> BTreeMap<(usize, usize), PanelType> {
        self.roots()
            .into_iter()
            .map(|(c, r)| (c, self.panel.get(&r).copied().unwrap_or_default()))
            .collect()
    }

    /// Panels with area (between mullion axes) on axis `axis`; tilt increases the height by 1/cos.
    pub fn panels(&self, axis: &Axis) -> Vec<Panel> {
        let l = self.layout(axis, 0.0);
        let k = 1.0 / self.tilt.cos();
        let nv = self.rows();
        self.panels_raw()
            .into_iter()
            .map(|cells| Panel {
                area: cells
                    .iter()
                    .map(|&(i, j)| l.cells[i * nv + j].area)
                    .sum::<f64>()
                    * k,
                kind: self.panel.get(&cells[0]).copied().unwrap_or_default(),
                cells,
            })
            .collect()
    }

    /// Mullion count: segments with a section plus corner ones.
    pub fn mullion_count(&self) -> usize {
        let segs = [true, false]
            .iter()
            .flat_map(|&vert| {
                (0..self.lines(vert).len())
                    .flat_map(move |k| (0..self.segs_of(vert)).map(move |s| (vert, k, s)))
            })
            .filter(|&s| self.section(s) != Section::None)
            .count();
        segs + self
            .corner
            .iter()
            .flatten()
            .filter(|c| **c != Section::None)
            .count()
    }

    /// Total mullion length along axes (verticals account for tilt).
    pub fn mullion_length(&self, axis: &Axis) -> f64 {
        let k = 1.0 / self.tilt.cos();
        let mut sum = 0.0;
        for vert in [true, false] {
            for line in 0..self.lines(vert).len() {
                for s in 0..self.segs_of(vert) {
                    if self.section((vert, line, s)) == Section::None {
                        continue;
                    }
                    sum += if vert {
                        (self.v[s + 1] - self.v[s]) * k
                    } else {
                        (axis.point_at(self.u[s + 1]) - axis.point_at(self.u[s])).len()
                    };
                }
            }
        }
        let h = (self.v[self.rows()] - self.v[0]) * k;
        sum + self
            .corner
            .iter()
            .flatten()
            .filter(|c| **c != Section::None)
            .count() as f64
            * h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::v2;

    const E: f64 = 1e-9;
    const M: Section = Section::Rect { w: 0.05, d: 0.15 };

    fn wall() -> (Axis, CurtainEdit) {
        let axis = Axis::new(v2(0.0, 0.0), v2(6.0, 0.0), 0.0).unwrap();
        let e = CurtainEdit::new(vec![0.0, 2.0, 4.0, 6.0], vec![0.0, 1.5, 3.0], M, M);
        (axis, e)
    }

    #[test]
    fn mullion_count_exact() {
        let (_, mut e) = wall();
        // 4 vertical x 2 rows + 3 horizontal x 3 columns
        assert_eq!(e.mullion_count(), 8 + 9);
        e.remove_segment((true, 1, 0)).unwrap();
        e.remove_segment((false, 1, 2)).unwrap();
        assert_eq!(e.mullion_count(), 15);
        e.set_mullion((true, 2, 1), Section::None).unwrap();
        e.corner = [Some(M), Some(Section::Circle { d: 0.1 })];
        assert_eq!(e.mullion_count(), 16);
        assert!(e.remove_segment((true, 0, 0)).is_err());
        e.add_segment((true, 1, 0)).unwrap();
        assert_eq!(e.mullion_count(), 17);
    }

    #[test]
    fn merged_panel_area_exact() {
        let (axis, mut e) = wall();
        e.set_panel((1, 1), PanelType::Solid).unwrap();
        e.merge((1, 1), (2, 1)).unwrap();
        e.merge((1, 1), (1, 0)).unwrap();
        let ps = e.panels(&axis);
        assert_eq!(ps.len(), 6 - 2);
        let big = ps.iter().find(|p| p.cells.len() == 3).unwrap();
        assert!((big.area - 3.0 * 2.0 * 1.5).abs() < E);
        assert_eq!(big.kind, PanelType::Solid);
        assert!((ps.iter().map(|p| p.area).sum::<f64>() - 18.0).abs() < E);
        assert!(e.merge((0, 0), (2, 0)).is_err());
    }

    #[test]
    fn door_window_and_empty_panels() {
        let (_, mut e) = wall();
        assert!(e.set_panel((0, 1), PanelType::Door).is_err());
        e.merge((0, 0), (0, 1)).unwrap();
        e.set_panel((0, 1), PanelType::Door).unwrap();
        assert_eq!(e.kind_of((0, 0)), PanelType::Door);
        assert_eq!(e.section((false, 0, 0)), Section::None);
        e.set_panel((2, 1), PanelType::Window).unwrap();
        e.set_panel((1, 0), PanelType::Empty).unwrap();
        assert_eq!(e.kind_of((2, 1)), PanelType::Window);
        // 17 minus the middle segment above the door minus the bottom under the door
        assert_eq!(e.mullion_count(), 15);
    }

    #[test]
    fn add_line_pin_even_out() {
        let (_, mut e) = wall();
        e.remove_segment((false, 1, 2)).unwrap();
        let k = e.add_line(true, 5.0).unwrap();
        assert_eq!(k, 3);
        assert_eq!(e.u, vec![0.0, 2.0, 4.0, 5.0, 6.0]);
        // the removed segment of column 2 is split in two by the new line
        assert!(e.removed.contains(&(false, 1, 2)) && e.removed.contains(&(false, 1, 3)));
        e.pin(true, 1, true).unwrap();
        e.even_out(true);
        let want = [0.0, 2.0, 2.0 + 4.0 / 3.0, 2.0 + 8.0 / 3.0, 6.0];
        assert!(
            e.u.iter().zip(want).all(|(a, b)| (a - b).abs() < E),
            "{:?}",
            e.u
        );
        assert!(e.add_line(true, 2.0).is_err());
    }

    #[test]
    fn sloped_and_curved() {
        let (axis, mut e) = wall();
        e.tilt = 30f64.to_radians();
        let a: f64 = e.panels(&axis).iter().map(|p| p.area).sum();
        assert!((a - 18.0 / 30f64.to_radians().cos()).abs() < E);
        // arc: panels are chords; total area = sum of chords x height
        let arc = Axis::new(v2(0.0, 0.0), v2(6.0, 0.0), 0.3).unwrap();
        let mut c = CurtainEdit::new(
            vec![0.0, arc.length() / 2.0, arc.length()],
            vec![0.0, 3.0],
            M,
            M,
        );
        c.merge((0, 0), (1, 0)).unwrap();
        let p = c.panels(&arc);
        let chord = |s0: f64, s1: f64| (arc.point_at(s1) - arc.point_at(s0)).len();
        let want = (chord(0.0, c.u[1]) + chord(c.u[1], c.u[2])) * 3.0;
        assert_eq!(p.len(), 1);
        assert!((p[0].area - want).abs() < E);
        assert!(c.mullion_length(&arc) > 0.0);
    }

    #[test]
    fn lines_keep_types_and_fit_resize() {
        let (_, mut e) = wall();
        e.set_panel((1, 0), PanelType::Solid).unwrap();
        let mid = (e.u[1] + e.u[2]) / 2.0;
        let k = e.add_line(true, mid).unwrap();
        assert_eq!(
            (e.kind_of((1, 0)), e.kind_of((k, 0))),
            (PanelType::Solid, PanelType::Solid)
        );
        e.remove_line(true, k).unwrap();
        assert_eq!(e.kind_of((1, 0)), PanelType::Solid);
        let n = e.u.len();
        e.move_line(true, 1, e.u[1] + 0.1).unwrap();
        assert!(e.pin_u[1]);
        let len = *e.u.last().unwrap();
        let f = e.fit(len * 2.0, *e.v.last().unwrap());
        assert_eq!(f.u.len(), n);
        assert!((f.u[1] - e.u[1]).abs() < 1e-12, "pinned line stays");
        assert!((f.u[n - 1] - len * 2.0).abs() < 1e-12);
        let steps: Vec<f64> = f.u[1..].windows(2).map(|w| w[1] - w[0]).collect();
        assert!(
            steps.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-9),
            "even after resize"
        );
        assert!(e.set_tilt(1.2).is_err() && e.set_tilt(0.2).is_ok());
    }
}
