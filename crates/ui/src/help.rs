// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Built-in help: Markdown-lite topics `data/help/<lang>/<id>.md` (heading `# `, `@tags`,
//! `@context`, `@commands`, `@see` lines, sections `## steps|keys|tips`), search, context-sensitive
//! F1 topic, "What's new" (`data/help/<lang>/whats_new.md`).
use crate::fragments::HELP_ALL;
use crate::Lang;

const WHATS_NEW_FILE: &str = "whats_new.md";

/// Help files of one language; a locale without help falls back to English.
fn lang_files(lang: Lang) -> &'static [(&'static str, &'static str)] {
    let find = |code: &str| HELP_ALL.iter().find(|e| e.0 == code).map(|e| e.1);
    find(lang.code()).or_else(|| find("en")).unwrap_or(&[])
}

/// Sections required in every topic (labels — `help.sec.<name>`).
pub const REQUIRED: [&str; 3] = ["steps", "keys", "tips"];

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Bold(String),
    /// `Ctrl+Z` — a key or code.
    Key(String),
    /// `[[cmd:id]]` — a button that runs a command.
    Cmd(String),
    /// `[[topic:id]]` — link to a topic.
    Topic(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Heading `## x`: reserved names (`steps`…) are translated on display.
    Heading(String),
    Para(Vec<Inline>),
    Step(u32, Vec<Inline>),
    Bullet(Vec<Inline>),
}

#[derive(Debug, Clone, Default)]
pub struct Topic {
    pub id: String,
    pub title: String,
    pub tags: Vec<String>,
    /// Command ids, `sel:<category>`, `view:<view>` — F1 opens this topic for them.
    pub context: Vec<String>,
    pub commands: Vec<String>,
    pub see: Vec<String>,
    pub blocks: Vec<Block>,
    /// Body text lower-cased for search.
    body: String,
}

impl Topic {
    /// Blocks of section `## name` up to the next heading.
    pub fn section(&self, name: &str) -> Vec<&Block> {
        let mut on = false;
        let mut out = vec![];
        for b in &self.blocks {
            match b {
                Block::Heading(h) => on = h == name,
                _ if on => out.push(b),
                _ => {}
            }
        }
        out
    }
    pub fn has_section(&self, name: &str) -> bool {
        self.blocks
            .iter()
            .any(|b| matches!(b, Block::Heading(h) if h == name))
    }
    /// Intro text: paragraphs before the first heading.
    pub fn intro(&self) -> impl Iterator<Item = &Vec<Inline>> {
        self.blocks
            .iter()
            .take_while(|b| !matches!(b, Block::Heading(_)))
            .filter_map(|b| match b {
                Block::Para(p) => Some(p),
                _ => None,
            })
    }
    /// All links in the body: (commands, topics).
    pub fn links(&self) -> (Vec<&str>, Vec<&str>) {
        let (mut c, mut t) = (vec![], vec![]);
        for b in &self.blocks {
            let spans = match b {
                Block::Para(s) | Block::Step(_, s) | Block::Bullet(s) => s,
                Block::Heading(_) => continue,
            };
            for s in spans {
                match s {
                    Inline::Cmd(x) => c.push(x.as_str()),
                    Inline::Topic(x) => t.push(x.as_str()),
                    _ => {}
                }
            }
        }
        (c, t)
    }
}

/// Body line: `**bold**`, `` `key` ``, `[[cmd:id]]`, `[[topic:id]]`.
pub fn inline(s: &str) -> Vec<Inline> {
    let mut out = vec![];
    let mut text = String::new();
    let mut rest = s;
    let flush = |text: &mut String, out: &mut Vec<Inline>| {
        if !text.is_empty() {
            out.push(Inline::Text(std::mem::take(text)));
        }
    };
    while let Some(c) = rest.chars().next() {
        let tail = &rest[c.len_utf8()..];
        if let Some(r) = rest.strip_prefix("[[") {
            if let Some(end) = r.find("]]") {
                let link = &r[..end];
                let span = if let Some(id) = link.strip_prefix("cmd:") {
                    Some(Inline::Cmd(id.trim().to_string()))
                } else {
                    link.strip_prefix("topic:")
                        .map(|id| Inline::Topic(id.trim().to_string()))
                };
                if let Some(span) = span {
                    flush(&mut text, &mut out);
                    out.push(span);
                    rest = &r[end + 2..];
                    continue;
                }
            }
        } else if let Some(r) = rest.strip_prefix("**") {
            if let Some(end) = r.find("**") {
                flush(&mut text, &mut out);
                out.push(Inline::Bold(r[..end].to_string()));
                rest = &r[end + 2..];
                continue;
            }
        } else if c == '`' {
            if let Some(end) = tail.find('`') {
                flush(&mut text, &mut out);
                out.push(Inline::Key(tail[..end].to_string()));
                rest = &tail[end + 1..];
                continue;
            }
        }
        text.push(c);
        rest = tail;
    }
    flush(&mut text, &mut out);
    out
}

