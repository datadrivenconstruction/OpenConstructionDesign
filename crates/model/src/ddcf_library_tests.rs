// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The built-in library under the `.ddcf` v2 format: every family upgrades to the same
//! semantics, the canonical print is stable.

use crate::components::library_sources;
use ddc_geom::component::ddcf::{self, Meta};
use ddc_geom::component::{Family, Lod};
use std::collections::HashSet;

#[test]
fn set_uid_errors_without_a_uid_line() {
    use crate::components::set_uid;
    let with = "ddcf 2\nfamily A\nuid aaaaaaaa\n";
    let out = set_uid(with, "bbbbbbbb").unwrap();
    assert!(out.contains("uid bbbbbbbb") && !out.contains("aaaaaaaa"), "{out}");
    assert!(set_uid("family A\n", "bbbbbbbb").is_err());
}

fn volume(f: &Family) -> Option<f64> {
    let r = f.resolve(&[], &[]).ok()?;
    f.build(&r, Lod::Medium).ok().map(|b| b.volume)
}

#[test]
fn ddcf_library_upgrades_with_same_semantics() {
    let srcs = library_sources();
    assert!(srcs.len() > 50, "{}", srcs.len());
    let mut uids = HashSet::new();
    for s in srcs {
        let v1 = Family::parse(s).unwrap();
        assert_eq!(v1.meta.format, 1, "{}", v1.name);
        let up = ddcf::upgrade(s).unwrap_or_else(|e| panic!("{}: {e}", v1.name));
        let mut v2 = ddcf::check(&up).unwrap_or_else(|e| panic!("{}: {e}\n{up}", v1.name));
        assert_eq!(v2.meta.format, 2);
        assert_eq!(v2.meta.version.map(|v| v.to_string()).as_deref(), Some("1.0.0"));
        assert!(uids.insert(v2.meta.uid.clone().unwrap()), "uid clash: {}", v1.name);
        v2.meta = Meta::default();
        assert_eq!(v2, v1, "{}", v1.name);
        assert_eq!(volume(&v2).map(f64::to_bits), volume(&v1).map(f64::to_bits), "{}", v1.name);
        assert_eq!(ddcf::upgrade(&up).unwrap(), up, "{}", v1.name);
        assert_eq!(ddcf::fmt(&up).unwrap(), up, "{}", v1.name);
    }
}

#[test]
fn ddcf_library_fmt_is_stable() {
    for s in library_sources() {
        let c = ddcf::fmt(s).unwrap();
        assert_eq!(ddcf::fmt(&c).unwrap(), c);
        assert_eq!(Family::parse(&c).unwrap(), Family::parse(s).unwrap());
        assert!(c.lines().all(|l| !l.trim().is_empty() && l == l.trim()));
    }
}

/// R1: an instance row holds its type, its placement and host links, and its own values; a
/// type row holds its family and values. No definition text or geometry lives on either.
#[test]
fn instance_row_has_only_type_and_values() {
    let m = crate::Model::open(":memory:").unwrap();
    let cols = |t: &str| -> Vec<String> {
        let mut st = m
            .db
            .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
            .unwrap();
        let v: Vec<String> = st.query_map([t], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        v
    };
    let comp = [
        "id", "level", "ctype", "x", "y", "z", "rotation", "mirror", "host_wall", "host_slab",
        "host_ceiling", "vals", "tilt",
    ];
    let have = cols("comp");
    assert!(have.iter().all(|c| comp.contains(&c.as_str())), "{have:?}");
    assert!(["ctype", "vals"].iter().all(|c| have.iter().any(|h| h == c)), "{have:?}");
    assert_eq!(cols("comp_type"), ["id", "family", "name", "vals"]);
}

/// A project keeps one family per uid: a copy saved under a new name gets a fresh uid, the
/// original keeps its own.
#[test]
fn ddcf_family_copy_gets_fresh_uid() {
    let mut m = crate::Model::open(":memory:").unwrap();
    let a = "ddcf 2\nfamily A\nversion 1.0.0\nuid 0123456789abcdef\ncategory furniture\n\
             param W length type 1m\nextrude Box rect(0, 0, W, W) z(0, 1m)\n";
    let ia = m.load_family_def(a, false).unwrap().family;
    let ib = m.load_family_def(&a.replace("family A", "family B"), false).unwrap().family;
    assert_ne!(ia, ib);
    let uid = |id: i64| Family::parse(&m.family_def(id).unwrap()).unwrap().meta.uid.unwrap();
    let (ua, ub) = (uid(ia), uid(ib));
    assert_eq!(ua, "0123456789abcdef");
    assert_ne!(ub, ua);
    assert!(ddcf::uid_ok(&ub), "{ub}");
}
