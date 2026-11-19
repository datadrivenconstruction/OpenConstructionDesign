// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Names the program writes into a document when it creates sheets, sheet sets, title block
//! fields, revisions and demo levels. They come from the catalog of the interface language at
//! the moment of creation; names already stored in a document are never renamed.

use ddc_ui::Catalog;

/// Level name `Level n` / its Russian equivalent in the catalog language.
pub fn level_name(cat: &Catalog, n: u32) -> String {
    cat.tf("names.level", &[("n", &n.to_string())])
}

/// Names of the sheet demo scene (`--sheet-demo`): sheets, set, title block, revision.
pub struct SheetDemoNames {
    pub plan_section: String,
    pub plan_view: String,
    pub site_plan: String,
    pub set_name: String,
    pub owner: String,
    pub creator: String,
    pub approver: String,
    pub document_type: String,
    pub status: String,
    pub revision_date: String,
    pub revision_description: String,
    pub revision_author: String,
    pub no_levels: String,
    pub no_sheet: String,
}

impl SheetDemoNames {
    pub fn new(cat: &Catalog) -> Self {
        let t = |k: &str| cat.t(k);
        Self {
            plan_section: t("names.sheet.plan_section"),
            plan_view: t("names.sheet.plan_view"),
            site_plan: t("names.sheet.site_plan"),
            set_name: t("names.sheet_set.issue"),
            owner: t("names.title.owner"),
            creator: t("names.title.creator"),
            approver: t("names.title.approver"),
            document_type: t("names.title.document_type"),
            status: t("names.title.status"),
            revision_date: t("names.revision.date"),
            revision_description: t("names.revision.description"),
            revision_author: t("names.revision.author"),
            no_levels: t("names.demo.no_levels"),
            no_sheet: t("names.demo.no_sheet"),
        }
    }

    /// Title block fields (key, value) of the first demo sheet.
    pub fn title_fields(&self) -> [(&'static str, &str); 5] {
        [
            ("legal_owner", &self.owner),
            ("creator", &self.creator),
            ("approver", &self.approver),
            ("document_type", &self.document_type),
            ("status", &self.status),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::shell::EditorApp;
    use ddc_ui::Lang;

    fn cyr(s: &str) -> bool {
        s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
    }

    fn sheet_strings(ed: &Editor) -> Vec<String> {
        let m = &ed.model;
        let mut out = vec![];
        for (id, d) in m.sheets().unwrap() {
            out.push(d.number.clone());
            out.push(d.name.clone());
            out.extend(d.fields.values().cloned());
            for r in m.revisions(id).unwrap() {
                out.extend([r.mark, r.date, r.description, r.author]);
            }
            out.extend(m.viewports(id).unwrap().into_iter().map(|v| v.1.title));
        }
        out.extend(m.sheet_sets().unwrap().into_iter().map(|s| s.1));
        out
    }

    fn built(lang: Lang) -> (Editor, i64) {
        let cat = Catalog::new(lang);
        let mut ed = Editor::new_in_memory().unwrap();
        let names = SheetDemoNames::new(&cat);
        let (a, set, _) = crate::sheet_view::build_demo(&mut ed, &names).unwrap();
        assert!(set.is_some());
        (ed, a)
    }

    #[test]
    fn sheet_demo_in_english_has_no_cyrillic_names() {
        let (ed, a) = built(Lang::En);
        let all = sheet_strings(&ed);
        assert!(all.len() > 10, "{all:?}");
        for s in &all {
            assert!(!cyr(s), "Cyrillic in an English name: {s}");
        }
        let d = ed.model.sheet(a).unwrap();
        assert_eq!(d.fields["status"], "In progress");
        assert_eq!(d.fields["document_type"], "Drawing");
        assert_eq!(ed.model.sheet_sets().unwrap()[0].1, "Issue 1");
    }

    #[test]
    fn sheet_demo_in_russian_has_russian_names() {
        let (ed, a) = built(Lang::Ru);
        let d = ed.model.sheet(a).unwrap();
        for k in ["legal_owner", "creator", "approver", "document_type", "status"] {
            assert!(cyr(&d.fields[k]), "{k}: {}", d.fields[k]);
        }
        assert!(cyr(&d.name));
        assert!(cyr(&ed.model.sheet_sets().unwrap()[0].1));
        let sheets = ed.model.sheets().unwrap();
        assert_eq!(sheets[1].1.name, Catalog::new(Lang::Ru).t("names.sheet.site_plan"));
        assert!(cyr(&sheets[1].1.name));
    }

    #[test]
    fn names_fragments_have_the_same_keys_and_right_scripts() {
        let en = Catalog::new(Lang::En);
        let ru = Catalog::new(Lang::Ru);
        let probe = SheetDemoNames::new(&en);
        assert!(!cyr(&probe.owner));
        for k in ["names.level", "names.slab", "names.roof_slab", "names.sheet.site_plan"] {
            assert_ne!(en.t(k), k, "{k} missing in en");
            assert!(!cyr(&en.t(k)), "{k}");
            assert!(cyr(&ru.t(k)), "{k}");
        }
    }

    #[test]
    fn tool_demo_levels_and_types_follow_the_catalog_language() {
        for (lang, ru) in [(Lang::En, false), (Lang::Ru, true)] {
            let new_app = || {
                let mut app = EditorApp::new(Editor::new_in_memory().unwrap(), None, "t".into());
                app.sh.cat = Catalog::new(lang);
                app
            };
            let mut stair = new_app();
            crate::tools::demo(&mut stair, "A4").unwrap();
            let mut shaft = new_app();
            crate::tools::demo(&mut shaft, "E8").unwrap();
            let mut made: Vec<String> = vec![];
            for app in [&stair, &shaft] {
                let seeded = app.ed.levels().into_iter().next().unwrap().0;
                made.extend(app.ed.levels().into_iter().filter(|l| l.0 != seeded).map(|l| l.1));
            }
            assert!(made.len() >= 3, "{made:?}");
            let mut st = shaft.ed.model.sql().prepare("SELECT name FROM slab_type").unwrap();
            let slabs: Vec<String> = st
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert!(slabs.len() >= 2, "{slabs:?}");
            for n in made.iter().chain(slabs.iter()) {
                assert_eq!(cyr(n), ru, "{n} ({lang:?})");
            }
        }
    }

    #[test]
    fn creation_modules_hold_no_cyrillic_default_names() {
        let sources = [
            ("create_names.rs", include_str!("create_names.rs")),
            ("tools/demo.rs", include_str!("tools/demo.rs")),
            ("sheet_view.rs", include_str!("sheet_view.rs")),
        ];
        for (file, src) in sources {
            let prod = src.split("#[cfg(test)]\nmod tests").next().unwrap();
            for (i, line) in prod.lines().enumerate() {
                let code = line.split("//").next().unwrap();
                assert!(!cyr(code), "{file}:{}: raw Cyrillic", i + 1);
                let mut rest = line;
                while let Some(p) = rest.find("\\u{") {
                    rest = &rest[p + 3..];
                    let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                    let cp = u32::from_str_radix(&hex, 16).unwrap_or(0);
                    assert!(
                        !(0x400..=0x4ff).contains(&cp),
                        "{file}:{}: Cyrillic escape in creation code",
                        i + 1
                    );
                }
            }
        }
    }
}
