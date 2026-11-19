// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan constraints: a panel over the selected walls (lock, EQ, align, ∥, ⊥; for a single
//! wall: align/lock to the nearest parallel axis and top to the level above),
//! badges on walls (clicking a badge removes the constraint), conflict report in the status line.
//! Each action is one model operation, undone with Ctrl+Z.

use ddc_geom::{v2, V2};
use ddc_model::{ConstraintKind, ConstraintRef, ConstraintSpec, Error, Model};

use crate::editor::{Editor, OpFn, Sel};
use crate::units::Units;

/// Readable edit error text; a constraint conflict comes with a hint on what to do.
/// Why an edit broke a constraint, in the user's words.
pub fn conflict_why(e: &Error) -> String {
    match e {
        Error::Conflict(m) => m.replace("edit breaks constraints", &crate::tr("modify_ops.con.breaks", &[])),
        e => e.to_string(),
    }
}

/// Show a failed edit: a constraint conflict as a refusal, any other error as its text.
pub fn show(ed: &mut crate::editor::Editor, e: &Error) {
    match crate::refusals::constraint_conflict(e) {
        Some(r) => ed.refuse(r),
        None => ed.status = Some(report(e)),
    }
}

pub fn report(e: &Error) -> String {
    match e {
        Error::Conflict(_) => crate::tr("modify_ops.con.refused", &[("why", &conflict_why(e))]),
        e if e.to_string().contains("element is pinned") => crate::tr("clip.pinned", &[]),
        // already in the user's language; the generic «degenerate» text would hide the reason
        Error::Degenerate(m) if crate::grips::is_ring_refusal(m) => m.clone(),
        e => e.to_string(),
    }
}

fn axis(m: &Model, w: i64) -> Option<(V2, V2)> {
    m.sql()
        .query_row("SELECT ax, ay, bx, by FROM wall WHERE id=?1", [w], |r| {
            Ok((v2(r.get(0)?, r.get(1)?), v2(r.get(2)?, r.get(3)?)))
        })
        .ok()
}

fn dir(m: &Model, w: i64) -> Option<V2> {
    axis(m, w).map(|(a, b)| {
        let d = b - a;
        d * (1.0 / d.len().max(1e-12))
    })
}

/// Constraints applicable to a set of walls (order matches the panel buttons).
pub fn suggest(m: &Model, walls: &[i64]) -> Vec<ConstraintKind> {
    let ds: Vec<V2> = walls.iter().filter_map(|&w| dir(m, w)).collect();
    if ds.len() != walls.len() || walls.is_empty() {
        return vec![];
    }
    let par = ds.windows(2).all(|w| w[0].cross(w[1]).abs() < 0.02);
    match walls.len() {
        1 => {
            let mut v = vec![ConstraintKind::LockLength];
            if near_grid(m, walls[0]).is_some() {
                v.extend([ConstraintKind::AlignGrid, ConstraintKind::LockDistGrid]);
            }
            if level_above(m, walls[0]).is_some() {
                v.push(ConstraintKind::TopToLevel);
            }
            v
        }
        2 if par => vec![
            ConstraintKind::LockDist,
            ConstraintKind::Align,
            ConstraintKind::Parallel,
        ],
        2 => vec![ConstraintKind::Perp, ConstraintKind::Parallel],
        _ if par => vec![ConstraintKind::Equal],
        _ => vec![],
    }
}

/// Nearest parallel straight grid axis within 3 m of the wall midpoint.
pub fn near_grid(m: &Model, w: i64) -> Option<i64> {
    let (a, b) = axis(m, w)?;
    let u = dir(m, w)?;
    let c = (a + b) * 0.5;
    m.grids()
        .ok()?
        .into_iter()
        .filter(|g| g.spec.bulge == 0.0)
        .filter_map(|g| {
            let (p, q) = (v2(g.spec.a.0, g.spec.a.1), v2(g.spec.b.0, g.spec.b.1));
            let d = q - p;
            let gu = d * (1.0 / d.len().max(1e-12));
            if gu.cross(u).abs() >= 0.02 {
                return None;
            }
            let dist = gu.cross(c - p).abs();
            (dist <= 3.0).then_some((g.id, dist))
        })
        .min_by(|x, y| x.1.total_cmp(&y.1))
        .map(|x| x.0)
}

