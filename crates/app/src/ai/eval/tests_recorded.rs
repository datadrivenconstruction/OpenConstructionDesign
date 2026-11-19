// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Scoring a recorded result: the file reader and the verdict on a saved model.

use super::case::Case;
use super::metrics::Status;
use super::recorded::{parse_result, run_recorded, Run};

const RESULT: &str = r#"{"id": "x", "run": {"model_text": "Sprinklers on every floor.", "tool_calls": ["get_context", "help", "generate_project"],
    "tool_errors": ["help", {"name": "generate_project"}], "input_tokens": 1200, "output_tokens": 80, "cost_usd": 0.25}}"#;

#[test]
fn result_json_gives_answer_tools_refusals_and_cost() {
    assert_eq!(
        parse_result(RESULT).unwrap(),
        Run {
            answer: "Sprinklers on every floor.".into(),
            tools: 3,
            refused: vec!["help".into(), "generate_project".into()],
            tokens_in: 1200,
            tokens_out: 80,
            usd: 0.25
        }
    );
    assert!(parse_result("{}").is_err());
    assert!(parse_result("not json").is_err());
}

#[test]
fn a_missing_recorded_result_is_not_run() {
    let c = Case::parse("id=\"nothing_here\"\n[[turn]]\nsay=\"x\"\n").unwrap();
    let m = run_recorded(&c, &std::env::temp_dir().join("ddc_no_such_root"));
    assert_eq!(m.status, Status::NotRun);
    assert!(m.note.contains("no recorded result"), "{}", m.note);
}

/// A saved empty project is the recorded failure of "the model built nothing": every count
/// oracle fails, the answer is scored as the run said it.
#[test]
fn a_recorded_empty_model_fails_the_counts_and_keeps_the_answer() {
    let root = std::env::temp_dir().join(format!("ddc_rec_{}", std::process::id()));
    let dir = root.join("rec_empty");
    std::fs::create_dir_all(&dir).unwrap();
    let mut ed = super::tests::editor();
    ed.save_project_as(&dir.join("model.ddc")).unwrap();
    std::fs::write(dir.join("result.json"), RESULT).unwrap();
    let c = Case::parse(
        "id=\"rec_empty\"\n[[turn]]\nsay=\"x\"\n[[oracle.count]]\nspec=\"wall\"\nmin=1.0\n[[oracle.text]]\nname=\"says sprinklers\"\nre=\"[Ss]prinkler\"\n",
    )
    .unwrap();
    let m = run_recorded(&c, &root);
    assert_eq!(m.status, Status::Fail, "{m:?}");
    assert_eq!((m.steps, m.tokens_in, m.refusals.get("help").copied()), (3, 1200, Some(1)));
    let by = |n: &str| m.oracles.iter().find(|o| o.name == n).unwrap_or_else(|| panic!("{n}")).pass;
    assert!(!by("count wall"));
    assert!(by("text says sprinklers"));
}

#[test]
fn scores_table_lists_every_oracle_of_every_case() {
    use super::metrics::{scores_json, CaseMetrics};
    use super::oracle::OracleResult;
    let o = |n: &str, pass: bool| OracleResult { name: n.into(), expected: ">= 1".into(), measured: "0".into(), pass };
    let mut a = CaseMetrics::new("A");
    a.status = Status::Fail;
    a.oracles = vec![o("count wall", false), o("text x", true)];
    let t = scores_json(&[a, CaseMetrics::not_run("B", "no recorded result")]);
    assert_eq!(t["A"]["passed"], 1);
    assert_eq!(t["A"]["total"], 2);
    assert_eq!(t["A"]["status"], "fail");
    assert_eq!(t["A"]["oracles"][0]["name"], "count wall");
    assert_eq!(t["A"]["oracles"][0]["pass"], false);
    assert_eq!(t["B"]["status"], "not_run");
    assert_eq!(t["B"]["total"], 0);
}
