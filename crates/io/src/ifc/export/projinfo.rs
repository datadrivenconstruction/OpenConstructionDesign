//! Project information on the IFC project: Name, LongName, Phase, a `DDC_ProjectInfo` property
//! set with every non-empty field, and the postal address on IFCSITE and IFCBUILDING.

use ddc_model::{Model, ProjectInfo};

use super::{Ctx, NONE};
use crate::ifc::step::{list, r, text};

/// Name written when the model has none.
const DEFAULT_NAME: &str = "Project";

pub(super) fn load(model: &Model) -> Result<ProjectInfo, String> {
    model
        .project_info()
        .map(|p| p.trimmed())
        .map_err(|e| format!("project info: {e}"))
}

pub(super) fn name(info: &ProjectInfo) -> &str {
    if info.name.is_empty() {
        DEFAULT_NAME
    } else {
        &info.name
    }
}

/// IFCPROJECT attributes after Description: ObjectType, LongName, Phase.
pub(super) fn head(info: &ProjectInfo) -> [String; 3] {
    let opt = |v: &str| if v.is_empty() { NONE.to_string() } else { text(v) };
    [NONE.to_string(), opt(&info.name), opt(&info.stage)]
}

/// One IFCPOSTALADDRESS (one line per text line); `$` without an address.
pub(super) fn address(c: &mut Ctx, info: &ProjectInfo) -> String {
    let lines: Vec<String> = info
        .address
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(text)
        .collect();
    if lines.is_empty() {
        return NONE.to_string();
    }
    let id = c.s.add(
        "IFCPOSTALADDRESS",
        &[
            ".SITE.".to_string(),
            NONE.to_string(),
            NONE.to_string(),
            NONE.to_string(),
            format!("({})", lines.join(",")),
            NONE.to_string(),
            NONE.to_string(),
            NONE.to_string(),
            NONE.to_string(),
            NONE.to_string(),
        ],
    );
    r(id)
}

/// The property set on the project; nothing when every field is empty.
pub(super) fn pset(c: &mut Ctx, project: u32, info: &ProjectInfo) {
    let props: Vec<u32> = info
        .fields()
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| {
            c.s.add(
                "IFCPROPERTYSINGLEVALUE",
                &[
                    text(k),
                    NONE.to_string(),
                    format!("IFCTEXT({})", text(v)),
                    NONE.to_string(),
                ],
            )
        })
        .collect();
    if props.is_empty() {
        return;
    }
    let set = c.rooted("IFCPROPERTYSET", "project-pset", 0, "DDC_ProjectInfo", &[list(&props)]);
    c.rel(
        "IFCRELDEFINESBYPROPERTIES",
        "project-pset-rel",
        0,
        &[list(&[project]), r(set)],
    );
}

#[cfg(test)]
mod tests {
    use crate::ifc::{export_ifc, import_ifc};
    use ddc_model::{Model, ProjectInfo};

    fn model_with(info: &ProjectInfo) -> Model {
        let mut m = Model::open(":memory:").unwrap();
        m.set_project_info(info).unwrap();
        m
    }

    fn sample() -> ProjectInfo {
        ProjectInfo {
            name: "Tower \u{041D}\u{043E}\u{0432}\u{044B}\u{0439} it's".into(),
            number: "P-2026/07".into(),
            address: "Main St 1\n\u{041C}\u{043E}\u{0441}\u{043A}\u{0432}\u{0430} 101000".into(),
            stage: "Design".into(),
            client: "ACME".into(),
            ..ProjectInfo::default()
        }
    }

    #[test]
    fn export_writes_name_number_address() {
        let t = export_ifc(&model_with(&sample())).unwrap();
        assert!(t.contains("=IFCPROJECT("));
        assert!(
            t.contains(r"\X2\041D\X0\\X2\043E\X0\\X2\0432\X0\\X2\044B\X0\\X2\0439\X0\"),
            "cyrillic escaped per char"
        );
        assert!(t.is_ascii());
        assert!(t.contains("it''s"));
        assert!(t.contains("'DDC_ProjectInfo'"));
        assert!(t.contains("IFCTEXT('P-2026/07')"));
        assert!(t.contains("=IFCPOSTALADDRESS(.SITE.,$,$,$,('Main St 1',"));
        assert!(t.contains("'Design'"));
        let site = t.lines().find(|l| l.contains("=IFCSITE(")).unwrap();
        let bld = t.lines().find(|l| l.contains("=IFCBUILDING(")).unwrap();
        assert!(!site.ends_with(",$);") && !bld.ends_with(",$);"));
    }

    #[test]
    fn project_info_ifc_round_trip() {
        let src = sample();
        let t = export_ifc(&model_with(&src)).unwrap();
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, t.as_bytes()).unwrap();
        let got = d.project_info().unwrap();
        assert_eq!(got.name, src.name);
        assert_eq!(got.number, src.number);
        assert_eq!(got.address, src.address);
        assert_eq!(got.stage, src.stage);
        assert_eq!(got.client, src.client);
    }

