// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Modelling guides an agent reads before it works: plain text served as MCP resources
//! `ddc://skills/<id>`. No code runs from a guide; the texts live in `skills/` at the repo root.

/// The scheme of every guide address.
pub const URI_PREFIX: &str = "ddc://skills/";
/// A guide longer than this costs the agent more context than it saves.
pub const MAX_BYTES: usize = crate::limits::SKILL_BYTES.us();
pub const MIME: &str = "text/markdown";

pub struct Skill {
    pub id: &'static str,
    pub title: &'static str,
    /// What the agent sees in `resources/list` and chooses by: when this guide applies.
    pub description: &'static str,
    pub text: &'static str,
}

pub static SKILLS: &[Skill] = &[
    Skill {
        id: "structure",
        title: "Load-bearing structures",
        description: "Read first when drafting a structural system of members: lattice towers, masts, trusses, space frames, gridshells, arches, bridges, frames with connections.",
        text: include_str!("../../../skills/structure.md"),
    },
    Skill {
        id: "mep",
        title: "Building services networks",
        description: "Read first when drafting ducts, pipes, trays or the equipment they serve: ventilation, heating and cooling, water, drainage, sprinklers, process piping.",
        text: include_str!("../../../skills/mep.md"),
    },
    Skill {
        id: "safe_edits",
        title: "Safe edits of an existing model",
        description: "Read first when changing a model that already has content: add to it, move, resize, replace types, delete or fix findings without touching the rest.",
        text: include_str!("../../../skills/safe_edits.md"),
    },
];

impl Skill {
    pub fn uri(&self) -> String {
        format!("{URI_PREFIX}{}", self.id)
    }
}

pub fn find(id: &str) -> Option<&'static Skill> {
    SKILLS.iter().find(|s| s.id == id)
}

/// The guide an address names; `None` for any other address.
pub fn by_uri(uri: &str) -> Option<&'static Skill> {
    uri.strip_prefix(URI_PREFIX).and_then(find)
}

/// The guide an agent read first among the resource reads of one request, in their order:
/// the trigger oracle. Reads of other addresses do not count.
pub fn first_read<'a>(uris: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    uris.into_iter().find_map(by_uri).map(|s| s.id)
}

/// Whether one trigger case held: a "yes" prompt must make its guide the first one read, a "no"
/// prompt must not.
pub fn trigger_holds(skill: &str, expect_yes: bool, first: Option<&str>) -> bool {
    (first == Some(skill)) == expect_yes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guides_are_small_and_complete() {
        for s in SKILLS {
            assert!(
                s.text.len() <= MAX_BYTES,
                "{}: {} bytes",
                s.id,
                s.text.len()
            );
            assert!(s.text.starts_with("# "), "{}: no title", s.id);
            for part in [
                "## When to use",
                "## Order of tools",
                "## Refusals and what to do",
            ] {
                assert!(s.text.contains(part), "{}: no {part}", s.id);
            }
            // the order the guides teach: read, propose in a group, check after it, look
            let at = |t: &str| s.text.find(t).unwrap_or_else(|| panic!("{}: no {t}", s.id));
            let last_check = s.text.rfind("`run_checks`").unwrap_or(0);
            assert!(at("query_elements") < at("changeset_begin"), "{}", s.id);
            assert!(at("changeset_begin") < at("changeset_commit"), "{}", s.id);
            assert!(
                at("changeset_commit") < last_check,
                "{}: no check after the writes",
                s.id
            );
            assert!(
                last_check < s.text.rfind("`view_snapshot`").unwrap_or(0),
                "{}",
                s.id
            );
            assert!(
                !s.description.is_empty() && s.description.len() <= 240,
                "{}",
                s.id
            );
        }
    }

    #[test]
    fn guides_name_only_tools_that_exist() {
        for s in SKILLS {
            for word in s.text.split('`').skip(1).step_by(2) {
                let name = word.split([' ', ':']).next().unwrap_or("");
                let snake =
                    name.contains('_') && name.chars().all(|c| c.is_ascii_lowercase() || c == '_');
                if snake || name == "clash" {
                    assert!(
                        crate::mcp::tools::find(name).is_some(),
                        "{}: `{name}` is no tool",
                        s.id
                    );
                }
            }
        }
    }

    #[test]
    fn addresses_resolve() {
        for s in SKILLS {
            assert_eq!(by_uri(&s.uri()).map(|x| x.id), Some(s.id));
        }
        assert!(by_uri("ddc://skills/none").is_none());
        assert!(by_uri("file:///etc/passwd").is_none());
        assert_eq!(
            first_read([
                "ddc://other/x",
                "ddc://skills/mep",
                "ddc://skills/structure"
            ]),
            Some("mep")
        );
        assert!(trigger_holds("mep", true, Some("mep")));
        assert!(!trigger_holds("mep", true, Some("structure")));
        assert!(!trigger_holds("mep", false, Some("mep")));
        assert!(trigger_holds("mep", false, None));
    }
}
