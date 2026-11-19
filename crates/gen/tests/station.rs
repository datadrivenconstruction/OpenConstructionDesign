//! The underground station: the defaults give the deep box of a cut-and-cover station that
//! the hard metro scenario asks for, every part follows its rule, the parts never clash, a rule
//! that cannot be met is refused with the rules to change, and the builder holds no number
//! outside the rules.

use ddc_gen::project::rules::{self, Layer, RuleSet};
use ddc_gen::station::{self, bounds, GStation, Lvl, ShaftKind, SlabKind, WallKind};
use serde_json::json;

fn built(set: &[(&str, f64)]) -> GStation {
    let mut rs = RuleSet::new();
    for (id, v) in set {
        rs.set(Layer::Brief, id, *v).unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    station::plan(&rs).unwrap_or_else(|e| panic!("{set:?}: {e}"))
}

fn refused(set: &[(&str, f64)]) -> String {
    let mut rs = RuleSet::new();
    for (id, v) in set {
        rs.set(Layer::Brief, id, *v).unwrap();
    }
    station::plan(&rs).expect_err("refused").to_string()
}

fn count(s: &GStation, k: ShaftKind) -> usize {
    s.shafts.iter().filter(|x| x.kind == k).count()
}

#[test]
fn the_defaults_give_a_deep_long_station_with_its_parts() {
    let s = built(&[]);
    // what the hard metro scenario measures
    assert!(s.metric("lowest_z").unwrap() <= -12.0);
    assert!(s.inner.0 >= 140.0, "{:?}", s.inner);
    assert_eq!(s.metric("escalators"), Some(6.0));
    assert!(count(&s, ShaftKind::Vent) >= 2);
    assert_eq!(count(&s, ShaftKind::Lift), 2);
    assert_eq!(s.slabs.len(), 4);
    assert!(s.rooms.iter().any(|r| r.class == "platform" && r.level == Lvl::Platform));
    assert!(s.rooms.iter().any(|r| r.class == "concourse" && r.level == Lvl::Concourse));
    assert_eq!(s.levels.len(), 3);
    assert!(s.levels.iter().all(|l| l.1 < 0.0), "{:?}", s.levels);
    assert!(s.issues.is_empty(), "{:?}", s.issues);
    // the box: 150 m platform and two 20 m end zones; 12 m platform and two 4.8 m trackways
    assert!((s.inner.0 - 190.0).abs() < 1e-9 && (s.inner.1 - 21.6).abs() < 1e-9, "{:?}", s.inner);
    // excavation to the underside of the base slab: 18 + 0.6 + 1.2
    assert!((s.metric("excavation_depth").unwrap() - 19.8).abs() < 1e-9);
    // 19.8 m x 0.04 = 0.79 m, rounded up to the 0.1 m step: never below the least wall
    assert!((s.dwall_thickness - 0.8).abs() < 1e-9, "{}", s.dwall_thickness);
    assert_eq!(s.walls.iter().filter(|w| w.kind == WallKind::Diaphragm).count(), 12);
    assert_eq!(s.stairs.len(), 2);
    assert!(!s.columns.is_empty());
}

#[test]
fn every_level_band_of_the_wall_meets_the_next() {
    let s = built(&[]);
    let top_of = |l: Lvl| {
        let w = s.walls.iter().find(|w| w.level == l && w.kind == WallKind::Diaphragm).unwrap();
        (s.elevation(l) + w.base_offset, s.elevation(l) + w.base_offset + w.height)
    };
    let (t0, t1) = top_of(Lvl::Track);
    let (p0, p1) = top_of(Lvl::Platform);
    let (c0, c1) = top_of(Lvl::Concourse);
    assert!((t0 - s.metric("lowest_z").unwrap()).abs() < 1e-9);
    assert!((t1 - p0).abs() < 1e-9 && (p1 - c0).abs() < 1e-9);
    assert!(c1.abs() < 1e-9, "the wall reaches the street");
    let roof = s.slabs.iter().find(|x| x.kind == SlabKind::Roof).unwrap();
    assert!((s.elevation(Lvl::Concourse) + roof.offset - -rules::def("station.cover").unwrap().default.unwrap()).abs() < 1e-9);
}

#[test]
fn shafts_stay_in_the_box_apart_and_columns_keep_out_of_them() {
    let s = built(&[]);
    let (l, w) = s.inner;
    let rects: Vec<_> = s.shafts.iter().map(|x| (x, bounds(&x.ring))).collect();
    for (sh, r) in &rects {
        assert!(r.0 >= -1e-9 && r.1 >= -1e-9 && r.2 <= l + 1e-9 && r.3 <= w + 1e-9, "{:?} {r:?}", sh.kind);
    }
    // floors by depth: ground 0, concourse 1, platform 2, track 3. A shaft is a hole in every
    // floor from its top down to its base; a column stands on the floors below its top.
    let floor = |l: Lvl| match l {
        Lvl::Ground => 0,
        Lvl::Concourse => 1,
        Lvl::Platform => 2,
        Lvl::Track => 3,
    };
    let holes = |x: &station::SShaft| floor(x.top)..=floor(x.level);
    let meet = |a: std::ops::RangeInclusive<i32>, b: std::ops::RangeInclusive<i32>| a.start().max(b.start()) <= a.end().min(b.end());
    for i in 0..rects.len() {
        for j in i + 1..rects.len() {
            let ((a, ra), (b, rb)) = (&rects[i], &rects[j]);
            let apart = ra.2 <= rb.0 || rb.2 <= ra.0 || ra.3 <= rb.1 || rb.3 <= ra.1;
            assert!(apart || !meet(holes(a), holes(b)), "{:?} {ra:?} and {:?} {rb:?} overlap", a.kind, b.kind);
        }
    }
    for c in &s.columns {
        let stands = floor(c.top.unwrap_or(Lvl::Ground)) + 1..=floor(c.level);
        for (sh, r) in &rects {
            if !meet(stands.clone(), holes(sh)) {
                continue;
            }
            let h = c.size * 0.5;
            let inside = c.at.0 + h > r.0 && c.at.0 - h < r.2 && c.at.1 + h > r.1 && c.at.1 - h < r.3;
            assert!(!inside, "column at {:?} on floors {stands:?} stands in a {:?} {r:?}", c.at, sh.kind);
        }
    }
    // each escalator well holds its bank side by side, each one climbing its rise
    let rise = s.elevation(Lvl::Concourse) - s.elevation(Lvl::Platform);
    let tan = rules::def("station.escalator_angle").unwrap().default.unwrap().to_radians().tan();
    for e in s.escalators.iter().filter(|e| (e.bottom.2 - s.elevation(Lvl::Platform)).abs() < 1e-9) {
        assert!(((e.top.0 - e.bottom.0) - rise / tan).abs() < 1e-6, "{e:?}");
    }
}

#[test]
fn no_shaft_crosses_the_trackways_the_trains_run_through() {
    for set in [vec![], vec![("station.vent_flow", 300.0), ("station.end_zone", 30.0)], vec![("station.peak_flow", 30000.0)]] {
        let s = built(&set);
        let tw = rules::def("station.trackway_width").unwrap().default.unwrap();
        let w = s.inner.1;
        for sh in &s.shafts {
            // a shaft down to the platform or the track is a hole where the trains pass
            if !matches!(sh.level, Lvl::Platform | Lvl::Track) {
                continue;
            }
            let r = bounds(&sh.ring);
            assert!(r.1 >= tw - 1e-9 && r.3 <= w - tw + 1e-9, "{set:?}: {:?} {r:?} crosses a trackway", sh.kind);
        }
    }
}

#[test]
fn a_changed_rule_changes_the_station() {
    let base = built(&[]);
    let busy = built(&[("station.peak_flow", 30000.0)]);
    assert!(busy.metric("escalators").unwrap() > base.metric("escalators").unwrap());
    let deep = built(&[("station.rail_depth", 26.0)]);
    assert!(deep.metric("lowest_z").unwrap() < base.metric("lowest_z").unwrap() - 8.0);
    assert!(deep.dwall_thickness > base.dwall_thickness, "a deeper box takes a thicker wall");
    let long = built(&[("station.platform_length", 220.0)]);
    assert!((long.inner.0 - base.inner.0 - 70.0).abs() < 1e-9);
    let airy = built(&[("station.vent_flow", 170.0)]);
    assert!(airy.metric("vent_shaft_side").unwrap() > base.metric("vent_shaft_side").unwrap());
    // the reasons name the rules and the layer that set them
    let w = deep.why.iter().find(|w| w.rule == "station.rail_depth").unwrap();
    assert_eq!(w.source, "brief");
    assert!(w.effect.contains("26"), "{}", w.effect);
    assert!(base.why.iter().all(|w| w.source == "builtin" && !w.effect.contains('{')));
}

#[test]
fn rules_that_cannot_be_met_are_refused_with_the_rules_to_change() {
    let shallow = refused(&[("station.rail_depth", 9.0)]);
    assert!(shallow.contains("station.platform_clear") && shallow.contains("station.rail_depth"), "{shallow}");
    let narrow = refused(&[("station.platform_width", 6.0), ("station.peak_flow", 40000.0)]);
    assert!(narrow.contains("station.platform_width"), "{narrow}");
    let short = refused(&[("station.platform_length", 40.0)]);
    assert!(short.contains("station.platform_length"), "{short}");
    let windy = refused(&[("station.vent_flow", 600.0), ("station.vent_velocity", 3.0)]);
    assert!(windy.contains("station.vent_flow"), "{windy}");
}

#[test]
fn a_slow_evacuation_is_named_not_hidden() {
    let s = built(&[("station.train_load", 4000.0), ("station.evac_time", 2.0)]);
    assert_eq!(s.issues.len(), 1, "{:?}", s.issues);
    assert!(s.issues[0].contains("station.evac_time"));
    assert!(s.metric("evac_min").unwrap() > 2.0);
}

#[test]
fn every_station_rule_has_a_default_a_reason_and_both_languages() {
    let en: serde_json::Value = serde_json::from_str(include_str!("../../ui/data/i18n/en/gen_station.json")).unwrap();
    let ru: serde_json::Value = serde_json::from_str(include_str!("../../ui/data/i18n/ru/gen_station.json")).unwrap();
    let ids: Vec<&str> = station::RULES.iter().copied().chain([station::COLUMN_SIZE]).collect();
    let defs: Vec<_> = rules::defs().iter().filter(|d| d.group == "station").collect();
    assert_eq!(defs.len(), ids.len(), "every station rule is read, every one read exists");
    for id in ids {
        let d = rules::def(id).unwrap_or_else(|| panic!("{id}"));
        assert!(d.default.is_some() && !d.why.is_empty(), "{id}");
        for (lang, cat) in [("en", &en), ("ru", &ru)] {
            for k in [d.title_key(), d.desc_key()] {
                assert!(cat[&k].as_str().is_some_and(|s| !s.is_empty()), "{lang}: {k}");
            }
        }
    }
    for (key, text) in station::why::EN {
        let k = format!("gen.why.e.{key}");
        assert_eq!(en[&k], json!(text), "{k}");
        let slots = |s: &str| {
            let mut v: Vec<String> = s.split('{').skip(1).map(|p| p.split('}').next().unwrap().to_string()).collect();
            v.sort();
            v
        };
        assert_eq!(slots(ru[&k].as_str().unwrap()), slots(text), "{k}");
    }
    assert!(en["gen.group.station"].is_string() && ru["gen.group.station"].is_string());
}

/// Numeric literals outside comments, strings and char literals.
fn literals(src: &str) -> Vec<(usize, String)> {
    let mut out = vec![];
    for (n, line) in src.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        let mut s = String::new();
        let mut quoted = false;
        for c in code.chars() {
            if c == '"' {
                quoted = !quoted;
            } else if !quoted {
                s.push(c);
            }
        }
        let b: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < b.len() {
            let prev = if i == 0 { ' ' } else { b[i - 1] };
            if b[i].is_ascii_digit() && !(prev.is_alphanumeric() || prev == '_' || prev == '.' || prev == '\'') {
                let mut t = String::new();
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == '.' || b[i] == 'e' || b[i] == '-') {
                    if b[i] == '-' && !t.ends_with('e') {
                        break;
                    }
                    t.push(b[i]);
                    i += 1;
                }
                out.push((n + 1, t.trim_end_matches('.').to_string()));
            } else {
                i += 1;
            }
        }
    }
    out
}

