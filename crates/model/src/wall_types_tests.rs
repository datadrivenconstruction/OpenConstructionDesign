use super::*;
use crate::WallSpec;

fn l(m: &str, t: f64, f: LayerFn) -> TypeLayer {
    TypeLayer::new(m, t, f)
}

/// render 0.02 | brick 0.25 (core) | insulation 0.10 | gypsum 0.0125
fn ext() -> Vec<TypeLayer> {
    vec![
        l("render", 0.02, LayerFn::Finish1),
        l("brick", 0.25, LayerFn::Structure),
        l("insulation", 0.10, LayerFn::Insulation),
        l("gypsum", 0.0125, LayerFn::Finish2),
    ]
}

fn specs(v: &[TypeLayer]) -> Vec<LayerSpec> {
    v.iter().map(TypeLayer::spec).collect()
}

/// A model with one level, type `ext()` and two walls: one centred, one with an offset.
fn model_at(path: &str) -> (Model, i64, i64, i64) {
    let mut m = Model::open(path).unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m.add_wall_type("Ext 380", &specs(&ext())).unwrap();
    m.edit_wall_type(t, "Ext 380", &ext(), Align::Core).unwrap();
    let w = |off: Option<f64>, y: f64| WallSpec {
        a: (0.0, y),
        b: (5.0, y),
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type: t,
        offset0: off,
    };
    let a = m.add_wall(&w(None, 0.0)).unwrap();
    let b = m.add_wall(&w(Some(-0.1), 4.0)).unwrap();
    (m, t, a, b)
}

fn model() -> (Model, i64, i64, i64) {
    model_at(":memory:")
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("ddc-wtypes-{tag}-{}-{ns}.ddc", std::process::id()))
}

/// World position of the core centre of a wall along its left normal.
fn core_centre(m: &Model, wall: i64, t: i64) -> f64 {
    let ls = m.wall_type_layers_full(t).unwrap();
    let (a, b) = core_range(&ls);
    m.wall_offset0(wall).unwrap() + total(&ls[..a]) + total(&ls[a..=b]) / 2.0
}

#[test]
fn functions_and_priorities_round_trip() {
    let (m, t, _, _) = model();
    let got = m.wall_type_layers_full(t).unwrap();
    assert_eq!(got, ext());
    assert_eq!(got[1].priority, 1);
    assert_eq!(got[3].priority, 5);
    // the core flag the geometry reads follows the function
    let flags = m.wall_layer_flags(t).unwrap();
    assert_eq!(flags.iter().map(|f| f.0).collect::<Vec<_>>(), [false, true, false, false]);
}

#[test]
fn a_type_without_stored_functions_gets_a_structure() {
    let mut m = Model::open(":memory:").unwrap();
    let raw = [
        l("plaster", 0.01, LayerFn::Finish1),
        l("concrete", 0.2, LayerFn::Finish1),
        l("insulation", 0.3, LayerFn::Finish1),
    ];
    let t = m.add_wall_type("old", &specs(&raw)).unwrap();
    let got = m.wall_type_layers_full(t).unwrap();
    let f: Vec<LayerFn> = got.iter().map(|x| x.function).collect();
    // the thickest layer insulates: the concrete carries the wall
    assert_eq!(f, [LayerFn::Finish1, LayerFn::Structure, LayerFn::Insulation]);
}

#[test]
fn check_type_refuses_bad_build_ups() {
    assert!(check_type("x", &[]).is_err(), "at least one layer");
    assert!(check_type(" ", &ext()).is_err(), "a name");
    let mut v = ext();
    v[0].thickness = 0.0;
    assert!(check_type("x", &v).is_err(), "thickness > 0");
    v[0].thickness = f64::NAN;
    assert!(check_type("x", &v).is_err());
    let mut v = ext();
    v[2].material = "  ".into();
    assert!(check_type("x", &v).is_err());
    let mut v = ext();
    v[2].priority = 0;
    assert!(check_type("x", &v).is_err());
    assert!(check_type("x", &ext()).is_ok());
}

