// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Origin of the selected element in the properties: what made it (a recipe and its part, an AI
//! batch with its plan text, a script, an import or a person), whether a rerun of its recipe
//! still rebuilds it, and how many of its values the assistant filled without a check.

use crate::editor::Editor;
use ddc_ops::Actor;
use ddc_ui::theme::Role;

/// What the block shows, read once per cache key: the journal lookup scans the `op` table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Origin {
    /// Recipe name, part key, the part was edited by hand since the recipe built it.
    pub recipe: Option<(String, String, bool)>,
    pub made: Option<ddc_model::MadeBy>,
    /// (source file, GUID) of an imported element.
    pub import: Option<(String, String)>,
    /// Values set by the assistant, of them not verified.
    pub ai_values: (usize, usize),
}

pub fn read(ed: &Editor, kind: &str, id: i64) -> Origin {
    let m = &ed.model;
    let recipe = m.recipe_of(kind, id).ok().flatten().and_then(|(rid, key)| {
        let r = m.recipe(rid).ok().flatten()?;
        let edited = m.recipe_held(rid).is_ok_and(|h| h.edited.contains(&key));
        Some((r.name, key, edited))
    });
    let srcs = m.value_srcs_of(&[(kind.to_string(), id)]).unwrap_or_default();
    let ai: Vec<_> = srcs.iter().filter(|(_, s)| s.kind.by_ai()).collect();
    Origin {
        recipe,
        made: m.made_by(kind, id).ok().flatten(),
        import: m.ext_guid(kind, id).ok().flatten(),
        ai_values: (ai.len(), ai.iter().filter(|(_, s)| !s.verified()).count()),
    }
}

/// "Made by" line: the most specific source known.
pub fn made_line(o: &Origin, ru: bool) -> String {
    let t = |k: &'static str| crate::tr_str(ru, k).to_string();
    if let Some((name, key, _)) = &o.recipe {
        return crate::tr_lang(ru, "origin.recipe", &[("name", name), ("part", key)]);
    }
    let Some(made) = &o.made else {
        return match &o.import {
            Some((src, _)) if !src.is_empty() => crate::tr_lang(ru, "origin.import_file", &[("file", src)]),
            Some(_) => t("origin.import"),
            None => t("origin.unknown"),
        };
    };
    match &made.actor {
        Some(Actor::Ai { model, .. }) => crate::tr_lang(ru, "origin.ai", &[("model", model)]),
        Some(Actor::Agent { name }) => crate::tr_lang(ru, "origin.agent", &[("name", name)]),
        Some(Actor::Script { name }) => crate::tr_lang(ru, "origin.script", &[("name", name)]),
        Some(Actor::Xlsx) => t("origin.xlsx"),
        Some(Actor::User) => t("origin.user"),
        None if o.import.is_some() => t("origin.import"),
        None => t("origin.unknown"),
    }
}

pub fn props(ui: &mut egui::Ui, ed: &mut Editor, ru: bool) {
    let Some((kind, id)) = ed.selection.first().and_then(|s| crate::phase_ui::sel_ref(*s)) else {
        return;
    };
    let rk = ed.cache_key();
    let ck = egui::Id::new(("ddc.props.origin", kind, id));
    let hit = ui.data(|d| d.get_temp::<((u64, u64), Origin)>(ck)).filter(|c| c.0 == rk);
    let o = match hit {
        Some(c) => c.1,
        None => {
            let o = read(ed, kind, id);
            ui.data_mut(|d| d.insert_temp(ck, (rk, o.clone())));
            o
        }
    };
    // nothing known: a trimmed journal would make this "not in the history" on most elements
    if o.recipe.is_none() && o.made.is_none() && o.import.is_none() && o.ai_values.0 == 0 {
        return;
    }
    if !crate::ui_kit::heading(ui, crate::tr_str(ru, "origin.title")) {
        return;
    }
    let th = ddc_ui::theme::of(ui.ctx());
    let made = made_line(&o, ru);
    crate::ui_kit::props(ui, "sel.origin", |ui| {
        crate::ui_kit::kv(ui, crate::tr_str(ru, "origin.made_by"), made.clone());
        if let Some(m) = &o.made {
            let what = m.ai_text.as_deref().or(m.title.as_deref()).unwrap_or_default();
            let first = what.lines().next().unwrap_or_default().trim();
            if !first.is_empty() {
                crate::ui_kit::kv(ui, crate::tr_str(ru, "origin.request"), first.to_string());
            }
            if let Some(at) = m.ai_at {
                crate::ui_kit::kv(ui, crate::tr_str(ru, "origin.at"), crate::source_ui::date(at));
            }
        }
        if let Some((_, guid)) = &o.import {
            crate::ui_kit::kv(ui, "GUID", guid.clone());
        }
    });
    if let Some((_, _, edited)) = &o.recipe {
        let (key, color) = if *edited {
            ("origin.held", th.c("warning"))
        } else {
            ("origin.rebuilt", crate::ui_kit::weak(ui))
        };
        ui.label(th.rich(Role::Caption, crate::tr_str(ru, key)).color(color));
    }
    let (n, open) = o.ai_values;
    if n > 0 {
        let (sn, so) = (n.to_string(), open.to_string());
        let msg = crate::tr_lang(ru, "origin.ai_values", &[("n", &sn), ("open", &so)]);
        let color = if open > 0 { th.c("warning") } else { crate::ui_kit::weak(ui) };
        ui.label(th.rich(Role::Caption, &msg).color(color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(actor: Option<Actor>) -> Origin {
        Origin {
            made: Some(ddc_model::MadeBy {
                op_seq: 1,
                actor,
                title: None,
                why: None,
                ai_at: None,
                ai_text: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn the_recipe_names_the_part_before_the_journal() {
        let mut o = origin(Some(Actor::User));
        o.recipe = Some(("Frame".into(), "post#1".into(), false));
        assert!(made_line(&o, false).contains("post#1"));
    }

    #[test]
    fn each_actor_has_its_own_line_in_both_languages() {
        let ai = Actor::Ai { session: "s".into(), model: "m1".into() };
        for ru in [false, true] {
            let lines: Vec<String> = [Some(ai.clone()), Some(Actor::User), Some(Actor::Xlsx), None]
                .into_iter()
                .map(|a| made_line(&origin(a), ru))
                .collect();
            assert!(lines[0].contains("m1"), "{lines:?}");
            let set: std::collections::BTreeSet<_> = lines.iter().collect();
            assert_eq!(set.len(), 4, "{lines:?}");
            assert!(lines.iter().all(|l| !l.contains("origin.")), "{lines:?}");
        }
    }

    #[test]
    fn a_wall_with_assistant_values_counts_them() {
        let (ed, w1, _) = crate::value_src_ui::test_editor();
        let o = read(&ed, "wall", w1);
        assert_eq!(o.recipe, None);
        assert!(o.ai_values.0 > 0 && o.ai_values.1 > 0, "{o:?}");
    }
}
