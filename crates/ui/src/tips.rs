// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! First-run tips: a short deck of `tip.N` strings shown one at a time until dismissed.

/// Tip keys in showing order (`tip.<k>.title` / `tip.<k>.text`).
pub const TIPS: &[&str] = &[
    "palette", "args", "repeat", "menu", "hud", "multi", "chain", "sets", "history", "keys",
];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tips {
    pub open: bool,
    pub idx: usize,
    /// «Do not show again» — persisted by the host.
    pub done: bool,
}

impl Tips {
    /// First run: open unless already done.
    pub fn start(done: bool) -> Tips {
        Tips {
            open: !done,
            idx: 0,
            done,
        }
    }
    pub fn key(&self) -> &'static str {
        TIPS[self.idx.min(TIPS.len() - 1)]
    }
    /// Next tip; past the last one the deck closes and counts as done.
    pub fn next(&mut self) {
        if self.idx + 1 >= TIPS.len() {
            self.finish();
        } else {
            self.idx += 1;
        }
    }
    pub fn prev(&mut self) {
        self.idx = self.idx.saturating_sub(1);
    }
    pub fn finish(&mut self) {
        self.open = false;
        self.done = true;
    }
    /// Show again from the first tip (command «Tips»).
    pub fn reopen(&mut self) {
        self.open = true;
        self.idx = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deck_walks_and_closes() {
        let mut t = Tips::start(false);
        assert!(t.open);
        t.prev();
        assert_eq!(t.key(), "palette");
        for _ in 1..TIPS.len() {
            t.next();
        }
        assert_eq!(t.key(), *TIPS.last().unwrap());
        t.next();
        assert!(!t.open && t.done);
        t.reopen();
        assert!(t.open && t.idx == 0);
        assert!(!Tips::start(true).open);
        // every tip has both strings in both languages
        for lang in [crate::Lang::En, crate::Lang::Ru] {
            let c = crate::Catalog::new(lang);
            for k in TIPS {
                for part in ["title", "text"] {
                    let key = format!("tip.{k}.{part}");
                    assert!(c.table(lang).contains_key(&key), "{lang:?} {key}");
                }
            }
        }
    }
}
