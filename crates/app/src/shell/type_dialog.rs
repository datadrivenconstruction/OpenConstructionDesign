//! Type dialog: Family · Type · Rename · parameter table
//! (Name | Value | Formula | Unit) · "Structure…" for multilayer types · preview · OK/Cancel/Apply.
//! Editing a wall type produces an `Edit` batch (one model operation, one undo step).
use crate::editor::{Editor, OpFn};
use crate::ui_kit as kit;
use crate::units::{display_length, edit_length, parse_edit_length, Units};
use ddc_model::{Edit, LayerSpec, MemberShape, MepResize, Model, TypeTable};
use ddc_ui::theme::{layout, space, Role};
use ddc_ui::widgets;
use ddc_ui::Theme;
use egui::{self, RichText};
use std::sync::Arc;

/// System type family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fam {
    Wall,
    Slab,
    Member,
    Mep,
}

pub const FAMS: [Fam; 4] = [Fam::Wall, Fam::Slab, Fam::Member, Fam::Mep];

impl Fam {
    pub fn label(self, ru: bool) -> &'static str {
        crate::tr_str(
            ru,
            match self {
                Fam::Wall => "type_dialog.fam.wall",
                Fam::Slab => "type_dialog.fam.slab",
                Fam::Member => "type_dialog.fam.member",
                Fam::Mep => "type_dialog.fam.mep",
            },
        )
    }
    /// Structure (layers) is editable only for walls; for floors it is view-only.
    pub fn layered(self) -> bool {
        matches!(self, Fam::Wall | Fam::Slab)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub pos: i64,
    pub material: String,
    /// Thickness as text in project units (before OK).
    pub thickness: String,
}

/// Type parameter table row: name, value, formula (none for system types), unit.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamLine {
    pub name: String,
    pub value: String,
    pub unit: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeDraft {
    pub fam: Fam,
    pub id: i64,
    pub name: String,
    pub layers: Vec<Layer>,
    orig_name: String,
    orig: Vec<(String, f64)>,
    pub structure: bool,
    pub error: Option<String>,
    /// Editable values: member — material and section dimensions; routes — size per group.
    pub vals: Vec<String>,
    porig: Vec<String>,
    /// Member: section kind; routes: groups (domain, shape, w, h).
    kind: String,
    groups: Vec<(String, String, f64, f64)>,
    /// Units the texts were loaded in (dirty detection compares numbers, not strings).
    u: Units,
    /// Another type asked for from outside (picker, browser) while this draft had edits.
    want: Option<(Fam, i64)>,
}

/// Opens `d` in the dialog slot. A draft with unsaved edits is never replaced from outside:
/// the request waits in `want` and the dialog asks the unsaved-changes question first.
pub fn open_in(slot: &mut Option<TypeDraft>, d: TypeDraft, m: &Model) {
    match slot {
        Some(cur) if cur.dirty(m) => {
            if (cur.fam, cur.id) != (d.fam, d.id) {
                cur.want = Some((d.fam, d.id));
            }
        }
        _ => *slot = Some(d),
    }
}

/// Most layers a type may have in the editor.
pub const MAX_LAYERS: usize = 24;
/// Thickness of a freshly inserted layer, in model units.
const NEW_LAYER_T: f64 = 0.01;

/// The "unsaved changes" rule and answers are shared with the library and style windows.
pub use crate::unsaved::{decide_switch, Choice, Switch};

/// Where the dialog goes once the question is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Type(Fam, i64),
    Close,
}

