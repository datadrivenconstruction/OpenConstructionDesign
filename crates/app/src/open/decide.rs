//! The rules of opening: which format does what in which context, through which door.
//! Pure, no UI and no disk: the table in `docs/design/OPEN_SAVE.md` section 2.2 is tested here.
use std::path::{Path, PathBuf};

/// Where the request came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Door {
    /// Ctrl+O and the start screen's Open.
    OpenDialog,
    /// `file.import` (Ctrl+I): always adds to the current project.
    Import,
    Drop,
    /// Command line, double click, "open with".
    Arg,
    Recent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Project,
    /// `.ifc`, `.ifczip`, `.rvt`: a whole model with its own source.
    Model,
    /// `.rfa`
    Component,
    Drawing,
    Table,
    Unsupported,
}

pub fn ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn family(path: &Path) -> Family {
    match ext(path).as_str() {
        "ddc" => Family::Project,
        "ifc" | "ifczip" | "rvt" => Family::Model,
        e if ddc_io::meshx::READ_EXTS.contains(&e) => Family::Model,
        "rfa" => Family::Component,
        "dwg" | "dxf" | "pdf" => Family::Drawing,
        "xlsx" => Family::Table,
        _ => Family::Unsupported,
    }
}

/// What happens to one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// A `.ddc`: replaces the current project (unsaved-work prompt applies).
    OpenProject,
    /// A new project made from the file; in an empty project the file simply goes in.
    OpenNew,
    /// Into the current project as one undo step.
    Add,
    /// Drawing on a level; the placement form opens with the level already picked.
    Underlay,
    /// Table preview.
    Preview,
    /// Another `.ddc` beyond the first: a second process.
    NewProcess,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub path: PathBuf,
    pub act: Act,
}

/// The only questions the open flow asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// A model dropped on a project with content.
    ModelDrop,
    /// Over 20 files or over 2 GB.
    Large,
}

pub const LARGE_FILES: usize = 20;
pub const LARGE_BYTES: u64 = 2 << 30;

#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    /// A new untouched project, or the start screen.
    pub empty: bool,
    pub door: Door,
    pub large: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// Project files refused by `file.import`.
    pub refused: Vec<PathBuf>,
    /// Tables beyond the first one: one preview at a time.
    pub tables_left: usize,
    pub ask: Option<Ask>,
}

impl Plan {
    /// The "Add to this project" answer of the choice card.
    pub fn as_add(&mut self) {
        for s in &mut self.steps {
            if s.act == Act::OpenNew {
                s.act = Act::Add;
            }
        }
        if self.ask == Some(Ask::ModelDrop) {
            self.ask = None;
        }
    }

    fn has_project(&self) -> bool {
        self.steps.iter().any(|s| s.act == Act::OpenProject)
    }
}

