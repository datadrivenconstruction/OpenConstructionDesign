// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Correcting one drafted value on the card without asking the model again. Text is read the
//! way the properties panel reads it; a valid edit changes the draft only, never the model.

use super::plan::{NewElem, PlanItem, PlanOp};
use crate::editor::Editor;
use crate::query::Val;
use crate::units::{DecimalMark, Units};
use ddc_model::{PVal, ParamType};
use ddc_ui::i18n::Catalog;

/// Values listed under a row before the rest waits behind the row's disclosure.
pub const SHOWN: usize = 4;
/// Elements of one create item whose numbers are offered on the card.
pub const ELEMS: usize = 8;
/// Range of a size the properties panel has no row for (metres).
const SIZE: (f64, f64) = (0.001, 1e3);

/// A number of a created element that the card offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Num {
    Length,
    Height,
    BaseOffset,
    TopOffset,
    Width,
    Sill,
    Offset,
    Overhang,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// A `set` entry of an Assign item, by its key.
    Assign(String),
    /// Number `num` of element `index` of a Create item.
    Create { index: usize, num: Num },
}

/// What a valid edit writes into the draft.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Machine text of an Assign value, as a tool argument carries it.
    Text(String),
    /// Metres of a created element.
    Metres(f64),
}

/// How the typed text is read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Read {
    /// A project parameter: read like its Properties field.
    Param(ParamType),
    /// An element field holding a plain number, in the unit the draft wrote it.
    Number,
    Text,
    /// A length in project units, kept in the Properties row's range.
    Len { lo: f64, hi: f64 },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Label {
    /// Catalog key of a geometry row (`geom.height`).
    Key(&'static str),
    /// The parameter's or field's own name.
    Name(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Editable {
    pub field: Field,
    pub label: Label,
    pub read: Read,
    pub value: Value,
}

fn param_type(ed: &Editor, items: &[PlanItem], key: &str) -> Option<ParamType> {
    let k = key.trim().to_lowercase();
    let live = ed.model.params().ok().and_then(|defs| {
        defs.into_iter()
            .find(|d| d.spec.name.to_lowercase() == k)
            .map(|d| d.spec.ptype)
    });
    // a parameter the same draft adds is read with the type it will have
    live.or_else(|| {
        items.iter().find_map(|i| match &i.op {
            PlanOp::NewParam(p) if p.name.to_lowercase() == k => Some(p.ptype),
            _ => None,
        })
    })
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

/// The numbers of one created element the card offers, with the geometry row they answer to.
fn nums(e: &NewElem) -> Vec<(Num, f64)> {
    use Num::*;
    match e {
        NewElem::Wall { a, b, base_offset, height, .. }
        | NewElem::Curtain { a, b, base_offset, height, .. } => {
            vec![(Length, dist(*a, *b)), (Height, *height), (BaseOffset, *base_offset)]
        }
        NewElem::Opening { width, height, sill, .. } => {
            vec![(Width, *width), (Height, *height), (Sill, *sill)]
        }
        NewElem::Slab { offset, .. } => vec![(Offset, *offset)],
        NewElem::Room { height, .. } | NewElem::Ceiling { height, .. } => vec![(Height, *height)],
        NewElem::Column { base_offset, top_offset, .. } => {
            vec![(BaseOffset, *base_offset), (TopOffset, *top_offset)]
        }
        NewElem::Roof { offset, overhang, .. } => vec![(Offset, *offset), (Overhang, *overhang)],
        _ => vec![],
    }
}

fn geom_kind(e: &NewElem) -> &'static str {
    match e {
        NewElem::Wall { .. } => "wall",
        NewElem::Curtain { .. } => "curtain_wall",
        NewElem::Opening { .. } => "opening",
        NewElem::Slab { .. } => "slab",
        NewElem::Room { .. } => "room",
        NewElem::Column { .. } => "col",
        NewElem::Ceiling { .. } => "ceiling",
        NewElem::Roof { .. } => "roof",
        _ => "",
    }
}

pub fn label_key(n: Num) -> &'static str {
    match n {
        Num::Length => "geom.length",
        Num::Height => "geom.height",
        Num::BaseOffset => "geom.base_offset",
        Num::TopOffset => "geom.top_offset",
        Num::Width => "geom.width",
        Num::Sill => "geom.sill",
        Num::Offset => "geom.offset",
        Num::Overhang => "geom.overhang",
    }
}

/// The Properties row's range for this number; a size without a row keeps sizes positive.
fn range(e: &NewElem, n: Num) -> (f64, f64) {
    let (kind, key) = (geom_kind(e), label_key(n));
    crate::geom_props::rows()
        .iter()
        .find(|r| r.kind == kind && r.key == key)
        .map(|r| (r.lo, r.hi))
        .unwrap_or(SIZE)
}

/// Values of `item` the card can correct in place: literal Assign values and the numbers of
/// created elements. A relative value (`+0.5`) stays the model's to change.
pub fn editables(ed: &Editor, items: &[PlanItem], item: &PlanItem) -> Vec<Editable> {
    let mut out = vec![];
    match &item.op {
        PlanOp::Assign { set, .. } => {
            for a in set {
                let Val::Lit(text) = &a.val else { continue };
                let read = match param_type(ed, items, &a.key) {
                    Some(t) => Read::Param(t),
                    // num-lint: the plan's machine text, point decimal, not typed by the person
                    None if text.trim().parse::<f64>().is_ok() => Read::Number,
                    None => Read::Text,
                };
                out.push(Editable {
                    field: Field::Assign(a.key.clone()),
                    label: Label::Name(a.key.clone()),
                    read,
                    value: Value::Text(text.clone()),
                });
            }
        }
        PlanOp::Create { items: elems, .. } => {
            for (index, e) in elems.iter().take(ELEMS).enumerate() {
                for (num, v) in nums(e) {
                    let (lo, hi) = range(e, num);
                    out.push(Editable {
                        field: Field::Create { index, num },
                        label: Label::Key(label_key(num)),
                        read: Read::Len { lo, hi },
                        value: Value::Metres(v),
                    });
                }
            }
        }
        _ => {}
    }
    out
}

/// A machine number (`5.5`) in the UI decimal mark.
fn local(s: &str, mark: DecimalMark) -> String {
    match mark {
        DecimalMark::Comma => s.replace('.', ","),
        DecimalMark::Point => s.to_string(),
    }
}

/// The text the field shows for the value now in the draft.
pub fn shown(e: &Editable, u: Units, mark: DecimalMark) -> String {
    match (&e.value, e.read) {
        (Value::Metres(m), _) => crate::ui_kit::len_text(*m, u, mark),
        (Value::Text(t), Read::Param(ParamType::Length)) => {
            match ddc_model::num_text::parse_stored(t.trim()) {
                Some(m) => crate::ui_kit::len_text(m, u, mark),
                None => t.clone(),
            }
        }
        (Value::Text(t), Read::Param(ParamType::Number | ParamType::Area | ParamType::Volume))
        | (Value::Text(t), Read::Number) => local(t, mark),
        (Value::Text(t), _) => t.clone(),
    }
}

/// A number as tool text: point decimal, no groups, no exponent.
fn machine_num(x: f64) -> String {
    format!("{x}")
}

fn machine(v: &PVal) -> String {
    match v {
        PVal::Num(x) => machine_num(*x),
        PVal::Int(i) => i.to_string(),
        PVal::Bool(b) => (if *b { "yes" } else { "no" }).to_string(),
        PVal::Text(s) => s.clone(),
    }
}

/// Reads typed `text` for `e` like the properties panel would. `Err` is the reason in the
/// catalog's language; nothing is sent then and the draft keeps its value.
pub fn check(e: &Editable, text: &str, u: Units, cat: &Catalog) -> Result<Value, String> {
    let mark = DecimalMark::for_lang(cat.lang);
    let ru = cat.lang == ddc_ui::Lang::Ru;
    let t = text.trim();
    let name = match &e.label {
        Label::Name(n) => n.clone(),
        Label::Key(k) => cat.t(k),
    };
    let bad = || cat.tf("ai.value.bad", &[("text", t), ("name", &name)]);
    match e.read {
        Read::Param(_) if t.is_empty() => Ok(Value::Text(String::new())),
        Read::Param(p) => crate::schedule::extra::typed_value(p, t, u, mark)
            .map(|v| Value::Text(machine(&v)))
            .ok_or_else(bad),
        Read::Number => crate::units::parse_locale_number(t, mark)
            .map(|x| Value::Text(machine_num(x)))
            .map_err(|_| bad()),
        Read::Text => Ok(Value::Text(t.to_string())),
        Read::Len { lo, hi } => crate::ui_kit::len_commit(t, u, lo, hi, mark)
            .map(Value::Metres)
            .map_err(|err| cat.tf("ai.value.bad_len", &[("name", &name), ("why", &err.text(ru))])),
    }
}

fn set_num(e: &mut NewElem, n: Num, v: f64) -> bool {
    use Num::*;
    match (e, n) {
        (NewElem::Wall { a, b, .. } | NewElem::Curtain { a, b, .. }, Length) => {
            let len = dist(*a, *b);
            if len < 1e-9 || v <= 0.0 {
                return false;
            }
            // the start stays, the end moves along the wall
            let k = v / len;
            *b = (a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k);
            true
        }
        (
            NewElem::Wall { height, .. }
            | NewElem::Curtain { height, .. }
            | NewElem::Opening { height, .. }
            | NewElem::Room { height, .. }
            | NewElem::Ceiling { height, .. },
            Height,
        ) => {
            *height = v;
            true
        }
        (
            NewElem::Wall { base_offset, .. }
            | NewElem::Curtain { base_offset, .. }
            | NewElem::Column { base_offset, .. },
            BaseOffset,
        ) => {
            *base_offset = v;
            true
        }
        (NewElem::Column { top_offset, .. }, TopOffset) => {
            *top_offset = v;
            true
        }
        (NewElem::Opening { width, .. }, Width) => {
            *width = v;
            true
        }
        (NewElem::Opening { sill, .. }, Sill) => {
            *sill = v;
            true
        }
        (NewElem::Slab { offset, .. } | NewElem::Roof { offset, .. }, Offset) => {
            *offset = v;
            true
        }
        (NewElem::Roof { overhang, .. }, Overhang) => {
            *overhang = v;
            true
        }
        _ => false,
    }
}

/// Why [`write`] left the draft as it was: the card checks first, so this is a value the drafted
/// element cannot take (a wall of no length) or a value the item no longer has.
pub fn refusal(field: &Field, value: &Value, u: Units, ru: bool) -> String {
    let name = match field {
        Field::Assign(k) => k.clone(),
        Field::Create { num, .. } => crate::tr_lang(ru, label_key(*num), &[]),
    };
    let lang = if ru { ddc_ui::Lang::Ru } else { ddc_ui::Lang::En };
    let text = match value {
        Value::Metres(m) => crate::ui_kit::len_text(*m, u, DecimalMark::for_lang(lang)),
        Value::Text(t) => t.clone(),
    };
    crate::tr_lang(ru, "ai.value.bad", &[("text", &text), ("name", &name)])
}

/// Puts the value into the drafted op; false when the op has no such value (left unchanged).
pub fn write(op: &mut PlanOp, field: &Field, value: &Value) -> bool {
    match (op, field, value) {
        (PlanOp::Assign { set, .. }, Field::Assign(key), Value::Text(t)) => {
            match set.iter_mut().find(|a| a.key == *key && matches!(a.val, Val::Lit(_))) {
                Some(a) => {
                    a.val = Val::Lit(t.clone());
                    true
                }
                None => false,
            }
        }
        (PlanOp::Create { items, .. }, Field::Create { index, num }, Value::Metres(v)) => {
            items.get_mut(*index).is_some_and(|e| set_num(e, *num, *v))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::plan::Targets;
    use crate::query::Assign;
    use ddc_ui::Lang;

    fn wall_item() -> PlanItem {
        PlanItem {
            id: 1,
            title: "Wall".into(),
            why: String::new(),
            op: PlanOp::Create {
                slot0: 1,
                items: vec![NewElem::Wall {
                    level: 1,
                    a: (0.0, 5.0),
                    b: (6.0, 5.0),
                    bulge: 0.0,
                    base_offset: 0.0,
                    height: 3.0,
                    wtype: None,
                }],
            },
            checked: true,
            depends_on: vec![],
            actor: "person".into(),
            conflict: None,
            sets: Default::default(),
        }
    }

    fn set_item(key: &str, val: &str) -> PlanItem {
        PlanItem {
            op: PlanOp::Assign {
                target: Targets::Selection,
                set: vec![Assign {
                    key: key.into(),
                    val: Val::Lit(val.into()),
                }],
            },
            ..wall_item()
        }
    }

    fn rating(ptype: ParamType) -> PlanItem {
        PlanItem {
            id: 2,
            op: PlanOp::NewParam(ddc_model::ParamSpec {
                name: "Rating".into(),
                ptype,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            }),
            ..wall_item()
        }
    }

    #[test]
    fn a_wall_length_moves_only_the_end_and_keeps_the_direction() {
        let mut it = wall_item();
        let f = Field::Create { index: 0, num: Num::Length };
        assert!(write(&mut it.op, &f, &Value::Metres(5.5)));
        let PlanOp::Create { items, .. } = &it.op else { unreachable!() };
        let NewElem::Wall { a, b, .. } = &items[0] else { unreachable!() };
        assert_eq!(*a, (0.0, 5.0));
        assert!((b.0 - 5.5).abs() < 1e-12 && (b.1 - 5.0).abs() < 1e-12, "{b:?}");
        // a value the op does not hold leaves it as it was
        let before = format!("{:?}", it.op);
        let far = Field::Create {
            index: 3,
            num: Num::Height,
        };
        assert!(!write(&mut it.op, &far, &Value::Metres(2.0)));
        assert!(!write(&mut it.op, &Field::Assign("Rating".into()), &Value::Text("B".into())));
        assert_eq!(format!("{:?}", it.op), before);
    }

    #[test]
    fn a_wall_offers_length_height_and_base_offset_in_range() {
        let ed = Editor::new_in_memory().unwrap();
        let it = wall_item();
        let e = editables(&ed, std::slice::from_ref(&it), &it);
        let nums: Vec<_> = e
            .iter()
            .map(|x| match x.field {
                Field::Create { num, .. } => num,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(nums, vec![Num::Length, Num::Height, Num::BaseOffset]);
        assert_eq!(e[0].value, Value::Metres(6.0));
        for x in &e {
            let Read::Len { lo, hi } = x.read else { panic!("{x:?}") };
            assert!(lo < hi, "{x:?}");
        }
    }

    #[test]
    fn typed_text_is_read_like_the_properties_panel_in_both_languages() {
        let ed = Editor::new_in_memory().unwrap();
        let u = ed.tool.units;
        let it = wall_item();
        let len = editables(&ed, std::slice::from_ref(&it), &it).remove(0);
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            let at = Editable { value: Value::Metres(5.5), ..len.clone() };
            let text = shown(&at, u, DecimalMark::for_lang(lang));
            let back = check(&len, &text, u, &cat).unwrap();
            assert_eq!(back, Value::Metres(5.5), "{lang:?}: the shown text {text:?} reads back");
            let err = check(&len, "abc", u, &cat).unwrap_err();
            assert!(err.contains(&cat.t("geom.length")), "{lang:?}: {err}");
            assert!(!err.contains("ai.value."), "{lang:?}: {err}");
        }
    }

    #[test]
    fn a_parameter_value_is_checked_by_its_type_and_sent_as_machine_text() {
        let ed = Editor::new_in_memory().unwrap();
        let u = ed.tool.units;
        let num = rating(ParamType::Number);
        let it = set_item("Rating", "1.5");
        let items = vec![num, it.clone()];
        let e = editables(&ed, &items, &it).remove(0);
        assert_eq!(e.read, Read::Param(ParamType::Number));
        let en = Catalog::new(Lang::En);
        let ru = Catalog::new(Lang::Ru);
        assert_eq!(check(&e, "2.25", u, &en), Ok(Value::Text("2.25".into())));
        assert_eq!(check(&e, "2,25", u, &ru), Ok(Value::Text("2.25".into())));
        assert_eq!(shown(&e, u, DecimalMark::for_lang(Lang::Ru)), "1,5");
        for cat in [&en, &ru] {
            let err = check(&e, "abc", u, cat).unwrap_err();
            assert!(err.contains("abc") && err.contains("Rating"), "{err}");
        }
        // a text parameter takes any word; an empty field clears the value
        let text = rating(ParamType::Text);
        let items = vec![text, it.clone()];
        let e = editables(&ed, &items, &it).remove(0);
        assert_eq!(check(&e, "B", u, &en), Ok(Value::Text("B".into())));
        assert_eq!(check(&e, "  ", u, &en), Ok(Value::Text(String::new())));
    }

    /// vsrc1: a value `write` cannot take is named with its field in both languages, and the
    /// draft keeps what it had.
    #[test]
    fn a_value_the_draft_cannot_take_is_refused_by_name() {
        let ed = Editor::new_in_memory().unwrap();
        let u = ed.tool.units;
        let len = Field::Create { index: 0, num: Num::Length };
        let mut op = wall_item().op;
        assert!(!write(&mut op, &len, &Value::Metres(0.0)), "a wall of no length");
        assert_eq!(format!("{op:?}"), format!("{:?}", wall_item().op), "the draft keeps its wall");
        for ru in [false, true] {
            let lang = if ru { Lang::Ru } else { Lang::En };
            let cat = Catalog::new(lang);
            let why = refusal(&len, &Value::Metres(0.0), u, ru);
            let zero = crate::ui_kit::len_text(0.0, u, DecimalMark::for_lang(lang));
            let want = cat.tf("ai.value.bad", &[("text", &zero), ("name", &cat.t("geom.length"))]);
            assert_eq!(why, want, "{lang:?}");
            assert!(!why.contains("ai.value") && !why.contains("geom."), "{why}");
        }
        let key = Field::Assign("Rating".into());
        let mut op = wall_item().op;
        assert!(!write(&mut op, &key, &Value::Text("A".into())), "a wall has no assignments");
        let why = refusal(&key, &Value::Text("A".into()), u, false);
        assert!(why.contains("\"A\"") && why.contains("Rating"), "{why}");
    }
}
