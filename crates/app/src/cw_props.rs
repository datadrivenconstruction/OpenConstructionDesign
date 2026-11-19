//! Properties of a curtain wall, its panel and slab: numbers are edited in place and committed
//! on release/focus loss as one undoable operation.
use crate::editor::cw::grid_step;
use crate::editor::{Editor, Sel};
use crate::ui_kit::{self, Tokens};
use crate::units::{display_length, fmt_percent, unit_label};
use ddc_model::{GridLines, MullionSection};

/// Edit buffer: the selection and values before commit.
pub type Buf = Option<(Sel, [f64; 5])>;

fn num(
    ui: &mut egui::Ui,
    name: &str,
    v: &mut f64,
    speed: f64,
    range: std::ops::RangeInclusive<f64>,
) -> bool {
    ui.label(name);
    let r = ui.add(
        egui::DragValue::new(v)
            .speed(speed)
            .range(range)
            .max_decimals(3),
    );
    ui.end_row();
    r.drag_stopped() || r.lost_focus()
}

/// true if the selection was a curtain wall, panel or slab (the panel was drawn).
pub fn show(ui: &mut egui::Ui, _t: &Tokens, ed: &mut Editor, buf: &mut Buf) -> bool {
    let Some(sel) = ed
        .selection
        .iter()
        .copied()
        .find(|s| matches!(s, Sel::Curtain(_) | Sel::Panel(..) | Sel::Slab(_)))
    else {
        return false;
    };
    let u = ed.tool.units;
    match sel {
        Sel::Curtain(id) => {
            let Some(c) = ed.curtain(id) else {
                return false;
            };
            let sp = c.spec().clone();
            let q = c.qty.clone();
            let mw = match sp.interior {
                MullionSection::Rect { w, d } => (w, d),
                MullionSection::Circle { d } => (d, d),
                MullionSection::None => (0.0, 0.0),
            };
            let (gu, gv) = (grid_step(&sp.u), grid_step(&sp.v));
            let init = [sp.height, gu.unwrap_or(0.0), gv.unwrap_or(0.0), mw.0, mw.1];
            let mut cur = match buf {
                Some((s, v)) if *s == sel => *v,
                _ => init,
            };
            let mut commit = false;
            let len = c.length();
            ui_kit::props(ui, "sel", |ui| {
                ui_kit::kv(ui, &crate::tr("curtain_edit.cw.title", &[]), format!("#{id}"));
                ui_kit::kv_num(ui, &crate::tr("curtain_edit.cw.length", &[]), display_length(len, u));
                let un = unit_label(u);
                let len = |ui: &mut egui::Ui, k: &str, v: &mut f64, lo: f64, hi: f64| {
                    let salt = egui::Id::new(("cw_len_field", id, k));
                    ui_kit::kv_field(ui, salt, k, v, ui_kit::NumKind::Len(u), lo, hi, un)
                };
                commit |= len(ui, &crate::tr("curtain_edit.cw.height", &[]), &mut cur[0], 0.1, 200.0);
                if gu.is_some() {
                    commit |= len(ui, &crate::tr("curtain_edit.cw.grid_step_u", &[]), &mut cur[1], 0.1, 50.0);
                } else {
                    ui_kit::kv(ui, &crate::tr("curtain_edit.cw.grid_u", &[]), sp.u.text());
                }
                if gv.is_some() {
                    commit |= len(ui, &crate::tr("curtain_edit.cw.grid_step_v", &[]), &mut cur[2], 0.1, 50.0);
                } else {
                    ui_kit::kv(ui, &crate::tr("curtain_edit.cw.grid_v", &[]), sp.v.text());
                }
                if matches!(sp.interior, MullionSection::Rect { .. }) {
                    commit |= len(ui, &crate::tr("curtain_edit.cw.mullion_w", &[]), &mut cur[3], 0.01, 0.5);
                    commit |= len(ui, &crate::tr("curtain_edit.cw.mullion_d", &[]), &mut cur[4], 0.01, 1.0);
                }
                ui_kit::kv_num(
                    ui,
                    &crate::tr("curtain_edit.cw.panels", &[]),
                    crate::tr(
                        "curtain_edit.cw.panels_val",
                        &[
                            ("n", &q.panels.to_string()),
                            ("d", &q.doors.to_string()),
                            ("w", &q.windows.to_string()),
                        ],
                    ),
                );
                ui_kit::kv_num(ui, &crate::tr("curtain_edit.cw.glass_area", &[]), crate::unit_text::area_ui(q.glass_area));
                ui_kit::kv_num(ui, &crate::tr("curtain_edit.cw.gross_area", &[]), crate::unit_text::area_ui(q.gross_area));
                ui_kit::kv_num(ui, &crate::tr("curtain_edit.cw.mullion_length", &[]), display_length(q.mullion_length, u));
            });
            crate::geom_props::show(ui, ed, "curtain_wall", id);
            *buf = Some((sel, cur));
            if commit && cur != init {
                *buf = None;
                ed.edit_curtain(id, |s| {
                    s.height = cur[0];
                    if gu.is_some() {
                        s.u = GridLines::Spacing(cur[1]);
                    }
                    if gv.is_some() {
                        s.v = GridLines::Spacing(cur[2]);
                    }
                    if let MullionSection::Rect { .. } = s.interior {
                        let r = MullionSection::Rect {
                            w: cur[3],
                            d: cur[4],
                        };
                        s.interior = r;
                        s.border = r;
                    }
                });
            }
            ui_kit::hint(ui, crate::tr("curtain_edit.cw.hint", &[]));
        }
        Sel::Panel(id, k) => return crate::elem::curtain::panel_section(ui, ed, id, k),
        Sel::Slab(id) => {
            let Some(s) = ed.slab(id) else { return false };
            let pct = s.slope_percent();
            let mut cur = match buf {
                Some((b, v)) if *b == sel => *v,
                _ => [pct, 0.0, 0.0, 0.0, 0.0],
            };
            let (th, top, area, surf, vol) =
                (s.thickness, s.top, s.area, s.surface_area(), s.volume);
            let mut commit = false;
            ui_kit::props(ui, "sel", |ui| {
                ui_kit::kv(ui, &crate::tr("curtain_edit.cw.slab", &[]), format!("#{id}"));
                ui_kit::kv_num(ui, &crate::tr("curtain_edit.cw.slab.thickness", &[]), display_length(th, u));
                ui_kit::kv_num(ui, &crate::tr("curtain_edit.cw.slab.top", &[]), display_length(top, u));
                commit |= num(ui, &crate::tr("curtain_edit.cw.slab.slope", &[]), &mut cur[0], 0.1, -100.0..=100.0);
                ui_kit::kv_num(ui, &crate::tr("curtain_edit.cw.slab.plan_area", &[]), crate::unit_text::area_ui(area));
                ui_kit::kv_num(ui, &crate::tr("curtain_edit.cw.slab.surface_area", &[]), crate::unit_text::area_ui(surf));
                ui_kit::kv_num(ui, &crate::tr("curtain_edit.cw.slab.volume", &[]), crate::unit_text::vol_ui(vol));
            });
            crate::geom_props::show(ui, ed, "slab", id);
            *buf = Some((sel, cur));
            if commit && (cur[0] - pct).abs() > 1e-9 {
                *buf = None;
                ed.set_slab_slope_percent(id, cur[0]);
            }
            ui_kit::hint(
                ui,
                crate::tr("curtain_edit.cw.slab.hint", &[("pct", &fmt_percent(pct))]),
            );
        }
        _ => return false,
    }
    true
}