/// Nearest level above the wall base (target of "top to level").
pub fn level_above(m: &Model, w: i64) -> Option<i64> {
    let (base, off, h): (i64, f64, f64) = m
        .sql()
        .query_row(
            "SELECT level, base_offset, height FROM wall WHERE id=?1",
            [w],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()?;
    let ls = m.level_list().ok()?;
    let z0 = ls.iter().find(|l| l.0 == base)?.2 + off;
    let top = z0 + h;
    ls.into_iter()
        .filter(|l| l.2 > z0 + 0.01)
        .min_by(|x, y| (x.2 - top).abs().total_cmp(&(y.2 - top).abs()))
        .map(|l| l.0)
}

/// Icon of the kit set for the bar button and the plan marker of a constraint kind.
pub fn icon(k: ConstraintKind) -> &'static str {
    match k {
        ConstraintKind::LockLength | ConstraintKind::LockDist | ConstraintKind::LockDistGrid => {
            "lock"
        }
        ConstraintKind::Equal => "distribute",
        ConstraintKind::Align => "align",
        ConstraintKind::AlignGrid => "grid",
        ConstraintKind::Parallel => "parallel",
        ConstraintKind::Perp => "ortho",
        ConstraintKind::LevelGap => "level",
        ConstraintKind::TopToLevel => "level-up",
    }
}

/// Hover text of the bar button: what this constraint keeps on every edit.
pub fn tip_lang(ru: bool, k: ConstraintKind) -> String {
    let key = match k {
        ConstraintKind::LockLength => "constraints.con.tip.lock_length",
        ConstraintKind::LockDist => "constraints.con.tip.lock_dist",
        ConstraintKind::Equal => "constraints.con.tip.equal",
        ConstraintKind::Align => "constraints.con.tip.align",
        ConstraintKind::Parallel => "constraints.con.tip.parallel",
        ConstraintKind::Perp => "constraints.con.tip.perp",
        ConstraintKind::AlignGrid => "constraints.con.tip.align_grid",
        ConstraintKind::LockDistGrid => "constraints.con.tip.lock_dist_grid",
        ConstraintKind::LevelGap => "constraints.con.tip.level_gap",
        ConstraintKind::TopToLevel => "constraints.con.tip.top_to_level",
    };
    crate::tr_lang(ru, key, &[])
}

pub fn title(k: ConstraintKind) -> String {
    title_lang(crate::ui_is_ru(), k)
}

pub fn title_lang(ru: bool, k: ConstraintKind) -> String {
    let key = match k {
        ConstraintKind::LockLength => "modify_ops.con.lock_length",
        ConstraintKind::LockDist => "modify_ops.con.lock_dist",
        ConstraintKind::Equal => "modify_ops.con.equal",
        ConstraintKind::Align => "modify_ops.con.align",
        ConstraintKind::Parallel => "modify_ops.con.parallel",
        ConstraintKind::Perp => "modify_ops.con.perp",
        ConstraintKind::AlignGrid => "modify_ops.con.align_grid",
        ConstraintKind::LockDistGrid => "modify_ops.con.lock_dist_grid",
        ConstraintKind::LevelGap => "modify_ops.con.level_gap",
        ConstraintKind::TopToLevel => "modify_ops.con.top_to_level",
    };
    crate::tr_lang(ru, key, &[])
}

fn selected_walls(ed: &Editor) -> Vec<i64> {
    ed.selection
        .iter()
        .filter_map(|s| match s {
            Sel::Wall(w) => Some(*w),
            _ => None,
        })
        .collect()
}