fn mep_groups(m: &Model, sys: i64) -> Vec<(String, String, f64, f64, i64)> {
    let Ok(mut st) = m.sql().prepare(
        "SELECT domain, shape, w, h, count(*) FROM mep_run WHERE system=?1 \
         GROUP BY domain, shape, w, h ORDER BY domain, w, h",
    ) else {
        return vec![];
    };
    st.query_map([sys], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .map(|it| it.flatten().collect())
    .unwrap_or_default()
}

/// Route size text in project units: "Ø200" or "300×200".
fn size_text(shape: &str, w: f64, h: f64, u: Units) -> String {
    if shape == "round" {
        format!("Ø{}", edit_length(w, u))
    } else {
        format!("{}×{}", edit_length(w, u), edit_length(h, u))
    }
}

/// Inverse of [`size_text`]: (w, h); for round sections h = w.
pub fn parse_size(s: &str, round: bool, u: Units) -> Option<(f64, f64)> {
    let s = s.trim().trim_start_matches(['Ø', 'ø']).trim();
    let ok = |x: &f64| x.is_finite() && *x > 0.0;
    if round {
        return parse_edit_length(s, u).filter(ok).map(|w| (w, w));
    }
    let (a, b) = s.split_once(['×', 'x', 'X', '*'])?;
    let w = parse_edit_length(a.trim(), u).filter(ok)?;
    let h = parse_edit_length(b.trim(), u).filter(ok)?;
    Some((w, h))
}

/// Family types: (id, name).
pub fn types_of(m: &Model, fam: Fam) -> Vec<(i64, String)> {
    let q = match fam {
        Fam::Wall => "SELECT id, name FROM wall_type ORDER BY id",
        Fam::Slab => "SELECT id, name FROM slab_type ORDER BY id",
        Fam::Member => "SELECT id, name FROM member_type ORDER BY id",
        Fam::Mep => "SELECT id, name FROM mep_system ORDER BY id",
    };
    let Ok(mut st) = m.sql().prepare(q) else {
        return vec![];
    };
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

fn layers_of(m: &Model, fam: Fam, id: i64) -> Vec<(i64, String, f64)> {
    let q = match fam {
        Fam::Wall => {
            "SELECT pos, material, thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos"
        }
        Fam::Slab => {
            "SELECT pos, material, thickness FROM slab_type_layer WHERE stype=?1 ORDER BY pos"
        }
        _ => return vec![],
    };
    let Ok(mut st) = m.sql().prepare(q) else {
        return vec![];
    };
    st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

/// Layer function by material: the thickest non-insulation layer is the structural one.
pub fn layer_function(layers: &[(String, f64)], i: usize, ru: bool) -> &'static str {
    let m = layers[i].0.to_lowercase();
    let core = layers
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.0.to_lowercase().contains("insul"))
        .max_by(|a, b| a.1 .1.total_cmp(&b.1 .1))
        .map(|(k, _)| k);
    crate::tr_str(
        ru,
        match (
            core == Some(i),
            m.contains("insul") || m.contains("\u{443}\u{442}\u{435}\u{43f}\u{43b}"),
        ) {
            (true, _) => "type_dialog.layer.structure",
            (_, true) => "type_dialog.layer.thermal",
            _ => "type_dialog.layer.finish",
        },
    )
}

impl TypeDraft {
    pub fn load(m: &Model, fam: Fam, id: i64, u: Units) -> Self {
        let name = types_of(m, fam)
            .into_iter()
            .find(|t| t.0 == id)
            .map(|t| t.1)
            .unwrap_or_default();
        let ls = layers_of(m, fam, id);
        let (mut vals, mut kind, mut groups) = (vec![], String::new(), vec![]);
        match fam {
            Fam::Member => {
                if let Ok((s, mat)) = m.sql().query_row(
                    "SELECT shape, material FROM member_type WHERE id=?1",
                    [id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                ) {
                    let mut it = s.split_whitespace();
                    kind = it.next().unwrap_or("").to_string();
                    vals.push(mat);
                    vals.extend(it.map(|x| edit_length(x.parse().unwrap_or(0.0), u)));
                }
            }
            Fam::Mep => {
                for (d, sh, w, h, _) in mep_groups(m, id) {
                    vals.push(size_text(&sh, w, h, u));
                    groups.push((d, sh, w, h));
                }
            }
            _ => {}
        }
        TypeDraft {
            porig: vals.clone(),
            vals,
            kind,
            groups,
            fam,
            id,
            orig_name: name.clone(),
            name,
            layers: ls
                .iter()
                .map(|(pos, mat, t)| Layer {
                    pos: *pos,
                    material: mat.clone(),
                    thickness: edit_length(*t, u),
                })
                .collect(),
            orig: ls.into_iter().map(|(_, m, t)| (m, t)).collect(),
            structure: false,
            error: None,
            u,
            want: None,
        }
    }

    /// True when the draft differs from what the model holds for this type now.
    pub fn dirty(&self, m: &Model) -> bool {
        if !self.editable() {
            return false;
        }
        let base = TypeDraft::load(m, self.fam, self.id, self.u);
        if self.name.trim() != base.name.trim()
            || self.layers.len() != base.layers.len()
            || self.vals.len() != base.vals.len()
        {
            return true;
        }
        let same_num = |a: &str, b: &str| {
            a.trim() == b.trim()
                || matches!(
                    (parse_edit_length(a, self.u), parse_edit_length(b, self.u)),
                    (Some(x), Some(y)) if (x - y).abs() < 1e-9
                )
        };
        let layers = self.layers.iter().zip(&base.layers).any(|(a, b)| {
            a.material.trim() != b.material.trim() || !same_num(&a.thickness, &b.thickness)
        });
        let vals = self.vals.iter().zip(&base.vals).enumerate().any(|(i, (a, b))| {
            // member material (first value) is text, every other value a length or size
            if self.fam == Fam::Member && i == 0 {
                a.trim() != b.trim()
            } else if self.fam == Fam::Member {
                !same_num(a, b)
            } else {
                a.trim() != b.trim()
            }
        });
        layers || vals
    }

    /// Insert a layer below `i` (same material, 10 mm); false when out of range or full.
    pub fn insert_layer(&mut self, i: usize) -> bool {
        if i >= self.layers.len() || self.layers.len() >= MAX_LAYERS {
            return false;
        }
        let l = Layer {
            pos: 0,
            material: self.layers[i].material.clone(),
            thickness: edit_length(NEW_LAYER_T, self.u),
        };
        self.layers.insert(i + 1, l);
        self.renumber();
        true
    }

    /// Remove layer `i`; the only layer stays (a type needs one).
    pub fn delete_layer(&mut self, i: usize) -> bool {
        if i >= self.layers.len() || self.layers.len() <= 1 {
            return false;
        }
        self.layers.remove(i);
        self.renumber();
        true
    }

    fn renumber(&mut self) {
        for (k, l) in self.layers.iter_mut().enumerate() {
            l.pos = k as i64;
        }
    }

    /// Why value `i` of `vals` cannot be applied (None: fine or not checked).
    pub fn val_problem(&self, i: usize, ru: bool) -> Option<String> {
        let v = self.vals.get(i)?;
        let bad = || crate::tr_str(ru, "type_layers.value.bad").to_string();
        match self.fam {
            Fam::Member if i == 0 => v
                .trim()
                .is_empty()
                .then(|| crate::tr_str(ru, "type_layers.value.required").to_string()),
            Fam::Member => parse_edit_length(v, self.u)
                .filter(|x| x.is_finite() && *x > 0.0)
                .is_none()
                .then(bad),
            Fam::Mep => {
                let round = self.groups.get(i).is_some_and(|g| g.1 == "round");
                parse_size(v, round, self.u).is_none().then(bad)
            }
            _ => None,
        }
    }

    /// Why layer `i` cannot be applied (None: fine).
    pub fn layer_problem(&self, i: usize, ru: bool) -> Option<String> {
        let l = self.layers.get(i)?;
        match parse_edit_length(&l.thickness, self.u) {
            Some(t) if t > 0.0 => None,
            _ => Some(crate::tr_str(ru, "type_layers.value.bad").to_string()),
        }
    }

    /// First family type (or an empty draft).
    pub fn first(m: &Model, fam: Fam, u: Units) -> Self {
        let id = types_of(m, fam).first().map_or(0, |t| t.0);
        Self::load(m, fam, id, u)
    }

    pub fn editable(&self) -> bool {
        self.id > 0
    }

    /// The type's name as the model holds it (the draft may be renaming it).
    pub fn stored_name(&self) -> &str {
        &self.orig_name
    }

    /// Index into `vals` for a parameter table row (`None` — read-only row).
    pub fn val_index(&self, row: usize) -> Option<usize> {
        let i = match self.fam {
            Fam::Member => row.checked_sub(1)?,
            Fam::Mep => row,
            _ => return None,
        };
        (i < self.vals.len()).then_some(i)
    }

    fn layer_specs(&self, u: Units, ru: bool) -> Result<Vec<LayerSpec>, String> {
        self.layers
            .iter()
            .map(|l| {
                let t = parse_edit_length(&l.thickness, u)
                    .filter(|t| *t > 0.0)
                    .ok_or_else(|| {
                        crate::tr_lang(ru, "type_dialog.typedial.bad_layer_thickness", &[("m", l.material.as_str())])
                    })?;
                let mat = l.material.trim();
                if mat.is_empty() {
                    return Err(crate::tr_str(ru, "type_dialog.typedial.empty_layer_material")
                    .into());
                }
                Ok(LayerSpec {
                    material: mat.into(),
                    thickness: t,
                })
            })
            .collect()
    }

    /// Model operation for floors, members and routes (`None` — no changes).
    pub fn op(&self, u: Units, ru: bool) -> Result<Option<OpFn>, String> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err(crate::tr_str(ru, "type_dialog.typedial.empty_type_name")
            .into());
        }
        let id = self.id;
        let bad = |what: &str| -> String {
            crate::tr_lang(ru, "type_dialog.typedial.bad_value", &[("what", what)])
        };
        let f: OpFn = match self.fam {
            // same layer count: per-layer edits (see `edits`); added or removed layers replace the set
            Fam::Wall if self.layers.len() == self.orig.len() => return Ok(None),
            Fam::Wall => {
                let ls = self.layer_specs(u, ru)?;
                OpFn(Arc::new(move |m: &mut Model| {
                    m.set_wall_type_layers(id, &name, &ls)
                }))
            }
            Fam::Slab => {
                let ls = self.layer_specs(u, ru)?;
                let same = name == self.orig_name
                    && ls.len() == self.orig.len()
                    && ls
                        .iter()
                        .zip(&self.orig)
                        .all(|(a, b)| a.material == b.0 && (a.thickness - b.1).abs() < 1e-9);
                if same {
                    return Ok(None);
                }
                OpFn(Arc::new(move |m: &mut Model| {
                    m.set_slab_type(id, &name, &ls)
                }))
            }
            Fam::Member => {
                if name == self.orig_name && self.vals == self.porig {
                    return Ok(None);
                }
                let mat = self
                    .vals
                    .first()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                if mat.is_empty() {
                    return Err(bad(crate::tr_str(ru, "type_dialog.typedial.material")));
                }
                let mut text = self.kind.clone();
                for v in self.vals.iter().skip(1) {
                    let x = parse_edit_length(v, u)
                        .filter(|x| x.is_finite() && *x > 0.0)
                        .ok_or_else(|| bad(v))?;
                    text.push_str(&format!(" {x}"));
                }
                let shape = MemberShape::parse(&text).ok_or_else(|| bad(&text))?;
                shape.check()?;
                OpFn(Arc::new(move |m: &mut Model| {
                    m.set_member_type(id, &name, &shape, &mat)
                }))
            }
            Fam::Mep => {
                let mut ch = vec![];
                for ((v, o), (d, sh, w, h)) in self.vals.iter().zip(&self.porig).zip(&self.groups) {
                    if v.trim() == o.trim() {
                        continue;
                    }
                    let (nw, nh) = parse_size(v, sh == "round", u).ok_or_else(|| bad(v))?;
                    ch.push(MepResize {
                        domain: d.clone(),
                        shape: sh.clone(),
                        w: *w,
                        h: *h,
                        nw,
                        nh,
                    });
                }
                if ch.is_empty() {
                    return Ok(None);
                }
                OpFn(Arc::new(move |m: &mut Model| {
                    m.set_mep_sizes(id, &ch).map(|_| ())
                }))
            }
        };
        Ok(Some(f))
    }

    /// Layer `i` swaps with its neighbour (`up` — toward the start).
    pub fn move_layer(&mut self, i: usize, up: bool) {
        let j = if up { i.checked_sub(1) } else { Some(i + 1) };
        if let Some(j) = j.filter(|&j| j < self.layers.len()) {
            // positions stay in place, the contents move
            let (a, b) = (self.layers[i].clone(), self.layers[j].clone());
            self.layers[i] = Layer { pos: a.pos, ..b };
            self.layers[j] = Layer { pos: b.pos, ..a };
        }
    }

    pub fn total(&self, u: Units) -> Option<f64> {
        self.layers
            .iter()
            .map(|l| parse_edit_length(&l.thickness, u))
            .sum()
    }

    /// Edit batch relative to the loaded type (empty — no changes).
    pub fn edits(&self, u: Units, ru: bool) -> Result<Vec<Edit>, String> {
        if self.fam != Fam::Wall || !self.editable() {
            return Ok(vec![]);
        }
        let mut out = vec![];
        let name = self.name.trim();
        if name.is_empty() {
            return Err(crate::tr_str(ru, "type_dialog.typedial.empty_type_name")
            .into());
        }
        if name != self.orig_name {
            out.push(Edit::WallType {
                id: self.id,
                name: name.into(),
            });
        }
        for (l, o) in self.layers.iter().zip(&self.orig) {
            let t = parse_edit_length(&l.thickness, u)
                .filter(|t| *t > 0.0)
                .ok_or_else(|| {
                    crate::tr_lang(ru, "type_dialog.typedial.bad_layer_thickness", &[("m", l.material.as_str())])
                })?;
            let mat = l.material.trim();
            if mat.is_empty() {
                return Err(crate::tr_str(ru, "type_dialog.typedial.empty_layer_material")
                .into());
            }
            if mat != o.0 || (t - o.1).abs() > 1e-9 {
                out.push(Edit::Layer {
                    wtype: self.id,
                    pos: l.pos,
                    layer: LayerSpec {
                        material: mat.into(),
                        thickness: t,
                    },
                });
            }
        }
        Ok(out)
    }

    /// Type parameters for the table.
    pub fn params(&self, m: &Model, u: Units, ru: bool) -> Vec<ParamLine> {
        let l = |n: &str, v: String, unit: &str| ParamLine {
            name: n.into(),
            value: v,
            unit: unit.into(),
        };
        let lu = crate::units::unit_label_in(u, ru);
        match self.fam {
            Fam::Wall | Fam::Slab => {
                let tot = self.total(u).map_or("—".into(), |t| edit_length(t, u));
                vec![
                    l(crate::tr_str(ru, "type_dialog.panels.thickness"), tot, lu),
                    l(
                        crate::tr_str(ru, "type_dialog.panels.layers"),
                        self.layers.len().to_string(),
                        "",
                    ),
                ]
            }
            Fam::Member => {
                let shape: Option<(String, String)> = m
                    .sql()
                    .query_row(
                        "SELECT shape, material FROM member_type WHERE id=?1",
                        [self.id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .ok();
                let Some((s, mat)) = shape else {
                    return vec![];
                };
                let mut it = s.split_whitespace();
                let kind = it.next().unwrap_or("");
                let names: &[&str] = match kind {
                    "rect" => &["b", "h"],
                    "circ" => &["d"],
                    "i" | "c" | "t" => &["h", "b", "tw", "tf"],
                    "l" | "tube" => &["h", "b", "t"],
                    "pipe" => &["d", "t"],
                    _ => &[],
                };
                let mut v = vec![
                    l(
                        crate::tr_str(ru, "type_dialog.typedial.section"),
                        kind.to_uppercase(),
                        "",
                    ),
                    l(crate::tr_str(ru, "type_dialog.typedial.material"), mat, ""),
                ];
                for (n, x) in names.iter().zip(it) {
                    let x: f64 = x.parse().unwrap_or(0.0);
                    v.push(l(n, edit_length(x, u), lu));
                }
                v
            }
            Fam::Mep => {
                let Ok(mut st) = m.sql().prepare(
                    "SELECT domain, shape, w, h, count(*) FROM mep_run WHERE system=?1 \
                     GROUP BY domain, shape, w, h ORDER BY domain, w, h",
                ) else {
                    return vec![];
                };
                let rows: Vec<(String, String, f64, f64, i64)> = st
                    .query_map([self.id], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })
                    .map(|it| it.flatten().collect())
                    .unwrap_or_default();
                rows.into_iter()
                    .map(|(d, s, w, h, n)| {
                        let size = if s == "round" {
                            format!("Ø{}", display_length(w, u))
                        } else {
                            format!("{}×{}", edit_length(w, u), display_length(h, u))
                        };
                        let dn = crate::tr_str(
                            ru,
                            match d.as_str() {
                                "duct" => "type_dialog.run.duct",
                                "pipe" => "type_dialog.run.pipe",
                                "cable_tray" => "type_dialog.run.cable_tray",
                                "conduit" => "type_dialog.run.conduit",
                                _ => "type_dialog.run.run",
                            },
                        );
                        l(dn, size, &format!("×{n}"))
                    })
                    .collect()
            }
        }
    }
}

/// Apply the draft: one model operation; the draft is reloaded from the model.
pub fn apply(ed: &mut Editor, d: &mut TypeDraft, ru: bool) -> bool {
    let u = ed.tool.units;
    if d.fam != Fam::Wall || d.layers.len() != d.orig.len() {
        let r = d.op(u, ru).and_then(|f| match f {
            None => Ok(false),
            Some(f) => ed.run_op(f).map(|_| true).map_err(|e| e.to_string()),
        });
        return match r {
            Ok(changed) => {
                if changed {
                    ed.status = Some(crate::tr_lang(ru, "type_dialog.typedial.type_changed", &[("name", d.name.trim())]));
                    let st = d.structure;
                    *d = TypeDraft::load(&ed.model, d.fam, d.id, u);
                    d.structure = st;
                }
                d.error = None;
                true
            }
            Err(e) => {
                d.error = Some(e);
                false
            }
        };
    }
    match d.edits(u, ru) {
        Ok(e) if e.is_empty() => {
            d.error = None;
            true
        }
        Ok(e) => match ed.apply_xlsx(e, "") {
            Ok(()) => {
                ed.status = Some(crate::tr_lang(ru, "type_dialog.typedial.type_changed", &[("name", d.name.trim())]));
                *d = TypeDraft::load(&ed.model, d.fam, d.id, u);
                true
            }
            Err(e) => {
                d.error = Some(e);
                false
            }
        },
        Err(e) => {
            d.error = Some(e);
            false
        }
    }
}

/// Built-in material ids offered by the layer picker (names: i18n `mat.<id>`).
pub const MATERIALS: [&str; 12] = [
    "concrete",
    "masonry",
    "brick",
    "insulation",
    "plaster",
    "render",
    "gypsum",
    "glass",
    "wood",
    "steel",
    "membrane",
    "other",
];

/// Localized material name; unknown (user) ids are shown as typed.
pub fn material_label(cat: &ddc_ui::Catalog, lang: ddc_ui::Lang, id: &str) -> String {
    let key = format!("mat.{id}");
    let s = cat.t_in(lang, &key);
    if s == key {
        id.to_owned()
    } else {
        s
    }
}

fn mat_color(th: &Theme, m: &str) -> egui::Color32 {
    let k = crate::section_view::mat_class(m);
    th.c(&format!("canvas.mat.{}", crate::ui_kit::MAT_CLASSES[k]))
}

/// Preview: cross-section of layers across the wall (thicknesses to scale).
fn preview(ui: &mut egui::Ui, th: &Theme, d: &TypeDraft, u: Units) {
    let (r, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 64.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, th.radius("sm"), th.c("canvas.paper"));
    let ts: Vec<f64> = d
        .layers
        .iter()
        .map(|l| parse_edit_length(&l.thickness, u).unwrap_or(0.0).max(0.0))
        .collect();
    let tot: f64 = ts.iter().sum();
    if tot <= 0.0 {
        return;
    }
    let inner = r.shrink2(egui::vec2(12.0, 10.0));
    let mut x = inner.left();
    for (l, t) in d.layers.iter().zip(&ts) {
        let w = (*t / tot) as f32 * inner.width();
        let band =
            egui::Rect::from_min_size(egui::pos2(x, inner.top()), egui::vec2(w, inner.height()));
        p.rect_filled(band, 0.0, mat_color(th, &l.material));
        p.rect_stroke(
            band,
            0.0,
            egui::Stroke::new(1.0, th.c("canvas.cut")),
            egui::StrokeKind::Inside,
        );
        x += w;
    }
}

fn caption(ui: &mut egui::Ui, th: &Theme, s: &str) {
    ui.label(
        RichText::new(s)
            .font(th.font(Role::Label))
            .color(th.c("text.secondary")),
    );
}

/// Longest type name shown in the window title.
const TITLE_NAME_MAX: usize = 40;

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('\u{2026}');
    t
}