#[test]
fn layer_ops_add_remove_reorder() {
    let (mut m, t, _, _) = model();
    let mut v = m.wall_type_layers_full(t).unwrap();
    v.insert(3, l("membrane", 0.001, LayerFn::Membrane));
    v.remove(0);
    v.swap(0, 1);
    m.edit_wall_type(t, "Ext 380", &v, Align::Exterior).unwrap();
    let got = m.wall_type_layers_full(t).unwrap();
    let mats: Vec<&str> = got.iter().map(|x| x.material.as_str()).collect();
    assert_eq!(mats, ["insulation", "brick", "membrane", "gypsum"]);
    assert_eq!(got[2].function, LayerFn::Membrane);
    assert!(m.edit_wall_type(t, "Ext 380", &[], Align::Core).is_err());
}

#[test]
fn realign_pure_rules() {
    let old = ext();
    let mut new = ext();
    new[2].thickness = 0.2; // thicker insulation, inside the core
    // exterior: the face stays, a centred wall becomes explicit
    let s = -total(&old) / 2.0;
    assert_eq!(realign(&old, &new, None, Align::Exterior), Some(s));
    assert_eq!(realign(&old, &new, Some(-0.1), Align::Exterior), Some(-0.1));
    // core: the brick centre stays put
    let c_old = s + 0.02 + 0.125;
    let s2 = realign(&old, &new, None, Align::Core).unwrap();
    assert!((s2 + 0.02 + 0.125 - c_old).abs() < 1e-12);
    // nothing moves: a centred wall stays centred
    assert_eq!(realign(&old, &old, None, Align::Core), None);
    assert_eq!(realign(&old, &old, None, Align::Exterior), None);
}

#[test]
fn thickness_change_keeps_the_exterior_face() {
    let (mut m, t, a, b) = model();
    let before = [m.wall_offset0(a).unwrap(), m.wall_offset0(b).unwrap()];
    let mut v = m.wall_type_layers_full(t).unwrap();
    v[1].thickness = 0.38;
    let r = m.edit_wall_type(t, "Ext 510", &v, Align::Exterior).unwrap();
    assert_eq!(r.walls, 2);
    assert_eq!(r.shifted, 1, "only the centred wall needed an explicit offset");
    let after = [m.wall_offset0(a).unwrap(), m.wall_offset0(b).unwrap()];
    for (x, y) in before.iter().zip(after) {
        assert!((x - y).abs() < 1e-12, "exterior face moved: {x} -> {y}");
    }
}

#[test]
fn thickness_change_keeps_the_core_centre() {
    let (mut m, t, a, b) = model();
    let before = [core_centre(&m, a, t), core_centre(&m, b, t)];
    let mut v = m.wall_type_layers_full(t).unwrap();
    v[0].thickness = 0.05; // outer finish thicker
    v[2].thickness = 0.15; // insulation thicker
    let r = m.edit_wall_type(t, "Ext 380", &v, Align::Core).unwrap();
    assert_eq!(r.shifted, 2);
    let after = [core_centre(&m, a, t), core_centre(&m, b, t)];
    for (x, y) in before.iter().zip(after) {
        assert!((x - y).abs() < 1e-12, "core moved: {x} -> {y}");
    }
}

#[test]
fn an_edit_is_one_undo_step() {
    let (mut m, t, a, b) = model();
    let layers = m.wall_type_layers_full(t).unwrap();
    let offs = (m.wall_offset0(a).unwrap(), m.wall_offset0(b).unwrap());
    let mut v = layers.clone();
    v.push(l("tile", 0.01, LayerFn::Finish2));
    v[1].thickness = 0.3;
    m.edit_wall_type(t, "Renamed", &v, Align::Core).unwrap();
    m.undo().unwrap();
    assert_eq!(m.wall_type_layers_full(t).unwrap(), layers);
    assert_eq!((m.wall_offset0(a).unwrap(), m.wall_offset0(b).unwrap()), offs);
    let name: String = m
        .sql()
        .query_row("SELECT name FROM wall_type WHERE id=?1", [t], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "Ext 380");
    let raw: Option<f64> = m
        .sql()
        .query_row("SELECT offset0 FROM wall WHERE id=?1", [a], |r| r.get(0))
        .unwrap();
    assert_eq!(raw, None, "the centred wall is centred again, not explicit");
    // the step before is still the last wall: one more undo removes it, not the edit
    m.undo().unwrap();
    assert_eq!(m.walls_of_type(t).unwrap(), [a]);
}