/// Adds a constraint to the selected walls (one operation; replay uses the same specification).
pub fn add(ed: &mut Editor, kind: ConstraintKind) {
    let mut walls = selected_walls(ed);
    let target = match (kind, walls.first()) {
        (ConstraintKind::AlignGrid | ConstraintKind::LockDistGrid, Some(&w)) => {
            near_grid(&ed.model, w)
        }
        (ConstraintKind::TopToLevel, Some(&w)) => level_above(&ed.model, w),
        _ => None,
    };
    walls.extend(target);
    let spec = ConstraintSpec {
        kind,
        walls,
        value: None,
    };
    let f = OpFn(std::sync::Arc::new(move |m: &mut Model| {
        m.add_constraint(&spec).map(|_| ())
    }));
    ed.status = Some(match ed.run_op(f) {
        Ok(()) => crate::tr("modify_ops.con.added", &[("title", &title(kind))]),
        Err(e) => report(&e),
    });
}

pub fn remove(ed: &mut Editor, id: i64) {
    let f = OpFn(std::sync::Arc::new(move |m: &mut Model| {
        m.delete_constraint(id)
    }));
    ed.status = Some(match ed.run_op(f) {
        Ok(()) => crate::tr("modify_ops.con.removed", &[]),
        Err(e) => report(&e),
    });
}

/// Plan badge of one constraint: icon of its kind plus the locked value when it has one.
#[derive(Debug, Clone)]
pub struct Marker {
    pub id: i64,
    pub at: V2,
    pub kind: ConstraintKind,
    /// Locked length in the project units; the stored value (metres) is never changed.
    pub value: Option<String>,
}

/// Locked value as shown on the badge: project units, project precision, explicit language.
pub fn value_text(k: ConstraintKind, v: Option<f64>, ru: bool, u: Units) -> Option<String> {
    match (k, v) {
        (
            ConstraintKind::LockLength | ConstraintKind::LockDist | ConstraintKind::LockDistGrid,
            Some(v),
        ) => Some(crate::unit_text::len(v.abs(), ru, u)),
        _ => None,
    }
}

/// Constraint badges in project units and the UI language.
pub fn markers(m: &Model) -> Vec<Marker> {
    markers_in(m, crate::ui_is_ru(), crate::units::active_units())
}

/// Constraint badges: id, plan point, kind, value. Several on one wall are offset along the axis.
pub fn markers_in(m: &Model, ru: bool, u: Units) -> Vec<Marker> {
    let mut out: Vec<Marker> = vec![];
    let mut used: Vec<(i64, usize)> = vec![];
    for (id, s) in m.constraints().unwrap_or_default() {
        // the badge sits on the rule's last wall (for axis/level rules, on the wall itself)
        let Some(w) = s.refs().into_iter().rev().find_map(|r| match r {
            ConstraintRef::Wall(w) => Some(w),
            _ => None,
        }) else {
            continue;
        };
        let Some((a, b)) = axis(m, w) else { continue };
        let k = used.iter().filter(|u| u.0 == w).count();
        used.push((w, k));
        let d = b - a;
        let t = d * (1.0 / d.len().max(1e-12));
        let n = v2(-t.y, t.x);
        let p = (a + b) * 0.5 + t * (0.45 * k as f64) - n * 0.35;
        out.push(Marker {
            id,
            at: p,
            kind: s.kind,
            value: value_text(s.kind, s.value, ru, u),
        });
    }
    out
}

/// Badge size for an icon and a value text of width `text_w` (0 when there is no value).
pub fn marker_size(text_w: f32, line_h: f32) -> egui::Vec2 {
    use ddc_ui::theme::{icon_size, space};
    let gap = if text_w > 0.0 { space::XS + text_w } else { 0.0 };
    egui::vec2(
        2.0 * space::XS + icon_size::SM + gap,
        (2.0 * space::XXS + line_h).max(icon_size::SM + 2.0 * space::XXS),
    )
}

