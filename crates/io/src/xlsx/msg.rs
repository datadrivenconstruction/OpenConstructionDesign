//! Report texts are English templates; `{}` marks a substitution. The report stores the
//! English text, which doubles as the key: the app finds the template among the locale
//! entries `xl.msg.*` (see `crates/ui/data/i18n/<lang>/xl.json`) and renders it in the UI language.

use ddc_model::ParamType;

/// By-key match reasons: (locale key, English template). The texts live in
/// `crates/ui/data/i18n/<lang>/xlsx_confirm.json`, not in `xl.json`.
pub const RI1_MANY: (&str, &str) =
    ("xl.msg.key_match_many", "key matches {} elements; not matched, the row creates a new one");
pub const RI1_TAKEN: (&str, &str) = (
    "xl.msg.key_match_taken",
    "key matches an element used by another row; not matched, the row creates a new one",
);
pub const RI1_KEPT: (&str, &str) =
    ("xl.msg.key_match_kept", "kept: match rejected, row creates a new element");
/// Written by the app (`xlsx_win.rs` UNDECIDED); listed here for the locale check.
#[cfg(test)]
pub const RI1_UNDECIDED: (&str, &str) =
    ("xl.msg.key_match_undecided", "rows matched by name wait for a decision");
/// All by-key match reasons, for the locale check.
#[cfg(test)]
pub const RI1_REASONS: [(&str, &str); 4] = [RI1_MANY, RI1_TAKEN, RI1_KEPT, RI1_UNDECIDED];

/// Formulas in the app's own columns (`sheet_view.rs`, `import.rs`): (locale key, English
/// template). The texts live in `crates/ui/data/i18n/<lang>/xl_formulas.json`.
pub const FORMULA_LEFT: (&str, &str) = (
    "xl.msg.formula_in_a_model_column_not_applied",
    "formula in a model column: its result is not applied, the model value stays; type a value to change it",
);
pub const FORMULAS_DROPPED: (&str, &str) = (
    "xl.msg.x_formulas_in_column_x_on_sheet_x_replaced_by_values",
    "{} formulas in column '{}' on sheet '{}' replaced by values",
);
#[cfg(test)]
pub const FORMULA_REASONS: [(&str, &str); 2] = [FORMULA_LEFT, FORMULAS_DROPPED];

/// Rows repeating an element id (a copied row), see `dup.rs`: (locale key, English template).
/// The texts live in `crates/ui/data/i18n/<lang>/xlsx_dup.json`.
pub const DUP_SAME: (&str, &str) =
    ("xl.msg.dup_same", "duplicate id {}: same values as row {}; row ignored");
pub const DUP_UNCHANGED: (&str, &str) =
    ("xl.msg.dup_unchanged", "duplicate id {}: unchanged copy of the exported row; row ignored");
pub const DUP_CONFLICT: (&str, &str) = (
    "xl.msg.dup_conflict",
    "id {} is on rows {} with different values; nothing applied for this id",
);
pub const DUP_CONFLICT_KEEP: (&str, &str) = (
    "xl.msg.dup_conflict_keep",
    "id {} is on rows {} with different values; nothing applied for this id \u{2014} keep one row",
);
#[cfg(test)]
pub const DUP_REASONS: [(&str, &str); 4] = [DUP_SAME, DUP_UNCHANGED, DUP_CONFLICT, DUP_CONFLICT_KEEP];

/// Id and sheet rows of a repeated-id refusal that offers "create the copies as new elements".
pub fn dup_conflict_rows(reason: &str) -> Option<(i64, Vec<u32>)> {
    let a = match_template(DUP_CONFLICT.1, reason)?;
    let id = a[0].parse().ok()?;
    let rows: Option<Vec<u32>> = a[1].split(", ").map(|r| r.parse().ok()).collect();
    Some((id, rows?))
}

