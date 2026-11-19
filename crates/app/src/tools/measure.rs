// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Saved measurements on a plan or section: the same records the 3D tool writes, drawn as the
//! dimension primitives are (line between the measured points, value at its middle).

use ddc_geom::V2;
use ddc_model::{MeasureInfo, MeasureKind, MeasureReason, MeasureState, MeasureValue, ViewKey};
use eframe::egui::{self, Pos2, Stroke};

use crate::edit3d::measure_saved::{self, Anchor, Lens, Mode, Picks};
use crate::editor::Editor;
use crate::ui_kit::Tokens;

/// The plan and section tool: clicks pick wall faces (or points) and save the same record the
/// 3D tool writes. Esc leaves; a first click is kept until the second completes it.
#[derive(Clone, Debug)]
pub struct MeasureTool {
    pub mode: Mode,
    pub picks: Picks,
    pub cursor: V2,
    /// The number being typed over a driving measurement (Enter near a dimension opens it).
    pub edit: Option<DriveEdit>,
}

/// The input field of a driving measurement: its text and which support moves.
#[derive(Clone, Debug, PartialEq)]
pub struct DriveEdit {
    pub id: i64,
    pub text: String,
    /// Where the field sits, metres on the view.
    pub at: V2,
    fresh: bool,
}

pub enum MeasureOut {
    Nothing,
    Done,
    Exit,
    Refused(String),
    Error(String),
}

impl MeasureTool {
    pub fn new(mode: Mode) -> Self {
        MeasureTool { mode, picks: Picks::default(), cursor: ddc_geom::v2(0.0, 0.0), edit: None }
    }

