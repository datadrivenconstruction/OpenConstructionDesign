//! Search over commands and their keys for the reference panel: label, id and keys, ranked by
//! the palette scorer; typing a key code or chord puts its command first.
use crate::fuzzy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub label: String,
    /// Chords and the two-letter code, joined with ` · ` (shown as is).
    pub keys: String,
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A key typed as a query equals one of the row's keys.
fn key_exact(q: &str, keys: &str) -> bool {
    let q = squash(q);
    !q.is_empty() && keys.split('·').any(|k| squash(k) == q)
}

/// Rows matching `query`, best first; an empty query lists everything by label. Rows without
/// a label are not shown.
pub fn search<'a>(rows: &'a [Row], query: &str) -> Vec<&'a Row> {
    let q = query.trim();
    let mut hit: Vec<(i32, &Row)> = rows
        .iter()
        .filter(|r| !r.label.is_empty())
        .filter_map(|r| {
            if q.is_empty() {
                return Some((0, r));
            }
            if key_exact(q, &r.keys) {
                return Some((10_000, r));
            }
            let named = fuzzy::best(q, &[r.label.as_str(), r.id.as_str()]);
            // a partial key typed ("ctrl") is worth a lookup in the keys column too
            let keyed = fuzzy::score(q, &r.keys).map(|k| k - 200);
            named.max(keyed).map(|s| (s, r))
        })
        .collect();
    hit.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.label.to_lowercase().cmp(&b.1.label.to_lowercase()))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });
    hit.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, label: &str, keys: &str) -> Row {
        Row {
            id: id.into(),
            label: label.into(),
            keys: keys.into(),
        }
    }

    fn rows() -> Vec<Row> {
        vec![
            row("edit.undo", "Undo", "Ctrl+Z"),
            row("arch.wall", "Wall", "WA"),
            row("arch.wall_room", "Walls for a room", ""),
            row("arch.window", "Window", "WI · Ctrl+Alt+W"),
            row("hidden.cmd", "", "QQ"),
        ]
    }

    #[test]
    fn a_typed_key_finds_its_command_first() {
        let r = rows();
        assert_eq!(search(&r, "wa")[0].id, "arch.wall");
        assert_eq!(search(&r, "ctrl + z")[0].id, "edit.undo");
        assert_eq!(search(&r, "Ctrl+Alt+W")[0].id, "arch.window");
    }

    #[test]
    fn names_rank_by_the_palette_scorer() {
        let r = rows();
        let ids: Vec<&str> = search(&r, "wall").iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids[0], "arch.wall");
        assert!(ids.contains(&"arch.wall_room"));
        assert!(search(&r, "zzzz").is_empty());
    }

    #[test]
    fn empty_query_lists_labelled_rows_alphabetically() {
        let r = rows();
        let all: Vec<&str> = search(&r, "  ").iter().map(|r| r.label.as_str()).collect();
        assert_eq!(all, vec!["Undo", "Wall", "Walls for a room", "Window"]);
    }
}