fn has_digit(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
}

/// Width of the shared key column: the widest key of the language, within the kit limits.
fn key_width(ui: &egui::Ui, th: &Theme, keys: &[(&str, Role)]) -> f32 {
    let w = keys
        .iter()
        .map(|(k, role)| {
            // measured only, never drawn: the placeholder colour
            let c = egui::Color32::PLACEHOLDER;
            ui.fonts(|f| f.layout_no_wrap((*k).to_owned(), th.font(*role), c).size().x)
        })
        .fold(0.0_f32, f32::max);
    // a little air so the widest key is never cut by rounding
    (w + 1.0).clamp(layout::PROP_KEY_MIN, layout::PROP_KEY_MAX)
}

/// Key cell of the shared grid: one line in a fixed width, full text on hover when cut.
fn key_cell(ui: &mut egui::Ui, th: &Theme, role: Role, text: &str, w: f32) {
    let color = if role == Role::Label {
        th.c("text.secondary")
    } else {
        ui.visuals().text_color()
    };
    let r = ui.allocate_ui_with_layout(
        egui::vec2(w, th.d().control),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(w);
            widgets::elided_label(ui, text, th.font(role), color);
        },
    );
    note_cell(ui, text, r.response.rect, 'k');
}

/// Read-only value in the editable fields' geometry: number right-aligned at the field's text
/// edge, muted unit after it (text without digits stays left). Returns the cell rect.
fn readonly_value(ui: &mut egui::Ui, th: &Theme, w: f32, value: &str, unit: &str) -> egui::Rect {
    let d = th.d();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, d.control), egui::Sense::hover());
    let muted = th.c("text.secondary");
    let color = ui.visuals().text_color();
    let mut right = rect.right() - d.pad_x;
    if !unit.is_empty() {
        let g = ui.painter().layout_no_wrap(unit.to_owned(), th.font(Role::Body), muted);
        let at = egui::pos2(right - g.size().x, rect.center().y - g.size().y / 2.0);
        right = at.x - space::XS;
        ui.painter().galley(at, g, muted);
    }
    let numeric = has_digit(value);
    let font = th.font(if numeric { Role::Numeric } else { Role::Body });
    let g = widgets::elide(ui, value, font, color, (right - rect.left() - d.pad_x).max(1.0));
    let x = if numeric { right - g.size().x } else { rect.left() + d.pad_x };
    ui.painter().galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, color);
    rect
}

