//! What an import brought, in one shape for every reader: how many elements came as editable
//! (native) elements, how many as meshes, what was skipped and why, and the notes. Stored as
//! JSON in the project (`src_file.report`) so it can be reopened later.
use serde_json::{json, Value};

/// Category keys carry no text: the application translates `open_summary.cat.<key>`.
pub type Rows = Vec<(String, usize)>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportSummary {
    /// Display name of the source file.
    pub file: String,
    /// Native elements by category key.
    pub native: Rows,
    /// Mesh elements by their category name.
    pub mesh: Rows,
    /// Skipped source kinds, with the reason or class as the reader named it.
    pub skipped: Rows,
    /// First notes (warnings), at most [`MAX_NOTES`].
    pub notes: Vec<String>,
    /// All notes, also those not kept in `notes`.
    pub notes_total: usize,
}

/// The one set of numbers every reader (banner, toast, export sheets) shows.
/// Editable = every element kept in a native model table; meshes and skipped kinds are not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportCounts {
    pub walls: usize,
    pub openings: usize,
    pub slabs: usize,
    pub rooms: usize,
    /// Native elements outside the four named categories.
    pub other: usize,
    pub editable: usize,
    pub meshes: usize,
}

pub const MAX_NOTES: usize = 200;

fn sum(r: &Rows) -> usize {
    r.iter().map(|x| x.1).sum()
}

fn nonzero(rows: impl IntoIterator<Item = (String, usize)>) -> Rows {
    rows.into_iter().filter(|r| r.1 > 0).collect()
}

impl ImportSummary {
    pub fn native_total(&self) -> usize {
        sum(&self.native)
    }

    pub fn mesh_total(&self) -> usize {
        sum(&self.mesh)
    }

    pub fn counts(&self) -> ImportCounts {
        let get = |k: &str| {
            self.native
                .iter()
                .filter(|r| r.0 == k)
                .map(|r| r.1)
                .sum::<usize>()
        };
        let (walls, openings, slabs, rooms) =
            (get("wall"), get("opening"), get("slab"), get("room"));
        let editable = self.native_total();
        ImportCounts {
            walls,
            openings,
            slabs,
            rooms,
            other: editable - walls - openings - slabs - rooms,
            editable,
            meshes: self.mesh_total(),
        }
    }

    pub fn skipped_total(&self) -> usize {
        sum(&self.skipped)
    }

    /// Native categories a bridge report counts, in display order.
    pub fn from_bridge(file: &str, r: &crate::bridge::Report) -> Self {
        let native = [
            ("wall", r.wall_segments),
            ("opening", r.doors + r.windows + r.voids),
            ("slab", r.slabs),
            ("curtain_wall", r.curtain_walls),
            ("room", r.rooms),
            ("grid", r.grids),
            ("col", r.columns),
            ("beam", r.beams),
            ("ceiling", r.ceilings),
            ("mep_run", r.mep_runs),
        ];
        let mut s = Self {
            file: file.to_string(),
            native: nonzero(native.map(|(k, n)| (k.to_string(), n))),
            mesh: nonzero([("mesh".to_string(), r.meshes)]),
            ..Self::default()
        };
        s.set_skipped(r.skipped.iter().map(|(k, n)| (k.clone(), *n)));
        s.set_notes(&r.warnings, r.warning_count);
        s
    }

    /// An IFC import into the open project.
    pub fn from_ifc(file: &str, r: &crate::ifc::ImportReport) -> Self {
        let native = [
            ("wall", r.walls),
            ("opening", r.openings),
            ("slab", r.slabs),
            ("curtain_wall", r.curtain_walls),
            ("room", r.rooms + r.separators),
            ("grid", r.grids),
            ("col", r.columns),
            ("beam", r.beams),
            ("roof", r.roofs),
            ("ceiling", r.ceilings),
            ("stair", r.stairs),
            ("railing", r.railings),
            ("ramp", r.ramps),
            ("mep_run", r.mep_runs),
            ("comp", r.components),
        ];
        let mut s = Self {
            file: file.to_string(),
            native: nonzero(native.map(|(k, n)| (k.to_string(), n))),
            mesh: nonzero([("mesh".to_string(), r.meshes)]),
            ..Self::default()
        };
        s.set_skipped(r.skipped.iter().map(|(k, n)| (k.clone(), *n)));
        s.set_notes(&r.warnings, r.warnings.len());
        s
    }