pub fn paint(
    painter: &egui::Painter,
    ed: &Editor,
    to_s: &dyn Fn(V2) -> egui::Pos2,
    t: &crate::ui_kit::Tokens,
) {
    use ddc_ui::theme::{icon_size, radius, space, stroke};
    use egui::{pos2, vec2, Color32, FontId, Rect, Stroke, StrokeKind};
    let font = FontId::proportional(ddc_ui::theme::type_size::CAPTION);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    for mk in markers(&ed.model) {
        let gal = mk
            .value
            .map(|v| painter.layout_no_wrap(v, font.clone(), t.accent));
        let text_w = gal.as_ref().map_or(0.0, |g| g.size().x);
        let line_h = gal.as_ref().map_or(0.0, |g| g.size().y);
        let r = Rect::from_center_size(to_s(mk.at), marker_size(text_w, line_h));
        painter.rect_filled(r, radius::SM, t.paper);
        painter.rect_stroke(
            r,
            radius::SM,
            Stroke::new(stroke::HAIRLINE, t.accent),
            StrokeKind::Inside,
        );
        let ic = Rect::from_min_size(
            pos2(r.left() + space::XS, r.center().y - icon_size::SM / 2.0),
            vec2(icon_size::SM, icon_size::SM),
        );
        let tex = ddc_ui::widgets::with_icons(painter.ctx(), |icons| {
            icons.get(painter.ctx(), icon(mk.kind), icon_size::SM as u32)
        });
        painter.image(tex.id(), ic, uv, t.accent);
        if let Some(g) = gal {
            let at = pos2(ic.right() + space::XS, r.center().y - g.size().y / 2.0);
            painter.galley(at, g, Color32::PLACEHOLDER);
        }
    }
}

/// Clicking a constraint badge removes it; true if consumed.
pub fn click(ed: &mut Editor, p: V2) -> bool {
    let r = ed.tool.radius;
    let hit = markers(&ed.model)
        .into_iter()
        .map(|mk| (mk.id, (mk.at - p).len()))
        .filter(|x| x.1 <= r)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    match hit {
        Some((id, _)) => {
            remove(ed, id);
            true
        }
        None => false,
    }
}

/// Constraints panel over the plan area for the current wall selection.
pub fn toolbar(ui: &mut egui::Ui, ed: &mut Editor, rect: egui::Rect) {
    let walls = selected_walls(ed);
    let kinds = suggest(&ed.model, &walls);
    if kinds.is_empty() {
        return;
    }
    let pos = crate::ui_kit::canvas_bar_pos(rect, 0);
    let max_w = rect.width() - 2.0 * ddc_ui::theme::space::MD;
    let (chosen, _) = bar_at(ui.ctx(), pos, max_w, &kinds, crate::ui_is_ru());
    if let Some(k) = chosen {
        add(ed, k);
    }
}

