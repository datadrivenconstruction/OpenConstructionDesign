//! Type catalog import of the family editor: a plan read with the per-file number rule, a
//! preview of what changes, then Apply as one undo step. Nothing is written before Apply.

use ddc_geom::component::catalog::{self, CatalogError, Plan};
use ddc_geom::component::doc::FamDoc;
use ddc_geom::component::{Family, FamilyError, FamilyType, Value};
use ddc_io::num_file::{detect_mark, parse_num, Mark, NumErr};
use ddc_model::{CatalogLoad, Model};
use ddc_ui::widgets::field::NumberFormat;
use ddc_ui::Catalog;

/// Plan waiting for Apply, with the box text and document it was made from.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub csv: String,
    pub doc: String,
    pub plan: Plan,
}

impl Preview {
    /// Still describes what Apply would write: neither the box nor the document changed.
    pub fn current(&self, csv: &str, doc: &str) -> bool {
        self.csv == csv && self.doc == doc
    }
}

/// Plan of `csv` against the types of `f`. Comma-separated files use the point; otherwise the
/// file's number cells decide the decimal mark once (crates/io num_file rule).
pub fn plan(f: &Family, csv: &str, project_length: &str) -> Result<Plan, CatalogError> {
    plan_against(f, &f.types, csv, project_length)
}

/// [`plan`] against given current types (a project family's types in the model).
pub fn plan_against(
    f: &Family,
    current: &[FamilyType],
    csv: &str,
    project_length: &str,
) -> Result<Plan, CatalogError> {
    let sheet = catalog::read_sheet(csv)?;
    let mark = if sheet.comma_separated() {
        Some(Mark::Point)
    } else {
        detect_mark(sheet.number_cells(f))
    };
    let num = move |s: &str| {
        parse_num(s, mark).map(|n| n.value).map_err(|e| match e {
            NumErr::Ambiguous => format!(
                "'{}' reads as a decimal or as thousands; the file gives no decimal mark",
                s.trim()
            ),
            NumErr::Invalid => format!("'{}' is not a number", s.trim()),
        })
    };
    catalog::plan_import(f, current, &sheet, project_length, &num)
}

/// Project side: plan of `csv` against the family's types in the model, same number rule.
pub fn project_plan(
    m: &Model,
    family: i64,
    csv: &str,
    project_length: &str,
) -> Result<Plan, String> {
    let f = m.family(family).map_err(|e| e.to_string())?;
    let current = m.family_types(family).map_err(|e| e.to_string())?;
    plan_against(&f, &current, csv, project_length).map_err(|e| e.to_string())
}

/// Project side import: typed values go to the model as one undo step; bad cells come back in
/// `skipped` and do not stop the file; nothing is written when nothing changes.
pub fn project_import(
    m: &mut Model,
    family: i64,
    csv: &str,
    project_length: &str,
) -> Result<CatalogLoad, String> {
    let plan = project_plan(m, family, csv, project_length)?;
    m.import_type_catalog(family, &plan).map_err(|e| e.to_string())
}

/// Writes the plan as one undo step of the document; an empty plan writes nothing (false).
pub fn apply(doc: &mut FamDoc, f: &Family, plan: &Plan) -> Result<bool, FamilyError> {
    if plan.is_empty() {
        return Ok(false);
    }
    doc.set_types(f, &plan.types)?;
    Ok(true)
}

fn value_text(f: &Family, param: &str, v: &Value, imperial: bool, nf: NumberFormat) -> String {
    match (v, f.param(param)) {
        (Value::Text(t), _) => format!("\"{t}\""),
        (Value::Num(_), Some(p)) => {
            let (n, u) = super::tabs::num_parts(p.kind, v, imperial, nf);
            if u.is_empty() {
                n
            } else {
                format!("{n} {u}")
            }
        }
        (Value::Num(x), None) => x.to_string(),
    }
}

