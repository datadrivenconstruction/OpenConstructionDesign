// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Overwrite type values": before a load replaces project type values, one question names
//! the types and counts the values and placed instances. When nothing would change, nothing
//! is asked. The tick is cleared by every committed load (see `load_set`).

use ddc_model::TypeOverwrite;
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText};

use super::{Ask, Session};
use crate::ui_kit as kit;

/// Type names listed in the question; the rest is counted.
pub(super) const LIST_CAP: usize = 6;

/// The load that waits for the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    /// Load button of the document with this serial.
    Load(u64),
    /// Load on the close question.
    Close(Ask),
    /// Save on the project's unsaved-changes question: the project action waits for it.
    Project,
}

/// What the overwrite question raised by the project's Save ended in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    /// Replace: every edited family is in the model, the project save may run.
    Go,
    /// Back (or Esc): nothing loaded, the edits stay open, the project action is dropped.
    Back,
    /// Replace, but the model refused a family: nothing loaded, the family says why.
    Refused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwAsk {
    pub then: Then,
    /// First document that replaces values: shown when the user goes back.
    pub first: u64,
    /// Replaced types (`Family: Type` when several families load).
    pub names: Vec<String>,
    pub values: usize,
    pub instances: usize,
}

/// Question for the ticked documents among `docs`; `None` when no project value changes.
pub(super) fn ask<'a>(
    docs: impl IntoIterator<Item = &'a Session>,
    then: Then,
    preview: &mut dyn FnMut(&str) -> Vec<TypeOverwrite>,
) -> Option<OwAsk> {
    let mut found: Vec<(u64, String, Vec<TypeOverwrite>)> = vec![];
    for d in docs {
        if !d.overwrite_types {
            continue;
        }
        let ow = preview(d.doc.text());
        if !ow.is_empty() {
            found.push((d.serial, d.name(), ow));
        }
    }
    let first = found.first()?.0;
    let many = found.len() > 1;
    let mut a = OwAsk {
        then,
        first,
        names: vec![],
        values: 0,
        instances: 0,
    };
    for (_, fam, ow) in found {
        for t in ow {
            a.values += t.values;
            a.instances += t.instances;
            a.names.push(if many {
                format!("{fam}: {}", t.name)
            } else {
                t.name
            });
        }
    }
    Some(a)
}

/// Counts line and the capped list of type names.
pub(super) fn body(cat: &Catalog, a: &OwAsk) -> String {
    let n = |k: &str, v: usize| cat.plural(k, v as u64);
    let head = cat.tf(
        "family_overwrite.ow.body",
        &[
            ("values", &n("family_overwrite.ow.values", a.values)),
            ("types", &n("family_overwrite.ow.types", a.names.len())),
            ("instances", &a.instances.to_string()),
        ],
    );
    let mut list = a.names[..a.names.len().min(LIST_CAP)].join(", ");
    if a.names.len() > LIST_CAP {
        let more = (a.names.len() - LIST_CAP).to_string();
        list = format!("{list} {}", cat.tf("family_overwrite.ow.more", &[("n", &more)]));
    }
    let text = format!("{head}\n{}", cat.tf("family_overwrite.ow.list", &[("list", &list)]));
    if a.then == Then::Project {
        format!("{text}\n{}", cat.t("drafts_overwrite.ow.project"))
    } else {
        text
    }
}

/// true = replace the values, false = back to editing (also Esc and the backdrop).
pub(super) fn modal(ctx: &egui::Context, th: &Theme, cat: &Catalog, a: &OwAsk) -> Option<bool> {
    let text = body(cat, a);
    let mut pick = None;
    let m = kit::modal_dialog("family_overwrite.ow", ctx, |ui| {
        ui.set_width(ddc_ui::theme::layout::DIALOG_TOOL);
        kit::modal_title(ui, cat.t("family_overwrite.ow.title"));
        ui.label(RichText::new(text).color(th.c("text.secondary")));
        kit::dialog_footer(ui, |ui| {
            if kit::primary_action(ui, cat.t("family_overwrite.ow.back"), None) {
                pick = Some(false);
            }
            if kit::confirm_danger_button(ui, cat.t("family_overwrite.ow.replace"), true).clicked() {
                pick = Some(true);
            }
        });
    });
    if pick.is_none() && m.should_close() {
        pick = Some(false);
    }
    pick
}

