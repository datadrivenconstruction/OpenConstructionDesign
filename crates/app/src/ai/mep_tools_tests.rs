// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::{kind_of, schedule_table};
use ddc_model::{CircuitIssue, CircuitKind, CircuitReport, PanelSchedule, SystemKind};
use ddc_ui::{Catalog, Lang};

fn sched() -> PanelSchedule {
    let row = |number: u32, kind, load: f64, issues| CircuitReport {
        id: number as i64,
        number,
        name: format!("P1.{number}"),
        kind,
        poles: 1,
        breaker: 16.0,
        cable: 2.5,
        devices: 4,
        load,
        current: load / 230.0,
        length: 21.5,
        drop: 1.25,
        issues,
        rules: vec![],
        cos_phi: 0.9,
        load_type: "office".into(),
        conductor: ddc_model::Conductor::Cu,
        x0_estimated: number == 1,
        rcd: String::new(),
        cable_type: String::new(),
        laying: "conduit_wall".into(),
        ambient_c: 30.0,
        grouped: 1,
    };
    let over = vec![
        CircuitIssue::Overload { current: 18.2, breaker: 16.0 },
        CircuitIssue::Drop { pct: 4.6, max: 4.0, comp: 9 },
    ];
    PanelSchedule {
        panel: 3,
        rows: vec![
            row(1, CircuitKind::Power, 800.0, vec![]),
            row(2, CircuitKind::Lighting, 4200.0, over),
        ],
        load: 5000.0,
        current: 5000.0 / 230.0,
        phases: [5000.0 / 230.0, 0.0, 0.0],
    }
}

#[test]
fn schedule_table_in_both_languages_has_no_raw_keys() {
    let s = sched();
    for lang in [Lang::En, Lang::Ru] {
        let (head, rows) = schedule_table(&Catalog::new(lang), &s);
        assert_eq!(head.len(), 15);
        assert_eq!(rows.len(), 3, "two circuits and the total");
        assert!(rows.iter().all(|r| r.len() == head.len()));
        let all: Vec<&String> = head.iter().chain(rows.iter().flatten()).collect();
        assert!(all.iter().all(|c| !c.contains("mep.") && !c.contains('{')), "{all:?}");
        assert_eq!(rows[2][3], "8");
        assert_eq!(rows[2][4], "5000");
        assert!(rows[1][14].contains("18.2") && rows[1][14].contains("16.0"));
        assert_eq!(rows[1][10], "0.9");
        assert_eq!(rows[1][13], "1");
        assert!(rows[1][14].contains("comp:9"));
    }
    let (en, _) = schedule_table(&Catalog::new(Lang::En), &s);
    let (ru, rows) = schedule_table(&Catalog::new(Lang::Ru), &s);
    assert_eq!(en[7], "Cable, mm\u{b2}");
    assert_eq!(ru[2], "\u{422}\u{438}\u{43f}");
    assert_eq!(rows[2][0], "\u{418}\u{442}\u{43e}\u{433}\u{43e}");
    assert_ne!(rows[0][2], rows[1][2], "power and lighting read differently");
}

#[test]
fn system_kinds_parse_and_unknown_is_refused() {
    assert_eq!(kind_of("waste").unwrap(), SystemKind::Sanitary);
    assert_eq!(kind_of("power").unwrap(), SystemKind::Power);
    assert!(kind_of("steam").is_err());
}
