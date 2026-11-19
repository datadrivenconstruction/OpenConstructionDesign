// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Free names: the one rule for picking a name nobody uses yet.

/// `base` when free, else the first free `numbered(k)` for k = 2, 3, ... `taken` says whether a
/// candidate is in use; each caller keeps its own numbering style ("A (2)", "A 2", "A-2").
pub fn free_name<E>(
    base: &str,
    numbered: impl Fn(u32) -> String,
    mut taken: impl FnMut(&str) -> Result<bool, E>,
) -> Result<String, E> {
    if !taken(base)? {
        return Ok(base.to_string());
    }
    let mut k = 2;
    loop {
        let cand = numbered(k);
        if !taken(&cand)? {
            return Ok(cand);
        }
        k += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::free_name;

    #[test]
    fn the_base_then_the_first_free_number() {
        let used = ["A", "A (2)"];
        let pick = |b: &str| {
            free_name(b, |k| format!("{b} ({k})"), |c| Ok::<_, ()>(used.contains(&c))).unwrap()
        };
        assert_eq!(pick("B"), "B");
        assert_eq!(pick("A"), "A (3)");
    }
}
