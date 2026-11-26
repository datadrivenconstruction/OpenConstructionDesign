// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The AI's reading of the one product refusal (`ddc_model::refusal`, shared with the tool
//! spine): which codes the AI answers, their texts, and the wire forms. No second type here.
//! Texts: `refusal.<code>` (what will work), `refusal.<code>.what` (what happened), en+ru in
//! `ui/data/i18n/{en,ru}/ai_refusal.json`.

pub use ddc_model::refusal::{Fix, Refusal, RefusalCode};
use serde_json::{json, Map, Value as J};
use std::sync::OnceLock;

/// Every code the AI tools answer, in a stable order.
pub const AI_CODES: [RefusalCode; 22] = [
    RefusalCode::InvalidArgs,
    RefusalCode::UnknownTarget,
    RefusalCode::SnapshotsOff,
    RefusalCode::SnapshotUnavailable,
    RefusalCode::ToolRetired,
    RefusalCode::NotAllowedInMode,
    RefusalCode::NeedsConfirmation,
    RefusalCode::Busy,
    RefusalCode::PlanStale,
    RefusalCode::PlanConflict,
    RefusalCode::MassDeleteBlocked,
    RefusalCode::SinceExpired,
    RefusalCode::LimitExceeded,
    RefusalCode::Cancelled,
    RefusalCode::Internal,
    RefusalCode::DraftInvalid,
    RefusalCode::TextTooLarge,
    RefusalCode::UnknownFamily,
    RefusalCode::RepairStopped,
    RefusalCode::NameTaken,
    RefusalCode::NoEffect,
    RefusalCode::UnknownTool,
];

/// Key of the "what will work" text; codes outside the AI set have the spine's own keys.
pub fn msg_key(c: RefusalCode) -> &'static str {
    match c {
        RefusalCode::InvalidArgs => "refusal.invalid_args",
        RefusalCode::UnknownTarget => "refusal.unknown_target",
        RefusalCode::SnapshotsOff => "refusal.snapshots_off",
        RefusalCode::SnapshotUnavailable => "refusal.snapshot_unavailable",
        RefusalCode::ToolRetired => "refusal.tool_retired",
        RefusalCode::NotAllowedInMode => "refusal.not_allowed_in_mode",
        RefusalCode::NeedsConfirmation => "refusal.needs_confirmation",
        RefusalCode::Busy => "refusal.busy",
        RefusalCode::PlanStale => "refusal.plan_stale",
        RefusalCode::PlanConflict => "refusal.plan_conflict",
        RefusalCode::MassDeleteBlocked => "refusal.mass_delete_blocked",
        RefusalCode::SinceExpired => "refusal.since_expired",
        RefusalCode::LimitExceeded => "refusal.limit_exceeded",
        RefusalCode::Cancelled => "refusal.cancelled",
        RefusalCode::DraftInvalid => "refusal.draft_invalid",
        RefusalCode::TextTooLarge => "refusal.text_too_large",
        RefusalCode::UnknownFamily => "refusal.unknown_family",
        RefusalCode::RepairStopped => "refusal.repair_stopped",
        RefusalCode::NameTaken => "refusal.name_taken",
        RefusalCode::NoEffect => "refusal.no_effect",
        RefusalCode::UnknownTool => "refusal.unknown_tool",
        _ => "refusal.internal",
    }
}

/// Key of the "what happened" text.
pub fn what_key(msg_key: &str) -> String {
    format!("{msg_key}.what")
}

/// A refusal of an AI code with its own text key.
pub fn of(code: RefusalCode) -> Refusal {
    Refusal::new(code, msg_key(code))
}

/// Call an AI tool with these arguments.
pub fn call_tool(command: &'static str, args: Vec<(String, String)>) -> Fix {
    Fix {
        label_key: "refusal.fix.call_tool",
        command,
        args,
    }
}

/// The person opens the AI settings.
pub fn open_settings() -> Fix {
    Fix {
        label_key: "refusal.fix.open_settings",
        command: "ai.settings",
        args: Vec::new(),
    }
}

