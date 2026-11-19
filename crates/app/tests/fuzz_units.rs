// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Mutation fuzz of the typed-length parser. Ignored: run through `tools/fuzz.ps1`.

#[path = "../../io/tests/common/mutate.rs"]
mod mutate;

use ddc_app::expr_field::{eval_angle, Binding, Scope};
use ddc_app::units::{parse_length, parse_length_in, parse_typed, parse_typed_in, DecimalMark, Units};
use mutate::{mutate, run_target, Counting};

#[global_allocator]
static ALLOC: Counting = Counting;

fn look(n: &str) -> Option<Binding> {
    match n {
        "w" => Some(Binding::Length(2.0)),
        "a" => Some(Binding::Formula("a*2".into())),
        _ => None,
    }
}

#[test]
#[ignore]
fn fuzz_units_parser() {
    let seeds: Vec<Vec<u8>> = [
        "3500",
        "3500<90",
        "<45",
        "@1000,500",
        "@1000;500",
        "6000;4 000",
        "3,5<22,5",
        "3000+150",
        "12'6\"-1\"",
        "1.5e3",
        "  -7 ",
        "2*w+150",
        "(3000-200)/4",
        "((w))*-(2)",
        "a+1",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();

    run_target("units", &seeds, mutate, |b| {
        let s = String::from_utf8_lossy(b);
        let sc = Scope::new(&look);
        let _ = eval_angle(&s, &sc);
        for u in [Units::Metric, Units::Imperial] {
            let _ = parse_length(&s, u);
            let _ = parse_length_in(&s, u, &sc);
            for m in [DecimalMark::Point, DecimalMark::Comma] {
                let _ = parse_typed(&s, u, m);
                let _ = parse_typed_in(&s, u, &sc, m);
            }
        }
    });
}