    pub fn hint_tool(&self) -> &'static str {
        "measure"
    }

    pub fn escape(&mut self) -> MeasureOut {
        if self.edit.take().is_some() {
            return MeasureOut::Nothing;
        }
        if self.picks.anchors.is_empty() {
            MeasureOut::Exit
        } else {
            self.picks.clear();
            MeasureOut::Nothing
        }
    }

    /// The anchor a click at `p` (metres) lands on: a wall face within `r`, else a point on the
    /// level for the modes that take points.
    pub fn anchor_at(&self, ed: &Editor, p: V2, r: f64) -> Option<Anchor> {
        if let Some(a) = measure_saved::plan_anchor(&ed.model, ed.level, (p.x, p.y), r) {
            return Some(a);
        }
        matches!(self.mode, Mode::Distance | Mode::Angle).then(|| {
            let z = ed.levels().iter().find(|l| l.0 == ed.level).map_or(0.0, |l| l.2);
            Anchor::Point([p.x, p.y, z])
        })
    }

    pub fn click(&mut self, ed: &mut Editor, p: V2, r: f64) -> MeasureOut {
        let Some(a) = self.anchor_at(ed, p, r) else {
            return MeasureOut::Refused(crate::tr("measure.no_face", &[]));
        };
        self.pick(ed, ViewKey::Plan(ed.level), a)
    }

    /// A click on a section: a point of the cut plane (model metres). Faces are not picked there,
    /// so the modes that need them refuse.
    pub fn click_point(&mut self, ed: &mut Editor, view: ViewKey, p: [f64; 3]) -> MeasureOut {
        if !section_takes(self.mode) {
            return MeasureOut::Refused(crate::tr("measure.no_face", &[]));
        }
        self.pick(ed, view, Anchor::Point(p))
    }

    fn pick(&mut self, ed: &mut Editor, view: ViewKey, a: Anchor) -> MeasureOut {
        let Some(spec) = self.picks.click(&ed.model, self.mode, view, a) else {
            return MeasureOut::Nothing;
        };
        match ed.apply_ext(crate::tools::Rec::new("measure_add", move |m| m.add_measure(&spec).map(|_| 1))) {
            Ok(_) => MeasureOut::Done,
            Err(e) => MeasureOut::Error(e.to_string()),
        }
    }

    /// Opens the input field on the driving-capable measurement whose label is within `r` of the
    /// cursor. False when there is none.
    pub fn open_edit(&mut self, ed: &Editor, r: f64) -> bool {
        // Enter with the field open is its own Enter (drive), not a second opening
        if self.edit.is_some() {
            return true;
        }
        let Ok(list) = ed.model.measures_in_view(ViewKey::Plan(ed.level)) else { return false };
        let best = list
            .iter()
            .filter(|i| matches!(i.spec.kind, MeasureKind::Distance | MeasureKind::Clear))
            .filter(|i| i.points.len() >= 2)
            .filter_map(|i| {
                let MeasureValue::Ok(v) = i.value else { return None };
                let (a, b) = (i.points[0], i.points[1]);
                let mid = ddc_geom::v2((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0);
                let d = ((mid.x - self.cursor.x).powi(2) + (mid.y - self.cursor.y).powi(2)).sqrt();
                (d <= r).then_some((d, i.id, v, mid))
            })
            .min_by(|x, y| x.0.total_cmp(&y.0));
        let Some((_, id, v, at)) = best else { return false };
        self.edit = Some(DriveEdit { id, text: crate::units::display_length(v, ed.tool.units), at, fresh: true });
        true
    }

    /// The field itself: Enter drives (Shift moves the first support instead of the second),
    /// Esc closes it. The outcome of a drive is returned for the caller to report.
    pub fn edit_ui(&mut self, ui: &mut egui::Ui, ed: &mut Editor, to_s: &dyn Fn(V2) -> Pos2) -> MeasureOut {
        let Some(e) = &mut self.edit else { return MeasureOut::Nothing };
        let (mut enter, mut esc, mut first_side) = (false, false, false);
        egui::Area::new(egui::Id::new("measure_drive_edit"))
            .order(ddc_ui::z::Z::Canvas.order())
            .fixed_pos(to_s(e.at) - egui::vec2(40.0, 14.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut e.text).desired_width(80.0));
                    if e.fresh {
                        r.request_focus();
                        e.fresh = false;
                    }
                    let (k_enter, k_esc, shift) =
                        ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape), i.modifiers.shift));
                    enter = k_enter && !k_esc;
                    esc = k_esc;
                    first_side = shift;
                });
            });
        if esc {
            self.edit = None;
            return MeasureOut::Nothing;
        }
        if !enter {
            return MeasureOut::Nothing;
        }
        let Some(e) = self.edit.take() else { return MeasureOut::Nothing };
        let side = if first_side { ddc_model::MeasureSide::A } else { ddc_model::MeasureSide::B };
        drive_text(ed, e.id, &e.text, side)
    }

    /// The quick value under the cursor, computed and never written.
    pub fn lens(&self, ed: &Editor, r: f64) -> Option<String> {
        let a = self.anchor_at(ed, self.cursor, r)?;
        let view = ViewKey::Plan(ed.level);
        match Lens::over(&ed.model, self.mode, view, &self.picks, &a) {
            Lens::Hover(v) => {
                // a lone face reads as its area
                let k = if self.picks.anchors.len() + 1 == self.mode.arity() {
                    self.mode.kind()?
                } else {
                    MeasureKind::Area
                };
                Some(value_text(ed, k, v))
            }
            Lens::Off => None,
        }
    }
}

/// The running tool on the plan: the picked faces and the quick value at the cursor.
pub fn draw_tool(p: &egui::Painter, ed: &Editor, tool: &MeasureTool, to_s: &dyn Fn(V2) -> Pos2, r: f64, t: &Tokens) {
    if let Some(txt) = tool.lens(ed, r) {
        p.text(
            to_s(tool.cursor) + egui::vec2(16.0, 16.0),
            egui::Align2::LEFT_TOP,
            txt,
            ddc_ui::typo::font(ddc_ui::typo::Role::Small),
            t.text,
        );
    }
}