/// Argument names a refusal may carry. Reading a refusal back from the wire keeps these only.
pub const ARG_KEYS: [&str; 14] = [
    "path", "tool", "replacement", "since", "mode", "n", "of", "cat", "name", "limit", "token",
    "max", "items", "rows",
];

/// Labels of fixes.
pub const FIX_LABELS: [&str; 4] = [
    "refusal.fix.open_settings",
    "refusal.fix.call_tool",
    "refusal.fix.run_checks_full",
    "refusal.fix.ask_person",
];

/// UI commands a fix may name besides AI tools.
pub const UI_COMMANDS: [&str; 1] = ["ai.settings"];

/// Prefix of a refusal carried in a handler's error text (handlers answer `Err(String)`).
pub const WIRE_PREFIX: &str = "refused: ";

fn intern(s: &str, known: &[&'static str]) -> Option<&'static str> {
    known.iter().copied().find(|k| *k == s)
}

fn arg_pairs(r: &Refusal) -> Vec<(&str, &str)> {
    r.args.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// What happened, in a session language (`lang`).
pub fn message(r: &Refusal, lang: &str) -> String {
    text(lang, &what_key(r.msg_key), &arg_pairs(r))
}

/// What will work, in a session language (`lang`).
pub fn try_text(r: &Refusal, lang: &str) -> String {
    text(lang, r.msg_key, &arg_pairs(r))
}

fn args_json(r: &Refusal) -> J {
    J::Object(r.args.iter().map(|(k, v)| (k.to_string(), json!(v))).collect())
}

fn fix_json(f: &Fix) -> J {
    let args: Map<String, J> = f.args.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    json!({"label_key": f.label_key, "command": f.command, "args": args})
}

/// The machine part: code, arguments, fixes and detail, nothing translated.
pub fn to_json(r: &Refusal) -> J {
    let mut o = Map::new();
    o.insert("code".into(), json!(r.code.as_str()));
    if !r.args.is_empty() {
        o.insert("args".into(), args_json(r));
    }
    if !r.fixes.is_empty() {
        o.insert("fixes".into(), J::Array(r.fixes.iter().map(fix_json).collect()));
    }
    if let Some(d) = &r.detail {
        o.insert("detail".into(), json!(d));
    }
    J::Object(o)
}

/// What the model reads: `{code, message, try, fix, detail}`; message and try in the session
/// language, the code never translated. `fixes` lists every fix when there is more than one.
pub fn to_wire(r: &Refusal, lang: &str) -> J {
    let mut o = Map::new();
    o.insert("code".into(), json!(r.code.as_str()));
    o.insert("message".into(), json!(message(r, lang)));
    o.insert("try".into(), json!(try_text(r, lang)));
    if let Some(f) = r.fixes.first() {
        let mut fj = fix_json(f);
        fj["label"] = json!(text(lang, f.label_key, &[("tool", f.command)]));
        o.insert("fix".into(), fj);
    }
    if r.fixes.len() > 1 {
        o.insert("fixes".into(), J::Array(r.fixes.iter().map(fix_json).collect()));
    }
    if !r.args.is_empty() {
        o.insert("args".into(), args_json(r));
    }
    if let Some(d) = &r.detail {
        o.insert("detail".into(), json!(d));
    }
    J::Object(o)
}

/// Reads `to_json` or `to_wire` back. Argument names, commands and labels outside the known
/// lists are dropped; `known_command` resolves tool names to their static ids.
pub fn from_json(v: &J, known_command: &dyn Fn(&str) -> Option<&'static str>) -> Option<Refusal> {
    let code = RefusalCode::parse(v["code"].as_str()?)?;
    let mut r = of(code);
    if let Some(args) = v["args"].as_object() {
        for (k, val) in args {
            if let (Some(k), Some(s)) = (intern(k, &ARG_KEYS), val.as_str()) {
                r.args.push((k, s.to_string()));
            }
        }
    }
    let fixes: Vec<&J> = match v["fixes"].as_array() {
        Some(a) => a.iter().collect(),
        None => v.get("fix").into_iter().collect(),
    };
    for f in fixes {
        let cmd = f["command"].as_str().unwrap_or_default();
        let Some(command) = intern(cmd, &UI_COMMANDS).or_else(|| known_command(cmd)) else {
            continue;
        };
        let label_key = f["label_key"]
            .as_str()
            .and_then(|l| intern(l, &FIX_LABELS))
            .unwrap_or("refusal.fix.call_tool");
        let args: Vec<(String, String)> = f["args"]
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_string)))
                    .collect()
            })
            .unwrap_or_default();
        r.fixes.push(Fix { label_key, command, args });
    }
    r.detail = v["detail"].as_str().map(str::to_string);
    Some(r)
}