/// Test hook: (key, rect, kind) of every cell drawn since the log was cleared. Kinds: k key,
/// h value header, d dropdown, t text field, e numeric field (right-aligned), r read-only
/// number, l read-only text.
#[cfg(test)]
pub(crate) type CellRow = (String, egui::Rect, char);

#[cfg(test)]
pub(crate) fn cell_log_id() -> egui::Id {
    egui::Id::new("ddc.td.cells")
}

#[cfg(test)]
fn note_cell(ui: &egui::Ui, key: &str, rect: egui::Rect, kind: char) {
    ui.ctx().data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<CellRow>>(cell_log_id()).push((key.to_owned(), rect, kind))
    });
}

#[cfg(not(test))]
fn note_cell(_ui: &egui::Ui, _key: &str, _rect: egui::Rect, _kind: char) {}

/// Dialog window; `false` — close.
pub fn window(
    ctx: &egui::Context,
    th: &Theme,
    cat: &ddc_ui::Catalog,
    ru: bool,
    ed: &mut Editor,
    d: &mut TypeDraft,
) -> bool {
    let u = ed.tool.units;
    let mut keep = true;
    let mut open = true;
    let mut switch: Option<Target> = None;
    let mut close = false;
    let (mut ok, mut apply_now, mut dup) = (false, false, false);
    // the full name stays in the Name field below; a long one must not widen the title bar
    let title = format!(
        "{} — {}: {}",
        crate::tr_str(ru, "type_dialog.typedial.type_properties"),
        d.fam.label(ru),
        shorten(&d.name, TITLE_NAME_MAX)
    );
    kit::Dialog::new("type_properties", title, kit::DialogSize::Wide)
        .open(&mut open)
        .show(ctx, |ui| {
            let fam_items: Vec<&str> = FAMS.iter().map(|f| f.label(ru)).collect();
            let types = types_of(&ed.model, d.fam);
            let type_items: Vec<&str> = types.iter().map(|t| t.1.as_str()).collect();
            let vw = layout::PROP_VALUE * 2.0;
            let lu = crate::units::unit_label_in(u, ru);
            let rows = d.params(&ed.model, u, ru);
            let (l_fam, l_type, l_name) = (
                crate::tr_str(ru, "type_dialog.typedial.family"),
                crate::tr_str(ru, "type_dialog.panels.type"),
                crate::tr_str(ru, "type_dialog.typedial.name"),
            );
            let (h_par, h_val) = (
                crate::tr_str(ru, "type_dialog.typedial.h_param"),
                crate::tr_str(ru, "type_dialog.typedial.h_value"),
            );
            let mut keys: Vec<(&str, Role)> =
                [l_fam, l_type, l_name, h_par].into_iter().map(|k| (k, Role::Label)).collect();
            keys.extend(rows.iter().map(|p| (p.name.as_str(), Role::Body)));
            let kw = key_width(ui, th, &keys);
            // one grid for the head and the parameters: keys, values and the Duplicate button
            // each keep one column, so every row starts and ends at the same x
            egui::Grid::new("c.td.grid")
                .num_columns(3)
                .min_col_width(0.0)
                .min_row_height(th.d().control)
                .spacing([space::MD, space::SM])
                .show(ui, |ui| {
                    key_cell(ui, th, Role::Label, l_fam, kw);
                    let mut fi = FAMS.iter().position(|f| *f == d.fam).unwrap_or(0);
                    let r = widgets::dropdown(ui, th, "c.td.fam", &mut fi, &fam_items, Some(vw));
                    note_cell(ui, l_fam, r.rect, 'd');
                    if r.changed() {
                        switch = Some(Target::Type(FAMS[fi], -1));
                    }
                    ui.end_row();
                    key_cell(ui, th, Role::Label, l_type, kw);
                    let mut ti = types.iter().position(|t| t.0 == d.id).unwrap_or(0);
                    let r = widgets::dropdown(ui, th, "c.td.type", &mut ti, &type_items, Some(vw));
                    note_cell(ui, l_type, r.rect, 'd');
                    if r.changed() {
                        switch = Some(Target::Type(d.fam, types[ti].0));
                    }
                    let hook = crate::tr_str(ru, "type_dialog.typedial.systems_are_created_in");
                    let en = d.editable() && d.fam != Fam::Mep;
                    let r = ui
                        .add_enabled_ui(en, |ui| {
                            widgets::Button::secondary(crate::tr_str(ru, "type_dialog.typedial.duplicate"))
                                .show(ui, th)
                        })
                        .inner;
                    if r.on_disabled_hover_text(hook).clicked() {
                        dup = true;
                    }
                    ui.end_row();
                    key_cell(ui, th, Role::Label, l_name, kw);
                    let out = ui
                        .add_enabled_ui(d.editable(), |ui| {
                            widgets::TextField::new(&mut d.name)
                                .id_salt("c.td.name")
                                .width(vw)
                                .labelled_by(egui::Id::new("c.td.name.lbl"), l_name)
                                .show(ui, th)
                        })
                        .inner;
                    note_cell(ui, l_name, out.rect, 't');
                    ui.end_row();
                    // a grid cannot advance its cursor: the gap is a cell of its own
                    ui.allocate_space(egui::vec2(0.0, space::XS));
                    ui.end_row();
                    key_cell(ui, th, Role::Label, h_par, kw);
                    let (hr, _) = ui.allocate_exact_size(
                        egui::vec2(vw, th.d().control),
                        egui::Sense::hover(),
                    );
                    let hg = widgets::elide(
                        ui,
                        h_val,
                        th.font(Role::Label),
                        th.c("text.secondary"),
                        vw - 2.0 * th.d().pad_x,
                    );
                    let at = egui::pos2(
                        hr.right() - th.d().pad_x - hg.size().x,
                        hr.center().y - hg.size().y / 2.0,
                    );
                    ui.painter().galley(at, hg, th.c("text.secondary"));
                    note_cell(ui, h_val, hr, 'h');
                    ui.end_row();
                    for (row, p) in rows.into_iter().enumerate() {
                        key_cell(ui, th, Role::Body, &p.name, kw);
                        match d.val_index(row) {
                            Some(i) => {
                                let problem = d.val_problem(i, ru);
                                // member material is plain text; the rest are lengths or sizes
                                let text = d.fam == Fam::Member && i == 0;
                                let unit = if text {
                                    ""
                                } else if d.fam == Fam::Member {
                                    lu
                                } else {
                                    p.unit.as_str()
                                };
                                ui.vertical(|ui| {
                                    ui.set_max_width(vw);
                                    let mut f = widgets::TextField::new(&mut d.vals[i])
                                        .id_salt(("c.td.val", i))
                                        .width(vw)
                                        .unit(unit)
                                        .error(problem.is_some())
                                        .labelled_by(egui::Id::new(("c.td.val.lbl", i)), &p.name);
                                    if !text {
                                        f = f.right_aligned();
                                    }
                                    let out = f.show(ui, th);
                                    note_cell(ui, &p.name, out.rect, if text { 't' } else { 'e' });
                                    if let Some(msg) = &problem {
                                        problem_note(ui, th, msg);
                                    }
                                });
                            }
                            None => {
                                let (n, un) = match kit::split_unit(&p.value) {
                                    (n, un) if p.unit.is_empty() && !un.is_empty() => (n, un),
                                    _ => (p.value.as_str(), p.unit.as_str()),
                                };
                                let r = readonly_value(ui, th, vw, n, un);
                                note_cell(ui, &p.name, r, if has_digit(n) { 'r' } else { 'l' });
                            }
                        }
                        ui.end_row();
                    }
                });
            if d.fam.layered() {
                ui.add_space(space::SM);
                let mut open_s = d.structure;
                widgets::section_header(ui, th, crate::tr_str(ru, "type_dialog.typedial.structure"), &mut open_s);
                d.structure = open_s;
                if d.structure {
                    let extra = crate::view_gfx_ui::project_materials(ed);
                    structure(ui, th, cat, d, ru, &extra, u);
                }
                ui.add_space(space::SM);
                caption(ui, th, crate::tr_str(ru, "type_dialog.typedial.preview"));
                preview(ui, th, d, u);
            }
            if let Some(e) = &d.error {
                kit::error_note(ui, e);
            }
            kit::dialog_footer(ui, |ui| {
                ok = kit::primary_action_table(ui, "OK", None);
                if kit::cancel_button(ui, crate::tr_str(ru, "type_dialog.typedial.cancel")).clicked() {
                    close = true;
                }
                let r = ui
                    .add_enabled_ui(d.editable(), |ui| {
                        widgets::Button::secondary(crate::tr_str(ru, "type_dialog.typedial.apply")).show(ui, th)
                    })
                    .inner;
                apply_now = r
                    .on_disabled_hover_text(crate::tr_str(ru, "type_dialog.typedial.this_type_cannot_be"))
                    .clicked();
            });
        });
    if !open {
        close = true;
    }
    if ok || apply_now {
        let done = apply(ed, d, ru);
        if ok && done {
            keep = false;
        }
    }
    if dup {
        // the copy is made from the saved type, so unsaved edits are settled first
        if let Some(id) = duplicate(ed, d, ru) {
            switch = Some(Target::Type(d.fam, id));
        }
    }
    if switch.is_none() {
        if let Some((f, id)) = d.want.take() {
            switch = Some(Target::Type(f, id));
        }
    }
    if close && keep {
        switch = Some(Target::Close);
    }
    if !keep {
        set_pending(ctx, None);
        return false;
    }
    // an open question keeps being asked until it is answered
    if let Some(t) = switch.or_else(|| pending(ctx)) {
        keep = leave(ctx, th, ru, ed, d, t, u);
    }
    keep
}

