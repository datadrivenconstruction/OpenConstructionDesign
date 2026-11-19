//! Parity gate of views: one script replayed through the plan, the 3D view and a
//! section must leave the same journal, byte for byte. A fresh editor per view; the script's
//! model points reach the session as screen positions of that view (`script::run_in`).
use super::pointer::ViewKind;
use super::script::{run_in, Failure, Script};
use crate::editor::Editor;

/// Views a session works in (`ToolSession::views`); the gate replays every view of the mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ViewMask(pub u8);

impl ViewMask {
    pub const PLAN: ViewMask = ViewMask(1);
    pub const VIEW3D: ViewMask = ViewMask(2);
    pub const SECTION: ViewMask = ViewMask(4);
    pub const ALL: ViewMask = ViewMask(7);

    pub const fn with(self, o: ViewMask) -> ViewMask {
        ViewMask(self.0 | o.0)
    }
    pub fn has(self, v: ViewKind) -> bool {
        let bit = match v {
            ViewKind::Plan { .. } => Self::PLAN,
            ViewKind::View3d => Self::VIEW3D,
            ViewKind::Section { .. } => Self::SECTION,
        };
        self.0 & bit.0 != 0
    }
}

impl Default for ViewMask {
    fn default() -> Self {
        Self::ALL
    }
}

/// Where two views disagree.
#[derive(Clone, Debug, PartialEq)]
pub struct ParityDiff {
    pub view: ViewKind,
    /// Checks of the script that failed in that view.
    pub failures: Vec<Failure>,
    /// Journal length of the reference view and of this one.
    pub len: (usize, usize),
    /// First differing journal row: (index, reference row, this view's row).
    pub row: Option<(usize, String, String)>,
}

impl std::fmt::Display for ParityDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}: journal {} vs {}", self.view, self.len.0, self.len.1)?;
        if let Some((i, a, b)) = &self.row {
            write!(f, ", row {i}:\n  want {a}\n  got  {b}")?;
        }
        for x in &self.failures {
            write!(f, "\n  line {}: want {} got {}", x.line, x.want, x.got)?;
        }
        Ok(())
    }
}

/// Journal rows without the clock column: what "the same model" means for a replay.
pub fn journal_rows(ed: &Editor) -> Vec<String> {
    let db = ed.model.sql();
    let Ok(mut st) = db.prepare("SELECT name FROM pragma_table_info('op') WHERE name<>'stamp' ORDER BY cid")
    else {
        return vec![];
    };
    let cols: Vec<String> = match st.query_map([], |r| r.get::<_, String>(0)) {
        Ok(m) => m.filter_map(Result::ok).map(|c| format!("quote({c})")).collect(),
        Err(_) => return vec![],
    };
    if cols.is_empty() {
        return vec![];
    }
    let sql = format!("SELECT {} FROM op ORDER BY seq", cols.join("||'|'||"));
    let Ok(mut st) = db.prepare(&sql) else {
        return vec![];
    };
    let rows: Vec<String> = match st.query_map([], |r| r.get::<_, String>(0)) {
        Ok(m) => m.filter_map(Result::ok).collect(),
        Err(_) => vec![],
    };
    rows
}

/// Replay `script` through each of `views` (the first is the reference) on fresh editors;
/// the journals must be equal and the script's checks must hold in every view.
pub fn assert_parity(script: &Script, views: &[ViewKind]) -> Result<(), ParityDiff> {
    let mut want: Option<Vec<String>> = None;
    for &v in views {
        let mut ed = Editor::new_in_memory().expect("in-memory editor");
        let report = run_in(&mut ed, script, Some(v));
        let rows = journal_rows(&ed);
        let reference = want.get_or_insert_with(|| rows.clone());
        let row = reference
            .iter()
            .zip(&rows)
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| (i, a.clone(), b.clone()));
        if !report.ok() || row.is_some() || reference.len() != rows.len() {
            return Err(ParityDiff { view: v, failures: report.failures, len: (reference.len(), rows.len()), row });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interact::script::parse;

    #[test]
    fn mask_has_its_views() {
        let m = ViewMask::PLAN.with(ViewMask::VIEW3D);
        assert!(m.has(ViewKind::Plan { level: 3 }) && m.has(ViewKind::View3d));
        assert!(!m.has(ViewKind::Section { view: 1 }));
        assert!(ViewMask::default().has(ViewKind::Section { view: 1 }));
    }

    #[test]
    fn a_wall_drawn_in_3d_is_the_wall_drawn_in_plan() {
        let s = parse("tool(WA) click(0,0) click(6,0) key(Escape) → walls=1 journal=+1").unwrap();
        assert_eq!(assert_parity(&s, &[ViewKind::Plan { level: 0 }, ViewKind::View3d]), Ok(()));
    }

    #[test]
    fn a_different_journal_is_reported() {
        // a check that holds in no view makes every view fail on its first replay
        let s = parse("tool(WA) click(0,0) click(6,0) key(Escape) → walls=2").unwrap();
        let e = assert_parity(&s, &[ViewKind::Plan { level: 0 }, ViewKind::View3d]).unwrap_err();
        assert_eq!(e.view, ViewKind::Plan { level: 0 });
        assert_eq!(e.failures.len(), 1);
    }
}