/// The question as the project's Save raises it, drawn alone (layout contract).
#[cfg(test)]
pub(crate) fn layout_project_modal(ctx: &egui::Context, cat: &Catalog) -> Option<bool> {
    let th = Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let a = OwAsk {
        then: Then::Project,
        first: 1,
        names: (0..LIST_CAP + 2).map(|i| format!("Type {i}")).collect(),
        values: 12,
        instances: 40,
    };
    modal(ctx, &th, cat, &a)
}

#[cfg(test)]
mod tests {
    use super::super::{confirm_overwrite, load_doc, State};
    use super::*;
    use crate::editor::Editor;
    use ddc_geom::component::doc::FamDoc;
    use ddc_geom::component::Value;
    use ddc_model::Model;
    use ddc_ui::Lang;

    const BOX: &str = "family Crate\ncategory generic\nparam W length type 1m\n\
        ref L x ~ -400mm\nref R x ~ 400mm\nref C x = 0\neq L C R\ndim W L R\n\
        extrude B rect(L, 0, R, 1m) z(0, 1m)\ntype \"Std\" W=1m\n";

    fn ow(name: &str, values: usize, instances: usize) -> TypeOverwrite {
        TypeOverwrite {
            name: name.into(),
            values,
            instances,
        }
    }

    fn sess(def: &str, tick: bool) -> Session {
        let mut s = Session::new(FamDoc::new(def.to_string()), None);
        s.doc.dirty = true;
        s.overwrite_types = tick;
        s
    }

    fn q(names: usize, values: usize, instances: usize) -> OwAsk {
        OwAsk {
            then: Then::Load(1),
            first: 1,
            names: (0..names).map(|i| format!("T{i}")).collect(),
            values,
            instances,
        }
    }

    #[test]
    fn counts_shown_in_en_and_ru() {
        let (en, ru) = (Catalog::new(Lang::En), Catalog::new(Lang::Ru));
        let a = q(2, 3, 5);
        let e = body(&en, &a);
        assert!(e.contains("3 values") && e.contains("2 types") && e.contains('5'), "{e}");
        assert!(e.contains("T0, T1"), "{e}");
        let r = body(&ru, &a);
        // ru: "3 values", "2 types", "1 value", "1 type"
        let (v3, t2) = (
            "3 \u{437}\u{43d}\u{430}\u{447}\u{435}\u{43d}\u{438}\u{44f}",
            "2 \u{442}\u{438}\u{43f}\u{430}\u{445}",
        );
        assert!(r.contains(v3) && r.contains(t2) && r.contains('5'), "{r}");
        let one = body(&ru, &q(1, 1, 0));
        let (v1, t1) = (
            "1 \u{437}\u{43d}\u{430}\u{447}\u{435}\u{43d}\u{438}\u{435}",
            "1 \u{442}\u{438}\u{43f}\u{435}",
        );
        assert!(one.contains(v1) && one.contains(t1), "{one}");
        // the list is capped, the rest counted
        let a = q(LIST_CAP + 2, 9, 0);
        let e = body(&en, &a);
        assert!(e.contains(&format!("T{}", LIST_CAP - 1)), "{e}");
        assert!(!e.contains(&format!("T{LIST_CAP}")), "{e}");
        assert!(e.contains("and 2 more"), "{e}");
        assert!(body(&ru, &a).contains("\u{438} \u{435}\u{449}\u{451} 2"));
        assert_eq!(en.lang, Lang::En, "catalogs are explicit; no global language flip");
    }

    #[test]
    fn no_change_or_no_tick_asks_nothing() {
        let off = sess(BOX, false);
        let mut calls = 0;
        let r = ask([&off], Then::Load(off.serial), &mut |_| {
            calls += 1;
            vec![ow("Std", 1, 1)]
        });
        assert_eq!((r, calls), (None, 0), "tick off: no preview, no question");
        let on = sess(BOX, true);
        assert_eq!(ask([&on], Then::Load(on.serial), &mut |_| vec![]), None);
        let a = ask([&on], Then::Load(on.serial), &mut |_| {
            vec![ow("Std", 2, 3), ow("Big", 1, 0)]
        })
        .unwrap();
        assert_eq!(a.names, vec!["Std", "Big"]);
        assert_eq!((a.values, a.instances, a.first), (3, 3, on.serial));
        // several families: names say which family
        let two = sess(&BOX.replace("Crate", "Bin"), true);
        let a = ask([&on, &two], Then::Close(Ask::Window), &mut |_| vec![ow("Std", 1, 1)])
            .unwrap();
        assert_eq!(a.names, vec!["Crate: Std", "Bin: Std"]);
    }