/// Draws the bar at `pos` (wraps inside `max_w`); returns the clicked kind and the bar rect.
pub fn bar_at(
    ctx: &egui::Context,
    pos: egui::Pos2,
    max_w: f32,
    kinds: &[ConstraintKind],
    ru: bool,
) -> (Option<ConstraintKind>, egui::Rect) {
    let th = ddc_ui::theme::of(ctx);
    let mut chosen = None;
    let out = egui::Area::new(egui::Id::new("constraints_bar"))
        .fixed_pos(pos)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(max_w);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = ddc_ui::theme::space::XS;
                    ddc_ui::widgets::with_icons(ctx, |icons| {
                        for &k in kinds {
                            let (label, tip) = (title_lang(ru, k), tip_lang(ru, k));
                            let b = ddc_ui::widgets::Button::secondary(&label)
                                .small()
                                .tooltip(&tip, "")
                                .show_icon(ui, &th, icons, icon(k));
                            if b.clicked() {
                                chosen = Some(k);
                            }
                        }
                    });
                });
            });
        });
    (chosen, out.response.rect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{LayerSpec, WallSpec};

    #[test]
    fn suggest_by_geometry_and_markers() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "c".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let mut w = |a, b| {
            m.add_wall(&WallSpec {
                a,
                b,
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap()
        };
        let a = w((0.0, 0.0), (5.0, 0.0));
        let b = w((0.0, 2.0), (5.0, 2.0));
        let c = w((0.0, 0.0), (0.0, 2.0));
        let d = w((0.0, 4.0), (5.0, 4.0));
        assert_eq!(suggest(&m, &[a]), vec![ConstraintKind::LockLength]);
        assert_eq!(suggest(&m, &[a, b])[0], ConstraintKind::LockDist);
        assert_eq!(suggest(&m, &[a, c])[0], ConstraintKind::Perp);
        assert_eq!(suggest(&m, &[a, b, d]), vec![ConstraintKind::Equal]);
        assert!(suggest(&m, &[a, b, c]).is_empty());
        m.add_constraint(&ConstraintSpec {
            kind: ConstraintKind::LockLength,
            walls: vec![a],
            value: None,
        })
        .unwrap();
        let mk = markers(&m);
        assert_eq!(mk.len(), 1);
        assert_eq!(mk[0].kind, ConstraintKind::LockLength);
        assert!(mk[0].value.is_some());
        let e = m.set_wall_axis(a, (0.0, 0.0), (6.0, 0.0)).unwrap_err();
        let r = report(&e);
        assert!(
            r.contains(&crate::tr("modify_ops.con.breaks", &[])) && r.contains("lock_len"),
            "{r}"
        );
    }

    const ALL: [ConstraintKind; 10] = [
        ConstraintKind::LockLength,
        ConstraintKind::LockDist,
        ConstraintKind::Equal,
        ConstraintKind::Align,
        ConstraintKind::Parallel,
        ConstraintKind::Perp,
        ConstraintKind::AlignGrid,
        ConstraintKind::LockDistGrid,
        ConstraintKind::LevelGap,
        ConstraintKind::TopToLevel,
    ];

    /// Two parallel walls 5 m long, 2 m apart, one length lock and one distance lock.
    fn locked_model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "c".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let mut w = |a, b| {
            m.add_wall(&WallSpec {
                a,
                b,
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap()
        };
        let (a, b) = (w((0.0, 0.0), (5.0, 0.0)), w((0.0, 2.0), (5.0, 2.0)));
        for (kind, walls) in [
            (ConstraintKind::LockLength, vec![a]),
            (ConstraintKind::LockDist, vec![a, b]),
            (ConstraintKind::Parallel, vec![a, b]),
        ] {
            m.add_constraint(&ConstraintSpec {
                kind,
                walls,
                value: None,
            })
            .unwrap();
        }
        (m, a, b)
    }

    fn texts(m: &Model, ru: bool, u: Units) -> Vec<Option<String>> {
        markers_in(m, ru, u).into_iter().map(|k| k.value).collect()
    }

    #[test]
    fn marker_values_follow_project_units_and_language() {
        let (m, _, _) = locked_model();
        // lock of 5 m, lock of 2 m, no value on a parallel rule
        let ru = texts(&m, true, Units::Metric);
        assert_eq!(ru[0].as_deref(), Some("5\u{a0}000\u{a0}\u{43c}\u{43c}"));
        assert_eq!(ru[1].as_deref(), Some("2\u{a0}000\u{a0}\u{43c}\u{43c}"));
        assert_eq!(ru[2], None);
        let en = texts(&m, false, Units::Metric);
        assert_eq!(en[0].as_deref(), Some("5,000\u{a0}mm"));
        assert_eq!(en[1].as_deref(), Some("2,000\u{a0}mm"));
        assert_eq!(en[2], None);
        for ru in [false, true] {
            let im = texts(&m, ru, Units::Imperial);
            let five = crate::unit_text::len(5.0, ru, Units::Imperial);
            assert_eq!(im[0].as_deref(), Some(five.as_str()));
            assert!(five.starts_with("16'") && !five.contains("mm"), "{five}");
            assert_eq!(im[2], None);
        }
    }

    #[test]
    fn showing_values_never_changes_the_stored_ones() {
        let (m, _, _) = locked_model();
        let before: Vec<Option<f64>> = m.constraints().unwrap().iter().map(|c| c.1.value).collect();
        assert_eq!(before[0], Some(5.0));
        for ru in [false, true] {
            for u in [Units::Metric, Units::Imperial] {
                let _ = markers_in(&m, ru, u);
            }
        }
        let after: Vec<Option<f64>> = m.constraints().unwrap().iter().map(|c| c.1.value).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn value_text_covers_every_kind_and_sign() {
        for k in ALL {
            let locks = matches!(
                k,
                ConstraintKind::LockLength | ConstraintKind::LockDist | ConstraintKind::LockDistGrid
            );
            assert_eq!(value_text(k, Some(1.5), false, Units::Metric).is_some(), locks, "{k:?}");
            assert_eq!(value_text(k, None, false, Units::Metric), None, "{k:?}");
        }
        assert_eq!(
            value_text(ConstraintKind::LockDist, Some(-1.5), false, Units::Metric),
            value_text(ConstraintKind::LockDist, Some(1.5), false, Units::Metric)
        );
        assert_eq!(
            value_text(ConstraintKind::LockDist, Some(1.5), true, Units::Metric).as_deref(),
            Some("1\u{a0}500\u{a0}\u{43c}\u{43c}")
        );
    }

    #[test]
    fn every_kind_has_icon_title_and_its_own_tip_in_both_languages() {
        let mut tips = std::collections::HashSet::new();
        for k in ALL {
            assert!(ddc_ui::icons::source(icon(k)).is_some(), "{k:?}: {}", icon(k));
            assert!(ddc_ui::icons::source(&format!("{}.fill", icon(k))).is_some());
            for ru in [false, true] {
                let (t, tip) = (title_lang(ru, k), tip_lang(ru, k));
                assert!(!t.is_empty() && !t.contains("modify_ops."), "{k:?}: {t}");
                assert!(!tip.is_empty() && !tip.contains("constraints."), "{k:?}: {tip}");
                assert!(tips.insert((ru, tip)), "{k:?}: tip shared with another kind");
            }
            assert_ne!(tip_lang(false, k), tip_lang(true, k), "{k:?}");
        }
    }

    #[test]
    fn marker_badge_grows_with_its_value_only() {
        let bare = marker_size(0.0, 12.0);
        let valued = marker_size(40.0, 12.0);
        assert!(valued.x > bare.x + 40.0 && valued.y == bare.y);
        assert!(bare.y >= ddc_ui::theme::icon_size::SM);
    }

    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn frame(
        ctx: &egui::Context,
        input: egui::RawInput,
        kinds: &[ConstraintKind],
        ru: bool,
    ) -> (Option<ConstraintKind>, egui::Rect) {
        let rect = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        );
        let pos = crate::ui_kit::canvas_bar_pos(rect, 0);
        let mut out = (None, rect);
        let _ = ctx.run(input, |c| out = bar_at(c, pos, 1280.0 - 24.0, kinds, ru));
        out
    }

    fn input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        }
    }

    #[test]
    fn bar_fits_the_canvas_in_both_languages_at_its_slot() {
        let worst = [
            ConstraintKind::LockLength,
            ConstraintKind::AlignGrid,
            ConstraintKind::LockDistGrid,
            ConstraintKind::TopToLevel,
        ];
        for ru in [false, true] {
            let ctx = font_ctx();
            let mut r = frame(&ctx, input(), &worst, ru).1;
            for _ in 0..2 {
                r = frame(&ctx, input(), &worst, ru).1;
            }
            let slot = crate::ui_kit::canvas_bar_pos(r, 0);
            assert!(r.left() >= 0.0 && r.right() <= 1280.0 && r.bottom() <= 800.0, "{r:?}");
            assert!(r.width() > 100.0, "{r:?}");
            assert!(r.top() >= 0.0 && slot.y >= 0.0);
            // the second row of the shared slot starts below the first bar
            let row1 = crate::ui_kit::canvas_bar_pos(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            ), 1);
            assert!(row1.y >= r.bottom(), "{row1:?} {r:?}");
        }
    }

    #[test]
    fn clicking_the_first_button_reports_its_kind() {
        use egui::{Event, Modifiers, PointerButton};
        let kinds = [ConstraintKind::LockDist, ConstraintKind::Parallel];
        for ru in [false, true] {
            let ctx = font_ctx();
            let mut r = frame(&ctx, input(), &kinds, ru).1;
            for _ in 0..2 {
                r = frame(&ctx, input(), &kinds, ru).1;
            }
            let at = r.left_center() + egui::vec2(24.0, 0.0);
            let ev = |pressed: bool| {
                let mut i = input();
                i.events = vec![
                    Event::PointerMoved(at),
                    Event::PointerButton {
                        pos: at,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    },
                ];
                frame(&ctx, i, &kinds, ru).0
            };
            assert_eq!(ev(true), None);
            assert_eq!(ev(false), Some(ConstraintKind::LockDist), "ru={ru}");
        }
    }

    #[test]
    fn source_has_no_emoji_raw_buttons_or_metre_format() {
        let src = include_str!("constraints_ui.rs");
        let code = src.split("\n#[cfg(test)]").next().unwrap_or(src);
        assert!(
            !code.chars().any(|c| ('\u{1F000}'..='\u{1FFFF}').contains(&c)),
            "emoji glyph in constraints_ui.rs"
        );
        let raw_metres = ["{", ":.3}"].concat();
        let raw_button = ["ui", ".button("].concat();
        let hover = [".on_hover", "_text("].concat();
        let pad = ["vec2(12.0", ", 12.0)"].concat();
        let pad6 = ["Vec2::new(6.0", ", 2.0)"].concat();
        for bad in [raw_metres, raw_button, hover, pad, pad6] {
            assert!(!code.contains(&bad), "{bad}");
        }
        assert!(code.contains("canvas_bar_pos"));
    }
}

