// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! QA: small accessors called through their type path (is_empty, get, set, id, kind...).

use crate::drawq::*;
use crate::Model;

#[test]
fn drawing_libraries_by_type() {
    let mut pens = PenTable::default();
    let pen = Pen { name: "q".into(), columns: vec![(100, 0.3)] };
    assert!(PenTable::set(&mut pens, pen.clone()));
    assert_eq!(PenTable::get(&pens, "q"), Some(&pen));
    assert!(PenTable::remove(&mut pens, "q"));
    assert!(PenTable::get(&pens, "q").is_none());

    let mut pats = PatternLib::default();
    let p = LinePattern { name: "q".into(), segs: vec![Seg::Dash(2.0), Seg::Gap(1.0)] };
    assert!(PatternLib::set(&mut pats, p.clone()));
    assert_eq!(PatternLib::get(&pats, "q"), Some(p));
    assert!(PatternLib::remove(&mut pats, "q"));
    assert!(PatternLib::get(&pats, "q").is_none());

    let mut hatches = HatchLib::default();
    let fam = HatchFamily { angle: 0.0, origin: (0.0, 0.0), shift: 0.0, spacing: 1.0, dashes: vec![] };
    let h = HatchPattern { name: "q".into(), families: vec![fam], model: false };
    assert!(HatchLib::set(&mut hatches, h.clone()));
    assert_eq!(HatchLib::get(&hatches, "q"), Some(h));
    assert!(HatchLib::remove(&mut hatches, "q"));
    assert!(!HatchLib::remove(&mut hatches, "q"));
}

#[test]
fn is_empty_by_type() {
    assert!(StyleOverride::is_empty(&StyleOverride::default()));
    assert!(crate::changes::Changes::is_empty(&Default::default()));
    let busy = crate::changes::Changes { all: true, ..Default::default() };
    assert!(!crate::changes::Changes::is_empty(&busy));
    assert!(crate::clip::Clip::is_empty(&Default::default()));
    assert!(crate::curtain::LineSections::is_empty(&Default::default()));
    let mut ls = crate::curtain::LineSections::default();
    ls.u.push((0, None, ddc_geom::curtain::Section::None));
    assert!(!crate::curtain::LineSections::is_empty(&ls));
    assert!(crate::view_gfx::GfxOverride::is_empty(&Default::default()));
    assert!(crate::source::record::ChangeRecord::is_empty(&Default::default()));
}

#[test]
fn ids_and_kinds_by_type() {
    use crate::TagHost;
    use crate::topics::TopicImport;
    for i in [i64::MIN, -1, 0, 7, i64::MAX] {
        for h in [TagHost::Wall(i), TagHost::Opening(i), TagHost::Room(i), TagHost::Comp(i)] {
            assert_eq!(TagHost::id(h), i);
        }
        assert_eq!(TopicImport::id(TopicImport::Created(i)), i);
        assert_eq!(TopicImport::id(TopicImport::Updated(i)), i);
    }
    use crate::checks::ValueRule;
    assert_eq!(ValueRule::kind(&ValueRule::NonEmpty), "non_empty");
    assert_eq!(ValueRule::kind(&ValueRule::Regex("a".into())), "regex");
}

#[test]
fn element_params_by_type() {
    let mut m = Model::open(":memory:").unwrap();
    let id = m.create("wall").unwrap();
    assert_eq!(Model::get(&m, id, "h"), None);
    Model::set(&mut m, id, "h", 2.5).unwrap();
    assert_eq!(Model::get(&m, id, "h"), Some(2.5));
    Model::set(&mut m, id, "h", -0.0).unwrap();
    assert_eq!(Model::get(&m, id, "h"), Some(0.0));
    m.undo().unwrap();
    assert_eq!(Model::get(&m, id, "h"), Some(2.5));
    m.undo().unwrap();
    assert_eq!(Model::get(&m, id, "h"), None);
    assert_eq!(Model::get(&m, 9999, "h"), None);
}
