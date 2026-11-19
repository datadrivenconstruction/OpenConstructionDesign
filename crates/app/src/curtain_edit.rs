//! Curtain-wall editor on the elevation: click a grid segment to remove/restore it, a panel to
//! select it (Shift adds to the selection), "Merge" joins the selected adjacent panels,
//! panel and mullion types per segment from lists, the lock on a line pins/unpins it,
//! "Even spacing" redistributes unpinned lines. Logic: `ddc_geom::curtain_edit`.

use crate::ui_kit::Tokens;
use ddc_geom::curtain::{Axis, Section};
use ddc_geom::curtain_edit::{CurtainEdit, PanelType, Seg};
use ddc_geom::v2;
use egui::{self, Pos2, Rect, Stroke};

/// What is under the cursor on the elevation (coordinates: s along the axis, z height).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Seg(Seg),
    Cell(usize, usize),
    Pin(bool, usize),
}

pub struct CurtainEditor {
    pub axis: Axis,
    pub e: CurtainEdit,
    pub sel: Vec<(usize, usize)>,
    pub sel_seg: Option<Seg>,
    pub status: Option<String>,
}

pub const MULLION: Section = Section::Rect { w: 0.06, d: 0.15 };

impl CurtainEditor {
    pub fn demo() -> Self {
        let axis = Axis::new(v2(0.0, 0.0), v2(7.5, 0.0), 0.0).unwrap_or_else(|_| unreachable!());
        let mut e = CurtainEdit::new(
            vec![0.0, 1.5, 3.0, 4.5, 6.0, 7.5],
            vec![0.0, 1.2, 2.4, 3.6],
            MULLION,
            Section::Rect { w: 0.08, d: 0.2 },
        );
        let _ = e.merge((2, 0), (2, 1));
        let _ = e.set_panel((2, 0), PanelType::Door);
        let _ = e.merge((0, 2), (1, 2));
        let _ = e.set_panel((0, 2), PanelType::Solid);
        let _ = e.set_panel((4, 1), PanelType::Window);
        let _ = e.set_panel((3, 2), PanelType::Empty);
        let _ = e.set_mullion((false, 1, 4), Section::Circle { d: 0.1 });
        let _ = e.pin(true, 2, true);
        e.corner = [Some(Section::Rect { w: 0.12, d: 0.12 }), None];
        CurtainEditor {
            axis,
            e,
            sel: vec![(4, 0)],
            sel_seg: Some((true, 4, 2)),
            status: None,
        }
    }

    /// Hit test at an elevation point with tolerance `tol` (m): lock, then segment, then cell.
    pub fn hit(&self, s: f64, z: f64, tol: f64) -> Option<Hit> {
        let (u, v) = (&self.e.u, &self.e.v);
        let top = *v.last()?;
        if (z - (top + 0.25)).abs() < tol * 2.0 {
            if let Some(k) = u.iter().position(|x| (x - s).abs() < tol * 2.0) {
                return Some(Hit::Pin(true, k));
            }
        }
        let i = u.windows(2).position(|w| w[0] <= s && s <= w[1])?;
        let j = v.windows(2).position(|w| w[0] <= z && z <= w[1])?;
        if let Some(k) = u.iter().position(|x| (x - s).abs() < tol) {
            return Some(Hit::Seg((true, k, j)));
        }
        if let Some(k) = v.iter().position(|x| (x - z).abs() < tol) {
            return Some(Hit::Seg((false, k, i)));
        }
        Some(Hit::Cell(i, j))
    }

    /// Click: segment removes/restores, cell selects, lock pins/unpins.
    pub fn click(&mut self, h: Hit, add: bool) -> Result<(), String> {
        match h {
            Hit::Seg(s) => {
                self.sel_seg = Some(s);
                if self.e.removed.contains(&s) {
                    self.e.add_segment(s)
                } else {
                    self.e.remove_segment(s)
                }
            }
            Hit::Cell(i, j) => {
                if !add {
                    self.sel.clear();
                }
                self.sel.push((i, j));
                Ok(())
            }
            Hit::Pin(vert, k) => {
                let on = !self.e.pin_u.get(k).copied().unwrap_or(true);
                self.e.pin(vert, k, on)
            }
        }
    }

