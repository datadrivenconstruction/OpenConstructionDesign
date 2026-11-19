//! The generation-rules tools: a compact group on the wire, reads that change nothing, writes
//! that are plan edits, and argument checks before any handler runs.

use ddc_ai::catalog::{self, Access};
use ddc_ai::toolset;
use serde_json::json;

const READS: [&str; 5] = ["list_rules", "get_rule", "explain", "conflicts", "compare_variants"];
const WRITES: [&str; 4] = ["set_rule", "lock", "unlock", "regenerate"];
/// What the whole group may cost a request.
const MAX_TOKENS: u64 = 1500;

fn spec(n: &str) -> &'static catalog::ToolSpec {
    catalog::find(n).unwrap_or_else(|| panic!("{n} is not in the catalog"))
}

#[test]
fn the_gen_group_is_complete_and_within_its_token_budget() {
    let g = toolset::groups().iter().find(|g| g.name == "gen").expect("a gen group");
    let mut want: Vec<&str> = READS.iter().chain(WRITES.iter()).copied().collect();
    let mut have: Vec<&str> = g.tools.iter().map(String::as_str).collect();
    want.sort_unstable();
    have.sort_unstable();
    assert_eq!(have, want);
    let wire: Vec<_> = g.tools.iter().map(|n| toolset::wire(spec(n))).collect();
    let t = toolset::tokens_est(&wire);
    eprintln!("gen group: {} tools ~{t} tokens", wire.len());
    assert!(t <= MAX_TOKENS, "gen group {t} tokens > {MAX_TOKENS}");
    assert!(toolset::intent_groups("Lock the grid rule and explain the core").contains("gen"));
    // Russian: "lock the storey and explain"
    let ru = "\u{437}\u{430}\u{43a}\u{440}\u{435}\u{43f}\u{438} \u{44d}\u{442}\u{430}\u{436} \u{438} \u{43e}\u{431}\u{44a}\u{44f}\u{441}\u{43d}\u{438}";
    assert!(toolset::intent_groups(ru).contains("gen"));
}

#[test]
fn reads_change_nothing_and_writes_are_plan_edits() {
    for n in READS {
        let s = spec(n);
        assert_eq!(s.access, Access::Read, "{n}");
        assert!(s.read_only() && !s.destructive(), "{n}");
    }
    for n in WRITES {
        let s = spec(n);
        assert_eq!(s.access, Access::Propose, "{n}");
        assert!(s.destructive(), "{n}: a revision may delete what the rebuilt project drops");
    }
}

#[test]
fn calls_are_checked_before_a_handler_runs() {
    let ok = |n: &str, v: serde_json::Value| assert_eq!(catalog::validate(spec(n), &v), Ok(()), "{n} {v}");
    let bad = |n: &str, v: serde_json::Value| assert!(catalog::validate(spec(n), &v).is_err(), "{n} {v}");
    ok("list_rules", json!({}));
    ok("list_rules", json!({"group": "goal", "changed": true}));
    ok("get_rule", json!({"id": "grid.span"}));
    ok("set_rule", json!({"rules": {"grid.span": 6.0, "goal.compact": {"weight": 5, "tier": "strong"}}}));
    ok("lock", json!({"targets": [{"rule": "grid.span"}, {"storey": 1, "room": "101", "keep": "program"}]}));
    ok("lock", json!({"targets": [{"storey": 0, "zone": [[0, 0], [6, 0], [6, 6]]}], "project": "P1"}));
    ok("unlock", json!({"all": true}));
    ok("explain", json!({"element": "selection"}));
    ok("conflicts", json!({"changes": {"floors": 40}}));
    ok("compare_variants", json!({"n": 4, "seed": 7, "vary": ["grid.span"]}));
    ok("regenerate", json!({"variant": 2, "seed": 7}));
    bad("get_rule", json!({}));
    bad("set_rule", json!({}));
    bad("set_rule", json!({"rules": 3}));
    bad("lock", json!({"targets": [{"storey": 99}]}));
    bad("lock", json!({"targets": [{"rule": "grid.span", "keep": "all"}]}));
    bad("compare_variants", json!({"n": 5}));
    bad("compare_variants", json!({"vary": ["corridor.width"]}));
    bad("regenerate", json!({"geometry": [[0, 0]]}));
}
