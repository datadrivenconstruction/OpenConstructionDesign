//! The editor's side of typed refusals (`ddc_ai::refusal`): what an `AiError` refuses as, and the
//! text the person reads where the model read the refusal's JSON.

use super::AiError;
use ddc_ai::refusal::{self, Refusal, RefusalCode};
use ddc_ai::registry::{self, CmdError};
use ddc_ai::ToolOutcome;
use serde_json::Value as J;

impl From<AiError> for Refusal {
    fn from(e: AiError) -> Refusal {
        let code = match &e {
            AiError::EmptyPlan => RefusalCode::InvalidArgs,
            AiError::Stale => RefusalCode::PlanStale,
            AiError::Blocked | AiError::Diverged(_) => RefusalCode::PlanConflict,
            AiError::Busy => RefusalCode::Busy,
            AiError::ReviewNeeded | AiError::NeedsConfirmation => RefusalCode::NeedsConfirmation,
            // a model error's wording names what failed: a missing element is not an internal fault
            AiError::Model(m) => return CmdError::from_text(m).refusal(),
            AiError::AppliedNotRefreshed(_) | AiError::Provider(_) | AiError::Internal(_) => {
                RefusalCode::Internal
            }
        };
        refusal::of(code).detail(e.to_string())
    }
}

/// The coded outcome of an `AiError` answered to a tool call.
pub fn outcome(e: AiError) -> ToolOutcome {
    CmdError::refused(Refusal::from(e)).outcome()
}

/// The untranslated refusal code of a tool failure, for evals and logs.
pub fn code_of(detail: &str) -> Option<&'static str> {
    let v = serde_json::from_str::<J>(detail).ok()?;
    refusal::from_json(&v, &registry::known_command).map(|r| r.code.as_str())
}

/// A tool failure as the person reads it: a typed refusal in the UI language (what happened,
/// then what will work); any other text unchanged.
pub fn human(detail: &str) -> String {
    let Ok(v) = serde_json::from_str::<J>(detail) else {
        return detail.to_string();
    };
    let Some(r) = refusal::from_json(&v, &registry::known_command) else {
        return detail.to_string();
    };
    let args: Vec<(&str, &str)> = r.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let what = crate::tr(&refusal::what_key(r.msg_key), &args);
    let try_ = crate::tr(r.msg_key, &args);
    format!("{what} {try_}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ai::refusal::AI_CODES;

    #[test]
    fn every_ai_error_refuses_with_a_code() {
        let cases = [
            (AiError::EmptyPlan, RefusalCode::InvalidArgs),
            (AiError::Stale, RefusalCode::PlanStale),
            (AiError::Blocked, RefusalCode::PlanConflict),
            (AiError::Busy, RefusalCode::Busy),
            (AiError::ReviewNeeded, RefusalCode::NeedsConfirmation),
            (AiError::AppliedNotRefreshed("x".into()), RefusalCode::Internal),
            (AiError::NeedsConfirmation, RefusalCode::NeedsConfirmation),
            (AiError::Diverged("wall:3 moved".into()), RefusalCode::PlanConflict),
            (AiError::Model("no element wall:9".into()), RefusalCode::UnknownTarget),
            (AiError::Model("disk full".into()), RefusalCode::Internal),
            (AiError::Provider("x".into()), RefusalCode::Internal),
            (AiError::Internal("x".into()), RefusalCode::Internal),
        ];
        for (e, want) in cases {
            let text = e.to_string();
            let out = outcome(e);
            let v: J = serde_json::from_str(&out.text).unwrap();
            assert_eq!(v["refused"]["code"], want.as_str(), "{text}");
            assert!(v["refused"]["detail"].as_str().is_some_and(|d| d.contains(&text)), "{v}");
        }
    }

    #[test]
    fn the_person_reads_the_refusal_translated() {
        let r = refusal::of(RefusalCode::SnapshotsOff).fix(refusal::open_settings());
        let detail = refusal::to_wire(&CmdError::refused(r).refusal(), "en").to_string();
        let ru = crate::with_test_lang(true, || human(&detail));
        let en = crate::with_test_lang(false, || human(&detail));
        assert_eq!(ru, format!("{} {}", crate::tr_lang(true, "refusal.snapshots_off.what", &[]),
            crate::tr_lang(true, "refusal.snapshots_off", &[])));
        assert!(ru.chars().any(|c| ('\u{430}'..='\u{44f}').contains(&c)), "{ru}");
        assert!(en.contains("AI settings"), "{en}");
        assert!(!ru.contains('{') && !ru.contains("snapshots_off"), "no JSON for the person: {ru}");
        assert_eq!(human("plain failure"), "plain failure");
        assert_eq!((code_of(&detail), code_of("plain failure")), (Some("snapshots_off"), None));
        // every code has its texts in the app catalog too
        for c in AI_CODES {
            for ru in [false, true] {
                for k in [refusal::msg_key(c).to_string(), refusal::what_key(refusal::msg_key(c))] {
                    assert_ne!(crate::tr_lang(ru, &k, &[]), k, "{k} missing in the catalog");
                }
            }
        }
    }
}