    /// Merge the selected cells (each with an adjacent already-merged one).
    pub fn merge_selected(&mut self) -> Result<usize, String> {
        let mut todo = self.sel.clone();
        let Some(first) = todo.first().copied() else {
            return Err(crate::tr("curtain_edit.cwe.err.nothing_selected", &[]));
        };
        let mut done = vec![first];
        todo.remove(0);
        let mut n = 0;
        while !todo.is_empty() {
            let k = todo
                .iter()
                .position(|c| {
                    done.iter()
                        .any(|d| c.0.abs_diff(d.0) + c.1.abs_diff(d.1) == 1)
                })
                .ok_or_else(|| crate::tr("curtain_edit.cwe.err.not_contiguous", &[]))?;
            let c = todo.remove(k);
            let d = *done
                .iter()
                .find(|d| c.0.abs_diff(d.0) + c.1.abs_diff(d.1) == 1)
                .unwrap_or(&first);
            if self.e.kind_of(c) != self.e.kind_of(d) || !self.same_panel(c, d) {
                self.e.merge(d, c)?;
                n += 1;
            }
            done.push(c);
        }
        Ok(n)
    }

    fn same_panel(&self, a: (usize, usize), b: (usize, usize)) -> bool {
        self.e
            .panels(&self.axis)
            .iter()
            .any(|p| p.cells.contains(&a) && p.cells.contains(&b))
    }

