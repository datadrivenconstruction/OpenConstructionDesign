//! GlobalIds of exported entities. Elements that came from an import keep their external id
//! (`ext_ref`); the others get a stable generated one. Read-only: nothing is written to the model.

use std::collections::{BTreeMap, HashMap, HashSet};

use ddc_model::Model;

use super::super::step::{compress_guid, hash128, is_global_id};

/// Tables an exported entity can be linked to in `ext_ref`.
const TABLES: [&str; 22] = [
    "wall",
    "slab",
    "comp",
    "room",
    "curtain_wall",
    "room_sep",
    "ceiling",
    "roof",
    "mesh",
    "col",
    "beam",
    "opening",
    "level",
    "site",
    "building",
    "project",
    "opening_fill",
    "grid",
    "mep_run",
    "stair",
    "railing",
    "ramp",
];

/// `ext_ref.kind` (table) of an exported entity kind; None — never imported 1:1.
pub(super) fn ext_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "wall" => "wall",
        "slab" => "slab",
        "comp" => "comp",
        "space" => "room",
        "curtain" => "curtain_wall",
        "separator" => "room_sep",
        "ceiling" => "ceiling",
        "roof" => "roof",
        "mesh" => "mesh",
        "col" => "col",
        "beam" => "beam",
        "opening" => "opening",
        "door" | "window" => "opening_fill",
        "storey" => "level",
        "site" => "site",
        "building" => "building",
        "project" => "project",
        "mep-run" => "mep_run",
        "stair" => "stair",
        "railing" => "railing",
        "ramp" => "ramp",
        _ => return None,
    })
}

/// IFC GlobalId from a native-rvt unique id: `<8-4-4-4-12 guid>-<8 hex element id>` maps to the
/// guid with its low 32 bits xor-ed by the element id; any other text is hashed.
pub(super) fn rvt_global_id(uid: &str) -> Option<String> {
    let u = uid.trim();
    if u.is_empty() {
        return None;
    }
    if u.len() == 45 && u.is_ascii() && u.as_bytes()[36] == b'-' {
        let hex: String = u[..36].chars().filter(|c| *c != '-').collect();
        if let (Ok(v), Ok(id)) = (
            u128::from_str_radix(&hex, 16),
            u32::from_str_radix(&u[37..], 16),
        ) {
            return Some(compress_guid(v ^ id as u128));
        }
    }
    Some(compress_guid(hash128(&format!("rvt:{u}"))))
}

/// `ifc`, `ifc:<project id>` or a named source `ifc|<file hash>`.
fn ifc_source(source: &str) -> bool {
    source == "ifc" || source.starts_with("ifc:") || source.starts_with("ifc|")
}

/// Candidate GlobalId of one `ext_ref` row, with its rank (0 = main project, lower is better).
fn candidate(source: &str, guid: &str, main: &str) -> Option<(u8, String)> {
    if ifc_source(source) {
        // `g#1` links (extra pieces of one source element) are not GlobalIds
        return is_global_id(guid).then(|| (if source == main { 0 } else { 1 }, guid.to_string()));
    }
    let rvt_name = source.split('|').next().is_some_and(|b| b.contains("rvt"));
    let uid_form = guid.len() == 45 && guid.as_bytes().get(36) == Some(&b'-');
    if rvt_name || uid_form {
        return rvt_global_id(guid).map(|g| (2, g));
    }
    None
}

