//! Workbook report texts in the UI language. `ddc-io` reports English template texts;
//! the translations live in `crates/ui/data/i18n/<lang>/xl.json` (keys `xl.*`).

use std::sync::OnceLock;

use ddc_io::xlsx::{fill_template, match_template, Proposal};
use ddc_ui::i18n::Catalog;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    En,
    Ru,
}

fn cat() -> &'static Catalog {
    static CAT: OnceLock<Catalog> = OnceLock::new();
    CAT.get_or_init(|| Catalog::new(ddc_ui::i18n::Lang::En))
}

fn ui_lang(l: Lang) -> ddc_ui::i18n::Lang {
    match l {
        Lang::En => ddc_ui::i18n::Lang::En,
        Lang::Ru => ddc_ui::i18n::Lang::Ru,
    }
}

/// English entries `(key, text)` whose key starts with `prefix`.
fn entries(prefix: &'static str) -> impl Iterator<Item = (&'static String, &'static str)> {
    cat()
        .table(ddc_ui::i18n::Lang::En)
        .range(prefix.to_string()..)
        .take_while(move |(k, _)| k.starts_with(prefix))
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s)))
}

/// An OS error for a file another program holds or may not touch (sharing violation, access denied).
pub fn is_locked(text: &str) -> bool {
    let low = text.to_lowercase();
    ["(os error 32)", "(os error 33)", "(os error 5)", "used by another process", "access is denied", "permission denied"]
        .iter()
        .any(|k| low.contains(k))
}

/// Report text in the UI language; text not from a known template is returned as is.
pub fn tr(text: &str, lang: Lang) -> String {
    if is_locked(text) {
        return cat().t_in(ui_lang(lang), "xlw.book_locked");
    }
    if lang == Lang::En {
        return text.to_string();
    }
    let (c, ul) = (cat(), ui_lang(lang));
    if let Some(rest) = text.strip_prefix(ddc_io::xlsx::LOSS_PREFIX) {
        return format!("{}{}", c.t_in(ul, "xlloss.refused"), tr_losses(rest, lang));
    }
    for (key, en) in entries("xl.pfx.") {
        if let Some(rest) = text.strip_prefix(en) {
            return format!("{}{}", c.t_in(ul, key), tr(rest, lang));
        }
    }
    // the most specific template wins: the one with the longest fixed part
    let mut best: Option<(usize, &String, Vec<&str>)> = None;
    for (key, tpl) in entries("xl.msg.") {
        if let Some(args) = match_template(tpl, text) {
            let lit = tpl.len() - 2 * args.len();
            if best.as_ref().is_none_or(|b| lit > b.0) {
                best = Some((lit, key, args));
            }
        }
    }
    match best {
        Some((_, key, args)) => fill_template(&c.t_in(ul, key), &args),
        None => text.to_string(),
    }
}

/// Loss items joined by `LOSS_SEP`, each in the UI language.
pub fn tr_losses(items: &str, lang: Lang) -> String {
    items
        .split(ddc_io::xlsx::LOSS_SEP)
        .map(|i| tr(i, lang))
        .collect::<Vec<_>>()
        .join(ddc_io::xlsx::LOSS_SEP)
}

/// Sheet name in the report: the service sheet "(model)" is translated, book sheets stay as in the file.
pub fn sheet_name(sheet: &str, lang: Lang) -> String {
    if sheet == "(model)" {
        cat().t_in(ui_lang(lang), "xl.sheet.model")
    } else {
        sheet.to_string()
    }
}