/// The form a handler returns as its error text; the registry reads it back exactly.
pub fn wire(r: &Refusal) -> String {
    format!("{WIRE_PREFIX}{}", to_json(r))
}

pub fn from_wire(text: &str, known_command: &dyn Fn(&str) -> Option<&'static str>) -> Option<Refusal> {
    let body = text.strip_prefix(WIRE_PREFIX)?;
    from_json(&serde_json::from_str(body).ok()?, known_command)
}

// BUNDLES: (locale, ai_refusal.json) of every UI locale that has one, English first
include!(concat!(env!("OUT_DIR"), "/refusal_bundles.rs"));

/// Session languages of refusal texts: every UI locale with a bundle; anything else reads English.
pub fn langs() -> impl Iterator<Item = &'static str> {
    BUNDLES.iter().map(|b| b.0)
}

fn has(l: &str) -> Option<&'static str> {
    langs().find(|x| *x == l)
}

/// A language tag (`de`, `de-AT`, `pt_BR`, `RU`) as a UI locale with refusal texts: the
/// locale itself, else its base language; unknown or empty: `en`.
pub fn lang(tag: &str) -> &'static str {
    let loc = ddc_gen::project::names::locale(tag);
    has(loc).or_else(|| has(loc.split('-').next().unwrap_or_default())).unwrap_or("en")
}

/// The first language of an `Accept-Language` header that refusals speak; otherwise `en`.
pub fn lang_of_header(h: &str) -> &'static str {
    h.split(',')
        .map(|part| lang(part.split(';').next().unwrap_or_default()))
        .find(|l| *l != "en" || h.trim_start().to_ascii_lowercase().starts_with("en"))
        .unwrap_or("en")
}

fn bundle(lang_code: &str) -> Option<&'static Map<String, J>> {
    static B: OnceLock<Vec<Map<String, J>>> = OnceLock::new();
    let all = B.get_or_init(|| {
        BUNDLES.iter().map(|(_, s)| serde_json::from_str::<Map<String, J>>(s).unwrap_or_default()).collect()
    });
    BUNDLES.iter().position(|b| b.0 == lang_code).map(|at| &all[at])
}