/// The typed number of a driving measurement: parsed in the project units, then one journal
/// record moves the geometry. A support that cannot be driven is a refusal, a conflict an error.
pub fn drive_text(ed: &mut Editor, id: i64, text: &str, side: ddc_model::MeasureSide) -> MeasureOut {
    let value = match crate::units::parse_length(text, ed.tool.units) {
        Ok(v) => v,
        Err(e) => return MeasureOut::Refused(e.to_string()),
    };
    match ed.apply_ext(crate::tools::Rec::new("measure_drive", move |m| {
        m.drive_measure(id, value, side).map(|_| 1)
    })) {
        Ok(()) => MeasureOut::Done,
        Err(ddc_model::Error::Invalid(m)) if m.starts_with("unsupported") => MeasureOut::Refused(m),
        Err(e) => MeasureOut::Error(e.to_string()),
    }
}

/// Display text of a value: length in the project units, area/volume in metric, angle, percent.
pub fn value_text(ed: &Editor, kind: MeasureKind, v: MeasureValue) -> String {
    let ru = crate::ui_is_ru();
    match v {
        MeasureValue::Ok(x) => match kind {
            MeasureKind::Distance | MeasureKind::Clear | MeasureKind::Perimeter => {
                crate::units::display_length(x, ed.tool.units)
            }
            MeasureKind::Area => format!("{x:.2} {}", if ru { "\u{43c}\u{b2}" } else { "m²" }),
            MeasureKind::Volume => format!("{x:.2} {}", if ru { "\u{43c}\u{b3}" } else { "m³" }),
            MeasureKind::Angle => format!("{x:.1}°"),
            MeasureKind::Slope => format!("{x:.1} %"),
        },
        MeasureValue::Unavailable(r) => {
            let key = match r {
                MeasureReason::Orphan(_) => "measure.orphan",
                MeasureReason::NotPlanar => "measure.not_planar",
                MeasureReason::Unsupported(_) => "measure.unsupported",
            };
            crate::tr_lang(ru, key, &[])
        }
    }
}

/// Text of a stored measurement; an orphan shows its last value with the warning.
pub fn info_text(ed: &Editor, i: &MeasureInfo) -> String {
    match (i.state, i.last) {
        (MeasureState::Orphan, Some(l)) => format!(
            "{} ({})",
            value_text(ed, i.spec.kind, MeasureValue::Ok(l)),
            value_text(ed, i.spec.kind, i.value)
        ),
        _ => value_text(ed, i.spec.kind, i.value),
    }
}

/// The modes a section can take: they pick points of the cut plane, not faces.
pub fn section_takes(mode: Mode) -> bool {
    matches!(mode, Mode::Distance | Mode::Angle | Mode::Slope)
}

/// Measurements of the level's plan; returns how many were drawn.
pub fn draw(p: &egui::Painter, ed: &Editor, to_s: &dyn Fn(V2) -> Pos2, t: &Tokens) -> usize {
    paint(p, ed, ViewKey::Plan(ed.level), &|q| Some(to_s(ddc_geom::v2(q[0], q[1]))), t)
}

/// Where a model point lands on the section's canvas.
pub fn section_screen(ad: &crate::interact::SectionAdapter, q: [f64; 3]) -> Option<Pos2> {
    use crate::interact::ViewAdapter;
    ad.project_at(ddc_geom::v3(q[0], q[1], q[2])).map(|s| Pos2::new(s[0] as f32, s[1] as f32))
}

/// Measurements of a section: model points projected onto the cut by `to_screen`.
pub fn draw_section(p: &egui::Painter, ed: &Editor, id: i64, to_screen: &dyn Fn([f64; 3]) -> Option<Pos2>, t: &Tokens) -> usize {
    paint(p, ed, ViewKey::Section(id), to_screen, t)
}