/// One-line description of the project parameter a new column would create.
pub fn describe(p: &Proposal, lang: Lang) -> String {
    let (c, ul) = (cat(), ui_lang(lang));
    let cats: Vec<String> = p.category_keys().into_iter().map(|k| c.t_in(ul, k)).collect();
    let mut ty = c.t_in(ul, p.type_key());
    if !p.spec.unit.is_empty() {
        ty = format!("{ty}, {}", p.spec.unit);
    }
    let key = if p.spec.per_type { "xl.param.type" } else { "xl.param.project" };
    c.t_in(ul, key)
        .replace("{ty}", &ty)
        .replace("{cats}", &cats.join(&c.t_in(ul, "xl.and")))
        .replace("{n}", &p.spec.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected Russian text, kept in a data file.
    fn ru(key: &str) -> String {
        let m: serde_json::Value = serde_json::from_str(include_str!("../tests/data/xl_text_ru.json")).unwrap();
        m[key].as_str().unwrap().to_string()
    }

    #[test]
    fn misplaced_param_column_renders_in_russian() {
        let en = "parameter 'Fire' is a type parameter; edit it on the types sheet, not applied";
        let ru = tr(en, Lang::Ru);
        assert!(ru.contains("Fire") && ru != en && !ru.contains("types sheet"), "{ru}");
        let en = "parameter 'Mark' is an instance parameter; edit it on the elements sheet, not applied";
        let ru = tr(en, Lang::Ru);
        assert!(ru.contains("Mark") && ru != en && !ru.contains("elements sheet"), "{ru}");
    }

    #[test]
    fn loss_refusal_renders_item_by_item() {
        use ddc_io::xlsx::{LOSS_PREFIX, LOSS_SEP};
        let e = format!("{LOSS_PREFIX}charts on sheet 'Notes'{LOSS_SEP}3 cells of column 'Cost' on sheet 'Walls'");
        assert_eq!(
            tr(&e, Lang::Ru),
            ru("loss")
        );
        assert_eq!(tr(&e, Lang::En), e);
    }

    #[test]
    fn formula_texts_render_in_both_languages() {
        let item = "2 formulas in column 'height [m]' on sheet 'Walls' replaced by values";
        let left = "formula in a model column: its result is not applied, the model value stays; type a value to change it";
        assert_eq!(tr(item, Lang::En), item);
        assert_eq!(tr(left, Lang::En), left);
        let ru = cat().t_in(ddc_ui::i18n::Lang::Ru, "xl.msg.x_formulas_in_column_x_on_sheet_x_replaced_by_values");
        assert_eq!(tr(item, Lang::Ru), fill_template(&ru, &["2", "height [m]", "Walls"]));
        assert_eq!(tr(left, Lang::Ru), cat().t_in(ddc_ui::i18n::Lang::Ru, "xl.msg.formula_in_a_model_column_not_applied"));
        assert_ne!(tr(left, Lang::Ru), left);
    }

    #[test]
    fn templates_render_in_russian() {
        assert_eq!(tr("unknown wall id 7", Lang::Ru), ru("wall_id"));
        assert_eq!(
            tr("invalid: no such element wall 5", Lang::Ru),
            ru("no_element")
        );
        assert_eq!(
            tr("duplicate wall type name 'A'", Lang::Ru),
            ru("dup_type")
        );
        assert_eq!(
            tr("not a number or length: 'x' (e.g. 3.5, 3500 mm, 10'-6\")", Lang::Ru),
            ru("not_number")
        );
        assert_eq!(
            tr("unknown phase 'Missing'", Lang::Ru),
            ru("phase")
        );
        assert_eq!(
            tr("read-only sheet; not applied", Lang::Ru),
            ru("read_only")
        );
        assert_eq!(tr("something else", Lang::Ru), "something else");
        assert_eq!(
            tr("read-only sheet; not applied", Lang::En),
            "read-only sheet; not applied"
        );
        assert_eq!(sheet_name("(model)", Lang::Ru), ru("model_sheet"));
        assert_eq!(sheet_name("(model)", Lang::En), "(model)");
        assert_eq!(sheet_name("Walls", Lang::Ru), "Walls");
    }

    #[test]
    fn family_name_refusals_render_in_both_languages() {
        let cases = [
            ("unknown type 'A'", ru("type_a")),
            ("unknown choice 'B'", ru("choice_b")),
            (
                "type name 'A' is ambiguous (matches 1, 2); enter the number instead",
                ru("type_ambiguous"),
            ),
            (
                "choice 'B' is ambiguous (matches 0, 3); enter the number instead",
                ru("choice_ambiguous"),
            ),
        ];
        for (en, ru) in cases {
            assert_eq!(tr(en, Lang::En), en);
            assert_eq!(tr(en, Lang::Ru), ru);        }
    }

    #[test]
    fn locked_file_errors_get_the_plain_message() {
        let raw = "cannot open workbook: The process cannot access the file because it is being used by another process. (os error 32)";
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied).to_string();
        let c = cat();
        for lang in [Lang::En, Lang::Ru] {
            let want = c.t_in(ui_lang(lang), "xlw.book_locked");
            assert_eq!(tr(raw, lang), want);
            assert_eq!(tr(&denied, lang), want);
            assert_eq!(tr("Access is denied. (os error 5)", lang), want);
        }
        assert_ne!(tr(raw, Lang::En), tr(raw, Lang::Ru));
        assert_eq!(tr("value required", Lang::En), "value required");
    }

    #[test]
    fn every_param_type_and_category_has_both_languages() {
        let c = cat();
        for t in ddc_model::ParamType::ALL {
            let k = ddc_io::xlsx::ptype_key(t);
            assert_ne!(c.t_in(ddc_ui::i18n::Lang::Ru, k), c.t_in(ddc_ui::i18n::Lang::En, k));
        }
        for (cn, _) in ddc_model::PARAM_CATEGORIES {
            let k = ddc_io::xlsx::category_key(cn);
            assert_ne!(c.t_in(ddc_ui::i18n::Lang::Ru, k), c.t_in(ddc_ui::i18n::Lang::En, k), "{cn}");
        }
    }

    #[test]
    fn bad_id_reason_localized_ru() {
        let t = "unreadable id: elements without a row are not deleted in this import";
        assert_eq!(tr(t, Lang::Ru), ru("bad_id"));
        assert_eq!(
            tr("invalid: level name 'B' is already used", Lang::Ru),
            ru("level_used")
        );
    }
}
