//! Regional gate of automatic building services. The generator's services stage and the
//! assistant tools that size or lay out services by themselves follow the project setting
//! `gen.mep_auto`; unset, it is off for the US norm pack and the en-US locale and on elsewhere.
//! Routing by hand (`route_mep`, `place_fixtures`, `supply_fixtures`) is never gated.

use ddc_ai::refusal::{self, RefusalCode};
use ddc_model::Model;
use serde_json::{json, Value as J};

/// Project setting (a policy flag): automatic services on or off.
pub const POLICY_MEP_AUTO: &str = "gen.mep_auto";

/// The default of the setting for a norm pack id (empty: not chosen) and a UI language code.
pub fn default_on(pack: &str, ui_lang: &str) -> bool {
    // a chosen pack decides; with none, only the explicit US locale tag does (bare "en" is not US)
    match pack {
        "" => !ui_lang.trim().replace('_', "-").eq_ignore_ascii_case("en-US"),
        p => !p.eq_ignore_ascii_case("us"),
    }
}

/// The setting for the project: its own answer, else the regional default.
pub fn is_on(m: &Model, ui_lang: &str) -> bool {
    let pack = m.norm_locale().map(|n| n.pack).unwrap_or_default();
    let default = default_on(&pack, ui_lang);
    super::ui::connect::policy_flag(m, POLICY_MEP_AUTO, default)
}

/// `is_on` for the running session's interface language.
pub fn is_on_now(m: &Model) -> bool {
    is_on(m, ddc_ui::i18n::current().code())
}

/// The brief with `mep_auto: false` when the setting is off and the brief says nothing.
pub fn gated(m: &Model, input: &J) -> J {
    if !input.is_object() || !input["mep_auto"].is_null() || is_on_now(m) {
        return input.clone();
    }
    let mut out = input.clone();
    out["mep_auto"] = json!(false);
    out
}

/// The typed refusal of a tool that would build services on its own.
pub fn refused(tool: &str) -> String {
    let r = refusal::of(RefusalCode::NotAllowedInMode)
        .arg("mode", "mep_auto_off")
        .arg("tool", tool.to_string())
        .detail("automatic building services are off for this project (setting gen.mep_auto); route by hand with route_mep")
        .fix(refusal::open_settings());
    refusal::wire(&r)
}

/// The notice that goes with a draft built while the setting is off.
pub fn notice() -> J {
    json!({
        "code": "mep_auto_off",
        "setting": POLICY_MEP_AUTO,
        "text": "services, fire protection and ventilation were not generated; route by hand with route_mep, or turn on gen.mep_auto in the assistant settings",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_pack_is_off_by_default_and_de_is_on() {
        assert!(!default_on("us", "de"));
        assert!(default_on("de", "en"));
        assert!(default_on("de", "en-US"));
        assert!(default_on("ru", "ru"));
    }

    #[test]
    fn unchosen_pack_follows_the_locale() {
        assert!(default_on("", "en"));
        assert!(!default_on("", "en-US"));
        assert!(!default_on("", "en_us"));
        assert!(default_on("", "de"));
        assert!(default_on("", "en-GB"));
    }

    #[test]
    fn the_refusal_is_typed_and_names_the_setting() {
        let w = refused("power_devices");
        assert!(w.starts_with(ddc_ai::refusal::WIRE_PREFIX), "{w}");
        assert!(w.contains("gen.mep_auto"), "{w}");
        assert!(w.contains("mep_auto_off"), "{w}");
    }

    #[test]
    fn the_brief_gets_the_switch_only_when_it_is_silent() {
        let mut m = Model::open(":memory:").unwrap();
        let mut n = m.norm_locale().unwrap();
        n.pack = "us".into();
        m.set_norm_locale(&n).unwrap();
        let g = gated(&m, &json!({"floors": 2}));
        assert_eq!(g["mep_auto"], json!(false));
        let own = gated(&m, &json!({"mep_auto": true}));
        assert_eq!(own["mep_auto"], json!(true));
        m.set_ai_policy(POLICY_MEP_AUTO, "1").unwrap();
        assert!(gated(&m, &json!({"floors": 2}))["mep_auto"].is_null());
        n.pack = "de".into();
        m.set_norm_locale(&n).unwrap();
        m.set_ai_policy(POLICY_MEP_AUTO, "0").unwrap();
        assert_eq!(gated(&m, &json!({}))["mep_auto"], json!(false));
    }
}