/// Pin chip of the drawing tool: its icon is the rule the current snap would write; filled when armed.
pub fn paint_pin(
    painter: &eframe::egui::Painter,
    ed: &Editor,
    to_s: &dyn Fn(V2) -> eframe::egui::Pos2,
    t: &crate::ui_kit::Tokens,
) {
    use ddc_ui::theme::{icon_size, radius, space, stroke};
    use eframe::egui::{pos2, vec2, Rect, Stroke, StrokeKind};
    let (Some(at), Some(sn)) = (ed.tool.pin_chip(), ed.tool.snap()) else { return };
    let Some(kind) = crate::snap_pin::pin_kind(sn.kind) else { return };
    let r = Rect::from_center_size(to_s(at), vec2(icon_size::SM + 2.0 * space::XS, icon_size::SM + 2.0 * space::XS));
    let armed = ed.tool.pin_armed.is_some();
    painter.rect_filled(r, radius::SM, if armed { t.accent } else { t.paper });
    painter.rect_stroke(r, radius::SM, Stroke::new(stroke::HAIRLINE, t.accent), StrokeKind::Inside);
    let tex = ddc_ui::widgets::with_icons(painter.ctx(), |icons| icons.get(painter.ctx(), icon(kind), icon_size::SM as u32));
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    painter.image(tex.id(), Rect::from_center_size(r.center(), vec2(icon_size::SM, icon_size::SM)), uv, if armed { t.paper } else { t.accent });
}