pub(super) struct Guids {
    /// (ext_ref kind, row) -> best (rank, GlobalId)
    ext: HashMap<(&'static str, i64), (u8, String)>,
    /// the row that keeps a GlobalId several rows claim (the others get a generated one)
    owner: HashMap<String, (&'static str, i64)>,
    /// every imported GlobalId; generated ids avoid them
    reserved: HashSet<String>,
    project: Option<String>,
    /// namespace of generated ids (main source; empty for a model that was never imported)
    seed: String,
    done: HashMap<(String, i64), String>,
    used: HashSet<String>,
    pub warnings: Vec<String>,
}

impl Guids {
    pub(super) fn load(model: &Model) -> Result<Self, String> {
        let e = |e: rusqlite::Error| format!("db: {e}");
        let sql = model.sql();
        let has: i64 = sql
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='ext_ref'",
                [],
                |r| r.get(0),
            )
            .map_err(e)?;
        let mut rows: Vec<(String, String, String, i64)> = vec![];
        if has > 0 {
            let mut st = sql
                .prepare("SELECT source, guid, kind, ref FROM ext_ref ORDER BY source, guid")
                .map_err(e)?;
            let it = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(e)?;
            for r in it {
                rows.push(r.map_err(e)?);
            }
        }
        // main project = the IFC source with the most linked rows (ties: smallest name)
        let mut count: BTreeMap<&str, usize> = BTreeMap::new();
        for (src, ..) in &rows {
            if ifc_source(src) {
                *count.entry(src.as_str()).or_default() += 1;
            }
        }
        let mut main = "";
        let mut best = 0;
        for (src, n) in &count {
            if *n > best {
                (main, best) = (*src, *n);
            }
        }
        let project = main
            .strip_prefix("ifc:")
            .filter(|g| is_global_id(g))
            .map(str::to_string);
        let mut ext: HashMap<(&'static str, i64), (u8, String)> = HashMap::new();
        let mut reserved = HashSet::new();
        // what the source itself reported (bridge SourceIds frame) beats an id derived from its key
        let mut reported: HashMap<(String, String), String> = HashMap::new();
        // no such table in a store that never imported through a source
        if let Ok(mut st) =
            sql.prepare("SELECT source, guid, ifc_guid FROM src_base WHERE ifc_guid IS NOT NULL")
        {
            let it = st
                .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))
                .map_err(e)?;
            for r in it {
                let (k, g): ((String, String), String) = r.map_err(e)?;
                if is_global_id(&g) {
                    reported.insert(k, g);
                }
            }
        }
        for (src, guid, kind, id) in &rows {
            let known = reported.get(&(src.clone(), guid.clone()));
            let Some((rank, g)) = known
                .map(|g| (1, g.clone()))
                .or_else(|| candidate(src, guid, main))
            else {
                continue;
            };
            reserved.insert(g.clone());
            let Some(k) = TABLES.into_iter().find(|t| *t == kind.as_str()) else {
                continue;
            };
            let slot = ext.entry((k, *id)).or_insert((rank, g.clone()));
            if rank < slot.0 {
                *slot = (rank, g);
            }
        }
        let owner = Self::owners(model, &ext);
        let mut reserved = reserved;
        if let Some(p) = &project {
            reserved.insert(p.clone());
        }
        Ok(Guids {
            ext,
            owner,
            reserved,
            project,
            seed: main.to_string(),
            done: HashMap::new(),
            used: HashSet::new(),
            warnings: vec![],
        })
    }