fn plain(spans: &[Inline]) -> String {
    spans
        .iter()
        .map(|s| match s {
            Inline::Text(t) | Inline::Bold(t) | Inline::Key(t) => t.as_str(),
            Inline::Cmd(_) | Inline::Topic(_) => "",
        })
        .collect()
}

/// List after `@x`: space- or comma-separated (`@tags` — comma only: tags may be multi-word).
fn list(v: &str, comma_only: bool) -> Vec<String> {
    v.split(|c: char| c == ',' || (!comma_only && c.is_whitespace()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Parses a topic; `id` is the file name without extension.
pub fn parse(id: &str, src: &str) -> Result<Topic, String> {
    let mut lines = src.lines().map(|l| l.trim_end_matches('\r'));
    let first = lines.next().unwrap_or("");
    let title = first
        .strip_prefix("# ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("{id}: first line must be \"# Title\""))?;
    let mut t = Topic {
        id: id.to_string(),
        title: title.to_string(),
        ..Default::default()
    };
    let mut para: Vec<&str> = vec![];
    let end_para = |para: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            blocks.push(Block::Para(inline(&para.join(" "))));
            para.clear();
        }
    };
    let mut meta = true;
    for l in lines {
        let s = l.trim();
        if meta {
            if let Some(m) = s.strip_prefix('@') {
                let (k, v) = m.split_once(char::is_whitespace).unwrap_or((m, ""));
                match k {
                    "tags" => t.tags = list(v, true),
                    "context" => t.context = list(v, false),
                    "commands" => t.commands = list(v, false),
                    "see" => t.see = list(v, false),
                    _ => return Err(format!("{id}: unknown line @{k}")),
                }
                continue;
            }
            meta = false;
        }
        if s.is_empty() {
            end_para(&mut para, &mut t.blocks);
        } else if let Some(h) = s.strip_prefix("## ") {
            end_para(&mut para, &mut t.blocks);
            t.blocks.push(Block::Heading(h.trim().to_string()));
        } else if let Some(b) = s.strip_prefix("- ").or_else(|| s.strip_prefix("* ")) {
            end_para(&mut para, &mut t.blocks);
            t.blocks.push(Block::Bullet(inline(b.trim())));
        } else if let Some((n, rest)) = step(s) {
            end_para(&mut para, &mut t.blocks);
            t.blocks.push(Block::Step(n, inline(rest)));
        } else if s.starts_with('#') {
            return Err(format!("{id}: only \"## \" headings are allowed: {s}"));
        } else {
            // continuation of a list item — same list line
            match (para.is_empty(), t.blocks.last_mut(), l.starts_with("  ")) {
                (true, Some(Block::Step(_, sp) | Block::Bullet(sp)), true) => {
                    sp.push(Inline::Text(" ".into()));
                    sp.extend(inline(s));
                }
                _ => para.push(s),
            }
        }
    }
    end_para(&mut para, &mut t.blocks);
    let mut body = t.title.to_lowercase();
    for b in &t.blocks {
        body.push('\n');
        match b {
            Block::Heading(h) => body.push_str(h),
            Block::Para(s) | Block::Step(_, s) | Block::Bullet(s) => body.push_str(&plain(s)),
        }
    }
    t.body = body.to_lowercase();
    Ok(t)
}

/// `12. text` → (12, "text").
fn step(s: &str) -> Option<(u32, &str)> {
    let dot = s.find(". ")?;
    let n = s[..dot].parse().ok()?;
    Some((n, s[dot + 2..].trim()))
}

pub struct Library {
    pub lang: Lang,
    pub topics: Vec<Topic>,
    /// Parse errors (topic skipped); tests require an empty list.
    pub errors: Vec<String>,
}