#[test]
fn the_station_holds_no_number_outside_the_rules() {
    let src = include_str!("../src/station/mod.rs");
    // indices, halves, pairs and tolerances; 3: the last level index and array length; 46: the length of the rule list; 8, 8.0, 4.0: segments of the vault profile and the parabola factor; 60.0: minutes in an hour
    let ok = ["0", "1", "2", "0.0", "1.0", "2.0", "0.5", "1e-9", "3", "46", "60.0", "8", "8.0", "4.0"];
    let stray: Vec<_> = literals(src).into_iter().filter(|l| !ok.contains(&l.1.as_str())).collect();
    assert!(stray.is_empty(), "numbers without a rule: {stray:?}");
}

#[test]
fn platform_screen_doors_follow_the_rules() {
    let s = built(&[]);
    let screens = |s: &GStation| s.walls.iter().filter(|w| w.kind == WallKind::ScreenDoor).count();
    let doors = s.metric("screen_doors").unwrap();
    // 4.5 m train door pitch over the default platform, on both edges
    let pl = rules::rule("station.platform_length");
    assert_eq!(doors, 2.0 * (pl / rules::rule("station.train_door_pitch")).floor());
    assert!(screens(&s) as f64 >= doors, "{} screens for {doors} doors", screens(&s));
    // the screens stand on the platform edges, as high as the rule says, and keep clear of the doors
    let (tw, pw) = (rules::rule("station.trackway_width"), rules::rule("station.platform_width"));
    for w in s.walls.iter().filter(|w| w.kind == WallKind::ScreenDoor) {
        assert!((w.a.1 - tw).abs() < 1e-9 || (w.a.1 - tw - pw).abs() < 1e-9, "{w:?}");
        assert_eq!(w.a.1, w.b.1);
        assert!(w.b.0 > w.a.0 && (w.height - rules::rule("station.psd_height")).abs() < 1e-9);
    }
    assert!(s.why.iter().any(|w| w.rule == "station.psd"));
    // no screen runs through a shaft that is open on the platform floor, or past a column
    for w in s.walls.iter().filter(|w| w.kind == WallKind::ScreenDoor) {
        for sh in s.shafts.iter().filter(|x| matches!(x.level, Lvl::Platform) || matches!(x.top, Lvl::Platform)) {
            let r = bounds(&sh.ring);
            let across = w.a.1 > r.1 && w.a.1 < r.3 && w.b.0 > r.0 && w.a.0 < r.2;
            assert!(!across, "a screen {w:?} runs through a {:?} {r:?}", sh.kind);
        }
        for c in s.columns.iter().filter(|c| c.level == Lvl::Platform) {
            let h = c.size * 0.5;
            assert!(!(w.a.1 > c.at.1 - h && w.a.1 < c.at.1 + h && w.b.0 > c.at.0 - h && w.a.0 < c.at.0 + h), "{w:?} meets a column at {:?}", c.at);
        }
    }
    // the rule turns them off, and a coarser pitch gives fewer doors
    let off = built(&[("station.psd", 0.0)]);
    assert_eq!(screens(&off), 0);
    assert_eq!(off.metric("screen_doors"), Some(0.0));
    assert!(built(&[("station.train_door_pitch", 8.0)]).metric("screen_doors").unwrap() < doors);
}

