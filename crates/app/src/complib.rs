// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Component library sources: the built-in families, the project's families (loaded from the
//! library, edited, program-defined, imported) and the user's folder of family files
//! (`*.ddcf`, families split by a `---` line). Each entry carries its definition text (what
//! loading it writes), a hash of definition and type values (the thumbnail key) and its
//! readiness per output format, computed from what each exporter writes for it.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ddc_geom::component::{Family, FamilyType, Lod};
use ddc_model::Model;

/// Environment variable that points the user library at another folder.
pub const USER_DIR_ENV: &str = "DDC_COMPONENTS_DIR";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Source {
    #[default]
    Builtin,
    Project,
    User,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Builtin, Source::Project, Source::User];
    /// Catalog key of the source name.
    pub fn key(self) -> &'static str {
        match self {
            Source::Builtin => "complib.src.builtin",
            Source::Project => "complib.src.project",
            Source::User => "complib.src.user",
        }
    }
}

/// How far a format carries a family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ready {
    /// Its own object with type and values.
    Full,
    /// The body (and values where the format holds them), no native object.
    Body,
    /// Nothing: the family does not build.
    No,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Formats {
    pub ifc: Ready,
    pub gltf: Ready,
}

impl Formats {
    /// (format label, readiness) in the order the window shows them.
    pub fn list(&self) -> [(&'static str, Ready); 2] {
        [("IFC", self.ifc), ("glTF", self.gltf)]
    }
}

/// What each exporter writes for `f` (its first type): IFC from the medium body, glTF from the
/// fine body.
pub fn formats(f: &Family) -> Formats {
    let tv = f.types.first().map(|t| t.values.clone()).unwrap_or_default();
    let (medium, fine) = match f.resolve(&tv, &[]) {
        Ok(r) => (f.build(&r, Lod::Medium).ok(), f.build(&r, Lod::Fine).ok()),
        Err(_) => (None, None),
    };
    let body = medium.as_ref().is_some_and(|b| !b.mesh.triangles.is_empty());
    let ifc = match (body, ddc_io::ifc::component_type_class(&f.ifc.0)) {
        (false, _) => Ready::No,
        (true, Some(_)) => Ready::Full,
        (true, None) => Ready::Body,
    };
    // glTF: shared mesh with params in extras
    let gltf = if fine.is_some_and(|b| !b.mesh.triangles.is_empty()) {
        Ready::Full
    } else {
        Ready::No
    };
    Formats { ifc, gltf }
}

/// One family as the library shows it.
#[derive(Clone, Debug)]
pub struct Entry {
    pub fam: Family,
    pub source: Source,
    /// Definition text: what loading the family into a project writes.
    pub def: Arc<str>,
    /// The project has a family of this name.
    pub in_project: bool,
    /// In the project, and its definition differs from the built-in family of that name.
    pub edited: bool,
    /// Definition and type values: the thumbnail key.
    pub hash: u64,
    pub formats: Formats,
    /// File of a user-library family.
    pub path: Option<PathBuf>,
}

impl Entry {
    fn new(fam: Family, source: Source, def: Arc<str>, path: Option<PathBuf>) -> Entry {
        let mut h = DefaultHasher::new();
        def.hash(&mut h);
        for t in &fam.types {
            t.name.hash(&mut h);
            format!("{:?}", t.values).hash(&mut h);
        }
        let formats = formats(&fam);
        Entry {
            fam,
            source,
            def,
            in_project: false,
            edited: false,
            hash: h.finish(),
            formats,
            path,
        }
    }

    /// Hash of one type of the entry (thumbnail of the picked type).
    pub fn type_hash(&self, ty: &str) -> u64 {
        let mut h = DefaultHasher::new();
        self.hash.hash(&mut h);
        ty.hash(&mut h);
        h.finish()
    }
}

/// The user library folder: `DDC_COMPONENTS_DIR`, else `components` in the app data folder.
pub fn user_dir() -> Option<PathBuf> {
    std::env::var_os(USER_DIR_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::gfx::data_dir().map(|d| d.join("components")))
}

/// Families of the `*.ddcf` files of `dir` (sorted by file name), and the files or families
/// that could not be read, each with its reason.
pub fn read_user_dir(dir: &Path) -> (Vec<Entry>, Vec<String>) {
    let mut out = vec![];
    let mut bad = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (out, bad);
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ddcf")))
        .collect();
    files.sort();
    for p in files {
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) => {
                bad.push(format!("{}: {e}", p.display()));
                continue;
            }
        };
        for src in text
            .replace("\r\n", "\n")
            .split("\n---")
            .map(|s| s.trim_start_matches('\n'))
            .filter(|s| s.lines().any(|l| l.trim_start().starts_with("family ")))
        {
            match Family::parse(src) {
                Ok(f) => out.push(Entry::new(f, Source::User, Arc::from(src), Some(p.clone()))),
                Err(e) => bad.push(format!("{}: {e}", p.display())),
            }
        }
    }
    (out, bad)
}