impl Library {
    pub fn from_files(lang: Lang, files: &[(&str, &str)]) -> Self {
        let mut topics = vec![];
        let mut errors = vec![];
        for (name, src) in files {
            let id = name.strip_suffix(".md").unwrap_or(name);
            match parse(id, src) {
                Ok(t) => topics.push(t),
                Err(e) => errors.push(e),
            }
        }
        topics.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        Self {
            lang,
            topics,
            errors,
        }
    }
    pub fn builtin(lang: Lang) -> Self {
        Self::from_files(
            lang,
            &lang_files(lang)
                .iter()
                .filter(|f| f.0 != WHATS_NEW_FILE)
                .copied()
                .collect::<Vec<_>>(),
        )
    }
    pub fn get(&self, id: &str) -> Option<&Topic> {
        self.topics.iter().find(|t| t.id == id)
    }
    /// Topic for a context key (command id, `sel:wall`, `view:3d`).
    pub fn for_context(&self, key: &str) -> Option<&Topic> {
        self.topics
            .iter()
            .find(|t| t.context.iter().any(|c| c == key))
    }
    /// First topic found for the keys in priority order (tool → selection → view).
    pub fn for_keys<'a>(&self, keys: impl IntoIterator<Item = &'a str>) -> Option<&Topic> {
        keys.into_iter().find_map(|k| self.for_context(k))
    }
    /// Topics that mention the command (via a button or link).
    pub fn mentioning(&self, cmd: &str) -> Vec<&Topic> {
        self.topics
            .iter()
            .filter(|t| t.commands.iter().any(|c| c == cmd) || t.links().0.contains(&cmd))
            .collect()
    }
    /// Search: title and tags fuzzy, body by substring of every query word; best first.
    pub fn search(&self, query: &str) -> Vec<&Topic> {
        let q = query.trim();
        if q.is_empty() {
            return self.topics.iter().collect();
        }
        let alt = crate::fuzzy::other_layout(q);
        let words = |q: &str| -> Vec<String> {
            q.to_lowercase()
                .split_whitespace()
                .map(String::from)
                .collect()
        };
        let (w1, w2) = (words(q), alt.as_deref().map(words).unwrap_or_default());
        let mut v: Vec<(i32, &Topic)> = self
            .topics
            .iter()
            .filter_map(|t| {
                let mut fields: Vec<&str> = vec![t.title.as_str(), t.id.as_str()];
                fields.extend(t.tags.iter().map(String::as_str));
                let head = crate::fuzzy::best(q, &fields).filter(|s| *s >= 300);
                let body_hit =
                    |w: &[String]| !w.is_empty() && w.iter().all(|x| t.body.contains(x.as_str()));
                let body = if body_hit(&w1) {
                    Some(200)
                } else if body_hit(&w2) {
                    Some(150)
                } else {
                    None
                };
                head.max(body).map(|s| (s, t))
            })
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.title.cmp(&b.1.title)));
        v.into_iter().map(|x| x.1).collect()
    }
}

/// "What's new" parsed the same way (tags and sections are optional).
pub fn whats_new(lang: Lang) -> Topic {
    let src = lang_files(lang).iter().find(|f| f.0 == WHATS_NEW_FILE).map_or("", |f| f.1);
    parse("whats_new", src).unwrap_or_default()
}