#[test]
fn tunnel_vaults_cover_the_trackways_and_the_platform_under_the_concourse() {
    let s = built(&[]);
    let (len, wid) = s.inner;
    let tw = rules::def("station.trackway_width").and_then(|d| d.default).unwrap();
    let pw = rules::def("station.platform_width").and_then(|d| d.default).unwrap();
    let bay = rules::def("station.vault_bay").and_then(|d| d.default).unwrap();
    let nb = (len / bay - 1e-9).ceil() as usize;
    assert_eq!(s.vaults.len(), 3 * nb);
    let clear = s.metric("platform_clear").unwrap();
    for (y0, y1) in [(0.0, tw), (tw, tw + pw), (tw + pw, wid)] {
        let row: Vec<_> = s.vaults.iter().filter(|v| (v.a.1 - y0).abs() < 1e-9 && (v.b.1 - y1).abs() < 1e-9).collect();
        assert_eq!(row.len(), nb, "one segment per bay over {y0}..{y1}");
        // the segments tile the box along the tracks
        let total: f64 = row.iter().map(|v| v.depth).sum();
        assert!((total - len).abs() < 1e-6, "{total} vs {len}");
        for v in row {
            let crown = v.profile.iter().map(|p| p.1).fold(0.0, f64::max);
            assert!(crown > 0.2 && v.offset + crown <= clear + 1e-9, "crown {} under the clear height {clear}", v.offset + crown);
            assert!(v.profile.first().map(|p| p.1) == Some(0.0) && v.profile.last().map(|p| p.1) == Some(0.0), "springs at both ends");
            assert!((v.profile.last().unwrap().0 - (y1 - y0)).abs() < 1e-9, "profile spans the width");
            assert_eq!(v.level, Lvl::Platform);
        }
    }
}

