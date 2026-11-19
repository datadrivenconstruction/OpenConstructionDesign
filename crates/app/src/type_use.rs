//! How many elements take a change of a type: one counter for the workbook preview and its
//! Apply tick, the assistant's plan card and the properties panel.
use ddc_io::xlsx::Report;
use ddc_model::Model;

/// Element tables and their type column, per type table.
const TYPE_USERS: &[(&str, &[(&str, &str)])] = &[
    ("wall_type", &[("wall", "wtype")]),
    (
        "slab_type",
        &[("slab", "stype"), ("roof", "stype"), ("ceiling", "stype")],
    ),
    ("member_type", &[("col", "mtype"), ("beam", "mtype")]),
    ("comp_type", &[("comp", "ctype")]),
];

/// Elements of type `id` in type table `table`; None for a table that is not a type.
pub fn users(m: &Model, table: &str, id: i64) -> Option<usize> {
    let (_, users) = TYPE_USERS.iter().find(|(t, _)| *t == table)?;
    let mut n = 0usize;
    for (el, col) in users.iter() {
        let c: i64 = m
            .sql()
            .query_row(
                &format!("SELECT count(*) FROM {el} WHERE {col}=?1"),
                [id],
                |r| r.get(0),
            )
            .ok()?;
        n += usize::try_from(c).unwrap_or(0);
    }
    Some(n)
}

/// Elements the type rows of a workbook report reach, each type once. An unreadable count
/// counts as unknown, never as zero users.
pub fn report_users(m: &Model, rep: &Report) -> usize {
    ddc_io::xlsx::changed_types(rep)
        .into_iter()
        .map(|(t, id)| users(m, t, id).unwrap_or(1))
        .sum()
}

/// "affects 3 walls" in an explicit language; None when nothing uses the type.
pub fn affects_text(ru: bool, table: &str, n: usize) -> Option<String> {
    if n == 0 {
        return None;
    }
    let key = if table == "wall_type" { "type_change.affects_walls" } else { "type_change.affects" };
    Some(crate::tr_n_lang(ru, key, n as u64))
}

#[cfg(test)]
#[path = "type_use_tests.rs"]
mod tests;