fn paint(p: &egui::Painter, ed: &Editor, view: ViewKey, to_s: &dyn Fn([f64; 3]) -> Option<Pos2>, t: &Tokens) -> usize {
    let Ok(list) = ed.model.measures_in_view(view) else {
        return 0;
    };
    for i in &list {
        let col = if i.state == MeasureState::Live { t.text } else { t.ov().warning };
        // a measurement with a point off the canvas is skipped whole, not drawn as a lone label
        let Some(pts) = i.points.iter().map(|q| to_s(*q)).collect::<Option<Vec<Pos2>>>() else { continue };
        let text = info_text(ed, i);
        let at = match pts.as_slice() {
            [] => continue,
            [a] => *a,
            [a, b, ..] => {
                p.line_segment([*a, *b], Stroke::new(1.0, col));
                a.lerp(*b, 0.5)
            }
        };
        p.text(at, egui::Align2::CENTER_BOTTOM, text, ddc_ui::typo::font(ddc_ui::typo::Role::Small), col);
    }
    list.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::MeasureSide;

    fn room() -> (Editor, i64, Vec<i64>) {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::modify_tools::tests::walls(&mut ed, &[((0.0, 0.0), (6.0, 0.0)), ((0.0, 3.0), (6.0, 3.0))]);
        let w: Vec<i64> = {
            let mut st = ed.model.sql().prepare("SELECT id FROM wall ORDER BY id").unwrap();
            st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
        };
        (ed, 0, w)
    }

    fn clear(ed: &mut Editor, w: &[i64]) -> i64 {
        let anchors = [
            Anchor::Dim(ddc_model::DimRef::WallEnd { wall: w[0], face: 1, end: 0 }),
            Anchor::Dim(ddc_model::DimRef::WallEnd { wall: w[1], face: 0, end: 0 }),
        ];
        let spec = ddc_model::MeasureSpec {
            kind: MeasureKind::Clear,
            view: ViewKey::Plan(ed.level),
            pins: ed.model.measure_pins(&anchors).unwrap(),
            label: String::new(),
        };
        ed.model.add_measure(&spec).unwrap()
    }

    #[test]
    fn typed_number_drives_in_one_undo_step() {
        let (mut ed, _, w) = room();
        let id = clear(&mut ed, &w);
        let before = ed.model.journal_len().unwrap();
        let v0 = match ed.model.measure_value(id).unwrap() {
            MeasureValue::Ok(x) => x,
            o => panic!("{o:?}"),
        };
        let want = v0 + 0.5;
        let txt = crate::units::display_length(want, ed.tool.units);
        assert!(matches!(drive_text(&mut ed, id, &txt, MeasureSide::B), MeasureOut::Done));
        assert_eq!(ed.model.journal_len().unwrap(), before + 1, "one record");
        let now = match ed.model.measure_value(id).unwrap() {
            MeasureValue::Ok(x) => x,
            o => panic!("{o:?}"),
        };
        assert!((now - want).abs() < 1e-3, "{now} vs {want}");
        ed.undo();
        let back = match ed.model.measure_value(id).unwrap() {
            MeasureValue::Ok(x) => x,
            o => panic!("{o:?}"),
        };
        assert!((back - v0).abs() < 1e-9, "undo restores the gap");
    }

    #[test]
    fn unsupported_support_is_a_refusal_and_bad_text_too() {
        let (mut ed, _, w) = room();
        let id = clear(&mut ed, &w);
        assert!(matches!(drive_text(&mut ed, id, "abc", MeasureSide::B), MeasureOut::Refused(_)));
        // a point-to-point distance between wall faces is not drivable
        let anchors = [
            Anchor::Dim(ddc_model::DimRef::WallEnd { wall: w[0], face: 1, end: 0 }),
            Anchor::Dim(ddc_model::DimRef::WallEnd { wall: w[1], face: 0, end: 0 }),
        ];
        let spec = ddc_model::MeasureSpec {
            kind: MeasureKind::Distance,
            view: ViewKey::Plan(ed.level),
            pins: ed.model.measure_pins(&anchors).unwrap(),
            label: String::new(),
        };
        let d = ed.model.add_measure(&spec).unwrap();
        let before = ed.model.journal_len().unwrap();
        let txt = crate::units::display_length(5.0, ed.tool.units);
        assert!(matches!(drive_text(&mut ed, d, &txt, MeasureSide::B), MeasureOut::Refused(_)));
        assert_eq!(ed.model.journal_len().unwrap(), before, "nothing written");
    }

    fn at_label(ed: &Editor, id: i64) -> V2 {
        let i = ed.model.measures_in_view(ViewKey::Plan(ed.level)).unwrap().into_iter().find(|i| i.id == id).unwrap();
        let (a, b) = (i.points[0], i.points[1]);
        ddc_geom::v2((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0)
    }

    #[test]
    fn enter_near_a_label_opens_the_field_and_esc_closes_it() {
        let (mut ed, _, w) = room();
        let id = clear(&mut ed, &w);
        let mut tool = MeasureTool::new(crate::edit3d::measure_saved::Mode::Clear);
        tool.cursor = ddc_geom::v2(20.0, 20.0);
        assert!(!tool.open_edit(&ed, 0.25), "nothing near the cursor");
        tool.cursor = at_label(&ed, id);
        assert!(tool.open_edit(&ed, 0.25));
        assert_eq!(tool.edit.as_ref().map(|e| e.id), Some(id));
        assert!(matches!(tool.escape(), MeasureOut::Nothing));
        assert!(tool.edit.is_none(), "the first Esc closes the field, the tool stays");
    }

    #[test]
    fn field_enter_drives_the_measurement() {
        let (mut ed, _, w) = room();
        let id = clear(&mut ed, &w);
        let v0 = match ed.model.measure_value(id).unwrap() {
            MeasureValue::Ok(x) => x,
            o => panic!("{o:?}"),
        };
        let mut tool = MeasureTool::new(crate::edit3d::measure_saved::Mode::Clear);
        tool.cursor = at_label(&ed, id);
        assert!(tool.open_edit(&ed, 0.25));
        tool.edit.as_mut().unwrap().text = crate::units::display_length(v0 + 0.4, ed.tool.units);
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut out = MeasureOut::Nothing;
        let to_s = |p: V2| Pos2::new(p.x as f32 * 50.0, p.y as f32 * 50.0);
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                out = tool.edit_ui(ui, &mut ed, &to_s);
            });
        });
        assert!(matches!(out, MeasureOut::Done), "field Enter drives");
        assert!(tool.edit.is_none());
        match ed.model.measure_value(id).unwrap() {
            MeasureValue::Ok(x) => assert!((x - (v0 + 0.4)).abs() < 1e-3, "{x}"),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn section_click_saves_a_point_measurement_in_that_view() {
        let (mut ed, _, _) = room();
        let view = ViewKey::Section(7);
        let mut tool = MeasureTool::new(crate::edit3d::measure_saved::Mode::Distance);
        assert!(matches!(tool.click_point(&mut ed, view, [1.0, 0.0, 0.0]), MeasureOut::Nothing));
        assert!(matches!(tool.click_point(&mut ed, view, [1.0, 0.0, 2.5]), MeasureOut::Done));
        let list = ed.model.measures_in_view(view).unwrap();
        assert_eq!(list.len(), 1);
        match list[0].value {
            MeasureValue::Ok(x) => assert!((x - 2.5).abs() < 1e-9),
            o => panic!("{o:?}"),
        }
        assert!(ed.model.measures_in_view(ViewKey::Plan(ed.level)).unwrap().is_empty(), "not on the plan");
        // faces are not picked on a section: the modes that need them refuse
        let mut clear = MeasureTool::new(crate::edit3d::measure_saved::Mode::Clear);
        assert!(matches!(clear.click_point(&mut ed, view, [0.0, 0.0, 0.0]), MeasureOut::Refused(_)));
    }

    #[test]
    fn section_points_land_on_the_cut_canvas() {
        // a cut along x from the origin, 50 px per metre, drawing (0, 0) at canvas (100, 100)
        let ad = crate::interact::SectionAdapter::new(7, ddc_geom::v2(0.0, 0.0), ddc_geom::v2(6.0, 0.0), ddc_geom::v2(0.0, 0.0), 50.0, [100.0, 100.0]).unwrap();
        let p = section_screen(&ad, [3.0, 0.0, 2.0]).unwrap();
        assert!((p.x - 250.0).abs() < 1e-3 && p.y.abs() < 1e-3, "{p:?}");
    }
}