    /// Owner of each GlobalId claimed by more than one row: best source rank, then the lowest
    /// existing row (the original, not a copy), then table order. Rows that no longer exist do
    /// not hold an id.
    fn owners(
        model: &Model,
        ext: &HashMap<(&'static str, i64), (u8, String)>,
    ) -> HashMap<String, (&'static str, i64)> {
        let mut claims: HashMap<&str, Vec<(u8, i64, usize, &'static str)>> = HashMap::new();
        for ((k, id), (rank, g)) in ext {
            let t = TABLES.iter().position(|x| x == k).unwrap_or(usize::MAX);
            claims
                .entry(g.as_str())
                .or_default()
                .push((*rank, *id, t, *k));
        }
        let mut out = HashMap::new();
        for (g, mut v) in claims {
            v.sort();
            let pick = if v.len() == 1 {
                v[0]
            } else {
                // project, site and building have no row (ref 0)
                let alive = |c: &(u8, i64, usize, &'static str)| {
                    matches!(c.3, "project" | "site" | "building")
                        || model
                            .sql()
                            .query_row(
                                &format!("SELECT EXISTS(SELECT 1 FROM {} WHERE id=?1)", c.3),
                                [c.1],
                                |r| r.get::<_, i64>(0),
                            )
                            .map_or(true, |n| n != 0)
                };
                v.iter().copied().find(alive).unwrap_or(v[0])
            };
            out.insert(g.to_string(), (pick.3, pick.1));
        }
        out
    }

    /// Every (kind, id, GlobalId) given out so far.
    pub(super) fn issued(&self) -> impl Iterator<Item = (&str, i64, &str)> + '_ {
        self.done
            .iter()
            .map(|((k, i), g)| (k.as_str(), *i, g.as_str()))
    }

    /// This GlobalId came from an imported file.
    pub(super) fn is_imported(&self, g: &str) -> bool {
        self.reserved.contains(g)
    }

    fn fresh(&mut self, kind: &str, id: i64) -> String {
        for salt in 0u32.. {
            let mut key = if self.seed.is_empty() {
                format!("{kind}:{id}")
            } else {
                format!("{}|{kind}:{id}", self.seed)
            };
            if salt > 0 {
                key.push_str(&format!("|#{salt}"));
            }
            let g = compress_guid(hash128(&key));
            if !self.used.contains(&g) && !self.reserved.contains(&g) {
                return g;
            }
        }
        unreachable!("u32 salts exhausted")
    }

    /// GlobalId (quoted STEP string) of the entity (`kind`, `id`); the same pair always gives
    /// the same text within one export.
    pub(super) fn get(&mut self, kind: &str, id: i64) -> String {
        self.get_via(kind, id, &[])
    }

    /// As `get`, for an entity that stands for several imported rows (`via`: (table, row)): the
    /// first row with an imported GlobalId gives it.
    pub(super) fn get_via(&mut self, kind: &str, id: i64, via: &[(&'static str, i64)]) -> String {
        let key = (kind.to_string(), id);
        if let Some(g) = self.done.get(&key) {
            return format!("'{g}'");
        }
        let own = ext_kind(kind).map(|k| (k, id));
        let mut want = None;
        let mut lost: Option<(String, (&'static str, i64))> = None;
        for k in via.iter().chain(own.iter().filter(|_| via.is_empty())) {
            let Some((_, g)) = self.ext.get(k) else {
                continue;
            };
            match self.owner.get(g) {
                Some(o) if o == k => {
                    want = Some(g.clone());
                    break;
                }
                Some(o) => {
                    lost.get_or_insert((g.clone(), *o));
                }
                None => {}
            }
        }
        if want.is_none() && kind == "project" {
            want = self.project.clone();
        }
        if want.is_none() {
            if let Some((g, o)) = lost {
                self.warnings.push(format!(
                    "{kind} {id}: GlobalId {g} belongs to {} {}, generated a new one",
                    o.0, o.1
                ));
            }
        }
        let mut chosen = None;
        if let Some(w) = want {
            if self.used.contains(&w) {
                self.warnings.push(format!(
                    "{kind} {id}: GlobalId {w} is already used, generated a new one"
                ));
            } else {
                chosen = Some(w);
            }
        }
        let g = match chosen {
            Some(g) => g,
            None => self.fresh(kind, id),
        };
        self.used.insert(g.clone());
        self.done.insert(key, g.clone());
        format!("'{g}'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ifc::import::tests::SAMPLE_2X3;
    use crate::ifc::{export_ifc, export_ifc_with_warnings, import_ifc};
    use ddc_model::WallSpec;

    const WALL: &str = "0000000000000000000003";

    fn imported() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        import_ifc(&mut m, SAMPLE_2X3.as_bytes()).unwrap();
        m
    }

    /// The sample plus a site, a building and a second opening filled by a window.
    fn imported_full() -> Model {
        const EXTRA: &str = r"#90=IFCCARTESIANPOINT((2500.,200.,0.));
#91=IFCAXIS2PLACEMENT3D(#90,$,$);
#92=IFCLOCALPLACEMENT(#24,#91);
#93=IFCCARTESIANPOINT((450.,1300.));
#94=IFCAXIS2PLACEMENT2D(#93,$);
#95=IFCRECTANGLEPROFILEDEF(.AREA.,$,#94,900.,1000.);
#97=IFCEXTRUDEDAREASOLID(#95,#68,#21,400.);
#98=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#97));
#99=IFCPRODUCTDEFINITIONSHAPE($,$,(#98));
#100=IFCOPENINGELEMENT('0000000000000000000012',$,'O2',$,$,#92,#99,$);
#101=IFCRELVOIDSELEMENT('0000000000000000000013',$,$,$,#41,#100);
#102=IFCWINDOW('0000000000000000000014',$,'W2',$,$,#92,$,$,1000.,900.);
#103=IFCRELFILLSELEMENT('0000000000000000000015',$,$,$,#100,#102);
#110=IFCSITE('0000000000000000000016',$,'S',$,$,$,$,$,.ELEMENT.,$,$,$,$,$);
#111=IFCBUILDING('0000000000000000000017',$,'B',$,$,$,$,$,.ELEMENT.,$,$,$);
";
        let src = SAMPLE_2X3.replacen(
            "ENDSEC;\nEND-ISO",
            &format!(
                "{EXTRA}ENDSEC;\nEND-ISO"
            ),
            1,
        );
        let mut m = Model::open(":memory:").unwrap();
        import_ifc(&mut m, src.as_bytes()).unwrap();
        m
    }

    fn count(m: &Model, table: &str) -> i64 {
        m.sql()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn links(m: &Model) -> Vec<(String, String, String)> {
        let mut st = m
            .sql()
            .prepare("SELECT source, kind, guid FROM ext_ref ORDER BY source, kind, guid")
            .unwrap();
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    /// First argument of every entity line that is a well-formed GlobalId.
    fn global_ids(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|l| {
                let args = l.split_once('=')?.1.split_once('(')?.1;
                let g = args.strip_prefix('\'')?.get(..22)?;
                (args.as_bytes().get(23) == Some(&b'\'') && is_global_id(g)).then(|| g.to_string())
            })
            .collect()
    }

    fn all_unique(ids: &[String]) -> bool {
        ids.iter().collect::<HashSet<_>>().len() == ids.len()
    }

    #[test]
    fn imported_global_ids_survive_export_and_reimport() {
        let mut m = imported_full();
        let a = links(&m);
        let src = "ifc:0000000000000000000001".to_string();
        for (kind, g) in [
            ("project", "0000000000000000000001"),
            ("level", "0000000000000000000002"),
            ("wall", WALL),
            ("opening", "0000000000000000000006"),
            ("opening_fill", "0000000000000000000008"),
            ("opening", "0000000000000000000012"),
            ("opening_fill", "0000000000000000000014"),
            ("site", "0000000000000000000016"),
            ("building", "0000000000000000000017"),
        ] {
            let row = (src.clone(), kind.to_string(), g.to_string());
            assert!(a.contains(&row), "import did not link {row:?}");
        }
        let (text, warnings) = export_ifc_with_warnings(&m).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let ids = global_ids(&text);
        for (_, _, g) in &a {
            assert!(ids.contains(g), "export lost {g}");
        }
        assert!(all_unique(&ids));
        let mut m2 = Model::open(":memory:").unwrap();
        import_ifc(&mut m2, text.as_bytes()).unwrap();
        let b = links(&m2);
        for x in &a {
            assert!(b.contains(x), "lost {x:?}");
        }
        // the same file into the same model updates the linked rows, nothing is added
        let before = ["level", "wall", "opening"].map(|t| count(&m, t));
        import_ifc(&mut m, text.as_bytes()).unwrap();
        assert_eq!(["level", "wall", "opening"].map(|t| count(&m, t)), before);
        assert_eq!(links(&m), a);
    }

    #[test]
    fn duplicate_mapping_generates_a_new_id_with_a_warning() {
        let m = imported();
        // the opening is now also claimed (by another project) under the wall's GlobalId
        m.sql()
            .execute(
                "UPDATE ext_ref SET source='ifc:0000000000000000000099', guid=?1                  WHERE kind='opening' AND guid='0000000000000000000006'",
                [WALL],
            )
            .unwrap();
        let (text, warnings) = export_ifc_with_warnings(&m).unwrap();
        let ids = global_ids(&text);
        assert!(all_unique(&ids));
        assert_eq!(ids.iter().filter(|i| *i == WALL).count(), 1);
        assert!(text.contains(&format!("IFCWALL('{WALL}'")));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    /// The wall that keeps `WALL` after an export and a re-import: its axis.
    fn wall_axis_of(m: &Model, guid: &str) -> [i64; 4] {
        m.sql()
            .query_row(
                "SELECT w.ax, w.ay, w.bx, w.by FROM wall w JOIN ext_ref e                  ON e.kind='wall' AND e.ref=w.id WHERE e.guid=?1",
                [guid],
                |r| {
                    let mut v = [0i64; 4];
                    for (i, x) in v.iter_mut().enumerate() {
                        *x = (r.get::<_, f64>(i)? * 1e4).round() as i64;
                    }
                    Ok(v)
                },
            )
            .unwrap()
    }

    #[test]
    fn a_copy_sharing_the_ext_ref_never_takes_the_original_id() {
        let mut m = imported();
        let one = |m: &Model, q: &str| -> i64 { m.sql().query_row(q, [], |r| r.get(0)).unwrap() };
        let wtype = one(&m, "SELECT id FROM wall_type LIMIT 1");
        let orig = wall_axis_of(&m, WALL);
        // the copy sits on a lower level (exported first) and is linked to the same GlobalId
        // through another project's source
        let low = m.add_level("Low", -3.0).unwrap();
        let copy = m
            .add_wall(&WallSpec {
                a: (30.0, 30.0),
                b: (34.0, 30.0),
                level: low,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wtype,
                offset0: None,
            })
            .unwrap();
        m.sql()
            .execute(
                "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('ifc:0000000000000000000077',?1,'wall',?2)",
                rusqlite::params![WALL, copy],
            )
            .unwrap();
        let (text, warnings) = export_ifc_with_warnings(&m).unwrap();
        let ids = global_ids(&text);
        assert!(all_unique(&ids));
        assert_eq!(ids.iter().filter(|i| *i == WALL).count(), 1);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let mut m2 = Model::open(":memory:").unwrap();
        import_ifc(&mut m2, text.as_bytes()).unwrap();
        assert_eq!(wall_axis_of(&m2, WALL), orig);
        // and the next export is the same text
        assert_eq!(text, export_ifc(&m).unwrap());
    }

    #[test]
    fn a_stale_claim_does_not_hold_an_id() {
        let mut m = imported();
        let wtype: i64 = m
            .sql()
            .query_row("SELECT id FROM wall_type LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let level: i64 = m
            .sql()
            .query_row("SELECT id FROM level LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (30.0, 30.0),
                b: (34.0, 30.0),
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wtype,
                offset0: None,
            })
            .unwrap();
        // a link to a row that is gone (deleting a wall keeps its link) must not block the id
        m.sql()
            .execute(
                "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('ifc:0000000000000000000077','0000000000000000000055','wall',0)",
                [],
            )
            .unwrap();
        m.sql()
            .execute(
                "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('ifc:0000000000000000000088','0000000000000000000055','wall',?1)",
                [w],
            )
            .unwrap();
        let (text, warnings) = export_ifc_with_warnings(&m).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(global_ids(&text).contains(&"0000000000000000000055".to_string()));
    }

    #[test]
    fn new_elements_get_stable_ids_and_rvt_ids_are_derived() {
        let m = imported();
        let one = |q: &str| -> i64 { m.sql().query_row(q, [], |r| r.get(0)).unwrap() };
        let (level, wtype) = (
            one("SELECT id FROM level LIMIT 1"),
            one("SELECT id FROM wall_type LIMIT 1"),
        );
        let mut m = m;
        let w = m
            .add_wall(&WallSpec {
                a: (10.0, 10.0),
                b: (14.0, 10.0),
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wtype,
                offset0: None,
            })
            .unwrap();
        let t1 = export_ifc(&m).unwrap();
        assert_eq!(t1, export_ifc(&m).unwrap());
        let walls = |t: &str| -> Vec<String> {
            t.lines()
                .filter(|l| l.contains("=IFCWALL("))
                .map(|l| global_ids(l)[0].clone())
                .collect()
        };
        let ids = walls(&t1);
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().any(|i| i == WALL));
        assert!(all_unique(&global_ids(&t1)));

        // the same wall linked to a native-rvt unique id
        let uid = "11111111-2222-3333-4444-555555555555-0000000a";
        m.sql()
            .execute(
                "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('rvt|a.rvt',?1,'wall',?2)",
                rusqlite::params![uid, w],
            )
            .unwrap();
        let want = compress_guid(0x11111111222233334444555555555555u128 ^ 0xa);
        assert_eq!(rvt_global_id(uid).unwrap(), want);
        let t2 = export_ifc(&m).unwrap();
        assert!(walls(&t2).contains(&want));
        assert_eq!(t2, export_ifc(&m).unwrap());
    }

    #[test]
    fn rvt_ids_that_are_not_guid_shaped_are_hashed() {
        let g = rvt_global_id("some-id").unwrap();
        assert!(is_global_id(&g));
        assert_eq!(g, rvt_global_id("some-id").unwrap());
        assert!(rvt_global_id(" ").is_none());
    }
}