    pub fn set_selected_type(&mut self, t: PanelType) -> Result<(), String> {
        for c in self.sel.clone() {
            self.e.set_panel(c, t)?;
        }
        Ok(())
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let full = ui.available_rect_before_wrap();
        let p = ui.painter_at(full);
        p.rect_filled(full, 0.0, t.viewport);
        let side = 260.0;
        let r = Rect::from_min_max(
            full.min + egui::vec2(16.0, 36.0),
            full.max - egui::vec2(side + 24.0, 24.0),
        );
        p.rect_filled(r, t.radius as f32, t.paper);
        p.text(
            full.min + egui::vec2(16.0, 28.0),
            egui::Align2::LEFT_BOTTOM,
            &crate::tr("curtain_edit.cwe.title", &[]),
            egui::FontId::proportional(ddc_ui::theme::type_size::TITLE),
            t.text,
        );
        let len = self.axis.length();
        let top = *self.e.v.last().unwrap_or(&1.0);
        let k =
            ((r.width() - 60.0) as f64 / len).min((r.height() - 80.0) as f64 / (top + 0.5)) as f32;
        let o = Pos2::new(r.center().x - (len as f32 * k) / 2.0, r.max.y - 30.0);
        let sc = |s: f64, z: f64| Pos2::new(o.x + s as f32 * k, o.y - z as f32 * k);
        let fill = |pt: PanelType| match pt {
            PanelType::Glazed => t.mat[3],
            PanelType::Solid => t.mat[1],
            PanelType::Empty => t.paper,
            PanelType::Door => t.mat[4],
            PanelType::Window => t.mat[3].gamma_multiply(0.7),
        };
        let panels = self.e.panels(&self.axis);
        for pn in &panels {
            for &(i, j) in &pn.cells {
                let rect = Rect::from_two_pos(
                    sc(self.e.u[i], self.e.v[j]),
                    sc(self.e.u[i + 1], self.e.v[j + 1]),
                );
                p.rect_filled(rect, 0.0, fill(pn.kind));
                if self.sel.contains(&(i, j)) {
                    p.rect_filled(rect.shrink(2.0), 0.0, t.accent.gamma_multiply(0.25));
                }
            }
            let (i, j) = pn.cells[0];
            let c = sc(
                (self.e.u[i] + self.e.u[i + 1]) / 2.0,
                (self.e.v[j] + self.e.v[j + 1]) / 2.0,
            );
            p.text(
                c,
                egui::Align2::CENTER_CENTER,
                format!(
                    "{}\n{}",
                    crate::tr(&format!("curtain_props.cw.kind.{}", pn.kind.as_str()), &[]),
                    crate::unit_text::area_ui(pn.area)
                ),
                ddc_ui::typo::font(ddc_ui::theme::Role::CanvasTag),
                t.text_dim,
            );
        }
        let (nu, nv) = (self.e.u.len(), self.e.v.len());
        for vert in [true, false] {
            let lines = if vert { nu } else { nv };
            let segs = if vert { nv - 1 } else { nu - 1 };
            for line in 0..lines {
                for sg in 0..segs {
                    let s = (vert, line, sg);
                    let (a, b) = if vert {
                        (
                            sc(self.e.u[line], self.e.v[sg]),
                            sc(self.e.u[line], self.e.v[sg + 1]),
                        )
                    } else {
                        (
                            sc(self.e.u[sg], self.e.v[line]),
                            sc(self.e.u[sg + 1], self.e.v[line]),
                        )
                    };
                    let sec = self.e.section(s);
                    let w = (sec.width() as f32 * k).max(1.0);
                    let col = if self.sel_seg == Some(s) {
                        t.accent
                    } else {
                        t.cut
                    };
                    if sec == Section::None {
                        // a removed segment is a dashed ghost
                        p.add(egui::Shape::dashed_line(
                            &[a, b],
                            Stroke::new(1.0, t.beyond),
                            4.0,
                            4.0,
                        ));
                    } else {
                        p.line_segment([a, b], Stroke::new(w, col));
                        if matches!(sec, Section::Circle { .. }) {
                            p.circle_stroke(a.lerp(b, 0.5), w * 0.6, Stroke::new(1.0, t.panel));
                        }
                    }
                }
            }
        }
        for (k2, c) in self.e.corner.iter().enumerate() {
            if let Some(c) = c {
                let s = if k2 == 0 { 0.0 } else { len };
                let w = c.width() as f32 * k * 1.6;
                p.rect_filled(
                    Rect::from_two_pos(
                        sc(s, 0.0) - egui::vec2(w / 2.0, 0.0),
                        sc(s, top) + egui::vec2(w / 2.0, 0.0),
                    ),
                    0.0,
                    t.cut,
                );
            }
        }
        for (k2, x) in self.e.u.iter().enumerate() {
            let c = sc(*x, top + 0.25);
            let pinned = self.e.pin_u[k2];
            p.circle_filled(c, 7.0, if pinned { t.accent } else { t.panel });
            p.circle_stroke(c, 7.0, Stroke::new(1.0, t.border));
            p.text(
                c,
                egui::Align2::CENTER_CENTER,
                if pinned { "■" } else { "□" },
                ddc_ui::typo::font(ddc_ui::theme::Role::CanvasMicro),
                if pinned { t.sem().accent_text_on } else { t.text_dim },
            );
        }
        let resp = ui.interact(r, ui.id().with("cw_face"), egui::Sense::click());
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let (s, z) = (((pos.x - o.x) / k) as f64, ((o.y - pos.y) / k) as f64);
                if let Some(h) = self.hit(s, z, 6.0 / k as f64) {
                    let add = ui.input(|i| i.modifiers.shift);
                    self.status = self.click(h, add).err().map(|e| cwe_err(&e));
                }
            }
        }
        // side properties panel
        let sr = Rect::from_min_max(
            Pos2::new(full.max.x - side - 12.0, full.min.y + 36.0),
            full.max - egui::vec2(12.0, 24.0),
        );
        p.rect_filled(sr, t.radius as f32, t.panel);
        p.rect_stroke(
            sr,
            t.radius as f32,
            Stroke::new(1.0, t.border),
            egui::StrokeKind::Inside,
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(sr.shrink(10.0)));
        child.label(egui::RichText::new(crate::tr("curtain_edit.cwe.props", &[])).strong());
        child.label(crate::tr(
            "curtain_edit.cwe.counts",
            &[
                ("p", &panels.len().to_string()),
                ("m", &self.e.mullion_count().to_string()),
            ],
        ));
        child.label(crate::tr(
            "curtain_edit.cwe.mullion_len",
            &[(
                "v",
                &mullion_len_text(
                    self.e.mullion_length(&self.axis),
                    crate::units::active_units(),
                ),
            )],
        ));
        child.label(crate::tr(
            "curtain_edit.cwe.selected",
            &[("n", &self.sel.len().to_string())],
        ));
        child.horizontal_wrapped(|ui| {
            for pt in [
                PanelType::Glazed,
                PanelType::Solid,
                PanelType::Empty,
                PanelType::Door,
                PanelType::Window,
            ] {
                if ui.button(crate::tr(&format!("curtain_props.cw.kind.{}", pt.as_str()), &[])).clicked() {
                    self.status = self.set_selected_type(pt).err().map(|e| cwe_err(&e));
                }
            }
        });
        if child.button(crate::tr("curtain_edit.cwe.merge", &[])).clicked() {
            self.status = self.merge_selected().err().map(|e| cwe_err(&e));
        }
        if child.button(crate::tr("curtain_edit.cwe.even", &[])).clicked() {
            self.e.even_out(true);
        }
        if let Some(s) = self.sel_seg {
            child.separator();
            let dir = crate::tr(
                if s.0 { "curtain_edit.cwe.dir.v" } else { "curtain_edit.cwe.dir.h" },
                &[],
            );
            child.label(crate::tr(
                "curtain_edit.cwe.segment",
                &[
                    ("dir", &dir),
                    ("line", &s.1.to_string()),
                    ("idx", &s.2.to_string()),
                ],
            ));
            child.horizontal(|ui| {
                for (lab, sec) in [
                    (crate::tr("curtain_edit.cwe.mull.none", &[]), Section::None),
                    ("50×150".to_string(), MULLION),
                    ("Ø100".to_string(), Section::Circle { d: 0.1 }),
                ] {
                    if ui.button(&lab).clicked() {
                        self.status = self.e.set_mullion(s, sec).err().map(|e| cwe_err(&e));
                    }
                }
            });
        }
        if let Some(st) = &self.status {
            child.colored_label(t.accent, st);
        }
    }
}

