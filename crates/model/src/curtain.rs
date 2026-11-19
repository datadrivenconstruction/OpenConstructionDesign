//! Curtain walls: axis (segment/arc), bottom and height, U grid (along the axis) and V grid (along the height) by pitch
//! or explicit lines, mullions (interior and border), a panel per cell (`curtain_panel` stores
//! only deviations from glazed). Quantities are computed exactly from the `ddc_geom::curtain` layout.

use ddc_geom::curtain::{self, Axis, Layout, Section};
use ddc_geom::v2;
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

/// Grid lines: pitch from the start (the last cell is shorter) or interior positions.
#[derive(Clone, Debug, PartialEq)]
pub enum GridLines {
    Spacing(f64),
    At(Vec<f64>),
}

impl GridLines {
    /// Nodes from 0 to `len` inclusive.
    pub fn nodes(&self, len: f64) -> Vec<f64> {
        match self {
            GridLines::Spacing(s) => curtain::spacing_lines(len, *s),
            GridLines::At(v) => curtain::explicit_lines(len, v),
        }
    }
    /// Column text: `s 1.5` or `x 1.5 3` (empty: no lines).
    pub fn text(&self) -> String {
        match self {
            GridLines::Spacing(s) => format!("s {s}"),
            GridLines::At(v) => {
                let mut t = String::from("x");
                for x in v {
                    t.push_str(&format!(" {x}"));
                }
                t
            }
        }
    }
    pub fn parse(t: &str) -> Result<Self, Error> {
        let bad = || Error::Invalid(format!("bad grid '{t}'"));
        let mut it = t.split_whitespace();
        let nums = |it: std::str::SplitWhitespace| -> Result<Vec<f64>, Error> {
            it.map(|x| {
                x.parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .ok_or_else(bad)
            })
            .collect()
        };
        match it.next() {
            None => Ok(GridLines::At(vec![])),
            Some("s") => match nums(it)?.as_slice() {
                [s] if *s > 1e-3 => Ok(GridLines::Spacing(*s)),
                _ => Err(bad()),
            },
            Some("x") => Ok(GridLines::At(nums(it)?)),
            _ => Err(bad()),
        }
    }
}

/// Mullion profile text: empty, `r width depth`, `c diameter`.
pub fn section_text(s: &Section) -> String {
    match *s {
        Section::None => String::new(),
        Section::Rect { w, d } => format!("r {w} {d}"),
        Section::Circle { d } => format!("c {d}"),
    }
}

pub fn parse_section(t: &str) -> Result<Section, Error> {
    let bad = || Error::Invalid(format!("bad mullion '{t}'"));
    let v: Vec<&str> = t.split_whitespace().collect();
    let num = |s: &str| {
        s.parse::<f64>()
            .ok()
            .filter(|x| x.is_finite() && *x > 0.0)
            .ok_or_else(bad)
    };
    match v.as_slice() {
        [] => Ok(Section::None),
        ["r", w, d] => Ok(Section::Rect {
            w: num(w)?,
            d: num(d)?,
        }),
        ["c", d] => Ok(Section::Circle { d: num(d)? }),
        _ => Err(bad()),
    }
}

/// Profiles of individual grid lines on top of `interior`/`border`: (node, segment, profile); node 0 is
/// the start (bottom), the last is the end (top); segment `None` is the whole line, `Some(k)` is row `k`
/// of a vertical line or column `k` of a horizontal one. `mullion_lines` column text:
/// `u 2 r 0.05 0.1;v 1:0` (a segment without a profile has no mullion).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct LineSections {
    pub u: Vec<(usize, Option<usize>, Section)>,
    pub v: Vec<(usize, Option<usize>, Section)>,
}

impl LineSections {
    pub fn is_empty(&self) -> bool {
        self.u.is_empty() && self.v.is_empty()
    }
    pub fn text(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut parts = vec![];
        for (d, v) in [("u", &self.u), ("v", &self.v)] {
            for (k, seg, sec) in v {
                let at = seg.map_or(k.to_string(), |g| format!("{k}:{g}"));
                parts.push(
                    format!("{d} {at} {}", section_text(sec))
                        .trim_end()
                        .to_string(),
                );
            }
        }
        Some(parts.join(";"))
    }
    pub fn parse(t: &str) -> Result<Self, Error> {
        let mut out = Self::default();
        for part in t.split(';').filter(|p| !p.trim().is_empty()) {
            let bad = || Error::Invalid(format!("bad mullion line '{part}'"));
            let mut it = part.trim().splitn(3, ' ');
            let (d, at) = (it.next().ok_or_else(bad)?, it.next().ok_or_else(bad)?);
            let (k, seg) = match at.split_once(':') {
                Some((k, g)) => (k, Some(g.parse::<usize>().map_err(|_| bad())?)),
                None => (at, None),
            };
            let k: usize = k.parse().map_err(|_| bad())?;
            let sec = parse_section(it.next().unwrap_or(""))?;
            match d {
                "u" => out.u.push((k, seg, sec)),
                "v" => out.v.push((k, seg, sec)),
                _ => return Err(bad()),
            }
        }
        Ok(out)
    }
}