/// A formula cell in a model column that holds no calculated result: (locale key, template).
pub const NO_CALC: (&str, &str) = (
    "xl.msg.formula_no_value",
    "{}: formula {} has no calculated value \u{2014} open the book in a spreadsheet app, let it calculate, save",
);

/// An unknown word in the delete column: (locale key, English template); `{}` is the cell text.
pub const DELETE_UNKNOWN: (&str, &str) = (
    "xl.msg.delete_flag_unknown",
    "delete flag must be one of: delete, x, 1, true, yes, +, or empty; got '{}'",
);

/// A read-only cell that differs from the model: (locale key, English template); `{}` is the
/// model value formatted like the export.
pub const READ_ONLY: (&str, &str) = (
    "xl.msg.read_only_column_computed_value_is_x_edit_no",
    "read-only column, computed value is {}; edit not accepted",
);

/// Text cells the spreadsheet turned into numbers: (locale key, English template). `NUM_SAME`
/// is a note (model text kept, the key lives in `xl.json`); `NUM_LOSSY` refuses the cell, its
/// texts live in `crates/ui/data/i18n/<lang>/xl_convert.json`.
pub const NUM_SAME: (&str, &str) = (
    "xl.msg.the_spreadsheet_turned_this_code_into_a_number_format_the_column_as_text_and_type_it_again",
    "the spreadsheet turned this code into a number \u{2014} format the column as Text and type it again",
);
pub const NUM_LOSSY: (&str, &str) = (
    "xl.msg.num_lossy",
    "the spreadsheet turned this code into a number and dropped digits \u{2014} format the column as Text and type it again",
);

/// A blank-id Materials row whose name is no material while a row-less material is missing
/// from the book (a rename): (locale key, English template). The texts live in `xl.json`.
pub const MAT_RENAMED: (&str, &str) = (
    "xl.msg.mat_rowless_renamed",
    "no material named '{}': a row without an id is matched by its name, and '{}' is not in \
     the book; a renamed material is not taken \u{2014} keep the old name or rename it in the project",
);

/// Family-sheet name cells: (locale key, English template).
pub const FAM_TYPE_UNKNOWN: (&str, &str) = ("xl.msg.fam_type_unknown", "unknown type '{}'");
pub const FAM_CHOICE_UNKNOWN: (&str, &str) = ("xl.msg.fam_choice_unknown", "unknown choice '{}'");
pub const FAM_TYPE_AMBIGUOUS: (&str, &str) = (
    "xl.msg.fam_type_ambiguous",
    "type name '{}' is ambiguous (matches {}); enter the number instead",
);
pub const FAM_CHOICE_AMBIGUOUS: (&str, &str) = (
    "xl.msg.fam_choice_ambiguous",
    "choice '{}' is ambiguous (matches {}); enter the number instead",
);

/// Parameter columns the reader cannot bind: (locale key, English template). The texts live in
/// `crates/ui/data/i18n/<lang>/xl_param_head.json`.
pub const PARAM_AMBIGUOUS: (&str, &str) = (
    "xl.msg.param_ambiguous",
    "column '{}' could be any of the parameters {}; type the exact parameter name, not applied",
);
pub const PARAM_TAG_STALE: (&str, &str) = (
    "xl.msg.param_tag_stale",
    "column '{}' names a parameter this project does not have; not applied",
);
#[cfg(test)]
pub const PARAM_HEAD_REASONS: [(&str, &str); 2] = [PARAM_AMBIGUOUS, PARAM_TAG_STALE];

