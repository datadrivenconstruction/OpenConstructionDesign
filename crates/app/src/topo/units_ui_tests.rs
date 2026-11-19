//! Survey import units: every unit scales, the file's unit is the default, a mm survey is
//! suggested as mm, the extent in project units before Import, Import is one undo step.
//! Languages are passed explicitly; the global UI language is never switched.

use super::units_ui::{extent_doubtful, extent_text, refresh, source_text, unit_key};
use super::window::{run_import, ImportDlg, ImportKind, LenUnit};
use crate::editor::Editor;
use ddc_io::topo::{ColumnOrder, SurveyUnit, UnitGuess, UnitSource};
use ddc_ui::i18n::Lang;

/// A 20 m x 15 m local site, z 1..2 m, written in mm, no unit named.
const MM_SURVEY: &str = "x y z\n0 0 1000\n20000 0 1500\n0 15000 2000\n";

fn xyz(text: &str) -> ImportKind {
    ImportKind::Xyz {
        text: text.into(),
        head: text.into(),
        order: ColumnOrder::Auto,
    }
}

fn dlg(kind: ImportKind) -> ImportDlg {
    ImportDlg::new("survey.txt".into(), "Survey".into(), kind)
}

fn dxf_point_mm(insunits: Option<&str>) -> ImportKind {
    let text = "0\nSECTION\n2\nENTITIES\n0\nPOINT\n8\nGROUND\n10\n5000\n20\n6000\n30\n101500\n\
                0\nPOINT\n8\nGROUND\n10\n25000\n20\n6000\n30\n102000\n0\nENDSEC\n0\nEOF\n";
    let mut doc = ddc_io::dxf::read_str(text).unwrap();
    if let Some(v) = insunits {
        doc.header.push(ddc_io::dxf::HeaderVar {
            name: "$INSUNITS".into(),
            values: vec![ddc_io::dxf::Pair::new(70, v)],
        });
    }
    let layers = ddc_io::topo::dxf_layers(&doc)
        .into_iter()
        .map(|l| (l, true))
        .collect();
    ImportKind::Dxf {
        doc: Box::new(doc),
        layers,
        keep_flat: false,
        inserts: true,
        breaklines: true,
    }
}

#[test]
fn each_unit_scales_the_import() {
    let k = xyz(MM_SURVEY);
    for u in SurveyUnit::ALL {
        let s = k.read(u).unwrap();
        let far = s.points.iter().map(|p| p.0).fold(0.0, f64::max);
        assert!((far - 20000.0 * u.meters()).abs() < 1e-9, "{u:?}: {far}");
        let top = s.points.iter().map(|p| p.2).fold(0.0, f64::max);
        assert!((top - 2000.0 * u.meters()).abs() < 1e-9, "{u:?} z");
    }
    let d = dxf_point_mm(None);
    let p = d.read(SurveyUnit::Mm).unwrap().points[0];
    assert!((p.0 - 5.0).abs() < 1e-12 && (p.2 - 101.5).abs() < 1e-12);
}

#[test]
fn file_units_are_the_default() {
    let h = dlg(xyz("x(ft);y(ft);z(ft)\n0;0;10\n100;0;12\n"));
    assert_eq!(
        h.guess,
        UnitGuess {
            unit: SurveyUnit::Ft,
            source: UnitSource::Header
        }
    );
    assert_eq!(h.unit, SurveyUnit::Ft);
    let c = dlg(xyz("# units: cm\n0 0 100\n2000 0 150\n"));
    assert_eq!((c.unit, c.guess.source), (SurveyUnit::Cm, UnitSource::Header));
    let d = dlg(dxf_point_mm(Some("4")));
    assert_eq!((d.unit, d.guess.source), (SurveyUnit::Mm, UnitSource::Header));
    let f = dlg(dxf_point_mm(Some("2")));
    assert_eq!(f.unit, SurveyUnit::Ft, "$INSUNITS beats the extent");
}