    #[test]
    fn back_loads_nothing_replace_loads_once() {
        let mut st = State {
            open: true,
            docs: vec![sess(BOX, true), sess(BOX, true)],
            ..Default::default()
        };
        let sn = st.docs[1].serial;
        let a = q(1, 1, 1);
        let a = OwAsk {
            then: Then::Close(Ask::Tab(sn)),
            first: sn,
            ..a
        };
        confirm_overwrite(&mut st, a.clone(), false, &mut |_, _| panic!("Back must not load"));
        assert_eq!((st.docs.len(), st.active, st.ask), (2, 1, None));
        let mut loaded = vec![];
        confirm_overwrite(&mut st, a, true, &mut |d, w| {
            loaded.extend(w.iter().map(|&i| d[i].serial));
            Ok(())
        });
        assert_eq!(loaded, vec![sn], "the close question's Load runs once");
        assert_eq!(st.docs.len(), 1);
    }

    #[test]
    fn project_question_says_the_save_waits_and_reports_the_answer() {
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            let line = cat.t("drafts_overwrite.ow.project");
            let a = OwAsk {
                then: Then::Project,
                ..q(1, 1, 1)
            };
            assert!(body(&cat, &a).contains(&line), "{lang:?}");
            assert!(!body(&cat, &q(1, 1, 1)).contains(&line), "{lang:?}: only from the project");
        }
        let mut st = State {
            open: true,
            docs: vec![sess(BOX, true)],
            ..Default::default()
        };
        let a = OwAsk {
            then: Then::Project,
            first: st.docs[0].serial,
            ..q(1, 1, 1)
        };
        confirm_overwrite(&mut st, a.clone(), false, &mut |_, _| panic!("Back must not load"));
        assert_eq!((st.resume, st.docs.len()), (Some(Resume::Back), 1));
        let mut n = 0;
        confirm_overwrite(&mut st, a.clone(), true, &mut |_, w| {
            n += w.len();
            Ok(())
        });
        assert_eq!((n, st.resume), (1, Some(Resume::Go)), "Replace loads every edit once");
        assert!(st.open && st.docs.len() == 1, "the window stays until the file is written");
        confirm_overwrite(&mut st, a, true, &mut |_, w| Err(w[0]));
        assert_eq!(st.resume, Some(Resume::Refused));
        assert!(st.docs[0].doc.dirty, "a refusal keeps the edits");
    }

    fn std_w(m: &Model, fam: i64) -> f64 {
        match m.family_types(fam).unwrap()[0].values.as_slice() {
            [(k, Value::Num(v))] if k == "W" => *v,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn load_clears_tick_and_one_undo_restores_values() {
        let mut ed = Editor::seeded(Model::open(":memory:").unwrap()).unwrap();
        let cat = Catalog::new(Lang::En);
        let mut s = sess(BOX, false);
        assert!(load_doc(&mut ed, &cat, &mut s));
        let fam = s.family.unwrap();
        let wide = BOX.replace("W=1m", "W=2m");
        let mut s = sess(&wide, true);
        let m = &ed.model;
        let a = ask([&s], Then::Load(s.serial), &mut |d| m.type_overwrites(d).unwrap())
            .expect("a differing value asks");
        assert_eq!((a.names.len(), a.values), (1, 1));
        assert!((std_w(&ed.model, fam) - 1.0).abs() < 1e-9, "asking writes nothing");
        assert!(load_doc(&mut ed, &cat, &mut s));
        assert!(!s.overwrite_types, "tick is off after the load");
        assert!((std_w(&ed.model, fam) - 2.0).abs() < 1e-9);
        ed.undo();
        assert!((std_w(&ed.model, fam) - 1.0).abs() < 1e-9, "one undo restores the value");
    }
}