#[test]
fn duplicate_and_rename() {
    let (mut m, t, _, _) = model();
    let name = m.free_wall_type_name("Ext 380").unwrap();
    assert_eq!(name, "Ext 380 2");
    let d = m.duplicate_wall_type(t, &name).unwrap();
    assert_eq!(m.wall_type_layers_full(d).unwrap(), ext());
    assert!(m.rename_wall_type(d, "Ext 380").is_err(), "name taken");
    m.rename_wall_type(d, "Ext copy").unwrap();
    let u = m.wall_type_usage().unwrap();
    let names: Vec<(&str, usize)> = u.iter().map(|x| (x.name.as_str(), x.walls)).collect();
    assert_eq!(names, [("Ext 380", 2), ("Ext copy", 0)]);
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(m.wall_type_usage().unwrap().len(), 1);
}

#[test]
fn archive_moves_unused_types_and_restores_them() {
    let (mut m, t, _, _) = model();
    let spare = m.duplicate_wall_type(t, "Spare").unwrap();
    let free = m.archivable_wall_types().unwrap();
    assert_eq!(free, [(spare, "Spare".to_string())]);
    assert!(m.archive_wall_types(&[t]).is_err(), "a used type is never archived");
    let names = m.archive_wall_types(&[spare]).unwrap();
    assert_eq!(names, ["Spare"]);
    assert_eq!(m.wall_type_usage().unwrap().len(), 1);
    let arch = m.archived_wall_types().unwrap();
    assert_eq!((arch.len(), arch[0].name.as_str(), arch[0].src_id), (1, "Spare", spare));
    // undo brings the type back and empties the archive in one step
    m.undo().unwrap();
    assert_eq!(m.wall_type_layers_full(spare).unwrap(), ext());
    assert!(m.archived_wall_types().unwrap().is_empty());
    m.archive_wall_types(&[spare]).unwrap();
    let id = m.restore_archived_wall_type(m.archived_wall_types().unwrap()[0].id).unwrap();
    assert_eq!(id, spare);
    assert_eq!(m.wall_type_layers_full(id).unwrap(), ext());
    assert!(m.archived_wall_types().unwrap().is_empty());
}

/// A source project file with "Ext 380" (other build-up), "Int 100" and a material row.
fn source_file(tag: &str) -> std::path::PathBuf {
    let p = tmp(tag);
    let mut s = Model::open(p.to_str().unwrap()).unwrap();
    let thin = vec![
        l("render", 0.02, LayerFn::Finish1),
        l("aerated block", 0.3, LayerFn::Structure),
    ];
    let a = s.add_wall_type("Ext 380", &specs(&thin)).unwrap();
    s.edit_wall_type(a, "Ext 380", &thin, Align::Core).unwrap();
    let int = vec![
        l("gypsum", 0.0125, LayerFn::Finish1),
        l("stud", 0.075, LayerFn::Structure),
        l("gypsum", 0.0125, LayerFn::Finish1),
    ];
    let b = s.add_wall_type("Int 100", &specs(&int)).unwrap();
    s.edit_wall_type(b, "Int 100", &int, Align::Core).unwrap();
    let mut spec = crate::material_defaults("aerated block");
    spec.color = Some(0x00aa_bbcc);
    s.add_material(&spec).unwrap();
    drop(s);
    p
}

