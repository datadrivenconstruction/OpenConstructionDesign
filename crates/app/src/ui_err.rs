// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Human-readable status/toast texts: model and io error messages (English `Display`) become
//! «what happened + what to do» from `err.*` keys, internal ids are dropped.

/// One known error: `Display` text matcher → i18n key (`err.<key>` with `{msg}`/`{a}`/`{b}`).
fn classify(e: &str) -> Option<(&'static str, Vec<(&'static str, String)>)> {
    let s = e.trim();
    let num_pair = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect()
    };
    match s {
        "wall too short" => return Some(("wall_short", vec![])),
        "close needs three points" => return Some(("close_three", vec![])),
        "rect zero size" => return Some(("rect_zero", vec![])),
        _ => {}
    }
    if s == "nothing to undo" {
        return Some(("nothing_to_undo", vec![]));
    }
    if s == "nothing selected" {
        return Some(("nothing_selected", vec![]));
    }
    if s == "lines are parallel" {
        return Some(("parallel", vec![]));
    }
    if s == "edge is not parallel to the reference" {
        return Some(("not_parallel", vec![]));
    }
    if s == "count must be at least 2" {
        return Some(("count_min", vec![]));
    }
    if s.starts_with("degenerate") {
        return Some(("degenerate", vec![]));
    }
    if s.starts_with("geometry: ") {
        return Some(("geom", vec![]));
    }
    if let Some(rest) = s.strip_prefix("invalid: ") {
        return Some(("invalid", vec![("msg", rest.to_string())]));
    }
    if s.contains(" is still referenced by ") {
        let n = s.rsplit(" is still referenced by ").next().unwrap_or("");
        let n = n.split(", ").filter(|x| !x.is_empty()).count().max(1);
        return Some(("referenced", vec![("n", n.to_string())]));
    }
    if s.starts_with("opening is not inside its") || s.ends_with("would fall outside its wall") {
        let key = if s.starts_with("component") {
            "component_outside"
        } else {
            "opening_outside"
        };
        return Some((key, vec![]));
    }
    if s.starts_with("point is not on wall") {
        return Some(("off_wall", vec![]));
    }
    if s.starts_with("file schema version") {
        let v = num_pair(s);
        let (a, b) = (
            v.first().cloned().unwrap_or_default(),
            v.get(1).cloned().unwrap_or_default(),
        );
        return Some(("newer", vec![("a", a), ("b", b)]));
    }
    if s.starts_with("model file is damaged") {
        return Some(("corrupt", vec![]));
    }
    if s.ends_with(" not found") {
        return Some(("not_found", vec![]));
    }
    if s.contains(" that follows its ") && s.contains(" is not supported for ") {
        return Some(("rides_along", vec![]));
    }
    if s.contains(" is not supported for ") {
        return Some(("unsupported", vec![]));
    }
    if s.contains("(os error ") {
        return Some(("io", vec![]));
    }
    let low = s.to_ascii_lowercase();
    if low.contains("sqlite") || low.contains("constraint failed") || low.contains("database") {
        return Some(("internal", vec![]));
    }
    None
}

/// Status line: «<prefix>: <error>» → «<prefix>: <human text>»; unknown text unchanged
/// (English io/model fragments go through the report template translator in RU).
pub fn humanize(status: &str, ru: bool) -> String {
    let lang = if ru {
        crate::xl_text::Lang::Ru
    } else {
        crate::xl_text::Lang::En
    };
    let (head, tail) = match status.split_once(": ") {
        Some((h, t)) if classify(t).is_some() || classify(status).is_none() => (Some(h), t),
        _ => (None, status),
    };
    let Some((key, args)) = classify(tail) else {
        return status.to_string();
    };
    let args: Vec<(&str, String)> = args
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                if k == "msg" {
                    crate::xl_text::tr(&v, lang)
                } else {
                    v
                },
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let body = crate::tr(&format!("err.{key}"), &refs);
    match head {
        Some(h) if !h.is_empty() => format!("{h}: {body}"),
        _ => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_refusal_is_translated() {
        for ru in [false, true] {
            crate::lang_on_this_thread(ru);
            let s = humanize(
                "x: moving a footing that follows its column is not supported for Footing(3)",
                ru,
            );
            assert!(!s.contains("Footing") && !s.contains("follows"), "{s}");
            assert!(s.starts_with("x: "), "{s}");
        }
    }

    #[test]
    fn known_errors_become_human_and_drop_ids() {
        crate::lang_on_this_thread(true);
        let not_deleted = crate::app2_fx::fx("ue_not_deleted");
        let s = humanize(
            &format!("{not_deleted}level 3 is still referenced by wall 7, wall 9"),
            true,
        );
        assert!(s.starts_with(&not_deleted), "{s}");
        assert!(!s.contains("wall 7") && !s.contains('7'), "{s}");
        assert!(s.contains('2'), "{s}");
        let s = humanize(
            &format!("{}opening 12 would fall outside its wall", crate::app2_fx::fx("ue_opening_not_placed")),
            true,
        );
        assert!(!s.contains("12") && !s.contains("would fall"), "{s}");
        let s = humanize("file schema version 40 is newer than supported 29", true);
        assert!(
            s.contains("40") && s.contains("29") && !s.contains("schema"),
            "{s}"
        );
    }

    #[test]
    fn unknown_text_is_unchanged_and_every_key_exists() {
        let demolished = crate::app2_fx::fx("phase_demolished_1");
        assert_eq!(humanize(&demolished, true), demolished);
        let (en, ru) = (
            ddc_ui::Catalog::new(ddc_ui::i18n::Lang::En),
            ddc_ui::Catalog::new(ddc_ui::i18n::Lang::Ru),
        );
        for k in [
            "nothing_to_undo",
            "nothing_selected",
            "parallel",
            "not_parallel",
            "count_min",
            "degenerate",
            "geom",
            "invalid",
            "referenced",
            "opening_outside",
            "component_outside",
            "off_wall",
            "newer",
            "corrupt",
            "not_found",
            "unsupported",
            "io",
            "internal",
        ] {
            let key = format!("err.{k}");
            for c in [&en, &ru] {
                assert!(c.table(c.lang).contains_key(&key), "{key} {:?}", c.lang);
            }
        }
    }
}