/// Preview text: summary line, then each added or changed type with its values, then the
/// cells not applied and the warnings.
pub fn lines(
    cat: &Catalog,
    f: &Family,
    plan: &Plan,
    imperial: bool,
    nf: NumberFormat,
) -> Vec<String> {
    let added = plan.changes.iter().filter(|c| c.added).count();
    let mut out = vec![cat.tf(
        "fe.csv.preview",
        &[
            ("added", &added.to_string()),
            ("changed", &(plan.changes.len() - added).to_string()),
            ("same", &plan.unchanged.to_string()),
            ("values", &plan.value_count().to_string()),
        ],
    )];
    for c in &plan.changes {
        let key = if c.added {
            "fe.csv.type_added"
        } else {
            "fe.csv.type_changed"
        };
        out.push(cat.tf(key, &[("name", &c.name)]));
        for p in &c.params {
            out.push(cat.tf(
                "fe.csv.value",
                &[
                    ("param", &p.param),
                    ("old", &value_text(f, &p.param, &p.old, imperial, nf)),
                    ("new", &value_text(f, &p.param, &p.new, imperial, nf)),
                ],
            ));
        }
    }
    for e in &plan.skipped {
        out.push(cat.tf("fe.csv.skipped", &[("msg", &e.to_string())]));
    }
    for w in &plan.warnings {
        out.push(cat.tf("fe.csv.warning", &[("msg", &w.to_string())]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::component::FamilyType;
    use ddc_ui::Lang;

    const F: &str = "family Box\nparam Width length type 600mm\nparam Depth length type 400mm\n\
param Mat material type \"Oak\"\n\
extrude Body rect(0, 0, Width, Depth) z(0, 500mm)\n\
type \"Small\" Width=300mm, Mat=\"Ash\"\ntype \"Big\" Width=1200mm, Depth=600mm\n";

    fn doc() -> FamDoc {
        FamDoc::new(F)
    }

    fn val(types: &[FamilyType], ty: &str, k: &str) -> f64 {
        let t = types.iter().find(|t| t.name == ty).unwrap();
        match t.values.iter().find(|v| v.0 == k).map(|v| &v.1) {
            Some(Value::Num(x)) => *x,
            _ => f64::NAN,
        }
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn missing_column_and_empty_cell_keep_values() {
        let mut d = doc();
        let f = d.family().unwrap();
        let p = plan(&f, "Type,Width (mm),Mat\nBig,1000,\nSmall,,\n", "mm").unwrap();
        assert_eq!((p.changes.len(), p.unchanged, p.value_count()), (1, 1, 1));
        assert!(apply(&mut d, &f, &p).unwrap());
        let g = d.family().unwrap();
        assert!(near(val(&g.types, "Big", "Width"), 1.0));
        assert!(near(val(&g.types, "Big", "Depth"), 0.6), "Depth was reset");
        assert!(near(val(&g.types, "Small", "Width"), 0.3));
        let small = g.types.iter().find(|t| t.name == "Small").unwrap();
        assert!(small.values.iter().any(|v| v.1 == Value::Text("Ash".into())));
    }

    #[test]
    fn untouched_reimport_writes_nothing() {
        let mut d = doc();
        let f = d.family().unwrap();
        let before = d.text().to_string();
        let csv = catalog::export_csv(&f, &f.types);
        let p = plan(&f, &csv, "mm").unwrap();
        assert!(p.is_empty() && p.unchanged == 2, "{:?}", p.changes);
        assert!(!apply(&mut d, &f, &p).unwrap());
        assert_eq!(d.text(), before);
        assert!(!d.can_undo(), "no undo step for zero changes");
    }

    #[test]
    fn comma_and_point_files_follow_the_file_mark() {
        let f = doc().family().unwrap();
        // semicolon file whose cells prove the comma (0,5): 1,250 is 1.25 here
        let p = plan(&f, "Type;Width (mm);Depth (mm)\nBig;1,250;0,5\n", "mm").unwrap();
        assert!(p.skipped.is_empty(), "{:?}", p.skipped);
        assert!(near(val(&p.types, "Big", "Width"), 0.00125));
        assert!(near(val(&p.types, "Big", "Depth"), 0.0005));
        // tab file whose cells prove the point (500.5): 1,250 is 1250 here
        let p = plan(&f, "Type\tWidth (mm)\tDepth (mm)\nBig\t1,250\t500.5\n", "mm").unwrap();
        assert!(p.skipped.is_empty(), "{:?}", p.skipped);
        assert!(near(val(&p.types, "Big", "Width"), 1.25));
        assert!(near(val(&p.types, "Big", "Depth"), 0.5005));
        // comma-separated file: the point is the mark, a quoted 0,5 is not a number
        let p = plan(&f, "Type,Width (mm),Depth (mm)\nBig,1000.5,\"0,5\"\n", "mm").unwrap();
        assert_eq!(p.skipped.len(), 1, "{:?}", p.skipped);
        assert!(near(val(&p.types, "Big", "Width"), 1.0005));
        assert!(near(val(&p.types, "Big", "Depth"), 0.6), "bad cell kept the old value");
    }

    #[test]
    fn bad_and_ambiguous_numbers_are_reported_not_applied() {
        let mut d = doc();
        let f = d.family().unwrap();
        // no file mark: 1,250 reads two ways and is refused; abc is not a number
        let p = plan(&f, "Type;Width (mm);Depth (mm)\nBig;1,250;abc\n", "mm").unwrap();
        assert_eq!(p.skipped.len(), 2, "{:?}", p.skipped);
        assert!(p.skipped[0].msg.contains("Width") && p.skipped[0].line == 2);
        assert!(p.skipped[0].msg.contains("two") || p.skipped[0].msg.contains("decimal"));
        assert!(p.skipped[1].msg.contains("Depth"));
        assert!(p.is_empty(), "nothing valid to apply");
        assert!(!apply(&mut d, &f, &p).unwrap());
        assert!(!d.can_undo());
    }

    #[test]
    fn apply_is_one_undo_step() {
        let mut d = doc();
        let f = d.family().unwrap();
        let before = d.text().to_string();
        let csv = "Type,Width (mm),Depth (mm)\nBig,900,700\nSmall,350,\nNew,800,\n";
        let p = plan(&f, csv, "mm").unwrap();
        assert_eq!(p.changes.len(), 3);
        assert!(p.changes[2].added);
        assert!(apply(&mut d, &f, &p).unwrap());
        assert_eq!(d.family().unwrap().types.len(), 3);
        assert!(d.undo());
        assert_eq!(d.text(), before);
        assert!(!d.can_undo());
    }

    fn project() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let ld = m.load_family_def(F, false).unwrap();
        (m, ld.family)
    }

    #[test]
    fn project_import_follows_the_file_mark() {
        let (mut m, fam) = project();
        // semicolon file whose cells prove the comma (0,5): 1,250 is 1.25 m here
        let csv = "Type;Width (m);Depth (m)\nBig;1,250;0,5\n";
        let n = project_import(&mut m, fam, csv, "mm").unwrap();
        assert_eq!((n.types, n.skipped.len()), (1, 0), "{:?}", n.skipped);
        let t = m.family_types(fam).unwrap();
        assert!(near(val(&t, "Big", "Width"), 1.25));
        assert!(near(val(&t, "Big", "Depth"), 0.5));
        // tab file whose cells prove the point (500.5): 2,500 is 2500 mm here
        let csv = "Type\tWidth (mm)\tDepth (mm)\nBig\t2,500\t500.5\n";
        let n = project_import(&mut m, fam, csv, "mm").unwrap();
        assert_eq!((n.types, n.skipped.len()), (1, 0), "{:?}", n.skipped);
        let t = m.family_types(fam).unwrap();
        assert!(near(val(&t, "Big", "Width"), 2.5));
        assert!(near(val(&t, "Big", "Depth"), 0.5005));
        // comma-separated file: the point is the mark, a quoted 0,5 is reported and kept
        let csv = "Type,Width (mm),Depth (mm)\nBig,1000.5,\"0,5\"\n";
        let n = project_import(&mut m, fam, csv, "mm").unwrap();
        assert_eq!((n.types, n.skipped.len()), (1, 1), "{:?}", n.skipped);
        let t = m.family_types(fam).unwrap();
        assert!(near(val(&t, "Big", "Width"), 1.0005));
        assert!(near(val(&t, "Big", "Depth"), 0.5005), "bad cell kept the old value");
    }

    #[test]
    fn project_import_reports_bad_cells_and_is_one_undo_step() {
        let (mut m, fam) = project();
        let ops = m.journal_len().unwrap();
        // no file mark: 1,250 reads two ways, abc is not a number; New is still added
        let csv = "Type;Width (mm);Depth (mm)\nBig;1,250;abc\nNew;800;\n";
        let n = project_import(&mut m, fam, csv, "mm").unwrap();
        assert_eq!((n.types, n.skipped.len()), (1, 2), "{:?}", n.skipped);
        assert!(n.skipped[0].contains("Width") && n.skipped[1].contains("Depth"));
        assert_eq!(m.journal_len().unwrap(), ops + 1);
        let t = m.family_types(fam).unwrap();
        assert_eq!(t.len(), 3);
        assert!(near(val(&t, "Big", "Width"), 1.2), "bad cell kept the old value");
        assert!(near(val(&t, "New", "Width"), 0.8));
        m.undo().unwrap();
        assert_eq!(m.family_types(fam).unwrap().len(), 2);
    }

    #[test]
    fn project_untouched_reimport_writes_nothing() {
        let (mut m, fam) = project();
        let ops = m.journal_len().unwrap();
        let f = m.family(fam).unwrap();
        let types = m.family_types(fam).unwrap();
        for csv in [
            m.export_type_catalog(fam).unwrap(),
            catalog::export_csv_file(&f, &types, true),
            catalog::export_csv_file(&f, &types, false),
        ] {
            let n = project_import(&mut m, fam, &csv, "mm").unwrap();
            assert_eq!((n.types, n.skipped.len()), (0, 0), "{csv}");
        }
        assert_eq!(m.journal_len().unwrap(), ops, "no operation for zero changes");
    }

    #[test]
    fn project_quoted_type_names_round_trip_without_twins() {
        let (mut m, fam) = project();
        let csv = "Type,Width (mm)\n\"Rod \"\"A\"\", 2\",700\n";
        assert_eq!(project_import(&mut m, fam, csv, "mm").unwrap().types, 1);
        let ops = m.journal_len().unwrap();
        let f = m.family(fam).unwrap();
        let types = m.family_types(fam).unwrap();
        assert!(types.iter().any(|t| t.name == "Rod \"A\", 2"));
        for csv in [
            m.export_type_catalog(fam).unwrap(),
            catalog::export_csv_file(&f, &types, true),
        ] {
            assert_eq!(project_import(&mut m, fam, &csv, "mm").unwrap().types, 0, "{csv}");
        }
        assert_eq!(m.journal_len().unwrap(), ops);
        let csv = "Type;Width (mm)\n\"Rod \"\"A\"\", 2\";900\n";
        assert_eq!(project_import(&mut m, fam, csv, "mm").unwrap().types, 1);
        let t = m.family_types(fam).unwrap();
        assert_eq!(t.len(), 3, "no twin type");
        assert!(near(val(&t, "Rod \"A\", 2", "Width"), 0.9));
    }

    #[test]
    fn preview_lists_types_and_values() {
        let f = doc().family().unwrap();
        let p = plan(&f, "Type,Width (mm)\nSmall,350\nNew,500\nBig,12..5\n", "mm").unwrap();
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            let l = lines(&cat, &f, &p, false, NumberFormat::for_lang(lang));
            let all = l.join("\n");
            // summary, Small, its value, New, its value, the skipped cell
            assert_eq!(l.len(), 6, "{all}");
            assert!(all.contains("Small") && all.contains("New"), "{all}");
            assert!(all.contains("300 mm") && all.contains("350 mm"), "{all}");
            assert!(l[5].contains("Width") && l[5].contains("12..5"), "{all}");
            assert!(!all.contains("fe.csv."), "missing string: {all}");
        }
    }
}