/// "What's new" version — the first `## ` heading; shown once per version.
pub fn whats_new_version() -> String {
    whats_new(Lang::En)
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Heading(h) => Some(h.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Registry;
    use std::collections::BTreeSet;

    /// Commands the help may mention: the registry.
    fn known(reg: &Registry, id: &str) -> bool {
        reg.get(id).is_some()
    }

    #[test]
    fn inline_spans() {
        use crate::test_fx::fx;
        let (press, or, see, bold) = (fx("help_press"), fx("help_or"), fx("help_see"), fx("help_bold"));
        let v = inline(&format!(
            "{press} `Ctrl+Z` {or} [[cmd:edit.undo]], {see} [[topic:undo_history]] **{bold}**"
        ));
        assert_eq!(
            v,
            vec![
                Inline::Text(format!("{press} ")),
                Inline::Key("Ctrl+Z".into()),
                Inline::Text(format!(" {or} ")),
                Inline::Cmd("edit.undo".into()),
                Inline::Text(format!(", {see} ")),
                Inline::Topic("undo_history".into()),
                Inline::Text(" ".into()),
                Inline::Bold(bold.into()),
            ]
        );
        // unclosed markup is plain text
        assert_eq!(
            inline("a ` b ** [[x"),
            vec![Inline::Text("a ` b ** [[x".into())]
        );
        assert_eq!(
            inline("[[foo:bar]]"),
            vec![Inline::Text("[[foo:bar]]".into())]
        );
    }

    #[test]
    fn parse_topic() {
        use crate::test_fx::fx;
        let src = include_str!("../tests/data/help_topic_ru.md").replace("\r\n", "\n");
        let t = parse("walls", &src).unwrap();
        assert_eq!(t.title, fx("help_title"));
        assert_eq!(t.tags, [fx("help_tag1"), "wall", fx("help_tag3")]);
        assert_eq!(t.context, ["arch.wall", "sel:wall"]);
        assert_eq!(t.intro().count(), 1);
        assert_eq!(plain(t.intro().next().unwrap()), fx("help_intro"));
        let steps = t.section("steps");
        assert_eq!(steps.len(), 2);
        assert!(matches!(steps[0], Block::Step(1, s) if plain(s) == fx("help_step")));
        assert_eq!(t.section("keys").len(), 1);
        assert!(parse("x", fx("help_notitle")).is_err());
        assert!(parse("x", "# T\n@oops 1\n").is_err());
        assert!(parse("x", &format!("# T\n\n### {}\n", fx("help_small"))).is_err());
    }

    #[test]
    fn topic_files_parse_and_match_across_languages() {
        let (ru, en) = (Library::builtin(Lang::Ru), Library::builtin(Lang::En));
        assert_eq!(ru.errors, Vec::<String>::new());
        assert_eq!(en.errors, Vec::<String>::new());
        assert!(ru.topics.len() >= 40, "topics {}", ru.topics.len());
        let ids = |l: &Library| {
            l.topics
                .iter()
                .map(|t| t.id.clone())
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(ids(&ru), ids(&en));
        for t in &ru.topics {
            let e = en.get(&t.id).unwrap();
            assert_eq!(t.context, e.context, "{}: @context", t.id);
            assert_eq!(t.commands, e.commands, "{}: @commands", t.id);
            assert_eq!(t.see, e.see, "{}: @see", t.id);
            for l in [t, e] {
                for s in REQUIRED {
                    assert!(l.has_section(s), "{}: missing section {s}", l.id);
                }
                assert!(l.intro().count() > 0, "{}: no intro", l.id);
                assert!(
                    l.section("steps")
                        .iter()
                        .filter(|b| matches!(b, Block::Step(..)))
                        .count()
                        >= 2,
                    "{}: too few steps",
                    l.id
                );
            }
        }
    }

    #[test]
    fn every_help_link_resolves() {
        let reg = Registry::builtin();
        for lang in [Lang::Ru, Lang::En] {
            let lib = Library::builtin(lang);
            let mut ctx = BTreeSet::new();
            for t in &lib.topics {
                let (cmds, topics) = t.links();
                for c in t.commands.iter().map(String::as_str).chain(cmds) {
                    assert!(known(&reg, c), "{lang:?} {}: unknown command {c}", t.id);
                }
                for x in t.see.iter().map(String::as_str).chain(topics) {
                    assert!(lib.get(x).is_some(), "{lang:?} {}: unknown topic {x}", t.id);
                }
                for k in &t.context {
                    let ok = match k.split_once(':') {
                        Some(("sel", c)) => [
                            "wall", "opening", "curtain", "floor", "room", "mass", "mep", "comp",
                        ]
                        .contains(&c),
                        Some(("view", v)) => ["3d", "plan", "section", "sheet"].contains(&v),
                        Some(_) => false,
                        None => known(&reg, k),
                    };
                    assert!(ok, "{lang:?} {}: context {k}", t.id);
                    assert!(ctx.insert(k.clone()), "{lang:?}: context {k} in two topics");
                }
            }
        }
        let wn = whats_new(Lang::Ru);
        for c in wn.links().0 {
            assert!(known(&reg, c), "whats_new: unknown command {c}");
        }
        assert!(!whats_new_version().is_empty());
        assert!(!whats_new(Lang::En).title.is_empty());
    }

    #[test]
    fn context_and_search() {
        let lib = Library::builtin(Lang::Ru);
        assert_eq!(
            lib.for_context("arch.wall").map(|t| t.id.as_str()),
            Some("walls")
        );
        assert_eq!(
            lib.for_keys(["nope", "sel:wall"]).map(|t| t.id.as_str()),
            Some("walls")
        );
        assert_eq!(
            lib.for_context("app.help").map(|t| t.id.as_str()),
            Some("interface")
        );
        let s = lib.search(crate::test_fx::fx("wall_pre"));
        assert!(
            s.iter().take(3).any(|t| t.id == "walls"),
            "{:?}",
            s.iter().map(|t| &t.id).collect::<Vec<_>>()
        );
        // query in the other layout: "cntyf" is the Russian word for wall typed on Latin keys
        assert!(!lib.search("cntyf").is_empty());
        assert_eq!(lib.search("").len(), lib.topics.len());
        assert!(lib.search("qqqzzzxxx").is_empty());
        assert!(!lib.mentioning("arch.wall").is_empty());
        let en = Library::builtin(Lang::En);
        assert!(en.search("roof").iter().take(3).any(|t| t.id == "roofs"));
    }
}