/// Every family of the three sources: built-in, the project's (with the project's own types and
/// values), the user folder `user`. Marks what is already in the project and what was edited.
pub fn gather(m: &Model, user: Option<&Path>) -> (Vec<Entry>, Vec<String>) {
    let builtin: Vec<Entry> = ddc_model::library_sources()
        .into_iter()
        .filter_map(|s| {
            Family::parse(s)
                .ok()
                .map(|f| Entry::new(f, Source::Builtin, Arc::from(s), None))
        })
        .collect();
    let mut bad = vec![];
    let mut project = vec![];
    for (id, name) in m.families().unwrap_or_default() {
        let (Ok(mut f), Ok(def)) = (m.family(id), m.family_def(id)) else {
            bad.push(name);
            continue;
        };
        f.types = m
            .comp_types(id)
            .unwrap_or_default()
            .into_iter()
            .map(|(t, tn)| FamilyType {
                name: tn,
                values: m.comp_type_values(t).unwrap_or_default(),
            })
            .collect();
        let mut e = Entry::new(f, Source::Project, Arc::from(def.as_str()), None);
        e.in_project = true;
        e.edited = builtin
            .iter()
            .find(|b| b.fam.name == e.fam.name)
            .is_some_and(|b| b.def.trim() != e.def.trim());
        project.push(e);
    }
    let (user, more) = user.map(read_user_dir).unwrap_or_default();
    bad.extend(more);
    let names: HashSet<String> = project.iter().map(|e| e.fam.name.clone()).collect();
    let mut all = builtin;
    all.extend(project);
    all.extend(user);
    for e in &mut all {
        e.in_project = names.contains(&e.fam.name);
    }
    (all, bad)
}

/// Definition text of family `name` for loading it: the project's, else the built-in, else the
/// first user-library one.
pub fn def_of(m: &Model, name: &str) -> Option<String> {
    if let Ok(Some(id)) = m.family_id(name) {
        return m.family_def(id).ok();
    }
    if let Some(s) = ddc_model::library_sources()
        .into_iter()
        .find(|s| Family::parse(s).is_ok_and(|f| f.name == name))
    {
        return Some(s.to_string());
    }
    let dir = user_dir()?;
    read_user_dir(&dir)
        .0
        .into_iter()
        .find(|e| e.fam.name == name)
        .map(|e| e.def.to_string())
}

/// The family `name` as loading would give it (see `def_of`).
pub fn family_of(m: &Model, name: &str) -> Option<Family> {
    if let Ok(Some(id)) = m.family_id(name) {
        return m.family(id).ok();
    }
    def_of(m, name).and_then(|d| Family::parse(&d).ok())
}