/// `files` are supported, existing files; the plan orders the work itself.
pub fn decide(files: &[PathBuf], ctx: &Ctx) -> Plan {
    let mut plan = Plan::default();
    let mut first_project = true;
    let mut fresh = false;
    let mut tables = 0usize;
    let mut sorted: Vec<&PathBuf> = files.iter().collect();
    // stable: projects, models, components, drawings, tables
    sorted.sort_by_key(|p| match family(p) {
        Family::Project => 0,
        Family::Model => 1,
        Family::Component => 2,
        Family::Drawing => 3,
        Family::Table => 4,
        Family::Unsupported => 5,
    });
    let mut has_model = false;
    for p in sorted {
        let act = match family(p) {
            Family::Project if ctx.door == Door::Import => {
                plan.refused.push(p.clone());
                continue;
            }
            Family::Project if first_project => {
                first_project = false;
                Act::OpenProject
            }
            Family::Project => Act::NewProcess,
            Family::Model => {
                has_model = true;
                if !fresh && !plan.has_project() && ctx.door != Door::Import {
                    fresh = true;
                    Act::OpenNew
                } else {
                    Act::Add
                }
            }
            // a component is never a whole project: the build path reads models only
            Family::Component => Act::Add,
            Family::Drawing => Act::Underlay,
            Family::Table => {
                tables += 1;
                if tables > 1 {
                    plan.tables_left += 1;
                    continue;
                }
                Act::Preview
            }
            Family::Unsupported => continue,
        };
        plan.steps.push(Step {
            path: p.clone(),
            act,
        });
    }
    if ctx.door == Door::Drop && !ctx.empty && has_model && !plan.has_project() {
        plan.ask = Some(Ask::ModelDrop);
    } else if ctx.large && !plan.steps.is_empty() {
        plan.ask = Some(Ask::Large);
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn ctx(empty: bool, door: Door) -> Ctx {
        Ctx {
            empty,
            door,
            large: false,
        }
    }

    fn acts(plan: &Plan) -> Vec<(String, Act)> {
        plan.steps
            .iter()
            .map(|s| (s.path.to_string_lossy().into_owned(), s.act))
            .collect()
    }

    #[test]
    fn families_by_extension() {
        assert_eq!(family(&p("a.DDC")), Family::Project);
        for f in ["a.ifc", "a.IFCZIP", "a.rvt"] {
            assert_eq!(family(&p(f)), Family::Model, "{f}");
        }
        assert_eq!(family(&p("a.rfa")), Family::Component);
        for f in ["a.dwg", "a.dxf", "a.pdf"] {
            assert_eq!(family(&p(f)), Family::Drawing, "{f}");
        }
        assert_eq!(family(&p("a.xlsx")), Family::Table);
        assert_eq!(family(&p("a.txt")), Family::Unsupported);
        assert_eq!(family(&p("noext")), Family::Unsupported);
    }

    #[test]
    fn project_file_by_door() {
        for door in [Door::OpenDialog, Door::Drop, Door::Arg, Door::Recent] {
            for empty in [true, false] {
                let pl = decide(&[p("a.ddc")], &ctx(empty, door));
                assert_eq!(acts(&pl), vec![("a.ddc".into(), Act::OpenProject)]);
                assert_eq!(pl.ask, None);
            }
        }
        let pl = decide(&[p("a.ddc")], &ctx(false, Door::Import));
        assert!(pl.steps.is_empty());
        assert_eq!(pl.refused, vec![p("a.ddc")]);
    }

    #[test]
    fn model_in_empty_context_is_a_new_project() {
        for door in [Door::OpenDialog, Door::Drop, Door::Arg, Door::Recent] {
            let pl = decide(&[p("a.ifc")], &ctx(true, door));
            assert_eq!(acts(&pl), vec![("a.ifc".into(), Act::OpenNew)], "{door:?}");
            assert_eq!(pl.ask, None);
        }
    }

    #[test]
    fn model_over_a_project() {
        for door in [Door::OpenDialog, Door::Arg, Door::Recent] {
            let pl = decide(&[p("a.rvt")], &ctx(false, door));
            assert_eq!(acts(&pl), vec![("a.rvt".into(), Act::OpenNew)]);
            assert_eq!(pl.ask, None, "{door:?}");
        }
        let mut pl = decide(&[p("a.ifc")], &ctx(false, Door::Drop));
        assert_eq!(pl.ask, Some(Ask::ModelDrop));
        assert_eq!(
            pl.steps[0].act,
            Act::OpenNew,
            "Enter default is a new project"
        );
        pl.as_add();
        assert_eq!(pl.steps[0].act, Act::Add);
        assert_eq!(pl.ask, None);
        let pl = decide(&[p("a.ifc")], &ctx(false, Door::Import));
        assert_eq!(acts(&pl), vec![("a.ifc".into(), Act::Add)]);
        assert_eq!(pl.ask, None);
    }

    #[test]
    fn import_window_ifc_is_a_background_add_on_every_screen() {
        // the import window hands IFC to the Import door; on the start screen or an empty
        // project it must still add (never replace) and never block on a question card
        for name in ["a.ifc", "a.ifczip"] {
            for empty in [false, true] {
                let pl = decide(&[p(name)], &ctx(empty, Door::Import));
                assert_eq!(acts(&pl), vec![(name.into(), Act::Add)], "{name} {empty}");
                assert_eq!(pl.ask, None);
            }
        }
    }

    #[test]
    fn rfa_on_empty_starts_a_job() {
        // the Add path runs the module on a worker; OpenNew would fail with "not supported"
        for empty in [true, false] {
            for door in [Door::Drop, Door::OpenDialog, Door::Import] {
                let pl = decide(&[p("a.rfa")], &ctx(empty, door));
                assert_eq!(pl.steps[0].act, Act::Add, "{door:?} {empty}");
            }
        }
        for door in [Door::Drop, Door::OpenDialog, Door::Import] {
            let pl = decide(&[p("a.rfa")], &ctx(false, door));
            assert_eq!(pl.steps[0].act, Act::Add, "{door:?}");
            assert_eq!(pl.ask, None);
        }
    }

    #[test]
    fn drawings_and_tables_stay_in_the_project() {
        for empty in [true, false] {
            for door in [Door::Drop, Door::OpenDialog, Door::Import, Door::Arg] {
                let pl = decide(&[p("a.dxf"), p("b.pdf")], &ctx(empty, door));
                assert!(pl.steps.iter().all(|s| s.act == Act::Underlay));
                assert_eq!(pl.ask, None);
                let pl = decide(&[p("a.xlsx")], &ctx(empty, door));
                assert_eq!(acts(&pl), vec![("a.xlsx".into(), Act::Preview)]);
            }
        }
    }

    #[test]
    fn only_models_one_project_one_at_a_time() {
        let pl = decide(
            &[p("a.ifc"), p("b.rvt"), p("c.ifc")],
            &ctx(true, Door::OpenDialog),
        );
        assert_eq!(
            pl.steps.iter().map(|s| s.act).collect::<Vec<_>>(),
            vec![Act::OpenNew, Act::Add, Act::Add]
        );
    }

    #[test]
    fn project_plus_others() {
        let pl = decide(
            &[p("a.ifc"), p("x.ddc"), p("y.ddc"), p("z.ddc"), p("m.dxf")],
            &ctx(false, Door::Drop),
        );
        let a: Vec<Act> = pl.steps.iter().map(|s| s.act).collect();
        assert_eq!(
            a,
            vec![
                Act::OpenProject,
                Act::NewProcess,
                Act::NewProcess,
                Act::Add,
                Act::Underlay
            ]
        );
        assert_eq!(
            pl.ask, None,
            "a project in the batch makes the intent clear"
        );
    }

    #[test]
    fn many_tables_one_preview() {
        let pl = decide(
            &[p("a.xlsx"), p("b.xlsx"), p("c.xlsx")],
            &ctx(false, Door::Drop),
        );
        assert_eq!(pl.steps.len(), 1);
        assert_eq!(pl.tables_left, 2);
    }

    #[test]
    fn large_batches_ask_once() {
        let c = Ctx {
            empty: true,
            door: Door::OpenDialog,
            large: true,
        };
        assert_eq!(decide(&[p("a.dxf")], &c).ask, Some(Ask::Large));
        assert_eq!(decide(&[], &c).ask, None);
    }

    #[test]
    fn unsupported_is_dropped() {
        let pl = decide(&[p("a.txt"), p("b.ifc")], &ctx(true, Door::Drop));
        assert_eq!(pl.steps.len(), 1);
    }
}