fn pending_id() -> egui::Id {
    egui::Id::new("ddc.ux48a.pending")
}

fn pending(ctx: &egui::Context) -> Option<Target> {
    ctx.data(|d| d.get_temp::<Target>(pending_id()))
}

fn set_pending(ctx: &egui::Context, t: Option<Target>) {
    ctx.data_mut(|d| {
        match t {
            Some(t) => d.insert_temp(pending_id(), t),
            None => {
                d.remove::<Target>(pending_id());
            }
        };
    });
}

/// Moves to `t` (another type, or closing). Unsaved edits are never dropped silently: the
/// question is asked first and the draft stays as it is until it is answered. Returns `keep`.
fn leave(
    ctx: &egui::Context,
    _th: &Theme,
    ru: bool,
    ed: &mut Editor,
    d: &mut TypeDraft,
    t: Target,
    u: Units,
) -> bool {
    // Esc that opened the question must not answer it in the same frame
    let asked_before = pending(ctx).is_some();
    let dirty = d.dirty(&ed.model);
    let choice = if dirty {
        ask(ctx, ru, d, asked_before)
    } else {
        None
    };
    let go = match decide_switch(dirty, choice) {
        Switch::Now | Switch::DiscardThenGo => true,
        Switch::ApplyThenGo => apply(ed, d, ru),
        Switch::Ask => {
            set_pending(ctx, Some(t));
            return true;
        }
        Switch::Stay => false,
    };
    set_pending(ctx, None);
    if !go {
        return true;
    }
    match t {
        Target::Close => false,
        Target::Type(f, id) => {
            *d = if id < 0 {
                TypeDraft::first(&ed.model, f, u)
            } else {
                TypeDraft::load(&ed.model, f, id, u)
            };
            d.structure = f.layered();
            true
        }
    }
}

/// The "Unsaved changes" question; Esc is Cancel. None while unanswered.
fn ask(ctx: &egui::Context, ru: bool, d: &TypeDraft, esc_cancels: bool) -> Option<Choice> {
    crate::unsaved::ask(
        ctx,
        ru,
        "type_layers.unsaved",
        d.name.trim(),
        &crate::tr_str(ru, "type_layers.unsaved.body"),
        &crate::tr_str(ru, "type_dialog.typedial.apply"),
        esc_cancels,
    )
}

fn problem_note(ui: &mut egui::Ui, th: &Theme, s: &str) {
    ui.label(
        RichText::new(s)
            .font(th.font(Role::Caption))
            .color(ui.visuals().error_fg_color),
    );
}

/// Copy of the current type with the first free name "<name> 2", "<name> 3"…; returns the copy's id.
pub fn duplicate(ed: &mut Editor, d: &mut TypeDraft, ru: bool) -> Option<i64> {
    let t = match d.fam {
        Fam::Wall => TypeTable::Wall,
        Fam::Slab => TypeTable::Slab,
        Fam::Member => TypeTable::Member,
        Fam::Mep => return None,
    };
    let taken: Vec<String> = types_of(&ed.model, d.fam)
        .into_iter()
        .map(|t| t.1)
        .collect();
    let base = d.orig_name.clone();
    let name = (2..)
        .map(|k| format!("{base} {k}"))
        .find(|n| !taken.contains(n))?;
    let (id, nm) = (d.id, name.clone());
    let f = OpFn(Arc::new(move |m: &mut Model| {
        m.duplicate_type(t, id, &nm).map(|_| ())
    }));
    match ed.run_op(f) {
        Ok(()) => {
            ed.status = Some(crate::tr_lang(ru, "type_dialog.typedial.type_created", &[("name", name.as_str())]));
            types_of(&ed.model, d.fam)
                .into_iter()
                .find(|t| t.1 == name)
                .map(|t| t.0)
        }
        Err(e) => {
            d.error = Some(e.to_string());
            None
        }
    }
}

