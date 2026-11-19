//! Hook for structural results (sa12). Connections are geometry and schedule items only: no
//! strength or capacity calculation is done here, so every node reads "not checked by
//! analysis" until a certified analysis supplies results.

use super::Conn;

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// No analysis results for this node.
    NotChecked,
}

impl Verdict {
    /// Stable key for the status text (`steel.status.*` in the UI strings).
    pub fn key(&self) -> &'static str {
        match self {
            Verdict::NotChecked => "not_checked",
        }
    }
}

/// The status of a node. Always "not checked": no uncertified calculation is made.
pub fn verdict(_c: &Conn) -> Verdict {
    Verdict::NotChecked
}
