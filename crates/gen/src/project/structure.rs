// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Structural system of a project (`design.structure`): the wall, floor, roof and column types
//! each system builds with, read from `data/structure.json`. Geometry is the same for every
//! system; only the type names and their layers change.

use super::design::Structure;
use crate::WallRole;
use serde_json::Value as J;
use std::sync::OnceLock;

const DATA: &str = include_str!("../../data/structure.json");

/// A type name and its layers as (material, share of the thickness), outer layer first.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeDef {
    pub name: String,
    pub layers: Vec<(String, f64)>,
}

impl TypeDef {
    /// Layers of a type of this thickness, m.
    pub fn layers_at(&self, thickness: f64) -> Vec<(String, f64)> {
        self.layers.iter().map(|(m, s)| (m.clone(), s * thickness)).collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct System {
    pub id: String,
    /// Exterior, party, corridor, partition: the order of [`ROLES`].
    pub walls: [TypeDef; 4],
    pub floor: TypeDef,
    pub roof: TypeDef,
    /// Column type names start with this; the section follows.
    pub column_prefix: String,
    pub column_material: String,
}

pub const ROLES: [WallRole; 4] = [WallRole::Exterior, WallRole::Party, WallRole::Corridor, WallRole::Partition];
const ROLE_KEYS: [&str; 4] = ["exterior", "party", "corridor", "partition"];

impl System {
    /// The type of a wall role. A retaining wall is cast against the soil: reinforced concrete
    /// in every system, so it takes the concrete system's party wall, not this system's.
    pub fn wall(&self, role: WallRole) -> &TypeDef {
        match ROLES.iter().position(|r| *r == role) {
            Some(i) => &self.walls[i],
            None => &system(Structure::Concrete).walls[1],
        }
    }
}

fn type_def(v: &J, at: &str) -> TypeDef {
    let name = v["name"].as_str().unwrap_or_else(|| panic!("structure.json {at}.name")).to_string();
    let layers: Vec<(String, f64)> = v["layers"]
        .as_array()
        .unwrap_or_else(|| panic!("structure.json {at}.layers"))
        .iter()
        .map(|l| {
            let m = l[0].as_str().unwrap_or_else(|| panic!("structure.json {at} layer material"));
            (m.to_string(), l[1].as_f64().unwrap_or_else(|| panic!("structure.json {at} layer share")))
        })
        .collect();
    TypeDef { name, layers }
}

fn systems() -> &'static [System] {
    static CELL: OnceLock<Vec<System>> = OnceLock::new();
    CELL.get_or_init(|| {
        let j: J = serde_json::from_str(DATA).expect("structure.json");
        Structure::NAMES
            .iter()
            .map(|id| {
                let s = &j["systems"][*id];
                assert!(s.is_object(), "structure.json has no system {id}");
                System {
                    id: id.to_string(),
                    walls: ROLE_KEYS.map(|k| type_def(&s["walls"][k], &format!("{id}.walls.{k}"))),
                    floor: type_def(&s["floor"], &format!("{id}.floor")),
                    roof: type_def(&s["roof"], &format!("{id}.roof")),
                    column_prefix: s["column"]["prefix"].as_str().expect("structure.json column.prefix").to_string(),
                    column_material: s["column"]["material"].as_str().expect("structure.json column.material").to_string(),
                }
            })
            .collect()
    })
}

/// The types a structural system builds with.
pub fn system(s: Structure) -> &'static System {
    &systems()[Structure::ALL.iter().position(|x| *x == s).unwrap_or(0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_system_is_in_the_data_with_whole_layers() {
        for s in Structure::ALL {
            let sys = system(s);
            assert_eq!(sys.id, Structure::NAMES[Structure::ALL.iter().position(|x| *x == s).unwrap()]);
            for t in sys.walls.iter().chain([&sys.floor, &sys.roof]) {
                let sum: f64 = t.layers.iter().map(|l| l.1).sum();
                assert!((sum - 1.0).abs() < 1e-9 && t.layers.iter().all(|l| l.1 > 0.0), "{}: {sum}", t.name);
            }
        }
        // concrete keeps the names projects were built with, so revising them finds their types
        let c = system(Structure::Concrete);
        assert_eq!(c.wall(WallRole::Exterior).name, "Gen Exterior 300");
        assert_eq!((c.floor.name.as_str(), c.roof.name.as_str()), ("Gen Floor 200", "Gen Roof 250"));
    }

    #[test]
    fn a_retaining_wall_is_concrete_in_every_system() {
        for st in Structure::ALL {
            let w = system(st).wall(WallRole::Retaining);
            assert!(w.layers.iter().all(|l| l.0 == "Concrete"), "{st:?}: {w:?}");
            assert_eq!(w, system(Structure::Concrete).wall(WallRole::Party));
        }
    }

    #[test]
    fn timber_builds_clt_and_glulam_round_a_concrete_core() {
        let t = system(Structure::Timber);
        let has = |d: &TypeDef, w: &str| d.layers.iter().any(|l| l.0.to_lowercase().contains(w));
        assert!(has(t.wall(WallRole::Exterior), "timber") && has(t.wall(WallRole::Party), "timber"));
        assert!(has(&t.floor, "timber") && has(&t.roof, "timber"));
        assert!(t.column_material.to_lowercase().contains("glulam"));
        assert!(has(t.wall(WallRole::Corridor), "concrete"), "the core spine stays concrete");
        let names: Vec<&str> = t.walls.iter().map(|w| w.name.as_str()).collect();
        let concrete = system(Structure::Concrete);
        assert!(concrete.walls.iter().all(|w| !names.contains(&w.name.as_str())), "own type names");
        let at = t.wall(WallRole::Exterior).layers_at(0.3);
        assert!((at.iter().map(|l| l.1).sum::<f64>() - 0.3).abs() < 1e-9);
    }
}