fn structure(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &ddc_ui::Catalog,
    d: &mut TypeDraft,
    ru: bool,
    extra: &[String],
    u: Units,
) {
    let edit = d.editable();
    let mut mv: Option<(usize, bool)> = None;
    let mut ins: Option<usize> = None;
    let mut del: Option<usize> = None;
    let n = d.layers.len();
    let problems: Vec<Option<String>> = (0..n).map(|i| d.layer_problem(i, ru)).collect();
    let cur: Vec<(String, f64)> = d
        .layers
        .iter()
        .map(|l| (l.material.clone(), parse_edit_length(&l.thickness, u).unwrap_or(0.0)))
        .collect();
    let lu = crate::units::unit_label_in(u, ru);
    let avail = ui.available_width();
    egui::Frame::new()
        .fill(th.c("bg.sunken"))
        .corner_radius(th.radius("md"))
        .inner_margin(egui::Margin::same(space::SM as i8))
        .show(ui, |ui| {
            ui.set_min_width(avail - 2.0 * space::SM);
            // the material column takes what the fixed columns leave, never under 170 px
            let mat_w = (ui.available_width() - 360.0).max(170.0);
            let lang = if ru {
                ddc_ui::Lang::Ru
            } else {
                ddc_ui::Lang::En
            };
            egui::Grid::new("c.td.layers")
                .num_columns(5)
                .spacing([space::MD, space::XS])
                .show(ui, |ui| {
                    for h in [
                        crate::tr_str(ru, "type_dialog.typedial.h_num"),
                        crate::tr_str(ru, "type_dialog.typedial.h_function"),
                        crate::tr_str(ru, "type_dialog.typedial.h_material"),
                        crate::tr_str(ru, "type_dialog.typedial.h_thickness"),
                        "",
                    ] {
                        caption(ui, th, h);
                    }
                    ui.end_row();
                    for (i, l) in d.layers.iter_mut().enumerate() {
                        ui.label(format!("{}", i + 1));
                        ui.label(layer_function(&cur, i, ru));
                        ui.vertical(|ui| {
                            let mut ids: Vec<String> =
                                MATERIALS.iter().map(|m| (*m).to_owned()).collect();
                            for m in extra {
                                if !ids.contains(m) {
                                    ids.push(m.clone());
                                }
                            }
                            let labels: Vec<String> =
                                ids.iter().map(|m| material_label(cat, lang, m)).collect();
                            let other = cat.t("td.mat.custom");
                            let mut items: Vec<&str> = labels.iter().map(String::as_str).collect();
                            items.push(other.as_str());
                            let other_ix = ids.len();
                            // free-text material: chosen with "Other", or any id the list lacks
                            let flag = egui::Id::new(("c.td.mat.custom", i));
                            let custom = ui.data(|dd| dd.get_temp::<bool>(flag)).unwrap_or(false)
                                || !ids.contains(&l.material);
                            let mut sel = if custom {
                                other_ix
                            } else {
                                ids.iter().position(|m| *m == l.material).unwrap_or(0)
                            };
                            ui.horizontal(|ui| {
                                let (sw, _) = ui.allocate_exact_size(
                                    egui::vec2(space::MD, space::MD),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    sw,
                                    ddc_ui::theme::radius::SM,
                                    mat_color(th, &l.material),
                                );
                                let r = ui
                                    .add_enabled_ui(edit, |ui| {
                                        widgets::dropdown(
                                            ui,
                                            th,
                                            ("c.td.mat", i),
                                            &mut sel,
                                            &items,
                                            Some(mat_w),
                                        )
                                    })
                                    .inner;
                                if r.changed() {
                                    let to_custom = sel == other_ix;
                                    ui.data_mut(|dd| dd.insert_temp(flag, to_custom));
                                    if let Some(id) = ids.get(sel) {
                                        l.material = id.clone();
                                    }
                                }
                            });
                            if custom {
                                ui.add_enabled_ui(edit, |ui| {
                                    widgets::TextField::new(&mut l.material)
                                        .id_salt(("c.td.mat.text", i))
                                        .width(mat_w)
                                        .labelled_by(
                                            egui::Id::new(("c.td.mat.lbl", i)),
                                            crate::tr_str(ru, "type_dialog.typedial.h_material"),
                                        )
                                        .show(ui, th);
                                });
                            }
                        });
                        ui.vertical(|ui| {
                            ui.add_enabled_ui(edit, |ui| {
                                widgets::TextField::new(&mut l.thickness)
                                    .id_salt(("c.td.th", i))
                                    .width(layout::PROP_VALUE)
                                    .unit(lu)
                                    .right_aligned()
                                    .error(problems[i].is_some())
                                    .labelled_by(
                                        egui::Id::new(("c.td.th.lbl", i)),
                                        crate::tr_str(ru, "type_dialog.typedial.h_thickness"),
                                    )
                                    .show(ui, th);
                            });
                            if let Some(msg) = &problems[i] {
                                problem_note(ui, th, msg);
                            }
                        });
                        ui.horizontal(|ui| {
                            if widgets::glyph_button(ui, "move-up", edit && i > 0, &cat.t("rebar_list.move_up"))
                                .clicked()
                            {
                                mv = Some((i, true));
                            }
                            if widgets::glyph_button(ui, "move-down", edit && i + 1 < n, &cat.t("rebar_list.move_down"))
                                .clicked()
                            {
                                mv = Some((i, false));
                            }
                            let can_ins = edit && n < MAX_LAYERS;
                            if widgets::glyph_button(ui, "plus", can_ins, &cat.t("type_layers.layer.insert"))
                                .clicked()
                            {
                                ins = Some(i);
                            }
                            let del_tip = if n > 1 {
                                cat.t("type_layers.layer.delete")
                            } else {
                                cat.t("type_layers.layer.delete_last")
                            };
                            if widgets::glyph_button(ui, "delete", edit && n > 1, &del_tip).clicked() {
                                del = Some(i);
                            }
                        });
                        ui.end_row();
                    }
                });
            if !edit {
                ui.label(
                    RichText::new(crate::tr_str(ru, "type_dialog.typedial.layers_of_this_family"))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
                );
            }
        });
    if let Some((i, up)) = mv {
        d.move_layer(i, up);
    }
    if let Some(i) = ins {
        d.insert_layer(i);
    }
    if let Some(i) = del {
        d.delete_layer(i);
    }
}

/// Type shown by `layout_probe`.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Case {
    Wall,
    WallLong,
    Member,
    MemberLong,
}

