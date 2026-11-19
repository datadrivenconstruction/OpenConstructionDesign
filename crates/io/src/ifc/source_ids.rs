//! IFC export that keeps the source model's GlobalIds: elements imported from a .rvt (bridge
//! SourceIds frame) or an IFC file come back under their original ids, so pro BIM tools can
//! match the exported changes to the elements of the source project.
//! The exporter itself takes them from `ext_ref` (`export::guids`); this is the source-panel entry.

use std::path::Path;

use ddc_model::Model;

pub use super::step::is_global_id;

/// [`super::export_ifc`]: imported elements keep their source GlobalIds, new ones get hashed ids.
pub fn export_ifc_source_guids(model: &Model) -> Result<String, String> {
    super::export_ifc(model)
}

pub fn write_ifc_source_guids(model: &Model, path: &Path) -> Result<(), String> {
    std::fs::write(path, export_ifc_source_guids(model)?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_id_shape() {
        assert!(is_global_id("0YvctVUKr0kugbFTf53O9L"));
        assert!(is_global_id("3$$$$$$$$$$$$$$$$$$$$$"));
        assert!(!is_global_id("4YvctVUKr0kugbFTf53O9L"));
        assert!(!is_global_id("0YvctVUKr0kugbFTf53O9"));
        assert!(!is_global_id("0YvctVUKr0kugbFTf53O9-"));
        assert!(!is_global_id("1f2b6c3a-0000-4000-8000-00000004a1b2"));
    }

    #[test]
    fn source_guid_replaces_hash() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "ext",
                &[ddc_model::LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        let g = "2kQ$9Xyz0123456789abcd";
        m.sql()
            .execute(
                "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('ifc|a',?1,'wall',?2)",
                rusqlite::params![g, w],
            )
            .unwrap();
        let hashed = super::super::step::guid("wall", w);
        let text = export_ifc_source_guids(&m).unwrap();
        assert!(
            text.contains(&format!("IFCWALL('{g}'")),
            "source GlobalId kept"
        );
        assert!(!text.contains(&hashed));
    }
}