/// Localise a `ddc_geom::curtain_edit` error by prefix; unknown text passes through.
/// Mullion length in the project's display units (unit suffix included).
pub(crate) fn mullion_len_text(len_m: f64, u: crate::units::Units) -> String {
    crate::units::display_length(len_m, u)
}

pub fn cwe_err(msg: &str) -> String {
    cwe_err_in(crate::ui_lang_now(), msg)
}

pub fn cwe_err_in(ru: bool, msg: &str) -> String {
    use crate::tr_lang as tl;
    let seg = |raw: &str| -> String {
        let n: Vec<&str> = raw
            .trim_matches(|c| c == '(' || c == ')')
            .split(", ")
            .collect();
        match n.as_slice() {
            [v, l, i] if *v == "true" || *v == "false" => {
                let key = if *v == "true" { "curtain_edit.cwe.dir.v" } else { "curtain_edit.cwe.dir.h" };
                format!("{} {l}\u{b7}{i}", tl(ru, key, &[]))
            }
            _ => raw.to_string(),
        }
    };
    let k = |key: &str, a: &[(&str, &str)]| tl(ru, &format!("curtain_edit.cwe.{key}"), a);
    if let Some(r) = msg.strip_prefix("no segment ") {
        return k("no_segment", &[("seg", &seg(r))]);
    }
    if let Some(r) = msg
        .strip_prefix("grid line at ")
        .and_then(|r| r.strip_suffix(" is outside or duplicates a line"))
    {
        return k("grid_line", &[("at", r)]);
    }
    if let Some(r) = msg.strip_prefix("line ") {
        if let Some(l) = r.strip_suffix(" is a border or missing") {
            return k("line_border", &[("line", l)]);
        }
        if let Some((l, at)) = r.split_once(" cannot move to ") {
            return k("line_move", &[("line", l), ("at", at)]);
        }
    }
    if let Some(r) = msg
        .strip_prefix("segment ")
        .and_then(|r| r.strip_suffix(" is present"))
    {
        return k("segment_present", &[("seg", &seg(r))]);
    }
    if let Some(r) = msg.strip_prefix("no line ") {
        return k("no_line", &[("line", r)]);
    }
    if let Some(r) = msg.strip_prefix("no cell ") {
        return k("no_cell", &[("cell", r)]);
    }
    match msg {
        "tilt must be within \u{b1}60\u{b0}" => k("tilt", &[]),
        "border segments cannot be removed" => k("border_remove", &[]),
        "cells are not adjacent" => k("not_adjacent", &[]),
        "door panel must reach the bottom of the wall" => k("door_bottom", &[]),
        _ => msg.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const SRC: &str = include_str!("curtain_edit.rs");

    fn keys_in(src: &str) -> Vec<String> {
        let mut v = Vec::new();
        let mut rest = src;
        while let Some(i) = rest.find("curtain_edit.") {
            let tail = &rest[i..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_'))
                .unwrap_or(tail.len());
            if end > "curtain_edit.".len() && !tail[..end].ends_with('.') {
                v.push(tail[..end].to_string());
            }
            rest = &tail[end..];
        }
        v
    }

    fn prod(src: &str) -> &str {
        src.split("#[cfg(test)]").next().unwrap_or("")
    }

    #[test]
    fn qa12g_keys_present_en_ru() {
        let en: std::collections::BTreeMap<String, String> =
            serde_json::from_str(include_str!("../../ui/data/i18n/en/curtain_edit.json")).unwrap();
        let ru: std::collections::BTreeMap<String, String> =
            serde_json::from_str(include_str!("../../ui/data/i18n/ru/curtain_edit.json")).unwrap();
        let mut keys = keys_in(prod(SRC));
        keys.extend(keys_in(prod(include_str!("cw_props.rs"))));
        assert!(keys.len() > 20);
        for k in keys {
            assert!(en.contains_key(&k), "en missing {k}");
            assert!(ru.contains_key(&k), "ru missing {k}");
        }
        assert_eq!(en.len(), ru.len());
    }

    #[test]
    fn mullion_len_follows_active_units_mm() {
        let t = mullion_len_text(6.0, crate::units::Units::Metric);
        assert!(t.contains("000"), "{t}");
        assert!(!t.contains("6.00"), "{t}");
    }

    #[test]
    fn mullion_len_ft_in() {
        let t = mullion_len_text(3.048, crate::units::Units::Imperial);
        assert!(t.contains('\'') && t.contains('"'), "{t}");
    }

    #[test]
    fn mullion_len_key_has_no_hardcoded_unit() {
        for f in [
            include_str!("../../ui/data/i18n/en/curtain_edit.json"),
            include_str!("../../ui/data/i18n/ru/curtain_edit.json"),
        ] {
            let m: std::collections::BTreeMap<String, String> = serde_json::from_str(f).unwrap();
            assert!(m["curtain_edit.cwe.mullion_len"].trim_end().ends_with("{v}"));
        }
    }

    #[test]
    fn no_cyrillic_literals_in_cw_files() {
        for src in [SRC, include_str!("cw_props.rs")] {
            for l in prod(src).lines() {
                let code = l.split("//").next().unwrap_or("");
                assert!(
                    !code.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)),
                    "cyrillic literal: {l}"
                );
            }
        }
    }

    #[test]
    fn hit_and_click_cycle() {
        let mut c = CurtainEditor::demo();
        assert_eq!(c.hit(1.5, 0.5, 0.05), Some(Hit::Seg((true, 1, 0))));
        assert_eq!(c.hit(1.0, 1.2, 0.05), Some(Hit::Seg((false, 1, 0))));
        assert_eq!(c.hit(2.0, 0.5, 0.05), Some(Hit::Cell(1, 0)));
        let n = c.e.mullion_count();
        c.click(Hit::Seg((true, 1, 0)), false).unwrap();
        assert_eq!(c.e.mullion_count(), n - 1);
        c.click(Hit::Seg((true, 1, 0)), false).unwrap();
        assert_eq!(c.e.mullion_count(), n);
        assert!(c.click(Hit::Seg((true, 0, 0)), false).is_err());
        c.click(Hit::Pin(true, 3), false).unwrap();
        assert!(c.e.pin_u[3]);
    }

    #[test]
    fn merge_selection_area() {
        let mut c = CurtainEditor::demo();
        c.sel.clear();
        for cell in [(3, 0), (4, 0), (4, 1)] {
            c.click(Hit::Cell(cell.0, cell.1), true).unwrap();
        }
        assert_eq!(c.merge_selected().unwrap(), 2);
        let p = c.e.panels(&c.axis);
        let big = p.iter().find(|p| p.cells.contains(&(4, 1))).unwrap();
        assert!((big.area - 3.0 * 1.5 * 1.2).abs() < 1e-9);
        c.sel = vec![(0, 0), (4, 2)];
        assert!(c.merge_selected().is_err());
    }

    /// Panel snapshot in light and dark: docs/design/shots/edit/<name>-<theme>.png.
    pub(crate) fn shoot(name: &str, mut draw: impl FnMut(&mut egui::Ui, &Tokens)) {
        let (pick, _) = crate::gfx::choose(&crate::gfx::candidates(None, false), &mut |_| {})
            .expect("graphics adapter");
        let rs = crate::shot::render_state(pick).unwrap();
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/shots/edit");
        std::fs::create_dir_all(&dir).unwrap();
        let size = (1440u32, 900u32);
        for (theme, mode) in [("light", ddc_ui::Mode::Light), ("dark", ddc_ui::Mode::Dark)] {
            let mut th = ddc_ui::Theme::new(mode, ddc_ui::Density::Compact);
            let tok = crate::shell::tokens(&th);
            let ctx = egui::Context::default();
            let px = crate::shot::frame(&rs, &ctx, size, 1.0, 4, None, |c, _| {
                th.apply(c);
                egui::CentralPanel::default().show(c, |ui| draw(ui, &tok));
            });
            crate::shot::save_png(&dir.join(format!("{name}-{theme}.png")), size, &px).unwrap();
        }
    }

    const GEOM_MSGS: [&str; 11] = [
            "no segment (true, 1, 0)",
            "grid line at 1.5 is outside or duplicates a line",
            "line 2 is a border or missing",
            "line 2 cannot move to 3.1",
            "tilt must be within \u{b1}60\u{b0}",
            "border segments cannot be removed",
            "segment (false, 1, 2) is present",
            "no line 7",
            "cells are not adjacent",
            "no cell (3, 4)",
            "door panel must reach the bottom of the wall",
        ];

    #[test]
    fn cwe_err_every_geom_message_has_key() {
        for ru in [false, true] {
            for m in GEOM_MSGS {
                let o = cwe_err_in(ru, m);
                assert_ne!(o, m, "unmapped: {m}");
                assert!(!o.contains("curtain_edit.") && !o.contains('{'), "{o}");
            }
        }
        let o = cwe_err_in(false, "line 2 cannot move to 3.1");
        assert!(o.contains('2') && o.contains("3.1"), "{o}");
    }

    #[test]
    fn cwe_err_ru_no_latin_words() {
        for m in GEOM_MSGS {
            let o = cwe_err_in(true, m);
            assert!(!o.chars().any(|c| c.is_ascii_alphabetic()), "latin in {o}");
        }
    }

    #[test]
    fn cwe_err_unknown_passthrough() {
        assert_eq!(cwe_err_in(true, "something else"), "something else");
        assert_eq!(cwe_err_in(false, ""), "");
    }
}