/// Labels «Family : Type» of every family the Component tool can place: the project's (all of
/// them: loaded, edited, program-defined, imported) first, then built-in and user-library
/// families the project does not have yet.
pub fn type_labels(m: &Model) -> Vec<String> {
    let mut out = vec![];
    let mut seen: HashSet<String> = HashSet::new();
    for (id, name) in m.families().unwrap_or_default() {
        for (_, t) in m.comp_types(id).unwrap_or_default() {
            out.push(format!("{name} : {t}"));
        }
        seen.insert(name);
    }
    let user = user_dir().map(|d| read_user_dir(&d).0).unwrap_or_default();
    let fams = ddc_model::library()
        .unwrap_or_default()
        .into_iter()
        .chain(user.into_iter().map(|e| e.fam));
    for f in fams {
        if !seen.insert(f.name.clone()) {
            continue;
        }
        for t in &f.types {
            out.push(format!("{} : {}", f.name, t.name));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX: &str = "family Test Crate\ncategory generic\nhost floor\n\
        param S length type 400mm\ntype \"Small\" S=400mm\ntype \"Big\" S=800mm\n\
        extrude B rect(0, 0, S, S) z(0, S)\nsymbol rect(0, 0, S, S)\n";

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-complib-{name}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn user_folder_reads_families_and_reports_bad_ones() {
        let d = tmp("user");
        std::fs::write(d.join("a.ddcf"), format!("{BOX}\n---\nfamily Broken\nparam X nonsense\n"))
            .unwrap();
        std::fs::write(d.join("notes.txt"), "family Ignored\n").unwrap();
        let (e, bad) = read_user_dir(&d);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].fam.name, "Test Crate");
        assert_eq!(e[0].source, Source::User);
        assert_eq!(e[0].path.as_deref(), Some(d.join("a.ddcf").as_path()));
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(read_user_dir(&d.join("missing")).0.is_empty());
    }

    #[test]
    fn gather_lists_all_sources_and_marks_project_and_edited() {
        let d = tmp("gather");
        std::fs::write(d.join("box.ddcf"), BOX).unwrap();
        let mut m = Model::open(":memory:").unwrap();
        m.load_library_family("Chair").unwrap();
        // the project's Table differs from the built-in one; an extra type lives only here
        let table = ddc_model::library_sources()
            .into_iter()
            .find(|s| s.contains("family Table\n"))
            .unwrap()
            .replace("param Height length type 750mm", "param Height length type 760mm");
        let tid = m.load_family(&table).unwrap();
        let vals = m.comp_type_values(m.comp_types(tid).unwrap()[0].0).unwrap();
        m.add_comp_type(tid, "Project only", &vals).unwrap();
        let (all, bad) = gather(&m, Some(&d));
        assert!(bad.is_empty(), "{bad:?}");
        let n = ddc_model::library().unwrap().len();
        let of = |s: Source| all.iter().filter(|e| e.source == s).count();
        assert_eq!((of(Source::Builtin), of(Source::Project), of(Source::User)), (n, 2, 1));
        let get = |s: Source, name: &str| all.iter().find(|e| e.source == s && e.fam.name == name).unwrap();
        assert!(get(Source::Builtin, "Table").in_project);
        assert!(!get(Source::Builtin, "Bed").in_project);
        assert!(get(Source::Project, "Table").edited);
        assert!(!get(Source::Project, "Chair").edited);
        let pt = get(Source::Project, "Table");
        assert!(pt.fam.types.iter().any(|t| t.name == "Project only"));
        assert_ne!(pt.hash, get(Source::Builtin, "Table").hash);
        assert_ne!(pt.type_hash("A"), pt.type_hash("B"));
        // every built-in family builds, so every exporter carries it
        for e in all.iter().filter(|e| e.source == Source::Builtin) {
            let f = e.formats;
            assert_eq!(f.ifc, Ready::Full, "{}", e.fam.name);
            assert_eq!(f.gltf, Ready::Full, "{}", e.fam.name);
        }
    }

    #[test]
    fn a_family_that_does_not_build_is_ready_nowhere() {
        let f = Family::parse(
            "family Flat\ncategory generic\nparam S length type 0mm\nextrude B rect(0, 0, S, S) z(0, S)\n",
        )
        .unwrap();
        let r = formats(&f);
        assert_eq!((r.ifc, r.gltf), (Ready::No, Ready::No), "{r:?}");
    }

    #[test]
    fn type_labels_list_project_families_then_the_rest_once() {
        let mut m = Model::open(":memory:").unwrap();
        let ai = "family Program Bench\ncategory furniture\nhost floor\nstatus draft\n\
            param W length type 1200mm\ntype \"1200\" W=1200mm\n\
            extrude B rect(-W / 2, -200mm, W / 2, 200mm) z(0, 450mm)\n";
        m.load_family(ai).unwrap();
        m.load_library_family("Table").unwrap();
        let l = type_labels(&m);
        assert_eq!(l[0], "Program Bench : 1200");
        assert!(l.iter().filter(|x| x.starts_with("Table : ")).count() >= 3);
        let tables: Vec<&String> = l.iter().filter(|x| *x == "Table : 1600x800").collect();
        assert_eq!(tables.len(), 1, "a family in the project is not listed twice");
        assert!(l.iter().any(|x| x.starts_with("Bed : ")));
        assert_eq!(def_of(&m, "Program Bench").as_deref().map(str::trim), Some(ai.trim()));
        assert_eq!(family_of(&m, "Bed").unwrap().name, "Bed");
    }
}