/// The dialog drawn headless in an explicit language: the layout probe and the cells of the
/// last frame.
#[cfg(test)]
pub(crate) fn layout_probe(ru: bool, case: Case) -> (crate::layout_contract::Probe, Vec<CellRow>) {
    let mut ed = Editor::new_in_memory().unwrap();
    let u = ed.tool.units;
    let long = matches!(case, Case::WallLong | Case::MemberLong);
    let name = if long { "Long type name ".repeat(8) } else { "Type A".to_owned() };
    let (fam, id) = match case {
        Case::Wall | Case::WallLong => {
            let lay = LayerSpec { material: "Brick".into(), thickness: 0.2 };
            (Fam::Wall, ed.model.add_wall_type(&name, &[lay]).unwrap())
        }
        Case::Member | Case::MemberLong => {
            let shape = MemberShape::Rect { b: 0.4, h: 0.4 };
            (Fam::Member, ed.model.add_member_type(&name, &shape, "Concrete").unwrap())
        }
    };
    let mut d = TypeDraft::load(&ed.model, fam, id, u);
    let th = Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let cat = ddc_ui::Catalog::new(if ru { ddc_ui::Lang::Ru } else { ddc_ui::Lang::En });
    let mut cells = Vec::new();
    let p = crate::layout_contract::probe("type_properties", |ctx| {
        ctx.data_mut(|m| m.remove::<Vec<CellRow>>(cell_log_id()));
        window(ctx, &th, &cat, ru, &mut ed, &mut d);
        cells = ctx.data(|m| m.get_temp::<Vec<CellRow>>(cell_log_id())).unwrap_or_default();
    });
    (p, cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed() -> Editor {
        Editor::new_in_memory().unwrap()
    }

    const CASES: [Case; 4] = [Case::Wall, Case::WallLong, Case::Member, Case::MemberLong];

    fn of(cells: &[CellRow], kind: char) -> Vec<&CellRow> {
        cells.iter().filter(|c| c.2 == kind).collect()
    }

    #[test]
    fn keys_and_values_share_one_grid_in_both_languages() {
        for ru in [false, true] {
            for case in CASES {
                let (_, cells) = layout_probe(ru, case);
                let tag = format!("{case:?} ru={ru}");
                let keys = of(&cells, 'k');
                assert!(keys.len() >= 6, "{tag}: {} key cells", keys.len());
                let (kl, kr) = (keys[0].1.left(), keys[0].1.right());
                for k in &keys {
                    assert!((k.1.left() - kl).abs() < 0.5, "{tag}: key {:?} left", k.0);
                    assert!((k.1.right() - kr).abs() < 0.5, "{tag}: key {:?} width", k.0);
                }
                // every value cell (dropdown, field, read-only, header) has one left and right x
                let vals: Vec<&CellRow> = cells.iter().filter(|c| c.2 != 'k').collect();
                assert!(vals.len() >= 6, "{tag}: {} value cells", vals.len());
                let (vl, vr) = (vals[0].1.left(), vals[0].1.right());
                for v in &vals {
                    assert!((v.1.left() - vl).abs() < 0.5, "{tag}: value {:?} left {} vs {vl}", v.0, v.1.left());
                    assert!((v.1.right() - vr).abs() < 0.5, "{tag}: value {:?} right {} vs {vr}", v.0, v.1.right());
                }
                assert!(vl > kr, "{tag}: values start right of the keys");
            }
        }
    }

    #[test]
    fn numbers_are_right_aligned_and_text_is_not() {
        for ru in [false, true] {
            let (_, wall) = layout_probe(ru, Case::Wall);
            assert!(!of(&wall, 'r').is_empty(), "wall: read-only numbers");
            assert!(of(&wall, 'e').is_empty(), "wall has no editable numbers");
            let (_, member) = layout_probe(ru, Case::Member);
            // section (text) read-only, material (text) editable, b and h numeric
            assert_eq!(of(&member, 'l').len(), 1, "member: read-only text");
            assert_eq!(of(&member, 't').len(), 2, "member: name and material are text fields");
            assert!(of(&member, 'e').len() >= 2, "member: b and h are right-aligned fields");
        }
    }

    #[test]
    fn long_names_keep_the_window_and_footer_inside() {
        for ru in [false, true] {
            for case in [Case::WallLong, Case::MemberLong] {
                let (p, cells) = layout_probe(ru, case);
                crate::layout_contract::assert_contract(&format!("{case:?} ru={ru}"), &p, false);
                let win = p.inner.expect("window drawn");
                assert!(
                    win.width() <= layout::DIALOG_WIDE + 40.0,
                    "{case:?}: a long name widened the window to {}",
                    win.width()
                );
                let title = shorten(&"Long type name ".repeat(8), TITLE_NAME_MAX);
                assert_eq!(title.chars().count(), TITLE_NAME_MAX);
                assert!(title.ends_with('\u{2026}'));
                assert!(cells.iter().all(|c| c.1.right() <= win.right() + 0.5), "{case:?}: cell past the window");
            }
        }
    }

    #[test]
    fn short_name_is_not_cut() {
        assert_eq!(shorten("Type A", TITLE_NAME_MAX), "Type A");
    }

    #[test]
    fn dialog_source_has_no_literal_size() {
        let src = include_str!("type_dialog.rs");
        let body = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        for bad in [".size([", ".min_size(["] {
            assert!(!body.contains(bad), "type dialog sets {bad}..: use DialogSize");
        }
        assert!(body.contains("DialogSize::Wide"));
    }

    #[test]
    fn tests_use_the_kit_for_units() {
        assert_eq!(kit::split_unit("380 mm"), ("380", "mm"));
    }

    #[test]
    fn swap_and_rename_is_one_undo_step() {
        let mut e = ed();
        let u = e.tool.units;
        let id = types_of(&e.model, Fam::Wall)
            .into_iter()
            .find(|t| t.1 == crate::app2_fx::fx("td_exterior_380"))
            .unwrap()
            .0;
        let mut d = TypeDraft::load(&e.model, Fam::Wall, id, u);
        assert_eq!(d.layers.len(), 4);
        d.move_layer(1, true);
        d.layers[3].thickness = edit_length(0.02, u);
        d.name = "Exterior 390".into();
        let edits = d.edits(u, true).unwrap();
        assert_eq!(
            edits.len(),
            4,
            "rename + 2 swapped + 1 thickness: {edits:?}"
        );
        assert!(apply(&mut e, &mut d, true));
        let after = layers_of(&e.model, Fam::Wall, id);
        assert_eq!(after[0].1, "masonry");
        assert_eq!(after[1].1, "plaster");
        assert!((after[3].2 - 0.02).abs() < 1e-9);
        assert_eq!(d.name, "Exterior 390");
        let t = e.types.iter().find(|t| t.id == id).unwrap();
        assert!((t.thickness() - 0.39).abs() < 1e-9, "{}", t.thickness());
        e.undo();
        let back = layers_of(&e.model, Fam::Wall, id);
        assert_eq!(back[0].1, "plaster");
        assert!((back[3].2 - 0.01).abs() < 1e-9);
        assert!(types_of(&e.model, Fam::Wall)
            .iter()
            .any(|t| t.1 == crate::app2_fx::fx("td_exterior_380")));
        assert!(!e.can_undo());
        e.redo();
        assert_eq!(layers_of(&e.model, Fam::Wall, id)[0].1, "masonry");
    }

    #[test]
    fn bad_input_is_rejected_without_model_change() {
        let mut e = ed();
        let u = e.tool.units;
        let mut d = TypeDraft::first(&e.model, Fam::Wall, u);
        d.layers[0].thickness = "abc".into();
        assert!(!apply(&mut e, &mut d, true));
        assert!(d.error.as_deref().unwrap().contains(&crate::app2_fx::fx("td_thickness")));
        assert!(!e.can_undo());
        let mut d = TypeDraft::first(&e.model, Fam::Wall, u);
        assert!(d.edits(u, true).unwrap().is_empty());
        d.name = "  ".into();
        assert!(d.edits(u, true).is_err());
    }

    #[test]
    fn layer_functions() {
        let ls: Vec<(String, f64)> = [("plaster", 0.02), ("masonry", 0.25), ("insulation", 0.1)]
            .iter()
            .map(|(m, t)| (m.to_string(), *t))
            .collect();
        assert_eq!(layer_function(&ls, 1, true), crate::app2_fx::fx("td_structural"));
        assert_eq!(layer_function(&ls, 2, true), crate::app2_fx::fx("td_insulation"));
        assert_eq!(layer_function(&ls, 0, false), "Finish");
    }

    #[test]
    fn member_and_mep_params() {
        let mut e = ed();
        let u = e.tool.units;
        let mid = e
            .model
            .add_member_type(
                "Column 400×400",
                &ddc_model::MemberShape::Rect { b: 0.4, h: 0.4 },
                "Concrete",
            )
            .unwrap();
        let d = TypeDraft::load(&e.model, Fam::Member, mid, u);
        let p = d.params(&e.model, u, true);
        assert_eq!(p[0].value, "RECT");
        assert_eq!(p.len(), 4);
        assert!(d.edits(u, true).unwrap().is_empty());
        assert!(d.op(u, true).unwrap().is_none());
    }

    #[test]
    fn member_profile_edit_and_duplicate_undo_redo() {
        let mut e = ed();
        let u = e.tool.units;
        let mid = e
            .model
            .add_member_type(
                "Column 400×400",
                &ddc_model::MemberShape::Rect { b: 0.4, h: 0.4 },
                "Concrete",
            )
            .unwrap();
        let shape = |e: &Editor| -> String {
            e.model
                .sql()
                .query_row("SELECT shape FROM member_type WHERE id=?1", [mid], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        let before = shape(&e);
        let mut d = TypeDraft::load(&e.model, Fam::Member, mid, u);
        d.vals[1] = edit_length(0.5, u);
        assert!(apply(&mut e, &mut d, true));
        assert_ne!(shape(&e), before);
        e.undo();
        assert_eq!(shape(&e), before);
        e.redo();
        assert_ne!(shape(&e), before);
        d.vals[1] = "abc".into();
        assert!(!apply(&mut e, &mut d, true));
        let n = types_of(&e.model, Fam::Member).len();
        let mut d = TypeDraft::load(&e.model, Fam::Member, mid, u);
        let new = duplicate(&mut e, &mut d, true).unwrap();
        assert_eq!(types_of(&e.model, Fam::Member).len(), n + 1);
        assert!(types_of(&e.model, Fam::Member)
            .iter()
            .any(|t| t.0 == new && t.1 == "Column 400×400 2"));
        e.undo();
        assert_eq!(types_of(&e.model, Fam::Member).len(), n);
    }

    #[test]
    fn slab_layers_edit_is_one_undo_step() {
        let mut e = ed();
        let u = e.tool.units;
        let mut d = TypeDraft::first(&e.model, Fam::Slab, u);
        if d.id <= 0 {
            let id = e
                .model
                .add_slab_type(
                    "Slab",
                    &[LayerSpec {
                        material: "concrete".into(),
                        thickness: 0.2,
                    }],
                )
                .unwrap();
            d = TypeDraft::load(&e.model, Fam::Slab, id, u);
        }
        let id = d.id;
        let tot =
            |e: &Editor| -> f64 { layers_of(&e.model, Fam::Slab, id).iter().map(|l| l.2).sum() };
        let t0 = tot(&e);
        d.layers[0].thickness = edit_length(0.3, u);
        assert!(apply(&mut e, &mut d, true));
        assert!((tot(&e) - t0).abs() > 1e-6);
        e.undo();
        assert!((tot(&e) - t0).abs() < 1e-9);
        e.redo();
        assert!((tot(&e) - t0).abs() > 1e-6);
    }

    #[test]
    fn mep_size_text_round_trips() {
        let u = Units::default();
        let t = size_text("rect", 0.3, 0.2, u);
        let (w, h) = parse_size(&t, false, u).unwrap();
        assert!((w - 0.3).abs() < 1e-9 && (h - 0.2).abs() < 1e-9);
        let t = size_text("round", 0.16, 0.16, u);
        assert!((parse_size(&t, true, u).unwrap().0 - 0.16).abs() < 1e-9);
        assert!(parse_size("0×5", false, u).is_none());
    }

    fn wall_draft(e: &Editor) -> TypeDraft {
        let u = e.tool.units;
        let id = types_of(&e.model, Fam::Wall)
            .into_iter()
            .find(|t| t.1 == crate::app2_fx::fx("td_exterior_380"))
            .unwrap()
            .0;
    TypeDraft::load(&e.model, Fam::Wall, id, u)
    }

    #[test]
    fn dirty_follows_edits_and_apply() {
        let mut e = ed();
        let u = e.tool.units;
        let mut d = wall_draft(&e);
        assert!(!d.dirty(&e.model), "clean after load");
        let stored = d.layers[0].thickness.clone();
        d.layers[0].thickness = edit_length(0.123, u);
        assert!(d.dirty(&e.model), "thickness edit");
        d.layers[0].thickness = stored;
        assert!(!d.dirty(&e.model), "back to the stored value");
        d.name.push('x');
        assert!(d.dirty(&e.model), "rename");
        d.name.pop();
        assert!(d.insert_layer(0));
        assert!(d.dirty(&e.model), "layer count");
        assert!(apply(&mut e, &mut d, true));
        assert!(!d.dirty(&e.model), "clean after apply");
        let mid = e
            .model
            .add_member_type(
                "Col 400",
                &ddc_model::MemberShape::Rect { b: 0.4, h: 0.4 },
                "Concrete",
            )
            .unwrap();
        let mut d = TypeDraft::load(&e.model, Fam::Member, mid, u);
        assert!(!d.dirty(&e.model));
        d.vals[0].push('z');
        assert!(d.dirty(&e.model), "member material");
    }

    #[test]
    fn layer_insert_delete_bounds() {
        let e = ed();
        let mut d = wall_draft(&e);
        let n = d.layers.len();
        assert!(!d.insert_layer(n), "past the end");
        assert!(!d.delete_layer(n), "past the end");
        assert!(d.insert_layer(n - 1));
        assert_eq!(d.layers.len(), n + 1);
        assert_eq!(d.layers[n].material, d.layers[n - 1].material, "same material");
        let t = parse_edit_length(&d.layers[n].thickness, d.u).unwrap();
        assert!((t - NEW_LAYER_T).abs() < 1e-9);
        let pos: Vec<i64> = d.layers.iter().map(|l| l.pos).collect();
        assert_eq!(pos, (0..=n as i64).collect::<Vec<_>>());
        while d.layers.len() > 1 {
            assert!(d.delete_layer(0));
        }
        assert!(!d.delete_layer(0), "the last layer stays");
        assert_eq!(d.layers.len(), 1);
        while d.insert_layer(0) {}
        assert_eq!(d.layers.len(), MAX_LAYERS);
        // moving at the ends changes nothing
        let before = d.layers.clone();
        d.move_layer(0, true);
        d.move_layer(MAX_LAYERS - 1, false);
        assert_eq!(d.layers, before);
    }

    #[test]
    fn insert_and_delete_layers_is_one_undo_step() {
        let mut e = ed();
        let mut d = wall_draft(&e);
        let id = d.id;
        let before = layers_of(&e.model, Fam::Wall, id);
        assert!(d.insert_layer(1));
        assert!(d.insert_layer(1));
        assert!(d.delete_layer(0));
        assert_eq!(d.layers.len(), before.len() + 1);
        assert!(apply(&mut e, &mut d, true));
        let after = layers_of(&e.model, Fam::Wall, id);
        assert_eq!(after.len(), before.len() + 1);
        assert!(after.iter().enumerate().all(|(k, l)| l.0 == k as i64));
        assert!(d.error.is_none());
        e.undo();
        assert_eq!(layers_of(&e.model, Fam::Wall, id), before);
        assert!(!e.can_undo(), "one step for the whole edit");
        e.redo();
        assert_eq!(layers_of(&e.model, Fam::Wall, id).len(), before.len() + 1);
    }

    #[test]
    fn slab_layer_insert_applies() {
        let mut e = ed();
        let u = e.tool.units;
        let id = e
            .model
            .add_slab_type(
                "Plate",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let mut d = TypeDraft::load(&e.model, Fam::Slab, id, u);
        assert!(!d.delete_layer(0));
        assert!(d.insert_layer(0));
        assert!(apply(&mut e, &mut d, true));
        assert_eq!(layers_of(&e.model, Fam::Slab, id).len(), 2);
    }

    #[test]
    fn switch_rule_keeps_draft_until_confirmed() {
        assert_eq!(decide_switch(false, None), Switch::Now);
        assert_eq!(decide_switch(false, Some(Choice::Cancel)), Switch::Now);
        assert_eq!(decide_switch(true, None), Switch::Ask);
        assert_eq!(decide_switch(true, Some(Choice::Apply)), Switch::ApplyThenGo);
        assert_eq!(decide_switch(true, Some(Choice::Discard)), Switch::DiscardThenGo);
        assert_eq!(decide_switch(true, Some(Choice::Cancel)), Switch::Stay);
    }

    #[test]
    fn unsaved_edits_survive_switch_and_close_until_answered() {
        let mut e = ed();
        let u = e.tool.units;
        let mut d = wall_draft(&e);
        d.layers[0].thickness = edit_length(0.05, u);
        d.name = "Edited".into();
        let kept = d.clone();
        // switch or close with unsaved edits: ask first, draft untouched
        assert_eq!(decide_switch(d.dirty(&e.model), None), Switch::Ask);
        // cancel keeps the edits
        assert_eq!(decide_switch(d.dirty(&e.model), Some(Choice::Cancel)), Switch::Stay);
        assert_eq!(d.name, kept.name);
        assert_eq!(d.layers, kept.layers);
        assert!(d.dirty(&e.model));
        // discard: the other type loads clean, the model is unchanged
        let other = types_of(&e.model, Fam::Wall)
            .into_iter()
            .find(|t| t.0 != d.id)
            .unwrap()
            .0;
        assert!(!e.can_undo());
        let n = TypeDraft::load(&e.model, Fam::Wall, other, u);
        assert!(!n.dirty(&e.model));
        assert!(!e.can_undo(), "discard writes nothing");
        // apply then go: one undo step, then clean
        assert_eq!(decide_switch(true, Some(Choice::Apply)), Switch::ApplyThenGo);
        assert!(apply(&mut e, &mut d, true));
        assert!(!d.dirty(&e.model));
        e.undo();
        assert!(!e.can_undo());
    }

    #[test]
    fn opening_another_type_from_outside_never_drops_edits() {
        let e = ed();
        let u = e.tool.units;
        let mut d = wall_draft(&e);
        let other = types_of(&e.model, Fam::Wall)
            .into_iter()
            .find(|t| t.0 != d.id)
            .unwrap()
            .0;
        // clean draft: replaced at once
        let mut slot = Some(d.clone());
        open_in(&mut slot, TypeDraft::load(&e.model, Fam::Wall, other, u), &e.model);
        assert_eq!(slot.as_ref().unwrap().id, other);
        // dirty draft: kept with its edits, the request waits for the question
        d.name = "Edited".into();
        let mut slot = Some(d.clone());
        open_in(&mut slot, TypeDraft::load(&e.model, Fam::Wall, other, u), &e.model);
        let s = slot.unwrap();
        assert_eq!((s.id, s.name.as_str()), (d.id, "Edited"));
        assert_eq!(s.want, Some((Fam::Wall, other)));
        // the same type again keeps the edits and asks nothing
        let mut slot = Some(d.clone());
        open_in(&mut slot, TypeDraft::load(&e.model, Fam::Wall, d.id, u), &e.model);
        let s = slot.unwrap();
        assert_eq!((s.name.as_str(), s.want), ("Edited", None));
    }

    #[test]
    fn bad_layer_value_is_reported_per_field() {
        let e = ed();
        let mut d = wall_draft(&e);
        assert!(d.layer_problem(0, true).is_none());
        d.layers[0].thickness = "abc".into();
        assert!(d.layer_problem(0, true).is_some());
        d.layers[0].thickness = "0".into();
        assert!(d.layer_problem(0, false).is_some());
        assert!(d.layer_problem(99, true).is_none());
    }

    #[test]
    fn dialog_uses_kit_widgets_only() {
        let src = include_str!("type_dialog.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for raw in ["ComboBox", "selectable_label", "egui::Button", "TextEdit"] {
            assert!(!code.contains(raw), "{raw} in type_dialog.rs");
        }
    }
}