/// A refusal text in a session language, `{name}` filled from `args`; a key missing in that
/// language falls back to English, a missing key to the key itself.
pub fn text(lang_code: &str, key: &str, args: &[(&str, &str)]) -> String {
    let l = lang(lang_code);
    let raw = [l, l.split('-').next().unwrap_or(l), "en"]
        .iter()
        .find_map(|c| bundle(c)?.get(key)?.as_str())
        .unwrap_or(key);
    let mut s = raw.to_string();
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// Every text key the AI codes and fixes use.
pub fn text_keys() -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for c in AI_CODES {
        keys.push(msg_key(c).to_string());
        keys.push(what_key(msg_key(c)));
    }
    keys.extend(FIX_LABELS.iter().map(|k| k.to_string()));
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_codes_stable() {
        let got: Vec<&str> = AI_CODES.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            got,
            [
                "invalid_args",
                "unknown_target",
                "snapshots_off",
                "snapshot_unavailable",
                "tool_retired",
                "not_allowed_in_mode",
                "needs_confirmation",
                "busy",
                "plan_stale",
                "plan_conflict",
                "mass_delete_blocked",
                "since_expired",
                "limit_exceeded",
                "cancelled",
                "internal",
                "draft_invalid",
                "text_too_large",
                "unknown_family",
                "repair_stopped",
                "name_taken",
                "no_effect",
                "unknown_tool",
            ],
            "codes are a public wire: add new ones at the end, never rename"
        );
        for c in AI_CODES {
            assert_eq!(RefusalCode::parse(c.as_str()), Some(c));
            assert!(RefusalCode::ALL.contains(&c), "{c:?} is in the product enum's list");
            assert_eq!(msg_key(c), format!("refusal.{}", c.as_str()));
        }
    }

    #[test]
    fn every_code_reads_in_every_locale() {
        assert!(langs().count() >= 40 && langs().next() == Some("en"), "{:?}", langs().collect::<Vec<_>>());
        for key in text_keys() {
            for l in langs() {
                let s = bundle(l).and_then(|b| b.get(&key)).and_then(J::as_str);
                // a regional variant may leave a text to its base language
                if l.contains('-') {
                    assert!(s.is_none_or(|s| !s.trim().is_empty()), "{key} empty in {l}");
                    continue;
                }
                assert!(s.is_some_and(|s| !s.trim().is_empty()), "{key} missing in {l}");
                if l != "en" {
                    assert_ne!(text("en", &key, &[]), text(l, &key, &[]), "{key}: {l} is not translated");
                }
            }
        }
        for k in bundle("en").unwrap().keys() {
            assert!(text_keys().contains(k), "{k}: a text no code uses");
        }
    }

    #[test]
    fn session_language_falls_back_to_en() {
        assert_eq!((lang("de-AT"), lang("zh_CN"), lang("RU"), lang("fr"), lang("")), ("de", "zh", "ru", "fr", "en"));
        assert_eq!((lang("es_MX"), lang("pt-BR"), lang("en-GB"), lang("nb"), lang("xx-YY")), ("es-MX", "pt-BR", "en", "no", "en"));
        assert_eq!(lang_of_header("ja,en;q=0.8"), "ja");
        assert_eq!(lang_of_header("fr-FR, es;q=0.7"), "fr");
        assert_eq!(lang_of_header("en-US,ru;q=0.5"), "en");
        assert_eq!(text("xx", "refusal.busy", &[]), text("en", "refusal.busy", &[]));
        // a key a regional variant leaves out reads in its base language
        let gap = text_keys().into_iter().find(|k| bundle("es-MX").unwrap().get(k).is_none()).unwrap();
        assert_eq!(text("es-MX", &gap, &[]), text("es", &gap, &[]));
        assert_ne!(text("ar", "refusal.busy", &[]), text("en", "refusal.busy", &[]));
    }

    #[test]
    fn wire_round_trip() {
        let known = |s: &str| (s == "run_checks").then_some("run_checks");
        let r = of(RefusalCode::SinceExpired)
            .arg("since", "c:12")
            .fix(call_tool("run_checks", vec![]))
            .fix(open_settings())
            .detail("window lost");
        assert_eq!(from_wire(&wire(&r), &known), Some(r.clone()));
        let w = to_wire(&r, "ru");
        assert_eq!(w["code"], "since_expired");
        assert_eq!(w["fix"]["command"], "run_checks");
        assert!(w["try"].as_str().unwrap().chars().any(|c| ('\u{0430}'..='\u{044f}').contains(&c)), "{w}");
        let back = from_json(&w, &known).unwrap();
        assert_eq!((back.code, back.fixes.len(), back.arg_of("since")), (r.code, 2, Some("c:12")));
        // an unknown command never becomes a fix
        let none = |_: &str| None;
        assert!(from_wire(&wire(&r), &none).unwrap().fixes.iter().all(|f| f.command == "ai.settings"));
    }

    #[test]
    fn args_fill_the_text() {
        let r = of(RefusalCode::ToolRetired).arg("tool", "old_x").arg("replacement", "new_y");
        let t = try_text(&r, "en");
        assert!(t.contains("new_y") && !t.contains('{'), "{t}");
        assert!(message(&r, "en").contains("old_x"));
    }
}
