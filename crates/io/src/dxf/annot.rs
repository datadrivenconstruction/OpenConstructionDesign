// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan stand-ins for records kept verbatim: LEADER and MULTILEADER (leader lines, arrowheads,
//! dogleg, text), IMAGE (frame and file name) and REGION / 3DSOLID / BODY (extents of the
//! inline modeler text). Used by INSERT expansion and the drawing import, so DXF and DWG
//! sources draw the same; the verbatim record itself is kept unchanged.

use std::collections::HashMap;

use super::entities::*;
use super::{DxfDocument, Pair};

/// Largest modeler text scanned for extents, characters.
const MAX_SAT_SCAN: usize = 4 << 20;

/// The DXF modeler-text cipher (its own inverse): printable characters map to 159 - c.
pub(crate) fn sat_crypt(s: &str) -> String {
    s.chars()
        .map(|c| match c as u32 {
            33..=126 => char::from(159 - c as u8),
            _ => c,
        })
        .collect()
}

/// Records of a raw section, each starting at a code 0 pair.
fn records(pairs: &[Pair]) -> Vec<&[Pair]> {
    let mut out = vec![];
    let mut start = None;
    for (i, p) in pairs.iter().enumerate() {
        if p.code == 0 {
            if let Some(s) = start {
                out.push(&pairs[s..i]);
            }
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(&pairs[s..]);
    }
    out
}

/// IMAGEDEF handle (upper-case hex) to its file path, from the OBJECTS section.
pub(crate) fn image_paths(doc: &DxfDocument) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for s in doc.other_sections.iter().filter(|s| s.name == "OBJECTS") {
        for rec in records(&s.pairs) {
            if rec[0].value.trim() != "IMAGEDEF" {
                continue;
            }
            let get = |c: i32| rec.iter().find(|p| p.code == c).map(|p| p.value.trim());
            if let (Some(h), Some(path)) = (get(5), get(1)) {
                out.insert(h.to_uppercase(), path.to_string());
            }
        }
    }
    out
}

