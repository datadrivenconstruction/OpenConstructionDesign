// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::checks::norm_pack::PackSet;
use crate::finishes::FinishSource;
use crate::views::tests::room_6x4;

fn pack(id: &str) -> NormPack {
    PackSet::embedded().resolve(id).unwrap()
}

#[test]
fn class_follows_purpose_then_kind_then_name_words() {
    let p = pack("generic");
    assert_eq!(class_of(&p, None, "Office"), "work");
    assert_eq!(class_of(&p, None, "Kitchen 2"), "service");
    assert_eq!(class_of(&p, None, "Corridor"), "circulation");
    assert_eq!(class_of(&p, None, "Stair hall"), "circulation");
    assert_eq!(class_of(&p, None, "\u{41b}\u{43e}\u{434}\u{436}\u{438}\u{44f}"), "loggia");
    assert_eq!(class_of(&p, None, "Living room balcony"), "balcony");
    assert_eq!(class_of(&p, Some("kitchen"), "Room 7"), "service");
    assert_eq!(class_of(&p, None, "Zzz"), "other");
}

#[test]
fn spaces_outside_the_walls_count_by_the_pack_factor_only() {
    let (ru, generic) = (pack("ru"), pack("generic"));
    assert_eq!(weight(&ru, AreaMethod::Net, "loggia"), 0.5);
    assert_eq!(weight(&ru, AreaMethod::Net, "balcony"), 0.3);
    assert_eq!(weight(&generic, AreaMethod::Net, "loggia"), 0.0);
    assert_eq!(weight(&generic, AreaMethod::Net, "living"), 1.0);
    assert_eq!(weight(&generic, AreaMethod::Usable, "circulation"), 0.0);
    assert_eq!(weight(&generic, AreaMethod::Usable, "work"), 1.0);
}

#[test]
fn one_box_measures_gross_gross_internal_net_and_usable() {
    // 6 x 4 outer size, 0.2 walls on the outline: inside 5.6 x 3.6
    let (m, _) = room_6x4(&[("Brick", 0.2)]);
    let (net, gia, gross) = (5.6 * 3.6, 5.6 * 3.6, 24.0);
    for (id, method, want) in [
        ("generic", AreaMethod::Net, net),
        ("ru", AreaMethod::Net, net),
        ("de", AreaMethod::Usable, net),
        ("uk", AreaMethod::GrossInternal, gia),
        ("us", AreaMethod::Gross, gross),
    ] {
        let r = m.area_report(&pack(id)).unwrap();
        assert_eq!(r.method, method, "{id}");
        assert!((r.total - want).abs() < 1e-6, "{id}: {} vs {want}", r.total);
        assert!(!r.fallback, "{id}");
        let s = &r.storeys[0];
        assert!((s.net - net).abs() < 1e-6 && (s.usable - net).abs() < 1e-6, "{id}");
        assert!((s.gross.unwrap() - gross).abs() < 1e-6, "{id}: gross {:?}", s.gross);
        assert!((s.gross_internal.unwrap() - gia).abs() < 1e-6, "{id}: gia {:?}", s.gross_internal);
        assert_eq!(s.uses.len(), 1);
        assert_eq!(s.uses[0].class, "work");
    }
}

#[test]
fn finishes_follow_parameters_then_pack_then_room_rules() {
    let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
    let p = pack("generic");
    let f = m.room_finishes(&p).unwrap();
    assert_eq!(f.len(), 1);
    assert_eq!(
        (f[0].floor.as_str(), f[0].walls.as_str(), f[0].ceiling.as_str()),
        ("carpet_tile", "paint", "suspended_tile")
    );
    assert_eq!(f[0].source, FinishSource::Rule);
    assert!((f[0].floor_area - 5.6 * 3.6).abs() < 1e-6);
    assert!((f[0].wall_area - 2.0 * (5.6 + 3.6) * 3.0).abs() < 1e-6, "{}", f[0].wall_area);
    // a floor parameter on the room wins for the floor only
    assert_eq!(m.ensure_finish_params().unwrap(), 3);
    let id = m.param_by_name("FinishFloor").unwrap().unwrap().id;
    let room: i64 = m.db.query_row("SELECT id FROM room", [], |r| r.get(0)).unwrap();
    m.set_param_values(&[crate::ParamSet {
        param: id,
        kind: "room".into(),
        id: room,
        value: Some(PVal::Text("stone".into())),
    }])
    .unwrap();
    let f = m.room_finishes(&p).unwrap();
    assert_eq!(f[0].floor, "stone");
    assert_eq!(f[0].walls, "paint");
    assert_eq!(f[0].source, FinishSource::Param);
}

#[test]
fn plan_room_tags_carry_the_floor_finish() {
    let (m, lv) = room_6x4(&[("Brick", 0.2)]);
    let v = m
        .plan_view(&crate::PlanViewSpec { level: lv, cut_height: 1.2, grids: vec![] })
        .unwrap();
    let f: Vec<_> = v
        .prims
        .iter()
        .filter_map(|p| match p {
            crate::ViewPrim::RoomTag { finish, .. } => Some(finish.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(f, [Some("carpet_tile".to_string())]);
}