    pub fn set_skipped(&mut self, it: impl IntoIterator<Item = (String, usize)>) {
        let mut v: Rows = nonzero(it);
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.skipped = v;
    }

    /// `total` may exceed `list.len()` when the reader kept only the first notes.
    pub fn set_notes(&mut self, list: &[String], total: usize) {
        self.notes = list.iter().take(MAX_NOTES).cloned().collect();
        self.notes_total = total.max(list.len());
    }

    pub fn to_json(&self) -> String {
        let rows = |r: &Rows| r.iter().map(|(k, n)| json!([k, n])).collect::<Vec<_>>();
        json!({
            "v": 1,
            "file": self.file,
            "native": rows(&self.native),
            "mesh": rows(&self.mesh),
            "skipped": rows(&self.skipped),
            "notes": self.notes,
            "notes_total": self.notes_total,
        })
        .to_string()
    }

    /// None for text that is not a report of this version.
    pub fn from_json(text: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(text).ok()?;
        if v.get("v")?.as_u64()? != 1 {
            return None;
        }
        let rows = |k: &str| -> Option<Rows> {
            v.get(k)?
                .as_array()?
                .iter()
                .map(|r| {
                    Some((
                        r.get(0)?.as_str()?.to_string(),
                        r.get(1)?.as_u64()? as usize,
                    ))
                })
                .collect()
        };
        Some(Self {
            file: v.get("file")?.as_str()?.to_string(),
            native: rows("native")?,
            mesh: rows("mesh")?,
            skipped: rows("skipped")?,
            notes: v
                .get("notes")?
                .as_array()?
                .iter()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect(),
            notes_total: v.get("notes_total")?.as_u64()? as usize,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editable_equals_sum_of_element_counts() {
        let s = ImportSummary {
            native: vec![
                ("wall".into(), 6),
                ("opening".into(), 1),
                ("slab".into(), 4),
                ("grid".into(), 2),
            ],
            mesh: vec![("mesh".into(), 3)],
            ..Default::default()
        };
        let c = s.counts();
        assert_eq!(c.editable, 13);
        assert_eq!(c.walls + c.openings + c.slabs + c.rooms + c.other, c.editable);
        assert_eq!((c.rooms, c.other, c.meshes), (0, 2, 3));
        assert_eq!(ImportSummary::default().counts(), ImportCounts::default());
    }

    fn sample() -> ImportSummary {
        let mut s = ImportSummary {
            file: "House.rvt".into(),
            native: vec![("wall".into(), 1500), ("slab".into(), 506)],
            mesh: vec![("Furniture".into(), 3000), ("Plumbing".into(), 212)],
            ..Default::default()
        };
        s.set_skipped([
            ("IfcAnnotation".to_string(), 3),
            ("IfcSpace".to_string(), 9),
            ("x".to_string(), 0),
        ]);
        s.set_notes(&["a".to_string(), "b".to_string()], 95);
        s
    }

    #[test]
    fn totals_add_up() {
        let s = sample();
        assert_eq!(
            (s.native_total(), s.mesh_total(), s.skipped_total()),
            (2006, 3212, 12)
        );
        assert_eq!(s.notes_total, 95);
    }

    #[test]
    fn skipped_sorted_by_count_and_zero_dropped() {
        let s = sample();
        assert_eq!(s.skipped[0].0, "IfcSpace");
        assert_eq!(s.skipped.len(), 2);
    }

    #[test]
    fn json_round_trip() {
        let s = sample();
        assert_eq!(ImportSummary::from_json(&s.to_json()), Some(s));
        assert_eq!(ImportSummary::from_json("{}"), None);
        assert_eq!(ImportSummary::from_json("not json"), None);
    }

    #[test]
    fn bridge_report_counts_native_and_mesh() {
        let r = crate::bridge::Report {
            wall_segments: 10,
            doors: 2,
            windows: 3,
            meshes: 7,
            warning_count: 4,
            ..Default::default()
        };
        let s = ImportSummary::from_bridge("a.rvt", &r);
        assert_eq!(
            (s.native_total(), s.mesh_total(), s.notes_total),
            (15, 7, 4)
        );
    }
}
