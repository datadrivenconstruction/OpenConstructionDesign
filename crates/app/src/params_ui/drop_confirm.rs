//! Save of a parameter edit that unticks categories with values: a confirm names each
//! category and its value count; Cancel keeps everything, Confirm saves as one undo step.

use super::{category_name, plural, run};
use crate::editor::Editor;
use crate::ui_kit as kit;
use ddc_model::ParamSpec;

/// An edit whose Save waits for the answer: (category, values lost) in the parameter's order.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Pending {
    pub id: i64,
    pub spec: ParamSpec,
    pub drops: Vec<(String, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Ask {
    Confirm,
    Cancel,
}

/// Save of an edit. No value would go: saves now (Some). Otherwise parks it for the confirm
/// and returns None; the model is not touched.
pub(super) fn save_edit(
    ed: &mut Editor,
    pending: &mut Option<Pending>,
    id: i64,
    spec: ParamSpec,
) -> Option<Result<(), String>> {
    match ed.model.dropped_values(id, &spec.categories) {
        Err(e) => Some(Err(e.to_string())),
        Ok(drops) if drops.is_empty() => Some(run(ed, move |m| m.edit_param(id, &spec))),
        Ok(drops) => {
            *pending = Some(Pending { id, spec, drops });
            None
        }
    }
}

/// Applies the answer. Cancel: nothing changes (None). Confirm: the edit runs (Some).
pub(super) fn answer(
    ed: &mut Editor,
    pending: &mut Option<Pending>,
    ask: Ask,
) -> Option<Result<(), String>> {
    let Pending { id, spec, .. } = pending.take()?;
    match ask {
        Ask::Cancel => None,
        Ask::Confirm => Some(run(ed, move |m| m.edit_param(id, &spec))),
    }
}

/// One line per unticked category: its name and how many values go.
pub(super) fn lines(p: &Pending, ru: bool) -> Vec<String> {
    p.drops
        .iter()
        .map(|(c, n)| {
            crate::tr_lang(
                ru,
                "param_drop.line",
                &[
                    ("category", category_name(c, ru)),
                    ("values", &plural(ru, "params_list.n_values", *n)),
                ],
            )
        })
        .collect()
}

/// The modal; the answer of this frame, if any. Esc is Cancel; Enter never confirms.
pub(super) fn modal(ctx: &egui::Context, p: &Pending, ru: bool) -> Option<Ask> {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let th = ddc_ui::theme::of(ctx);
    let mut ev = None;
    let out = kit::modal_dialog("xs45.drop", ctx, |ui| {
        ui.set_width(th.d().row * 16.0);
        kit::modal_title(ui, tr("param_drop.title"));
        ui.label(crate::tr_lang(ru, "param_drop.intro", &[("name", &p.spec.name)]));
        for l in lines(p, ru) {
            ui.label(l);
        }
        ui.label(egui::RichText::new(tr("param_drop.undo")).color(th.sem().text_secondary));
        kit::dialog_footer(ui, |ui| {
            if kit::confirm_danger_button(ui, tr("param_drop.confirm"), true).clicked() {
                ev = Some(Ask::Confirm);
            }
            if kit::cancel_button(ui, tr("param_drop.cancel")).clicked() {
                ev = Some(Ask::Cancel);
            }
        });
    });
    if out.should_close() {
        ev = Some(Ask::Cancel);
    }
    ev
}

/// Draws the pending confirm and applies its answer; Some when an edit ran.
pub(super) fn show(
    ctx: &egui::Context,
    ed: &mut Editor,
    pending: &mut Option<Pending>,
    ru: bool,
) -> Option<Result<(), String>> {
    let ask = modal(ctx, pending.as_ref()?, ru)?;
    answer(ed, pending, ask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{LayerSpec, OpeningSpec, PVal, ParamSet, ParamType, WallSpec};

    /// Context with the named font weights: a bare one panics on the first labelled button.
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn enter() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        }
    }

    struct Fx {
        ed: Editor,
        pid: i64,
        doors: Vec<i64>,
        win: i64,
        wall: i64,
    }

    /// "Mark" on walls, doors and windows: 3 door values, 1 window value, 1 wall value.
    fn fixture() -> Fx {
        let mut ed = Editor::new_in_memory().unwrap();
        let m = &mut ed.model;
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let wall = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (5.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        let mut open = |along: f64, kind: &str| {
            m.add_opening(
                wall,
                &OpeningSpec {
                    along,
                    width: 0.9,
                    sill: 0.0,
                    height: 2.1,
                    kind: kind.into(),
                },
            )
            .unwrap()
        };
        let doors = vec![open(0.5, "door"), open(1.5, "door"), open(2.5, "door")];
        let win = open(3.8, "window");
        let pid = m.add_param(&spec(&["wall", "door", "window"])).unwrap();
        let mut sets: Vec<ParamSet> = doors
            .iter()
            .chain([&win])
            .map(|d| ParamSet {
                param: pid,
                kind: "opening".into(),
                id: *d,
                value: Some(PVal::Text(format!("V{d}"))),
            })
            .collect();
        sets.push(ParamSet {
            param: pid,
            kind: "wall".into(),
            id: wall,
            value: Some(PVal::Text("A1".into())),
        });
        m.set_param_values(&sets).unwrap();
        Fx {
            ed,
            pid,
            doors,
            win,
            wall,
        }
    }

    fn spec(cats: &[&str]) -> ParamSpec {
        ParamSpec {
            name: "Mark".into(),
            ptype: ParamType::Text,
            per_type: false,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: cats.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn count(f: &Fx) -> usize {
        f.ed.model.param_values(f.pid).unwrap().len()
    }

    #[test]
    fn untick_with_values_asks_with_the_count_and_changes_nothing() {
        let mut f = fixture();
        let mut pending = None;
        let r = save_edit(&mut f.ed, &mut pending, f.pid, spec(&["wall", "window"]));
        assert!(r.is_none(), "Save must wait for the confirm");
        let p = pending.clone().expect("confirm shown");
        assert_eq!(p.drops, vec![("door".to_string(), 3)]);
        assert_eq!(count(&f), 5);
        assert_eq!(f.ed.model.params().unwrap()[0].spec.categories.len(), 3);
        for ru in [false, true] {
            let l = lines(&p, ru);
            assert_eq!(l.len(), 1);
            assert!(l[0].contains(category_name("door", ru)) && l[0].contains('3'), "{l:?}");
            assert!(!l[0].contains('{'), "{l:?}");
        }
    }

    #[test]
    fn cancel_keeps_values_and_categories() {
        let mut f = fixture();
        let mut pending = None;
        let _ = save_edit(&mut f.ed, &mut pending, f.pid, spec(&["wall"]));
        assert_eq!(
            pending.as_ref().unwrap().drops,
            vec![("door".to_string(), 3), ("window".to_string(), 1)]
        );
        assert!(answer(&mut f.ed, &mut pending, Ask::Cancel).is_none());
        assert!(pending.is_none());
        assert_eq!(count(&f), 5);
        assert_eq!(f.ed.model.params().unwrap()[0].spec.categories.len(), 3);
    }

    #[test]
    fn confirm_removes_in_one_undo_step_and_spares_other_categories() {
        let mut f = fixture();
        let mut pending = None;
        let _ = save_edit(&mut f.ed, &mut pending, f.pid, spec(&["wall", "window"]));
        answer(&mut f.ed, &mut pending, Ask::Confirm).unwrap().unwrap();
        assert!(pending.is_none());
        let left = f.ed.model.param_values(f.pid).unwrap();
        assert_eq!(left.len(), 2);
        assert!(left.contains_key(&("opening".to_string(), f.win)));
        assert!(left.contains_key(&("wall".to_string(), f.wall)));
        f.ed.undo();
        for d in &f.doors {
            assert_eq!(
                f.ed.model.param_value(f.pid, "opening", *d).unwrap(),
                Some(PVal::Text(format!("V{d}")))
            );
        }
        assert_eq!(count(&f), 5);
        assert_eq!(f.ed.model.params().unwrap()[0].spec.categories.len(), 3);
    }

    #[test]
    fn untick_without_values_saves_without_asking() {
        let mut f = fixture();
        let sets: Vec<ParamSet> = f
            .doors
            .iter()
            .map(|d| ParamSet {
                param: f.pid,
                kind: "opening".into(),
                id: *d,
                value: None,
            })
            .collect();
        f.ed.model.set_param_values(&sets).unwrap();
        let mut pending = None;
        let r = save_edit(&mut f.ed, &mut pending, f.pid, spec(&["wall", "window"]));
        assert!(pending.is_none());
        r.expect("saved at once").unwrap();
        assert_eq!(f.ed.model.params().unwrap()[0].spec.categories.len(), 2);
        assert_eq!(count(&f), 2);
    }

    #[test]
    fn modal_draws_in_both_languages_and_enter_does_not_confirm() {
        let mut f = fixture();
        for ru in [false, true] {
            let ctx = font_ctx();
            let mut pending = None;
            let _ = save_edit(&mut f.ed, &mut pending, f.pid, spec(&["wall", "window"]));
            for _ in 0..3 {
                let _ = ctx.run(enter(), |c| {
                    assert!(show(c, &mut f.ed, &mut pending, ru).is_none());
                });
            }
            assert!(pending.is_some(), "Enter must not answer the confirm");
            assert_eq!(count(&f), 5);
            assert!(crate::tr_str(ru, "param_drop.title") != "param_drop.title");
        }
    }

    #[test]
    fn strings_exist_in_both_languages() {
        for key in ["param_drop.title", "param_drop.intro", "param_drop.line", "param_drop.undo", "param_drop.confirm", "param_drop.cancel"] {
            let (en, ru) = (crate::tr_str(false, key), crate::tr_str(true, key));
            assert_ne!(en, key);
            assert_ne!(ru, key);
            assert_ne!(en, ru, "{key} not translated");
        }
    }
}