fn num(p: &Pair) -> Option<f64> {
    p.value.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Layer, linetype and colours of the record (the entity header pairs).
fn common_of(pairs: &[Pair]) -> Common {
    let mut c = Common::default();
    let int = |p: &Pair| p.value.trim().parse::<i32>().ok();
    for p in pairs {
        match p.code {
            8 => c.layer = p.value.clone(),
            6 => c.linetype = Some(p.value.clone()),
            62 => c.color = int(p),
            420 => c.true_color = int(p),
            370 => c.lineweight = int(p),
            67 | 410 => c.extra.push(p.clone()),
            _ => {}
        }
    }
    c
}

type V2 = [f64; 2];

fn poly(c: &Common, pts: &[V2], closed: bool) -> Entity {
    Entity::LwPolyline(LwPolyline {
        common: c.clone(),
        flags: closed as i32,
        vertices: pts
            .iter()
            .map(|q| LwVertex {
                x: q[0],
                y: q[1],
                ..Default::default()
            })
            .collect(),
    })
}

/// Closed arrowhead with its tip at `tip`, pointing away from `from`; length `size`.
fn arrow(c: &Common, tip: V2, from: V2, size: f64) -> Option<Entity> {
    let (dx, dy) = (from[0] - tip[0], from[1] - tip[1]);
    let len = dx.hypot(dy);
    if len <= 1e-12 || size <= 1e-12 {
        return None;
    }
    let s = size.min(len);
    let (ux, uy) = (dx / len, dy / len);
    let (bx, by) = (tip[0] + ux * s, tip[1] + uy * s);
    let w = s / 6.0;
    let pts = [tip, [bx - uy * w, by + ux * w], [bx + uy * w, by - ux * w]];
    Some(poly(c, &pts, true))
}

/// Point components: code `base` starts a point, `base + 10` / `base + 20` complete it.
struct Pts {
    base: i32,
    v: Vec<[f64; 3]>,
}

impl Pts {
    fn new(base: i32) -> Self {
        Pts { base, v: vec![] }
    }
    fn take(&mut self, p: &Pair) -> bool {
        let Some(x) = num(p) else {
            return false;
        };
        match p.code - self.base {
            0 => self.v.push([x, 0.0, 0.0]),
            10 => match self.v.last_mut() {
                Some(q) => q[1] = x,
                None => return false,
            },
            20 => match self.v.last_mut() {
                Some(q) => q[2] = x,
                None => return false,
            },
            _ => return false,
        }
        true
    }
    fn xy(&self) -> Vec<V2> {
        self.v.iter().map(|q| [q[0], q[1]]).collect()
    }
}

fn leader(pairs: &[Pair], c: &Common) -> Vec<Entity> {
    let mut pts = Pts::new(10);
    let (mut on, mut h) = (true, 0.0);
    for p in pairs {
        if pts.take(p) {
            continue;
        }
        match p.code {
            71 => on = p.value.trim() != "0",
            40 => h = num(p).unwrap_or(0.0),
            _ => {}
        }
    }
    let v = pts.xy();
    if v.len() < 2 {
        return vec![];
    }
    let mut out = vec![poly(c, &v, false)];
    if on {
        let seg = (v[1][0] - v[0][0]).hypot(v[1][1] - v[0][1]);
        let size = if h > 0.0 { h } else { seg * 0.2 };
        out.extend(arrow(c, v[0], v[1], size.min(seg * 0.5)));
    }
    out
}

#[derive(Default)]
struct Root {
    conn: Option<V2>,
    dir: V2,
    landing: f64,
    lines: Vec<Vec<V2>>,
}

fn mleader(pairs: &[Pair], c: &Common) -> Vec<Entity> {
    #[derive(Clone, Copy, PartialEq)]
    enum At {
        Top,
        Ctx,
        Root,
        Line,
    }
    let mut at = At::Top;
    let mut roots: Vec<Root> = vec![];
    let mut line = Pts::new(10);
    let (mut size, mut th, mut width, mut rot) = (0.0, 0.0, 0.0, 0.0);
    let mut text: Option<String> = None;
    let mut loc = Pts::new(12);
    for p in pairs {
        let val = p.value.trim();
        match (at, p.code) {
            (At::Top, 300) if val == "CONTEXT_DATA{" => at = At::Ctx,
            (At::Ctx, 301) => at = At::Top,
            (At::Ctx, 302) => {
                roots.push(Root {
                    dir: [1.0, 0.0],
                    ..Default::default()
                });
                at = At::Root;
            }
            (At::Ctx, 140) => size = num(p).unwrap_or(0.0),
            (At::Ctx, 41) => th = num(p).unwrap_or(0.0),
            (At::Ctx, 43) => width = num(p).unwrap_or(0.0),
            (At::Ctx, 42) => rot = num(p).unwrap_or(0.0),
            (At::Ctx, 304) => text = Some(p.value.clone()),
            (At::Ctx, 12 | 22 | 32) => {
                loc.take(p);
            }
            (At::Root, 303) => at = At::Ctx,
            (At::Root, 304) if val == "LEADER_LINE{" => {
                line = Pts::new(10);
                at = At::Line;
            }
            (At::Root, 10 | 20 | 11 | 21 | 40) => {
                let (Some(r), Some(x)) = (roots.last_mut(), num(p)) else {
                    continue;
                };
                match p.code {
                    10 => r.conn = Some([x, 0.0]),
                    20 => {
                        if let Some(q) = &mut r.conn {
                            q[1] = x;
                        }
                    }
                    11 => r.dir[0] = x,
                    21 => r.dir[1] = x,
                    _ => r.landing = x,
                }
            }
            (At::Line, 305) => {
                if let Some(r) = roots.last_mut() {
                    r.lines.push(line.xy());
                }
                at = At::Root;
            }
            (At::Line, _) => {
                line.take(p);
            }
            _ => {}
        }
    }
    let mut out = vec![];
    for r in &roots {
        for l in &r.lines {
            let mut v = l.clone();
            v.extend(r.conn);
            if v.len() >= 2 {
                out.push(poly(c, &v, false));
                out.extend(arrow(c, v[0], v[1], size));
            }
        }
        if let Some(q) = r.conn {
            let n = r.dir[0].hypot(r.dir[1]);
            if r.landing > 0.0 && n > 1e-12 && !r.lines.is_empty() {
                let k = r.landing / n;
                let e = [q[0] + r.dir[0] * k, q[1] + r.dir[1] * k];
                out.push(poly(c, &[q, e], false));
            }
        }
    }
    if let (Some(t), Some(q)) = (text, loc.v.first()) {
        if !t.is_empty() && th > 0.0 {
            out.push(Entity::MText(MText {
                common: c.clone(),
                pos: *q,
                height: th,
                width,
                text: t,
                rotation: rot.to_degrees(),
                style: None,
                attach: Some(1),
            }));
        }
    }
    out
}

fn image(pairs: &[Pair], c: &Common, paths: &HashMap<String, String>) -> Vec<Entity> {
    let (mut o, mut u, mut v) = (Pts::new(10), Pts::new(11), Pts::new(12));
    let (mut sx, mut sy, mut def) = (0.0, 0.0, None);
    for p in pairs {
        if o.take(p) || u.take(p) || v.take(p) {
            continue;
        }
        match p.code {
            13 => sx = num(p).unwrap_or(0.0),
            23 => sy = num(p).unwrap_or(0.0),
            340 => def = Some(p.value.trim().to_uppercase()),
            _ => {}
        }
    }
    let (Some(o), Some(u), Some(v)) = (o.v.first(), u.v.first(), v.v.first()) else {
        return vec![];
    };
    let a = [u[0] * sx, u[1] * sx];
    let b = [v[0] * sy, v[1] * sy];
    let (wa, wb) = (a[0].hypot(a[1]), b[0].hypot(b[1]));
    if wa <= 1e-12 || wb <= 1e-12 {
        return vec![];
    }
    let p0 = [o[0], o[1]];
    let corners = [
        p0,
        [p0[0] + a[0], p0[1] + a[1]],
        [p0[0] + a[0] + b[0], p0[1] + a[1] + b[1]],
        [p0[0] + b[0], p0[1] + b[1]],
    ];
    let mut out = vec![poly(c, &corners, true)];
    if let Some(path) = def.and_then(|h| paths.get(&h)) {
        let name = path
            .rsplit(|ch: char| ch == '/' || ch == '\\')
            .next()
            .unwrap_or(path);
        if !name.is_empty() {
            out.push(Entity::Text(Text {
                common: c.clone(),
                pos: [p0[0], p0[1], o[2]],
                height: wa.min(wb) * 0.04,
                text: name.to_string(),
                rotation: a[1].atan2(a[0]).to_degrees(),
                style: None,
            }));
        }
    }
    out
}

/// Extents (min, max) of the vertex points in decoded modeler text, with the body
/// transform applied when the text holds exactly one.
pub(crate) fn sat_extents(sat: &str) -> Option<([f64; 3], [f64; 3])> {
    let toks: Vec<&str> = sat.split_whitespace().collect();
    let mut points = vec![];
    let mut xf: Vec<[f64; 13]> = vec![];
    let mut i = 0;
    while i < toks.len() {
        let kind = toks[i];
        if kind != "point" && kind != "transform" {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < toks.len() && toks[j] != "#" {
            j += 1;
        }
        let nums: Vec<f64> = toks[i + 1..j]
            .iter()
            .filter(|t| !t.starts_with('$'))
            .filter_map(|t| t.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .collect();
        if kind == "point" && nums.len() >= 3 {
            points.push([
                nums[nums.len() - 3],
                nums[nums.len() - 2],
                nums[nums.len() - 1],
            ]);
        } else if kind == "transform" && nums.len() >= 13 {
            // rows of the rotation, translation, scale (after an optional leading index)
            let s = &nums[nums.len() - 13..];
            let mut a = [0.0; 13];
            a.copy_from_slice(s);
            xf.push(a);
        }
        i = j + 1;
    }
    let apply = |p: [f64; 3]| -> [f64; 3] {
        let [a] = xf.as_slice() else {
            return p;
        };
        let k = if a[12] != 0.0 { a[12] } else { 1.0 };
        let mut q = [0.0; 3];
        for (c, out) in q.iter_mut().enumerate() {
            *out = k * (p[0] * a[c] + p[1] * a[3 + c] + p[2] * a[6 + c]) + a[9 + c];
        }
        q
    };
    let mut it = points.into_iter().map(apply);
    let first = it.next()?;
    let (mut lo, mut hi) = (first, first);
    for p in it {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    Some((lo, hi))
}

fn solid(pairs: &[Pair], c: &Common) -> Vec<Entity> {
    let mut sat = String::new();
    for p in pairs.iter().filter(|p| p.code == 1 || p.code == 3) {
        if sat.len() + p.value.len() > MAX_SAT_SCAN {
            return vec![];
        }
        if p.code == 1 && !sat.is_empty() {
            sat.push('\n');
        }
        sat.push_str(&sat_crypt(&p.value));
    }
    let Some((lo, hi)) = sat_extents(&sat) else {
        return vec![];
    };
    if hi[0] - lo[0] <= 1e-12 && hi[1] - lo[1] <= 1e-12 {
        return vec![];
    }
    let pts = [
        [lo[0], lo[1]],
        [hi[0], lo[1]],
        [hi[0], hi[1]],
        [lo[0], hi[1]],
    ];
    vec![poly(c, &pts, true)]
}

/// Table entity: an INSERT of its anonymous block (rendered lines and texts) at the table
/// origin, turned by the horizontal direction vector; scale is always 1.
fn table_ref(pairs: &[Pair], mut c: Common) -> Vec<Entity> {
    let mut i = Insert::default();
    let mut seen = [false; 6];
    let mut dir = [1.0f64, 0.0];
    for q in pairs.iter().skip(1) {
        let v = num(q).unwrap_or(0.0);
        let slot = match q.code {
            2 => 0,
            10 => 1,
            20 => 2,
            30 => 3,
            11 => 4,
            21 => 5,
            210 | 220 | 230 => {
                c.extra.push(q.clone());
                continue;
            }
            _ => continue,
        };
        if std::mem::replace(&mut seen[slot], true) {
            continue;
        }
        match slot {
            0 => i.block = q.value.trim().to_string(),
            1..=3 => i.pos[slot - 1] = v,
            4 => dir[0] = v,
            _ => dir[1] = v,
        }
    }
    if i.block.is_empty() {
        return vec![];
    }
    if dir[0].hypot(dir[1]) > 1e-9 {
        i.rotation = dir[1].atan2(dir[0]).to_degrees();
    }
    i.common = c;
    vec![Entity::Insert(i)]
}

/// Plan stand-in entities for a verbatim record (empty for kinds without one).
/// `images`: IMAGEDEF handle to file path (see [`image_paths`]).
/// Class entity derived from a block reference: an INSERT of its block (expanded by the caller).
fn block_ref(pairs: &[Pair], mut c: Common) -> Vec<Entity> {
    let mut i = Insert::default();
    let mut has_block = false;
    for q in pairs {
        let v = num(q);
        match q.code {
            2 => {
                i.block = q.value.trim().to_string();
                has_block = !i.block.is_empty();
            }
            10 | 20 | 30 => i.pos[((q.code - 10) / 10) as usize] = v.unwrap_or(0.0),
            41..=43 => i.scale[(q.code - 41) as usize] = v.unwrap_or(1.0),
            50 => i.rotation = v.unwrap_or(0.0),
            210 | 220 | 230 => c.extra.push(q.clone()),
            _ => {}
        }
    }
    if !has_block {
        return vec![];
    }
    i.common = c;
    vec![Entity::Insert(i)]
}

pub(crate) fn lower(pairs: &[Pair], images: &HashMap<String, String>) -> Vec<Entity> {
    let Some(head) = pairs.first() else {
        return vec![];
    };
    let kind = head.value.trim();
    if !matches!(
        kind,
        "LEADER"
            | "MULTILEADER"
            | "MLEADER"
            | "IMAGE"
            | "3DSOLID"
            | "REGION"
            | "BODY"
            | "ACIDBLOCKREFERENCE"
            | "ACAD_TABLE"
    ) {
        return vec![];
    }
    let c = common_of(pairs);
    match kind {
        "LEADER" => leader(pairs, &c),
        "MULTILEADER" | "MLEADER" => mleader(pairs, &c),
        "IMAGE" => image(pairs, &c, images),
        "ACIDBLOCKREFERENCE" => block_ref(pairs, c),
        "ACAD_TABLE" => table_ref(pairs, c),
        _ => solid(pairs, &c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxf::RawSection;

    fn pr(code: i32, v: impl ToString) -> Pair {
        Pair::new(code, v.to_string())
    }

    fn pt(out: &mut Vec<Pair>, base: i32, x: f64, y: f64) {
        out.extend([pr(base, x), pr(base + 10, y), pr(base + 20, 0)]);
    }

    fn verts(e: &Entity) -> Vec<V2> {
        match e {
            Entity::LwPolyline(l) => l.vertices.iter().map(|v| [v.x, v.y]).collect(),
            e => panic!("{e:?}"),
        }
    }

    fn head(kind: &str) -> Vec<Pair> {
        vec![
            pr(0, kind),
            pr(100, "AcDbEntity"),
            pr(8, "NOTES"),
            pr(62, 3),
        ]
    }

    #[test]
    fn leader_path_and_arrowhead() {
        let mut v = head("LEADER");
        v.extend([pr(100, "AcDbLeader"), pr(71, 1), pr(76, 3)]);
        pt(&mut v, 10, 0.0, 0.0);
        pt(&mut v, 10, 10.0, 0.0);
        pt(&mut v, 10, 10.0, 5.0);
        v.extend([pr(210, 0), pr(220, 0), pr(230, 1)]);
        let out = lower(&v, &HashMap::new());
        assert_eq!(out.len(), 2);
        assert_eq!(verts(&out[0]), vec![[0.0, 0.0], [10.0, 0.0], [10.0, 5.0]]);
        let a = verts(&out[1]);
        assert_eq!(a[0], [0.0, 0.0]);
        assert!((a[1][0] - 2.0).abs() < 1e-9 && (a[2][0] - 2.0).abs() < 1e-9);
        let Entity::LwPolyline(l) = &out[1] else {
            unreachable!()
        };
        assert!(l.closed() && l.common.layer == "NOTES" && l.common.color == Some(3));
        // Arrowhead disabled: the path only.
        let i = v.iter().position(|p| p.code == 71).unwrap();
        v[i] = pr(71, 0);
        assert_eq!(lower(&v, &HashMap::new()).len(), 1);
    }

    #[test]
    fn mleader_lines_dogleg_text() {
        let mut v = head("MULTILEADER");
        v.extend([
            pr(100, "AcDbMLeader"),
            pr(270, 2),
            pr(300, "CONTEXT_DATA{"),
            pr(40, 1),
        ]);
        pt(&mut v, 10, 99.0, 99.0);
        v.extend([pr(41, 2.5), pr(140, 1.0), pr(290, 1), pr(304, "Note")]);
        pt(&mut v, 12, 16.0, 6.0);
        v.extend([pr(42, 0), pr(43, 20), pr(302, "LEADER{")]);
        pt(&mut v, 10, 14.0, 5.0);
        pt(&mut v, 11, 1.0, 0.0);
        v.extend([pr(90, 0), pr(40, 2.0), pr(304, "LEADER_LINE{")]);
        pt(&mut v, 10, 0.0, 0.0);
        pt(&mut v, 10, 10.0, 5.0);
        v.extend([
            pr(91, 0),
            pr(305, "}"),
            pr(303, "}"),
            pr(301, "}"),
            pr(170, 1),
        ]);
        let out = lower(&v, &HashMap::new());
        assert_eq!(out.len(), 4, "{out:?}");
        assert_eq!(verts(&out[0]), vec![[0.0, 0.0], [10.0, 5.0], [14.0, 5.0]]);
        assert_eq!(verts(&out[1]).len(), 3);
        assert_eq!(verts(&out[2]), vec![[14.0, 5.0], [16.0, 5.0]]);
        let Entity::MText(t) = &out[3] else {
            panic!("{:?}", out[3]);
        };
        assert_eq!(
            (t.text.as_str(), t.pos, t.height, t.width),
            ("Note", [16.0, 6.0, 0.0], 2.5, 20.0)
        );
    }

    #[test]
    fn image_frame_and_file_name() {
        let mut v = head("IMAGE");
        v.extend([pr(100, "AcDbRasterImage"), pr(90, 0)]);
        pt(&mut v, 10, 100.0, 50.0);
        pt(&mut v, 11, 0.5, 0.0);
        pt(&mut v, 12, 0.0, 0.5);
        v.extend([pr(13, 640), pr(23, 480), pr(340, "2f")]);
        let doc = DxfDocument {
            other_sections: vec![RawSection {
                name: "OBJECTS".into(),
                pairs: vec![
                    pr(0, "DICTIONARY"),
                    pr(5, "C"),
                    pr(0, "IMAGEDEF"),
                    pr(5, "2F"),
                    pr(1, "C:\\img\\site.png"),
                ],
            }],
            ..Default::default()
        };
        let paths = image_paths(&doc);
        assert_eq!(
            paths.get("2F").map(String::as_str),
            Some("C:\\img\\site.png")
        );
        let out = lower(&v, &paths);
        assert_eq!(
            verts(&out[0]),
            vec![[100.0, 50.0], [420.0, 50.0], [420.0, 290.0], [100.0, 290.0]]
        );
        let Entity::Text(t) = &out[1] else {
            panic!("{:?}", out[1]);
        };
        assert_eq!((t.text.as_str(), t.pos[0]), ("site.png", 100.0));
        // Without the definition: the frame only.
        assert_eq!(lower(&v, &HashMap::new()).len(), 1);
    }

    #[test]
    fn solid_extents_from_modeler_text() {
        let sat =
            "700 0 1 0\nbody $-1 $1 $2 #\npoint $-1 -1 $-1 1 2 0 #\npoint $-1 -1 $-1 4 -3 7 #";
        assert_eq!(sat_crypt(&sat_crypt(sat)), sat);
        let (lo, hi) = sat_extents(sat).unwrap();
        assert_eq!((lo, hi), ([1.0, -3.0, 0.0], [4.0, 2.0, 7.0]));
        let moved = format!(
            "{sat}\ntransform $-1 1 0 0 0 1 0 0 0 1 10 20 0 1 no_rotate no_reflect no_shear #"
        );
        assert_eq!(sat_extents(&moved).unwrap().0, [11.0, 17.0, 0.0]);
        let mut v = head("3DSOLID");
        v.extend([pr(100, "AcDbModelerGeometry"), pr(70, 1)]);
        for l in sat.lines() {
            v.push(pr(1, sat_crypt(l)));
        }
        let out = lower(&v, &HashMap::new());
        assert_eq!(
            verts(&out[0]),
            vec![[1.0, -3.0], [4.0, -3.0], [4.0, 2.0], [1.0, 2.0]]
        );
        // Other verbatim kinds have no stand-in.
        assert!(lower(&head("HATCH"), &HashMap::new()).is_empty());
    }

    #[test]
    fn block_reference_class_lowers_to_insert() {
        let mut v = head("ACIDBLOCKREFERENCE");
        v.extend([pr(100, "AcDbBlockReference"), pr(2, "*I333")]);
        pt(&mut v, 10, -10.5, 2.0);
        v.extend([pr(41, 2), pr(50, 90), pr(210, 0), pr(220, 0), pr(230, -1)]);
        let out = lower(&v, &HashMap::new());
        let [Entity::Insert(i)] = out.as_slice() else {
            panic!("{out:?}")
        };
        assert_eq!(i.block, "*I333");
        assert_eq!(
            (i.pos, i.scale, i.rotation),
            ([-10.5, 2.0, 0.0], [2.0, 1.0, 1.0], 90.0)
        );
        assert!(i.common.layer == "NOTES" && i.common.color == Some(3));
        assert!(i
            .common
            .extra
            .iter()
            .any(|q| q.code == 230 && q.value == "-1"));
        // Without a block name nothing is drawn.
        assert!(lower(&head("ACIDBLOCKREFERENCE"), &HashMap::new()).is_empty());
    }
}