    #[test]
    fn empty_info_exports_default_name_and_no_address() {
        let t = export_ifc(&Model::open(":memory:").unwrap()).unwrap();
        assert!(t.contains("=IFCPROJECT("));
        assert!(t.contains(",'Project',$,$,$,"));
        assert!(!t.contains("IFCPOSTALADDRESS"));
        assert!(!t.contains("DDC_ProjectInfo"));
    }

    #[test]
    fn import_without_pset_keeps_name() {
        let t = export_ifc(&Model::open(":memory:").unwrap()).unwrap();
        let mut d = model_with(&ProjectInfo {
            name: "Keep".into(),
            number: "7".into(),
            ..ProjectInfo::default()
        });
        import_ifc(&mut d, t.as_bytes()).unwrap();
        let got = d.project_info().unwrap();
        assert_eq!((got.name.as_str(), got.number.as_str()), ("Keep", "7"));
    }

    #[test]
    fn foreign_file_name_and_postal_address() {
        let t = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_NAME('a.ifc','',(''),(''),'','','');\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#1=IFCPROJECT('0000000000000000000001',$,'Hospital',$,$,'Hospital North','Phase 2',$,$);\n\
#2=IFCPOSTALADDRESS($,$,$,$,('Road 5'),$,'Town','Reg','12345','Land');\n\
#3=IFCSITE('0000000000000000000002',$,'S',$,$,$,$,$,.ELEMENT.,$,$,$,$,#2);\n\
ENDSEC;\nEND-ISO-10303-21;\n";
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, t.as_bytes()).unwrap();
        let got = d.project_info().unwrap();
        assert_eq!(got.name, "Hospital");
        assert_eq!(got.stage, "Phase 2");
        assert_eq!(got.address, "Road 5\n12345 Town\nReg\nLand");
    }

    fn ops(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
            .unwrap()
    }

    fn local() -> ProjectInfo {
        ProjectInfo {
            name: "Keep".into(),
            stage: "Old".into(),
            ..ProjectInfo::default()
        }
    }

    #[test]
    fn import_with_project_info_is_one_undo_step() {
        let t = export_ifc(&model_with(&sample())).unwrap();
        let mut d = Model::open(":memory:").unwrap();
        let before = ops(&d);
        import_ifc(&mut d, t.as_bytes()).unwrap();
        assert_eq!(ops(&d), before + 1, "one journal entry");
        assert_eq!(d.project_info().unwrap(), sample());
        d.undo().unwrap();
        assert_eq!(d.project_info().unwrap(), ProjectInfo::default());
        assert_eq!(ops(&d), before);
    }

    #[test]
    fn undo_restores_local_info_and_add_keeps_local_name() {
        let t = export_ifc(&model_with(&sample())).unwrap();
        let mut d = model_with(&local());
        let before = ops(&d);
        import_ifc(&mut d, t.as_bytes()).unwrap();
        assert_eq!(ops(&d), before + 1);
        let got = d.project_info().unwrap();
        assert_eq!(got.name, "Keep");
        assert_eq!(got.stage, "Old");
        assert_eq!(got.number, "P-2026/07", "empty field filled from the file");
        d.undo().unwrap();
        assert_eq!(d.project_info().unwrap(), local());
    }

    #[test]
    fn new_project_takes_file_name() {
        let t = export_ifc(&model_with(&sample())).unwrap();
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, t.as_bytes()).unwrap();
        assert_eq!(d.project_info().unwrap().name, sample().name);
    }

    #[test]
    fn big_path_is_one_undo_step_and_keeps_local() {
        use crate::ifc::big::commit_plan;
        use crate::ifc::import::plan_bytes;
        let t = export_ifc(&model_with(&sample())).unwrap();
        let pl = plan_bytes(t.as_bytes()).unwrap();
        let mut fresh = Model::open(":memory:").unwrap();
        let b0 = ops(&fresh);
        commit_plan(&mut fresh, &pl).unwrap();
        assert_eq!(ops(&fresh), b0 + 1);
        assert_eq!(fresh.project_info().unwrap().name, sample().name);
        fresh.undo().unwrap();
        assert_eq!(fresh.project_info().unwrap(), ProjectInfo::default());

        let mut d = model_with(&local());
        let b1 = ops(&d);
        commit_plan(&mut d, &pl).unwrap();
        assert_eq!(ops(&d), b1 + 1);
        assert_eq!(d.project_info().unwrap().name, "Keep");
        d.undo().unwrap();
        assert_eq!(d.project_info().unwrap(), local());
    }
}