/// Grid profiles for the layout: default `interior`/`border` nodes, then per-line overrides,
/// then per-segment overrides; the top is `top`.
pub fn grid(
    nu: usize,
    nv: usize,
    interior: Section,
    border: Section,
    lines: &LineSections,
    top: &[(f64, f64)],
) -> curtain::Grid {
    let nodes = |n: usize, over: &[(usize, Option<usize>, Section)]| {
        let mut v = curtain::line_sections(n, interior, border);
        for &(k, seg, s) in over {
            if let (Some(x), None) = (v.get_mut(k), seg) {
                *x = s;
            }
        }
        v
    };
    let mut segs = vec![];
    for (vertical, over) in [(true, &lines.u), (false, &lines.v)] {
        segs.extend(
            over.iter()
                .filter_map(|&(k, seg, s)| seg.map(|g| (vertical, k, g, s))),
        );
    }
    curtain::Grid {
        su: nodes(nu, &lines.u),
        sv: nodes(nv, &lines.v),
        segs,
        top: top.to_vec(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PanelKind {
    #[default]
    Glazed,
    Solid,
    Door,
    Window,
    Empty,
}

impl PanelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PanelKind::Glazed => "glazed",
            PanelKind::Solid => "solid",
            PanelKind::Door => "door",
            PanelKind::Window => "window",
            PanelKind::Empty => "empty",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "glazed" => PanelKind::Glazed,
            "solid" => PanelKind::Solid,
            "door" => PanelKind::Door,
            "window" => PanelKind::Window,
            "empty" => PanelKind::Empty,
            _ => return None,
        })
    }
}

/// Cell panel; `name` is the panel type. Door: opens to the right of the axis / hinges at the cell end.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PanelSpec {
    pub kind: PanelKind,
    pub name: String,
    pub swing_side: bool,
    pub swing_hinge: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurtainSpec {
    pub level: i64,
    pub name: String,
    pub a: (f64, f64),
    pub b: (f64, f64),
    /// tan(angle/4) of an arc axis; 0 is a segment.
    pub bulge: f64,
    pub base_offset: f64,
    pub height: f64,
    pub u: GridLines,
    pub v: GridLines,
    pub interior: Section,
    pub border: Section,
    /// Lines and segments with a different profile (schema 10).
    pub lines: LineSections,
    /// Polyline top (s along the axis, height above the bottom) in ascending s; empty is straight (schema 10).
    pub top: Vec<(f64, f64)>,
}

/// Quantities: net areas (panel minus mullions), mullion length along the grid axes.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CurtainQty {
    pub cells: usize,
    /// Non-empty cells.
    pub panels: usize,
    pub glazed: usize,
    pub doors: usize,
    pub windows: usize,
    pub glass_area: f64,
    pub panel_area: f64,
    /// Length × height along the axis (chords for an arc).
    pub gross_area: f64,
    pub mullion_length: f64,
    pub interior_length: f64,
    pub border_length: f64,
}

/// Layout and panel of each cell (in `layout.cells` order).
#[derive(Clone, Debug)]
pub struct CurtainLayout {
    pub spec: CurtainSpec,
    pub layout: Layout,
    pub panels: Vec<PanelSpec>,
    /// Bottom elevation (level + base_offset).
    pub z0: f64,
}

impl CurtainLayout {
    pub fn quantities(&self) -> CurtainQty {
        let mut q = CurtainQty {
            cells: self.layout.cells.len(),
            ..Default::default()
        };
        for (c, p) in self.layout.cells.iter().zip(&self.panels) {
            q.gross_area += c.area;
            if p.kind == PanelKind::Empty || (!self.spec.top.is_empty() && c.net_area <= 1e-12) {
                continue;
            }
            q.panels += 1;
            q.panel_area += c.net_area;
            match p.kind {
                PanelKind::Glazed => {
                    q.glazed += 1;
                    q.glass_area += c.net_area;
                }
                PanelKind::Door => q.doors += 1,
                PanelKind::Window => q.windows += 1,
                _ => {}
            }
        }
        for b in &self.layout.bars {
            if b.section == Section::None {
                continue;
            }
            if b.border {
                q.border_length += b.length();
            } else {
                q.interior_length += b.length();
            }
        }
        q.mullion_length = q.interior_length + q.border_length;
        q
    }

    /// Body: panels (door/window/solid/glass, 25 mm thick) and mullions.
    pub fn mesh(&self) -> ddc_geom::mesh::Mesh {
        let t: Vec<Option<f64>> = self
            .panels
            .iter()
            .map(|p| (p.kind != PanelKind::Empty).then_some(0.025))
            .collect();
        curtain::mesh(&self.layout, &t)
    }
}

pub(crate) fn axis(s: &CurtainSpec) -> Result<Axis, Error> {
    Axis::new(v2(s.a.0, s.a.1), v2(s.b.0, s.b.1), s.bulge).map_err(Error::Geom)
}

/// Spec validation (level, geometry, at most 100 000 cells); the same as for edits.
pub fn check_curtain(db: &Connection, s: &CurtainSpec) -> Result<(), Error> {
    let lv: i64 = db.query_row("SELECT count(*) FROM level WHERE id=?1", [s.level], |r| {
        r.get(0)
    })?;
    if lv == 0 {
        return Err(Error::Invalid(format!(
            "curtain wall: no level {}",
            s.level
        )));
    }
    let fin = [s.a.0, s.a.1, s.b.0, s.b.1, s.bulge, s.base_offset, s.height];
    if fin.iter().any(|x| !x.is_finite()) || s.height <= 1e-6 {
        return Err(Error::Invalid("curtain wall: bad geometry".into()));
    }
    if s.bulge.abs() > 1.0 + 1e-12 {
        return Err(Error::Invalid(
            "curtain wall: arc over a half circle".into(),
        ));
    }
    let ax = axis(s)?;
    let top_ok = s.top.is_empty()
        || (s.top.len() >= 2
            && s.top
                .iter()
                .all(|p| p.0.is_finite() && p.1 > 0.0 && p.1 <= s.height + 1e-6 && p.0 >= -1e-6)
            && s.top.windows(2).all(|w| w[1].0 >= w[0].0)
            && s.top[s.top.len() - 1].0 <= ax.length() + 1e-6);
    if !top_ok {
        return Err(Error::Invalid("curtain wall: bad top profile".into()));
    }
    let cols = s.u.nodes(ax.length()).len() - 1;
    let rows = s.v.nodes(s.height).len() - 1;
    if cols * rows > 100_000 {
        return Err(Error::Invalid(format!("curtain wall: {cols}×{rows} cells")));
    }
    // override indices: lines 0..=n, segments 0..n (rows for u lines, columns for v lines)
    for (name, over, n_line, n_seg) in
        [("u", &s.lines.u, cols, rows), ("v", &s.lines.v, rows, cols)]
    {
        for &(k, seg, _) in over {
            if k > n_line || seg.is_some_and(|g| g >= n_seg) {
                let at = seg.map_or(k.to_string(), |g| format!("{k}:{g}"));
                return Err(Error::Invalid(format!(
                    "curtain wall: mullion override {name} {at} is outside the grid ({cols}×{rows})"
                )));
            }
        }
    }
    Ok(())
}

