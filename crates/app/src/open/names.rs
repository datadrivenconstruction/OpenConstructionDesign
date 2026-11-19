//! One wording rule and one default-name rule for saving.
//! "Save" writes the project file. Everything else is "Save copy as <format>" and is named
//! `<project name>-edited.<ext>`, whatever the format or what the copy holds.

/// Appended to the stem of every copy.
pub const COPY_SUFFIX: &str = "-edited";

/// Stem of a copy: the name without an extension, and without a suffix it already has.
pub fn copy_stem(stem: &str) -> String {
    let base = stem.strip_suffix(COPY_SUFFIX).unwrap_or(stem);
    format!("{base}{COPY_SUFFIX}")
}

/// Default file name of a copy in format `ext`.
pub fn copy_name(stem: &str, ext: &str) -> String {
    format!("{}.{ext}", copy_stem(stem))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_name_rule() {
        assert_eq!(copy_name("House", "ifc"), "House-edited.ifc");
        assert_eq!(copy_name("House", "xlsx"), "House-edited.xlsx");
        // saving a copy of a copy does not stack suffixes
        assert_eq!(copy_name("House-edited", "ifc"), "House-edited.ifc");
    }
}