#[test]
fn import_conflicts_rename_skip_overwrite() {
    let src = source_file("imp");
    let pack = read_type_pack(&src).unwrap();
    assert_eq!(pack.types.len(), 2);
    assert!(pack.materials.iter().any(|x| x.name == "aerated block"));

    let (mut m, t, a, _) = model();
    let plan = m.plan_type_import(&pack).unwrap();
    let fates: Vec<(&str, ImportFate, usize)> =
        plan.iter().map(|r| (r.name.as_str(), r.fate, r.walls)).collect();
    assert_eq!(fates, [("Ext 380", ImportFate::Conflict, 2), ("Int 100", ImportFate::New, 0)]);

    // skip: the project type stays, the new one comes with nothing else
    let r = m.import_wall_types(&pack, &[], Conflict::Skip, Align::Core).unwrap();
    assert_eq!(r.added, ["Int 100"]);
    assert_eq!(r.skipped, ["Ext 380"]);
    assert_eq!(m.wall_type_layers_full(t).unwrap(), ext());
    m.undo().unwrap();
    assert_eq!(m.wall_type_usage().unwrap().len(), 1, "one undo removes the whole import");

    // rename: a second type under a free name, the material row comes along
    let r = m.import_wall_types(&pack, &[], Conflict::Rename, Align::Core).unwrap();
    assert_eq!(r.renamed, [("Ext 380".to_string(), "Ext 380 2".to_string())]);
    assert_eq!(r.materials, ["aerated block"]);
    assert_eq!(m.material("aerated block").unwrap().spec.color, Some(0x00aa_bbcc));
    assert_eq!(m.wall_type_layers_full(t).unwrap(), ext());
    m.undo().unwrap();
    assert_eq!(m.material("aerated block").unwrap().id, 0, "the material row went with the undo");

    // overwrite: the project type takes the source layers, walls follow the exterior face
    let off = m.wall_offset0(a).unwrap();
    let pick = vec!["Ext 380".to_string()];
    let r = m.import_wall_types(&pack, &pick, Conflict::Overwrite, Align::Exterior).unwrap();
    assert_eq!((r.overwritten.len(), r.added.len(), r.walls), (1, 0, 2));
    assert_eq!(m.wall_type_layers_full(t).unwrap(), pack.types[0].layers);
    assert!((m.wall_offset0(a).unwrap() - off).abs() < 1e-12);

    // the same again: nothing to write, no journal entry
    let r = m.import_wall_types(&pack, &pick, Conflict::Overwrite, Align::Exterior).unwrap();
    assert_eq!((r.written(), r.same.len()), (0, 1));
    m.undo().unwrap();
    assert_eq!(m.wall_type_layers_full(t).unwrap(), ext());
    let _ = std::fs::remove_file(&src);
}

#[test]
fn reading_a_pack_leaves_the_source_file_unchanged() {
    let src = source_file("ro");
    let before = std::fs::read(&src).unwrap();
    let _ = read_type_pack(&src).unwrap();
    assert_eq!(std::fs::read(&src).unwrap(), before);
    assert!(read_type_pack(&tmp("missing")).is_err());
    let _ = std::fs::remove_file(&src);
}

#[test]
fn apply_step_replays_an_edit_and_an_import() {
    let src = source_file("step");
    let pack = read_type_pack(&src).unwrap();
    let (mut m, t, _, _) = model();
    let mut v = ext();
    v.remove(3);
    let edit = TypeLibStep::Edit {
        id: t,
        name: "Ext".into(),
        layers: v.clone(),
        align: Align::Core,
    };
    m.apply_type_lib(&edit).unwrap();
    assert_eq!(m.wall_type_layers_full(t).unwrap(), v);
    let step = TypeLibStep::Import { pack, pick: vec![], rule: Conflict::Skip, align: Align::Core };
    m.apply_type_lib(&step).unwrap();
    assert!(m.apply_type_lib(&step).is_err(), "a second run has nothing to write");
    let _ = std::fs::remove_file(&src);
}

#[test]
fn edits_survive_save_and_reopen() {
    let p = tmp("reopen");
    let path = p.to_str().unwrap().to_string();
    {
        let (mut m, t, _, _) = model_at(&path);
        let mut v = ext();
        v[3].priority = 7;
        v[2].function = LayerFn::Air;
        m.edit_wall_type(t, "Ext 380", &v, Align::Exterior).unwrap();
        let spare = m.duplicate_wall_type(t, "Spare").unwrap();
        m.archive_wall_types(&[spare]).unwrap();
    }
    let m = Model::open(&path).unwrap();
    let u = m.wall_type_usage().unwrap();
    assert_eq!(u.len(), 1);
    let got = m.wall_type_layers_full(u[0].id).unwrap();
    assert_eq!((got[2].function, got[3].priority), (LayerFn::Air, 7));
    assert_eq!(m.archived_wall_types().unwrap()[0].name, "Spare");
    drop(m);
    let _ = std::fs::remove_file(&p);
}
