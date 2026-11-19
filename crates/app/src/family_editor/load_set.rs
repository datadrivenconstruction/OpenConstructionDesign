// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Loading edited families into the project as one step: every definition is checked first,
//! then one `run_op` writes them all (one undo step). Any refusal writes nothing, names the
//! family and leaves every document with its edits.

use std::sync::{Arc, Mutex};

use ddc_geom::component::{Family, Lod};
use ddc_model::FamilyLoad;
use ddc_ui::Catalog;

use super::Session;
use crate::editor::{Editor, OpFn};

/// The checks `load_family_def` makes before it writes: it parses, every type resolves and builds.
pub(super) fn check_def(def: &str) -> Result<(), String> {
    let f = Family::parse(def).map_err(|e| e.to_string())?;
    for t in &f.types {
        let r = f.resolve(&t.values, &[]).map_err(|e| e.to_string())?;
        f.build(&r, Lod::Medium).map_err(|e| e.to_string())?;
    }
    if f.types.is_empty() {
        let r = f.resolve(&[], &[]).map_err(|e| e.to_string())?;
        f.build(&r, Lod::Medium).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// One model op loading every (definition, overwrite types) in order. A refusal rolls all of
/// them back (the op is one model step) and returns its position with the reason.
pub(super) fn commit(
    ed: &mut Editor,
    defs: Vec<(String, bool)>,
) -> Result<Vec<FamilyLoad>, (usize, String)> {
    let slot = Arc::new(Mutex::new((Vec::new(), None::<usize>)));
    let out = slot.clone();
    let op = OpFn(Arc::new(move |m: &mut ddc_model::Model| {
        let mut done = Vec::with_capacity(defs.len());
        for (k, (def, ow)) in defs.iter().enumerate() {
            match m.load_family_def(def, *ow) {
                Ok(r) => done.push(r),
                Err(e) => {
                    if let Ok(mut g) = out.lock() {
                        *g = (Vec::new(), Some(k));
                    }
                    return Err(e);
                }
            }
        }
        if let Ok(mut g) = out.lock() {
            *g = (done, None);
        }
        Ok(())
    }));
    let r = ed.run_op(op);
    let (reps, failed) = slot.lock().map(|g| g.clone()).unwrap_or_default();
    match r {
        Ok(()) => Ok(reps),
        Err(e) => Err((failed.unwrap_or(0), e.to_string())),
    }
}

/// Loads `docs[i]` for every `i` in `which` as one undo step. Err is the index of the refused
/// family: then nothing was written and every document keeps its edits.
pub(super) fn load_set(
    ed: &mut Editor,
    cat: &Catalog,
    docs: &mut [Session],
    which: &[usize],
) -> Result<(), usize> {
    let Some(&first) = which.first() else {
        return Ok(());
    };
    for &i in which {
        let text = docs[i].doc.text().to_string();
        if let Some(e) = docs[i].ai.as_mut().and_then(|m| m.blocked(&text)) {
            refuse(ed, cat, &mut docs[i], &e);
            return Err(i);
        }
        if let Err(e) = check_def(docs[i].doc.text()) {
            refuse(ed, cat, &mut docs[i], &e);
            return Err(i);
        }
    }
    let defs = which
        .iter()
        .map(|&i| (docs[i].doc.text().to_string(), docs[i].overwrite_types))
        .collect();
    let reps = match commit(ed, defs) {
        Ok(r) => r,
        Err((k, e)) => {
            let i = which.get(k).copied().unwrap_or(first);
            refuse(ed, cat, &mut docs[i], &e);
            return Err(i);
        }
    };
    let mut names = Vec::with_capacity(which.len());
    let mut last = String::new();
    for (k, &i) in which.iter().enumerate() {
        let s = &mut docs[i];
        let name = s.name();
        let rep = reps.get(k);
        if let Some(r) = rep {
            s.family = Some(r.family);
        }
        s.doc.dirty = false;
        // the tick is for one load: the next load keeps project values unless ticked again
        s.overwrite_types = false;
        last = loaded_msg(cat, &name, rep);
        s.msg = Some((false, last.clone()));
        names.push(name);
    }
    ed.status = Some(if names.len() == 1 {
        last
    } else {
        let n = names.len().to_string();
        cat.tf("family_load.loaded_all", &[("n", &n), ("names", &names.join(", "))])
    });
    Ok(())
}

fn refuse(ed: &mut Editor, cat: &Catalog, s: &mut Session, why: &str) {
    let m = cat.tf("family_load.refused", &[("name", &s.name()), ("msg", why)]);
    s.msg = Some((true, m.clone()));
    ed.status = Some(m);
}

fn loaded_msg(cat: &Catalog, name: &str, rep: Option<&FamilyLoad>) -> String {
    let Some(rep) = rep else {
        return cat.tf("fe.reloaded", &[("name", name), ("n", "?"), ("types", "?")]);
    };
    let key = if rep.created {
        "fe.loaded_new"
    } else {
        "fe.reloaded"
    };
    let (n, types) = (rep.instances.to_string(), rep.types.to_string());
    let m = cat.tf(key, &[("name", name), ("n", &n), ("types", &types)]);
    if rep.nested > 0 {
        let n = cat.tf("fe.nested_updated", &[("n", &rep.nested.to_string())]);
        format!("{m} {n}")
    } else {
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::component::doc::FamDoc;
    use ddc_geom::component::{Category, Hosting};
    use ddc_ui::Lang;

    const BROKEN: &str = "family Broken\nnonsense\n";

    fn def(name: &str) -> String {
        let t = FamDoc::blank(name, Category::Generic, Hosting::None).text().to_string();
        format!("{t}extrude B rect(0, 0, 1m, 1m) z(0, 1m)\n")
    }

    fn edited(text: &str) -> Session {
        let mut s = Session::new(FamDoc::new(text), None);
        s.doc.dirty = true;
        s
    }

    fn names(ed: &Editor) -> Vec<String> {
        let mut v: Vec<String> = ed.model.families().unwrap().into_iter().map(|f| f.1).collect();
        v.sort();
        v
    }

    fn has(ed: &Editor, n: &str) -> bool {
        names(ed).iter().any(|f| f == n)
    }

    #[test]
    fn definitions_are_checked_like_the_model() {
        assert!(check_def(&def("Alpha")).is_ok());
        assert!(check_def(BROKEN).is_err());
    }

    #[test]
    fn two_families_load_as_one_undo_step() {
        let cat = Catalog::new(Lang::En);
        let mut ed = Editor::new_in_memory().unwrap();
        let mut docs = vec![edited(&def("Alpha")), edited(&def("Gamma")), edited(&def("Beta"))];
        assert_eq!(load_set(&mut ed, &cat, &mut docs, &[0, 2]), Ok(()));
        assert!(has(&ed, "Alpha") && has(&ed, "Beta") && !has(&ed, "Gamma"));
        assert!(!docs[0].doc.dirty && !docs[2].doc.dirty, "loaded documents are clean");
        assert!(docs[1].doc.dirty, "a document outside the set is untouched");
        assert!(docs[0].family.is_some() && docs[2].family.is_some());
        let st = ed.status.clone().unwrap_or_default();
        assert!(st.contains("Alpha") && st.contains("Beta"), "{st}");
        // one Undo takes back both families, one Redo brings both back
        ed.undo();
        assert!(!has(&ed, "Alpha") && !has(&ed, "Beta"), "{:?}", names(&ed));
        ed.redo();
        assert!(has(&ed, "Alpha") && has(&ed, "Beta"), "{:?}", names(&ed));
    }

    #[test]
    fn refusal_in_the_middle_writes_nothing_and_keeps_edits() {
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            let mut ed = Editor::new_in_memory().unwrap();
            let (before, undo) = (names(&ed), ed.can_undo());
            let texts = [def("Alpha"), BROKEN.to_string(), def("Beta")];
            let mut docs: Vec<Session> = texts.iter().map(|t| edited(t)).collect();
            assert_eq!(load_set(&mut ed, &cat, &mut docs, &[0, 1, 2]), Err(1));
            assert_eq!(names(&ed), before, "model unchanged");
            assert_eq!(ed.can_undo(), undo, "no undo step");
            for (d, t) in docs.iter().zip(&texts) {
                assert!(d.doc.dirty && d.family.is_none(), "edits kept");
                assert_eq!(d.doc.text(), t.as_str());
            }
            let (err, m) = docs[1].msg.clone().unwrap();
            assert!(err && m.contains("Broken") && !m.contains("family_load."), "{m}");
            assert!(docs[0].msg.is_none(), "no success reported for the others");
        }
    }

    #[test]
    fn model_refusal_rolls_back_the_families_before_it() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (before, undo) = (names(&ed), ed.can_undo());
        let defs = vec![(def("Alpha"), false), (BROKEN.to_string(), false), (def("Beta"), false)];
        let r = commit(&mut ed, defs);
        assert_eq!(r.map(|v| v.len()).map_err(|e| e.0), Err(1));
        assert_eq!(names(&ed), before, "Alpha rolled back with the refused family");
        assert_eq!(ed.can_undo(), undo);
        // the same set without the refused one then loads, and Undo after it is one step
        let defs = vec![(def("Alpha"), false), (def("Beta"), false)];
        assert_eq!(commit(&mut ed, defs).map(|v| v.len()), Ok(2));
        ed.undo();
        assert_eq!(names(&ed), before);
        ed.redo();
        assert!(has(&ed, "Alpha") && has(&ed, "Beta"));
    }

    #[test]
    fn empty_set_runs_nothing() {
        let cat = Catalog::new(Lang::En);
        let mut ed = Editor::new_in_memory().unwrap();
        let undo = ed.can_undo();
        assert_eq!(load_set(&mut ed, &cat, &mut [], &[]), Ok(()));
        assert_eq!(ed.can_undo(), undo);
    }
}