/// Columns of a sheet that carry one field, see `dup_cols.rs`: (locale key, English template).
/// The texts live in `crates/ui/data/i18n/<lang>/xl_param_head.json`.
pub const DUP_COL_NOTE: (&str, &str) = (
    "xl.msg.dup_col_note",
    "column '{}' in {} repeats column '{}' in {}; equal values apply once, \
     a row where they differ keeps the model value",
);
pub const DUP_COL_DIFFERS: (&str, &str) = (
    "xl.msg.dup_col_differs",
    "columns '{}' and '{}' differ in {} and {}; the model value stays \u{2014} keep one column",
);
pub const DUP_COL_ID: (&str, &str) = (
    "xl.msg.dup_col_id",
    "columns '{}' and '{}' differ in {} and {}, the row cannot be identified; \
     nothing applied \u{2014} keep one column",
);
#[cfg(test)]
pub const DUP_COL_REASONS: [(&str, &str); 3] = [DUP_COL_NOTE, DUP_COL_DIFFERS, DUP_COL_ID];

/// Substitutions if `text` is an instance of `tpl`; None if it comes from another template.
pub fn match_template<'a>(tpl: &str, text: &'a str) -> Option<Vec<&'a str>> {
    let parts: Vec<&str> = tpl.split("{}").collect();
    let mut rest = text.strip_prefix(parts[0])?;
    if parts.len() == 1 {
        return rest.is_empty().then(Vec::new);
    }
    let mut args = Vec::with_capacity(parts.len() - 1);
    let last = parts.len() - 1;
    for (i, p) in parts.iter().enumerate().skip(1) {
        if i == last {
            args.push(rest.strip_suffix(p)?);
        } else {
            let k = rest.find(p)?;
            args.push(&rest[..k]);
            rest = &rest[k + p.len()..];
        }
    }
    Some(args)
}

/// Template with `{}` replaced by `args` in order.
pub fn fill_template(tpl: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(tpl.len() + 16);
    let mut it = args.iter();
    let mut parts = tpl.split("{}").peekable();
    while let Some(p) = parts.next() {
        out.push_str(p);
        if parts.peek().is_some() {
            out.push_str(it.next().copied().unwrap_or(""));
        }
    }
    out
}

/// Column as the report names it: the caption the book shows; when the user renamed it, the
/// original name follows in brackets so both can be found.
pub fn column_label(caption: &str, key: &str) -> String {
    let (c, k) = (caption.trim(), key.trim());
    if c.is_empty() || c.eq_ignore_ascii_case(k) {
        k.to_string()
    } else {
        format!("{c} [{k}]")
    }
}

/// Locale key of a parameter type name.
pub fn ptype_key(t: ParamType) -> &'static str {
    match t {
        ParamType::Text => "xl.ptype.text",
        ParamType::Int => "xl.ptype.int",
        ParamType::Number => "xl.ptype.number",
        ParamType::Length => "xl.ptype.length",
        ParamType::Area => "xl.ptype.area",
        ParamType::Volume => "xl.ptype.volume",
        ParamType::YesNo => "xl.ptype.yesno",
    }
}

/// Locale key of a category in the plural (`for walls`).
pub fn category_key(c: &str) -> &'static str {
    match c {
        "wall" => "xl.cat.wall",
        "door" => "xl.cat.door",
        "window" => "xl.cat.window",
        "curtain_wall" => "xl.cat.curtain_wall",
        "slab" => "xl.cat.slab",
        "room" => "xl.cat.room",
        "col" => "xl.cat.col",
        "beam" => "xl.cat.beam",
        "roof" => "xl.cat.roof",
        "ceiling" => "xl.cat.ceiling",
        "stair" => "xl.cat.stair",
        "railing" => "xl.cat.railing",
        "mep_run" => "xl.cat.mep_run",
        "comp" => "xl.cat.comp",
        "shaft" => "xl.cat.shaft",
        "ramp" => "xl.cat.ramp",
        _ => "xl.cat.other",
    }
}

#[cfg(test)]
mod tests {
    use super::column_label;

    #[test]
    fn renamed_caption_keeps_both_names() {
        assert_eq!(column_label("level (read-only)", "level (read-only)"), "level (read-only)");
        assert_eq!(column_label("Floor", "level (read-only)"), "Floor [level (read-only)]");
        assert_eq!(column_label(" ", "kind (read-only)"), "kind (read-only)");
    }
}