#[test]
fn vault_and_ground_rules_move_the_vaults_and_the_ground() {
    let base = built(&[]);
    let high = built(&[("station.vault_rise", 0.35)]);
    let rise = |s: &GStation| s.vaults[0].profile.iter().map(|p| p.1).fold(0.0, f64::max);
    assert!(rise(&high) > rise(&base), "{} vs {}", rise(&high), rise(&base));
    let long = built(&[("station.vault_bay", 60.0)]);
    assert!(long.vaults.len() < base.vaults.len());
    let (len, wid) = base.inner;
    let reach = |s: &GStation| s.ground.iter().map(|r| bounds(r)).fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)));
    let t = base.dwall_thickness;
    assert_eq!(reach(&base), (-t - 25.0, -t - 25.0, len + t + 25.0, wid + t + 25.0));
    let far = built(&[("station.ground_margin", 50.0)]);
    assert_eq!(reach(&far).0, -t - 50.0);
    assert!(base.why.iter().any(|w| w.key == "station_vaults") && base.why.iter().any(|w| w.key == "station_ground"));
}

/// Overlap of two rectangles, m2.
fn overlap(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> f64 {
    (a.2.min(b.2) - a.0.max(b.0)).max(0.0) * (a.3.min(b.3) - a.1.max(b.1)).max(0.0)
}

#[test]
fn the_ground_never_overlaps_the_station_box() {
    for set in [&[][..], &[("station.ground_margin", 5.0)][..], &[("station.rail_depth", 24.0)][..]] {
        let s = built(set);
        let (len, wid) = s.inner;
        let t = s.dwall_thickness;
        let outer = (-t, -t, len + t, wid + t);
        let rings: Vec<_> = s.ground.iter().map(|r| bounds(r)).collect();
        assert_eq!(rings.len(), 4, "{set:?}");
        for (i, r) in rings.iter().enumerate() {
            assert!(overlap(*r, outer) < 1e-9, "strip {i} {r:?} overlaps the box {outer:?}");
            for q in &rings[i + 1..] {
                assert!(overlap(*r, *q) < 1e-9, "strips {r:?} and {q:?} overlap");
            }
        }
        // the strips and the box together are the whole ground, margin included
        let m = s.ground.iter().map(|r| bounds(r)).fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)));
        let area: f64 = rings.iter().map(|r| (r.2 - r.0) * (r.3 - r.1)).sum::<f64>() + (outer.2 - outer.0) * (outer.3 - outer.1);
        assert!((area - (m.2 - m.0) * (m.3 - m.1)).abs() < 1e-6, "{area}");
        // no wall of the box reaches into a strip
        for w in &s.walls {
            let b = (w.a.0.min(w.b.0), w.a.1.min(w.b.1), w.a.0.max(w.b.0), w.a.1.max(w.b.1));
            assert!(rings.iter().all(|r| overlap(*r, b) < 1e-9), "wall {b:?} reaches the ground strips");
        }
    }
}

/// The eval's curved count takes a vault as curved only when its profile has five points or
/// more and a point above the chord of its neighbours by over 5 cm: the default vaults do.
#[test]
fn default_vault_profiles_are_arched_enough_to_count_as_curved() {
    let s = built(&[]);
    assert!(!s.vaults.is_empty());
    for v in &s.vaults {
        let p = &v.profile;
        assert!(p.len() >= 5, "{} points", p.len());
        let arched = p.windows(3).any(|w| {
            let t = (w[1].0 - w[0].0) / (w[2].0 - w[0].0);
            w[1].1 - (w[0].1 + (w[2].1 - w[0].1) * t) > 0.05
        });
        assert!(arched, "vault over {:?} to {:?} is flat: {p:?}", v.a, v.b);
    }
}