const LINE_MATCH: f64 = 1e-3;

/// New node index by position (end nodes map to end nodes); within 1 mm, else None.
fn map_node(old: &[f64], new: &[f64], k: usize) -> Option<usize> {
    let x = *old.get(k)?;
    if k == 0 {
        return Some(0);
    }
    if k + 1 == old.len() {
        return Some(new.len() - 1);
    }
    new.iter()
        .enumerate()
        .map(|(i, n)| (i, (n - x).abs()))
        .filter(|(_, d)| *d <= LINE_MATCH)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// The segment between nodes g and g+1 is carried over if both ends land on adjacent nodes of the new grid.
fn map_seg(old: &[f64], new: &[f64], g: usize) -> Option<usize> {
    let (a, b) = (map_node(old, new, g)?, map_node(old, new, g + 1)?);
    (b == a + 1).then_some(a)
}

/// Carries line overrides over to the new grid by position; lost ones become warnings.
fn remap_lines(
    id: i64,
    lines: &LineSections,
    (ou, ov): (&[f64], &[f64]),
    (nu, nv): (&[f64], &[f64]),
) -> (LineSections, Vec<String>) {
    let mut out = LineSections::default();
    let mut warn = vec![];
    for (name, over, (ol, os), (nl, ns), dst) in [
        ("u", &lines.u, (ou, ov), (nu, nv), &mut out.u),
        ("v", &lines.v, (ov, ou), (nv, nu), &mut out.v),
    ] {
        for &(k, seg, sec) in over {
            let m = map_node(ol, nl, k).and_then(|k2| match seg {
                None => Some((k2, None)),
                Some(g) => map_seg(os, ns, g).map(|g2| (k2, Some(g2))),
            });
            match m {
                Some((k2, g2)) => {
                    dst.retain(|e| (e.0, e.1) != (k2, g2));
                    dst.push((k2, g2, sec));
                }
                None => {
                    let at = seg.map_or(k.to_string(), |g| format!("{k}:{g}"));
                    warn.push(format!(
                        "curtain wall {id}: mullion override {name} {at} dropped (no matching grid line)"
                    ));
                }
            }
        }
    }
    (out, warn)
}

/// `id` None means a new rowid.
pub(crate) fn insert(db: &Connection, s: &CurtainSpec, id: Option<i64>) -> Result<i64, Error> {
    db.execute(
        "INSERT INTO curtain_wall(id,level,name,ax,ay,bx,by,bulge,base_offset,height,ugrid,\
         vgrid,mullion_in,mullion_border,mullion_lines,top_profile)          VALUES (?14,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?15,?16)",
        params![
            s.level,
            s.name,
            s.a.0,
            s.a.1,
            s.b.0,
            s.b.1,
            s.bulge,
            s.base_offset,
            s.height,
            s.u.text(),
            s.v.text(),
            section_text(&s.interior),
            section_text(&s.border),
            id,
            s.lines.text(),
            (!s.top.is_empty()).then(|| crate::walls::top_profile_text(&s.top))
        ],
    )?;
    Ok(db.last_insert_rowid())
}

pub(crate) fn read(db: &Connection, id: i64) -> Result<CurtainSpec, Error> {
    let row = db
        .query_row(
            "SELECT level,name,ax,ay,bx,by,bulge,base_offset,height,ugrid,vgrid,mullion_in,\
             mullion_border,mullion_lines,top_profile FROM curtain_wall WHERE id=?1",
            [id],
            |r| {
                Ok((
                    (r.get::<_, i64>(0)?, r.get::<_, String>(1)?),
                    (r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?),
                    (r.get(7)?, r.get(8)?),
                    (
                        r.get::<_, String>(9)?,
                        r.get::<_, String>(10)?,
                        r.get::<_, String>(11)?,
                        r.get::<_, String>(12)?,
                    ),
                    r.get::<_, Option<String>>(13)?,
                    r.get::<_, Option<String>>(14)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no such curtain wall {id}")))?;
    let ((level, name), (ax, ay, bx, by, bulge), (base_offset, height), (u, v, mi, mb), ml, tp) =
        row;
    Ok(CurtainSpec {
        level,
        name,
        a: (ax, ay),
        b: (bx, by),
        bulge,
        base_offset,
        height,
        u: GridLines::parse(&u)?,
        v: GridLines::parse(&v)?,
        interior: parse_section(&mi)?,
        border: parse_section(&mb)?,
        lines: ml
            .as_deref()
            .map_or(Ok(LineSections::default()), LineSections::parse)?,
        top: match tp {
            None => vec![],
            Some(t) => crate::walls::parse_top_profile(&t)
                .ok_or_else(|| Error::Invalid(format!("bad curtain top '{t}'")))?,
        },
    })
}

fn write_panel(db: &Connection, cw: i64, i: u32, j: u32, p: &PanelSpec) -> Result<(), Error> {
    db.execute(
        "DELETE FROM curtain_panel WHERE cw=?1 AND i=?2 AND j=?3",
        params![cw, i, j],
    )?;
    if *p != PanelSpec::default() {
        db.execute(
            "INSERT INTO curtain_panel(cw,i,j,kind,name,swing_side,swing_hinge) \
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                cw,
                i,
                j,
                p.kind.as_str(),
                p.name,
                p.swing_side as i64,
                p.swing_hinge as i64
            ],
        )?;
    }
    Ok(())
}

impl Model {
    pub fn add_curtain_wall(&mut self, s: &CurtainSpec) -> Result<i64, Error> {
        Ok(self.add_curtain_walls(std::slice::from_ref(s))?[0])
    }

    /// Several curtain walls in one operation (one undo).
    pub fn add_curtain_walls(&mut self, ss: &[CurtainSpec]) -> Result<Vec<i64>, Error> {
        if ss.is_empty() {
            return Err(Error::Invalid("no curtain walls".into()));
        }
        for s in ss {
            check_curtain(&self.db, s)?;
        }
        self.op("curtain_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for s in ss {
                let id = insert(db, s, None)?;
                inv = inv
                    .delete("curtain_panel", "cw", id)
                    .delete("curtain_wall", "id", id);
                ids.push(id);
            }
            Ok((ids.clone(), ids[0], inv))
        })
    }

    /// Changes the axis, height, grid or mullions. Panels move to the new cell containing
    /// the old cell's centre (outside the grid they are deleted; on a tie, the nearest centre). Returns
    /// warnings: a door/window in a cell of a different size (> 1 mm).
    ///
    /// If the caller did not change `lines`/`top` relative to the stored ones, they are adapted to
    /// the edit: line and segment overrides are carried to the new grid by position (≤ 1 mm, otherwise
    /// dropped with a warning), and the polyline top is scaled in height proportionally to the
    /// change in `height`.
    pub fn set_curtain_wall(&mut self, id: i64, s: &CurtainSpec) -> Result<Vec<String>, Error> {
        let old = self.curtain_layout(id)?;
        let mut warnings = vec![];
        let mut sn = s.clone();
        if sn.lines == old.spec.lines && !sn.lines.is_empty() {
            let ax = axis(&sn)?;
            let nu = sn.u.nodes(ax.length());
            let nv = sn.v.nodes(sn.height);
            let (l, w) = remap_lines(id, &sn.lines, (&old.layout.u, &old.layout.v), (&nu, &nv));
            sn.lines = l;
            warnings = w;
        }
        if !sn.top.is_empty()
            && sn.top == old.spec.top
            && old.spec.height > 1e-6
            && (sn.height - old.spec.height).abs() > 1e-9
        {
            let k = sn.height / old.spec.height;
            for p in &mut sn.top {
                p.1 *= k;
            }
        }
        let s = &sn;
        check_curtain(&self.db, s)?;
        let (new, z0n) = self.build_layout(s)?;
        let ax = axis(s)?;
        let (un, vn) = (&new.u, &new.v);
        let find = |nodes: &[f64], x: f64| {
            (x >= -1e-9 && x <= nodes[nodes.len() - 1] + 1e-9).then(|| {
                nodes
                    .windows(2)
                    .position(|w| x <= w[1])
                    .unwrap_or(nodes.len() - 2)
            })
        };
        let nv = vn.len() - 1;
        // new cell index → (distance to its centre, panel, old cell size)
        let mut moved: std::collections::BTreeMap<usize, (f64, PanelSpec, (f64, f64))> =
            Default::default();
        for (c, p) in old.layout.cells.iter().zip(&old.panels) {
            if *p == PanelSpec::default() {
                continue;
            }
            let m = (c.corners[0] + c.corners[2]) * 0.5;
            let su = ax.param_of(v2(m.x, m.y));
            let sz = m.z - z0n;
            // a door or window past the new end is kept in the nearest cell; other panels
            // that no longer fit are dropped with a warning
            let fixed = matches!(p.kind, PanelKind::Door | PanelKind::Window);
            let (su0, sz0) = (su, sz);
            let (su, sz) = if fixed {
                (
                    su.clamp(0.0, un[un.len() - 1]),
                    sz.clamp(0.0, vn[vn.len() - 1]),
                )
            } else {
                (su, sz)
            };
            let pulled_in = (su - su0).abs() > 1e-9 || (sz - sz0).abs() > 1e-9;
            let (Some(i), Some(j)) = (find(un, su), find(vn, sz)) else {
                warnings.push(format!(
                    "curtain wall {id}: {} panel in cell {},{} is outside the new wall and was removed",
                    p.kind.as_str(),
                    c.i,
                    c.j
                ));
                continue;
            };
            let k = i * nv + j;
            let nc = &new.cells[k];
            // a door or window pulled in from beyond the new end stays only if it still fits
            if pulled_in && (nc.width + 1e-3 < c.width || nc.height + 1e-3 < c.height) {
                warnings.push(format!(
                    "curtain wall {id}: {} panel in cell {},{} no longer fits at the new end and was removed",
                    p.kind.as_str(),
                    c.i,
                    c.j
                ));
                continue;
            }
            let nm = (nc.corners[0] + nc.corners[2]) * 0.5;
            let d = (nm - m).len();
            match moved.get(&k) {
                Some(e) if d >= e.0 => warnings.push(format!(
                    "curtain wall {id}: {} panel in cell {},{} lost its cell to another panel and was removed",
                    p.kind.as_str(),
                    c.i,
                    c.j
                )),
                prev => {
                    if let Some(e) = prev {
                        warnings.push(format!(
                            "curtain wall {id}: {} panel lost its cell to another panel and was removed",
                            e.1.kind.as_str()
                        ));
                    }
                    moved.insert(k, (d, p.clone(), (c.width, c.height)));
                }
            }
        }
        for (k, (_, p, (w, h))) in &moved {
            let c = &new.cells[*k];
            let fixed = matches!(p.kind, PanelKind::Door | PanelKind::Window);
            if fixed && ((c.width - w).abs() > 1e-3 || (c.height - h).abs() > 1e-3) {
                warnings.push(format!(
                    "curtain wall {id}: {} in cell {},{} is now {:.3}×{:.3} (was {w:.3}×{h:.3})",
                    p.kind.as_str(),
                    c.i,
                    c.j,
                    c.width,
                    c.height
                ));
            }
        }
        self.op("curtain_edit", |db| {
            let inv = Inv::default()
                .delete("curtain_panel", "cw", id)
                .restore(db, "curtain_panel", "cw", id)?
                .delete("curtain_wall", "id", id)
                .restore(db, "curtain_wall", "id", id)?;
            if db.execute("DELETE FROM curtain_wall WHERE id=?1", [id])? == 0 {
                return Err(Error::Invalid(format!("no such curtain wall {id}")));
            }
            db.execute("DELETE FROM curtain_panel WHERE cw=?1", [id])?;
            insert(db, s, Some(id))?;
            for (k, (_, p, _)) in &moved {
                let c = &new.cells[*k];
                write_panel(db, id, c.i as u32, c.j as u32, p)?;
            }
            Ok(((), id, inv))
        })?;
        Ok(warnings)
    }

    pub fn set_curtain_panel(
        &mut self,
        cw: i64,
        i: u32,
        j: u32,
        p: &PanelSpec,
    ) -> Result<(), Error> {
        let l = self.curtain_layout(cw)?;
        if !l
            .layout
            .cells
            .iter()
            .any(|c| c.i == i as usize && c.j == j as usize)
        {
            return Err(Error::Invalid(format!(
                "curtain wall {cw}: no cell {i},{j}"
            )));
        }
        self.op("curtain_panel", |db| {
            let inv = Inv::default().delete("curtain_panel", "cw", cw).restore(
                db,
                "curtain_panel",
                "cw",
                cw,
            )?;
            write_panel(db, cw, i, j, p)?;
            Ok(((), cw, inv))
        })
    }

    /// Sets many panels of one wall in one undo step; every cell must exist.
    pub fn set_curtain_panels(&mut self, cw: i64, edits: &[(u32, u32, PanelSpec)]) -> Result<(), Error> {
        if edits.is_empty() {
            return Ok(());
        }
        let l = self.curtain_layout(cw)?;
        for (i, j, _) in edits {
            if !l.layout.cells.iter().any(|c| c.i == *i as usize && c.j == *j as usize) {
                return Err(Error::Invalid(format!("curtain wall {cw}: no cell {i},{j}")));
            }
        }
        self.op("curtain_panel", |db| {
            let inv = Inv::default().delete("curtain_panel", "cw", cw).restore(db, "curtain_panel", "cw", cw)?;
            for (i, j, p) in edits {
                write_panel(db, cw, *i, *j, p)?;
            }
            Ok(((), cw, inv))
        })
    }

    pub fn delete_curtain_wall(&mut self, id: i64) -> Result<(), Error> {
        // no `ensure` here: a delete creating the edit tables left them behind after its undo
        let mut owned = vec![("curtain_panel", "cw")];
        if crate::rebar::has_table(&self.db, "curtain_edit")? {
            owned.push(("curtain_edit", "cw"));
        }
        self.delete_ref("curtain_wall", "curtain_delete", id, &owned)
    }

    pub fn curtain_walls(&self) -> Result<Vec<i64>, Error> {
        let mut st = self.db.prepare("SELECT id FROM curtain_wall ORDER BY id")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    pub fn curtain_wall(&self, id: i64) -> Result<CurtainSpec, Error> {
        read(&self.db, id)
    }

    /// Layout with the panels of all cells.
    fn build_layout(&self, spec: &CurtainSpec) -> Result<(Layout, f64), Error> {
        build_layout(&self.db, spec)
    }

    pub fn curtain_layout(&self, id: i64) -> Result<CurtainLayout, Error> {
        layout_db(&self.db, id)
    }

    pub fn curtain_quantities(&self, id: i64) -> Result<CurtainQty, Error> {
        Ok(self.curtain_layout(id)?.quantities())
    }
}

/// Grid layout (without panels) and bottom elevation.
fn build_layout(db: &Connection, spec: &CurtainSpec) -> Result<(Layout, f64), Error> {
    let elev: f64 = db.query_row(
        "SELECT elevation FROM level WHERE id=?1",
        [spec.level],
        |r| r.get(0),
    )?;
    let ax = axis(spec)?;
    let u = spec.u.nodes(ax.length());
    let v = spec.v.nodes(spec.height);
    let z0 = elev + spec.base_offset;
    Ok((
        curtain::layout_grid(
            &ax,
            z0,
            &u,
            &v,
            &grid(
                u.len(),
                v.len(),
                spec.interior,
                spec.border,
                &spec.lines,
                &spec.top,
            ),
        ),
        z0,
    ))
}

/// Curtain wall body (panels and mullions).
pub(crate) fn mesh_db(db: &Connection, id: i64) -> Result<ddc_geom::mesh::Mesh, Error> {
    Ok(layout_db(db, id)?.mesh())
}

/// Layout from the grid edits (lines, removed segments, mullions, panel types and merges);
/// after the wall is resized the edits are carried over (`CurtainEdit::fit`).
fn edited_layout_db(
    db: &Connection,
    spec: CurtainSpec,
    e: &ddc_geom::curtain_edit::CurtainEdit,
) -> Result<CurtainLayout, Error> {
    use ddc_geom::curtain_edit::PanelType;
    let elev: f64 = db.query_row(
        "SELECT elevation FROM level WHERE id=?1",
        [spec.level],
        |r| r.get(0),
    )?;
    let ax = axis(&spec)?;
    let e = e.fit(ax.length(), spec.height);
    let z0 = elev + spec.base_offset;
    let layout = e.layout(&ax, z0);
    let kinds = e.cell_kinds();
    let nv = layout.v.len().saturating_sub(1);
    let mut panels = vec![PanelSpec::default(); layout.cells.len()];
    for (k, p) in panels.iter_mut().enumerate() {
        let c = layout
            .cells
            .get(k)
            .map_or((k / nv.max(1), k % nv.max(1)), |c| (c.i, c.j));
        p.kind = match kinds.get(&c).copied().unwrap_or_default() {
            PanelType::Glazed => PanelKind::Glazed,
            PanelType::Solid => PanelKind::Solid,
            PanelType::Empty => PanelKind::Empty,
            PanelType::Door => PanelKind::Door,
            PanelType::Window => PanelKind::Window,
        };
    }
    Ok(CurtainLayout {
        spec,
        layout,
        panels,
        z0,
    })
}

/// Layout with the panels of all cells (with grid edits, if any).
pub(crate) fn layout_db(db: &Connection, id: i64) -> Result<CurtainLayout, Error> {
    let spec = read(db, id)?;
    if let Some(e) = crate::wall_edit::curtain_edit_db(db, id)? {
        return edited_layout_db(db, spec, &e);
    }
    let (layout, z0) = build_layout(db, &spec)?;
    let v = &layout.v;
    let mut st =
        db.prepare("SELECT i,j,kind,name,swing_side,swing_hinge FROM curtain_panel WHERE cw=?1")?;
    let rows: Vec<(usize, usize, String, String, bool, bool)> = st
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)? as usize,
                r.get::<_, i64>(1)? as usize,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let nv = v.len() - 1;
    let mut panels = vec![PanelSpec::default(); layout.cells.len()];
    for (i, j, kind, name, side, hinge) in rows {
        let k = i * nv + j;
        if let (Some(p), Some(kind)) = (panels.get_mut(k), PanelKind::parse(&kind)) {
            if j < nv {
                *p = PanelSpec {
                    kind,
                    name,
                    swing_side: side,
                    swing_hinge: hinge,
                };
            }
        }
    }
    Ok(CurtainLayout {
        spec,
        layout,
        panels,
        z0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(level: i64) -> CurtainSpec {
        let m = Section::Rect { w: 0.05, d: 0.15 };
        CurtainSpec {
            level,
            name: "Storefront".into(),
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            bulge: 0.0,
            base_offset: 0.0,
            height: 3.0,
            u: GridLines::Spacing(1.5),
            v: GridLines::Spacing(1.0),
            interior: m,
            border: m,
            lines: Default::default(),
            top: vec![],
        }
    }

    #[test]
    fn many_panels_are_one_undo_step() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let id = m.add_curtain_wall(&spec(lv)).unwrap();
        let solid = PanelSpec { kind: PanelKind::Solid, ..Default::default() };
        let edits: Vec<(u32, u32, PanelSpec)> = (0..4).map(|i| (i, 0, solid.clone())).collect();
        m.set_curtain_panels(id, &edits).unwrap();
        m.assert_integrity();
        assert_eq!(m.curtain_layout(id).unwrap().panels.iter().filter(|p| p.kind == PanelKind::Solid).count(), 4);
        assert!(m.set_curtain_panels(id, &[(99, 0, solid)]).is_err());
        m.undo().unwrap();
        assert!(m.curtain_layout(id).unwrap().panels.iter().all(|p| p.kind == PanelKind::Glazed));
    }

    #[test]
    fn curtain_6x3_grid_1_5x1_counts_and_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let id = m.add_curtain_wall(&spec(lv)).unwrap();
        m.assert_integrity();
        let q = m.curtain_quantities(id).unwrap();
        assert_eq!((q.cells, q.panels, q.glazed), (12, 12, 12));
        assert!((q.gross_area - 18.0).abs() < 1e-12);
        assert!((q.glass_area - 16.1).abs() < 1e-12, "{}", q.glass_area);
        // verticals continuous (9 + 6), transoms between them (2·5.75 + 2·5.75)
        assert!((q.interior_length - 20.5).abs() < 1e-12);
        assert!((q.border_length - 17.5).abs() < 1e-12);
        assert!((q.mullion_length - 38.0).abs() < 1e-12);
        // door in the bottom of column 1: 1.45 × 0.925 leaves the glass
        let door = PanelSpec {
            kind: PanelKind::Door,
            name: "Door-Curtain".into(),
            swing_side: true,
            swing_hinge: false,
        };
        m.set_curtain_panel(id, 1, 0, &door).unwrap();
        let q = m.curtain_quantities(id).unwrap();
        assert_eq!((q.panels, q.glazed, q.doors), (12, 11, 1));
        assert!((q.glass_area - (16.1 - 1.45 * 0.925)).abs() < 1e-12);
        m.set_curtain_panel(
            id,
            3,
            2,
            &PanelSpec {
                kind: PanelKind::Empty,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(m.curtain_quantities(id).unwrap().panels, 11);
        assert!(m.set_curtain_panel(id, 4, 0, &door).is_err());
        // coarser grid: panels follow their cell centres; the door no longer fits its cell
        let mut s = spec(lv);
        s.u = GridLines::At(vec![2.0]);
        let warn = m.set_curtain_wall(id, &s).unwrap();
        assert_eq!(warn.len(), 1, "{warn:?}");
        assert!(warn[0].contains("door in cell 1,0"), "{warn:?}");
        let l = m.curtain_layout(id).unwrap();
        assert_eq!(l.layout.cells.len(), 6);
        assert_eq!(l.panels[3].kind, PanelKind::Door);
        assert_eq!(l.panels[5].kind, PanelKind::Empty);
        assert_eq!(
            l.panels
                .iter()
                .filter(|p| **p != PanelSpec::default())
                .count(),
            2
        );
        m.undo().unwrap();
        let l = m.curtain_layout(id).unwrap();
        assert_eq!(
            (l.layout.cells.len(), l.panels[3].kind),
            (12, PanelKind::Door)
        );
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.curtain_quantities(id).unwrap().glazed, 12);
        assert!(matches!(m.delete_level(lv), Err(Error::Referenced { .. })));
        m.set_curtain_panel(id, 0, 0, &door).unwrap();
        m.delete_curtain_wall(id).unwrap();
        assert!(m.curtain_walls().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.curtain_quantities(id).unwrap().doors, 1);
        m.assert_integrity();
        let mesh = m.curtain_layout(id).unwrap().mesh();
        assert!(mesh.is_watertight());
    }

    #[test]
    fn grid_shift_remaps_panels_by_position() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let id = m.add_curtain_wall(&spec(lv)).unwrap();
        let door = PanelSpec {
            kind: PanelKind::Door,
            ..Default::default()
        };
        m.set_curtain_panel(id, 2, 0, &door).unwrap();
        // a new line in front of the door: same-size door moves to column 3, no warning
        let mut s = spec(lv);
        s.u = GridLines::At(vec![1.0, 1.5, 3.0, 4.5]);
        assert!(m.set_curtain_wall(id, &s).unwrap().is_empty());
        let l = m.curtain_layout(id).unwrap();
        assert_eq!(l.panels[3 * 3].kind, PanelKind::Door);
        assert_eq!(l.panels[2 * 3].kind, PanelKind::Glazed);
        // shorter wall: the door's centre is past the end and the 1.0 m end cell is narrower
        // than the 1.5 m door, so it is removed with a warning
        s.b = (2.0, 0.0);
        s.u = GridLines::Spacing(1.0);
        let w = m.set_curtain_wall(id, &s).unwrap();
        assert_eq!(m.curtain_quantities(id).unwrap().doors, 0);
        assert!(w.iter().any(|x| x.contains("no longer fits")), "{w:?}");
        m.undo().unwrap();
        m.undo().unwrap();
        let l = m.curtain_layout(id).unwrap();
        assert_eq!(
            (l.layout.cells.len(), l.panels[2 * 3].kind),
            (12, PanelKind::Door)
        );
        // over the cell limit
        s = spec(lv);
        s.u = GridLines::Spacing(0.002);
        s.v = GridLines::Spacing(0.002);
        assert!(m.set_curtain_wall(id, &s).is_err());
    }

    #[test]
    fn plan_symbol_double_line_and_ticks() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        m.add_curtain_wall(&spec(lv)).unwrap();
        let v = m
            .plan_view(&crate::PlanViewSpec {
                level: lv,
                cut_height: 1.0,
                grids: vec![],
            })
            .unwrap();
        // 4 bays × 2 faces + 5 ticks
        assert_eq!(
            v.count(|p| matches!(p, crate::ViewPrim::CurtainLine { .. })),
            13
        );
    }

    #[test]
    fn grid_and_section_text_round_trip() {
        for g in [
            GridLines::Spacing(1.5),
            GridLines::At(vec![0.5, 2.25]),
            GridLines::At(vec![]),
        ] {
            assert_eq!(GridLines::parse(&g.text()).unwrap(), g);
        }
        for s in [
            Section::None,
            Section::Rect { w: 0.05, d: 0.15 },
            Section::Circle { d: 0.06 },
        ] {
            assert_eq!(parse_section(&section_text(&s)).unwrap(), s);
        }
        assert!(GridLines::parse("s 0").is_err() && parse_section("r 1").is_err());
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let mut s = spec(lv);
        s.height = 0.0;
        assert!(m.add_curtain_wall(&s).is_err());
        s = spec(99);
        assert!(m.add_curtain_wall(&s).is_err());
    }
    /// Schema 10: line / segment sections and a sloped top round-trip through the file, a
    /// schema 9 file gains both columns and keeps its curtain walls.
    #[test]
    fn lines_and_top_round_trip_and_schema_9_migrates() {
        let p = std::env::temp_dir().join(format!("ddc-cw10-{}.ddc", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let ps = p.to_str().unwrap();
        let mut s = spec(0);
        {
            let mut m = Model::open(ps).unwrap();
            s.level = m.add_level("L1", 0.0).unwrap();
            m.add_curtain_wall(&spec(s.level)).unwrap();
            s.lines = LineSections {
                u: vec![(2, None, Section::Circle { d: 0.1 })],
                v: vec![(1, Some(0), Section::None)],
            };
            s.top = vec![(0.0, 2.5), (3.0, 3.0), (6.0, 2.5)];
            let id = m.add_curtain_wall(&s).unwrap();
            assert_eq!(m.curtain_wall(id).unwrap(), s);
            let q = m.curtain_quantities(id).unwrap();
            let flat = m.curtain_quantities(1).unwrap();
            assert!(
                q.glass_area < flat.glass_area - 0.5,
                "{} {}",
                q.glass_area,
                flat.glass_area
            );
            m.assert_integrity();
        }
        // back to schema 9: drop the new columns of the plain wall's file
        {
            let raw = rusqlite::Connection::open(ps).unwrap();
            raw.execute_batch(
                "DELETE FROM curtain_wall WHERE id=2;                  ALTER TABLE curtain_wall DROP COLUMN mullion_lines;                  ALTER TABLE curtain_wall DROP COLUMN top_profile;                  UPDATE meta SET value=9 WHERE key='schema_version';",
            )
            .unwrap();
        }
        let mut m = Model::open(ps).unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        assert_eq!(m.curtain_wall(1).unwrap(), spec(s.level));
        let id = m.add_curtain_wall(&s).unwrap();
        assert_eq!(m.curtain_wall(id).unwrap(), s);
        drop(m);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{ps}{ext}"));
        }
    }

    fn model_with_wall(s: &mut CurtainSpec) -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        s.level = m.add_level("L1", 0.0).unwrap();
        let id = m.add_curtain_wall(s).unwrap();
        (m, id)
    }

    #[test]
    fn a_door_at_the_trimmed_edge_stays_when_the_end_cell_fits_it() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let id = m.add_curtain_wall(&spec(lv)).unwrap();
        let door = PanelSpec {
            kind: PanelKind::Door,
            ..Default::default()
        };
        m.set_curtain_panel(id, 3, 0, &door).unwrap();
        let mut s = spec(lv);
        s.b = (4.5, 0.0);
        let w = m.set_curtain_wall(id, &s).unwrap();
        assert_eq!(m.curtain_quantities(id).unwrap().doors, 1, "{w:?}");
        // a narrower end cell cannot hold it (fresh wall: the door is pulled in from beyond the end)
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let id = m.add_curtain_wall(&spec(lv)).unwrap();
        m.set_curtain_panel(id, 3, 0, &door).unwrap();
        let mut s = spec(lv);
        s.b = (4.0, 0.0);
        s.u = GridLines::Spacing(1.0);
        let w = m.set_curtain_wall(id, &s).unwrap();
        assert_eq!(m.curtain_quantities(id).unwrap().doors, 0);
        assert!(w.iter().any(|x| x.contains("no longer fits")), "{w:?}");
    }

    #[test]
    fn shortening_keeps_a_door_and_warns_about_dropped_panels() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let id = m.add_curtain_wall(&spec(lv)).unwrap();
        let door = PanelSpec {
            kind: PanelKind::Door,
            name: "Door-Curtain".into(),
            swing_side: true,
            swing_hinge: false,
        };
        let solid = PanelSpec {
            kind: PanelKind::Empty,
            ..Default::default()
        };
        m.set_curtain_panel(id, 3, 0, &door).unwrap();
        m.set_curtain_panel(id, 3, 2, &solid).unwrap();
        let mut s = spec(lv);
        s.b = (3.0, 0.0);
        let w = m.set_curtain_wall(id, &s).unwrap();
        let l = m.curtain_layout(id).unwrap();
        assert!(l.panels.iter().any(|p| p.kind == PanelKind::Door), "door kept");
        assert!(w.iter().any(|x| x.contains("removed")), "{w:?}");
    }

    #[test]
    fn spacing_edit_remaps_line_overrides_by_position() {
        let mut s = spec(0);
        s.lines = LineSections {
            u: vec![
                (2, None, Section::None),
                (1, Some(0), Section::Circle { d: 0.1 }),
            ],
            v: vec![],
        };
        let (mut m, id) = model_with_wall(&mut s);
        // 1.5 -> 1.0: u line 2 (x=3.0) becomes index 3; segment 1:0 (x=1.5) has no line -> dropped
        let mut e = m.curtain_wall(id).unwrap();
        e.u = GridLines::Spacing(1.0);
        let w = m.set_curtain_wall(id, &e).unwrap();
        let got = m.curtain_wall(id).unwrap();
        assert_eq!(got.lines.u, vec![(3, None, Section::None)]);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("u 1:0"));
        // back to 1.5: x=3.0 is index 2 again
        let mut e = got;
        e.u = GridLines::Spacing(1.5);
        assert!(m.set_curtain_wall(id, &e).unwrap().is_empty());
        assert_eq!(
            m.curtain_wall(id).unwrap().lines.u,
            vec![(2, None, Section::None)]
        );
        m.assert_integrity();
    }

    /// Regression (delete probe, curtain_wall): one undo of a delete gives back the same schema
    /// and rows; the delete used to create the wall edit tables, which the undo left behind.
    #[test]
    fn delete_undo_restores_schema_and_rows() {
        let mut s = spec(0);
        let (mut m, id) = model_with_wall(&mut s);
        let snap = |m: &Model| -> (Vec<String>, i64, i64) {
            let mut st = m
                .db
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap();
            let names: Vec<String> =
                st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
            let n = |sql: &str| m.db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
            let cw = n("SELECT count(*) FROM curtain_wall");
            (names, cw, n("SELECT count(*) FROM curtain_panel"))
        };
        let before = snap(&m);
        m.delete_curtain_wall(id).unwrap();
        assert_eq!(snap(&m).1, before.1 - 1);
        m.undo().unwrap();
        assert_eq!(snap(&m), before);
        m.assert_integrity();
    }

    #[test]
    fn check_rejects_override_outside_grid() {
        let mut s = spec(0);
        let (m, _) = model_with_wall(&mut s);
        s.lines.u = vec![(9, None, Section::None)];
        assert!(check_curtain(&m.db, &s).is_err());
        s.lines.u = vec![(1, Some(7), Section::None)];
        assert!(check_curtain(&m.db, &s).is_err());
        s.lines.u = vec![(1, Some(1), Section::None)];
        assert!(check_curtain(&m.db, &s).is_ok());
    }

    #[test]
    fn height_edit_scales_sloped_top() {
        let mut s = spec(0);
        s.top = vec![(0.0, 2.0), (3.0, 3.0), (6.0, 2.0)];
        let (mut m, id) = model_with_wall(&mut s);
        for h in [2.0, 4.5] {
            let mut e = m.curtain_wall(id).unwrap();
            let h0 = e.height;
            e.height = h;
            m.set_curtain_wall(id, &e).unwrap();
            let got = m.curtain_wall(id).unwrap();
            assert_eq!(got.height, h);
            let _ = h0;
            assert!((got.top[1].1 - h).abs() < 1e-6 && (got.top[0].1 - h * 2.0 / 3.0).abs() < 1e-6);
            let l = m.curtain_layout(id).unwrap();
            let q = l.quantities();
            let under: f64 = got
                .top
                .windows(2)
                .map(|w| (w[1].0 - w[0].0) * (w[0].1 + w[1].1) / 2.0)
                .sum();
            // gross is length x height by definition; panels lie under the scaled profile
            assert!((q.gross_area - 6.0 * h).abs() < 1e-6, "{}", q.gross_area);
            assert!(q.panel_area > 0.5 * under && q.panel_area <= under + 1e-9);
            // the top is reached by the grid: no row above the profile
            assert!(l.layout.v.last().copied().unwrap() >= got.top[1].1 - 1e-9);
        }
        m.assert_integrity();
    }
}