#[test]
fn mm_survey_is_suggested_as_mm_never_silently() {
    let d = dlg(xyz(MM_SURVEY));
    assert_eq!((d.unit, d.guess.source), (SurveyUnit::Mm, UnitSource::Extent));
    let m = dlg(xyz("x y z\n0 0 1\n20 0 1.5\n0 15 2\n"));
    assert_eq!((m.unit, m.guess.source), (SurveyUnit::M, UnitSource::Extent));
    let x = dlg(dxf_point_mm(None));
    assert_eq!((x.unit, x.guess.source), (SurveyUnit::Mm, UnitSource::Extent));
    // the source is always said
    assert_eq!(
        source_text(d.guess, Lang::En),
        "The file names no units. Suggested by the extent: mm \u{2014} check the result below"
    );
    let h = UnitGuess {
        unit: SurveyUnit::UsFt,
        source: UnitSource::Header,
    };
    assert_eq!(source_text(h, Lang::En), "The file names its units: US survey ft");
    assert_ne!(source_text(h, Lang::Ru), source_text(h, Lang::En));
}

#[test]
fn preview_shows_the_extent_in_project_units() {
    let d = dlg(xyz(MM_SURVEY));
    assert_eq!(
        extent_text(d.span, SurveyUnit::Mm, LenUnit::Mm, Lang::En),
        "Result in project units: 20,000.0 \u{d7} 15,000.0 mm in plan, height range 1,000.0 mm"
    );
    assert_eq!(
        extent_text(d.span, SurveyUnit::Mm, LenUnit::M, Lang::En),
        "Result in project units: 20.000 \u{d7} 15.000 m in plan, height range 1.000 m"
    );
    assert!(extent_text(d.span, SurveyUnit::Mm, LenUnit::M, Lang::Ru)
        .contains("20,000 \u{d7} 15,000 \u{43c}"));
    let ft = extent_text(d.span, SurveyUnit::Mm, LenUnit::Ft, Lang::En);
    assert!(ft.contains("65.617 \u{d7} 49.213 ft"), "{ft}");
    // read as metres the same file is 20 km wide: flagged
    assert!(extent_doubtful(d.span, SurveyUnit::M));
    assert!(!extent_doubtful(d.span, SurveyUnit::Mm));
    assert_eq!(
        extent_text(None, SurveyUnit::M, LenUnit::Mm, Lang::En),
        "Result: nothing to import with these choices"
    );
}

#[test]
fn span_follows_the_layer_choice() {
    let mut d = dlg(dxf_point_mm(None));
    assert!(d.span.is_some());
    if let ImportKind::Dxf { layers, .. } = &mut d.kind {
        layers.iter_mut().for_each(|l| l.1 = false);
    }
    refresh(&mut d);
    assert_eq!(d.span, None, "no layer ticked, nothing to measure");
}

#[test]
fn import_applies_the_unit_in_one_undo_step() {
    let mut e = Editor::new_in_memory().unwrap();
    let mut d = dlg(xyz(MM_SURVEY));
    d.unit = SurveyUnit::Mm;
    run_import(&mut e, d);
    let id = e.topo.active.expect("imported toposolid is active");
    let p = e.model.topo_points(id).unwrap();
    let far = p.iter().map(|q| q.1 .0).fold(0.0, f64::max);
    let top = p.iter().map(|q| q.1 .2).fold(0.0, f64::max);
    assert!((far - 20.0).abs() < 1e-9 && (top - 2.0).abs() < 1e-9, "{far} {top}");
    assert!(e.model.site().unwrap().base.0 == 0.0, "a local survey is not shifted");
    e.undo();
    assert!(e.model.toposolids().unwrap().is_empty(), "one undo removes it");
    assert!(!e.can_undo(), "the import was a single step");
}

#[test]
fn unit_strings_exist_in_both_languages() {
    let mut keys: Vec<&str> = SurveyUnit::ALL.iter().map(|u| unit_key(*u)).collect();
    keys.extend([
        "topo_units.unit.section",
        "topo_units.unit",
        "topo_units.unit.from_file",
        "topo_units.unit.suggested",
        "topo_units.extent",
        "topo_units.extent.none",
        "topo_units.extent.check",
    ]);
    for lang in [Lang::En, Lang::Ru] {
        for k in &keys {
            assert_ne!(crate::tr_in(lang, k, &[]), *k, "{k} missing in {lang:?}");
        }
    }
    assert_eq!(crate::tr_in(Lang::Ru, unit_key(SurveyUnit::Mm), &[]), "\u{43c}\u{43c}");
}
