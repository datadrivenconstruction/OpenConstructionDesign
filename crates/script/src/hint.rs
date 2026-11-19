// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Script errors as line, column, message and a hint the assistant can act on.

use crate::{ErrorKind, LimitKind, ScriptError, STD_NAMES};

pub(crate) const PARAMS: &str =
    "pass params as {name: value}; lengths as text with a unit like \"12 m\"";

pub(crate) fn limit(k: LimitKind) -> String {
    match k {
        LimitKind::Cpu | LimitKind::Ops => {
            "make loops shorter, use pattern() and components, or check for a loop that never ends"
        }
        LimitKind::Memory => "keep fewer values in tables; build parts as you go",
        LimitKind::Elements => {
            "fewer parts (coarser division), or ask the person to allow many elements"
        }
    }
    .to_string()
}

/// Innermost message and the innermost traceback that names a script line.
fn flatten(e: &mlua::Error) -> (String, String) {
    match e {
        mlua::Error::CallbackError { traceback, cause } => {
            let (m, t) = flatten(cause);
            let t = if line_in(&t).is_some() {
                t
            } else {
                traceback.clone()
            };
            (m, t)
        }
        mlua::Error::WithContext { cause, .. } => flatten(cause),
        mlua::Error::BadArgument { to, pos, cause, .. } => {
            let (m, t) = flatten(cause);
            let to = to.clone().unwrap_or_default();
            (format!("bad argument #{pos} to '{to}': {m}"), t)
        }
        mlua::Error::RuntimeError(m) => (m.clone(), String::new()),
        mlua::Error::SyntaxError { message, .. } => (message.clone(), String::new()),
        other => (other.to_string(), String::new()),
    }
}

fn line_in(s: &str) -> Option<u32> {
    let i = s.find("script:")?;
    let rest = &s[i + 7..];
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok().filter(|_| end > 0)
}

fn clean(m: &str) -> String {
    let m = m.split("\nstack traceback").next().unwrap_or(m);
    let m = match (m.find("script:"), line_in(m)) {
        (Some(i), Some(l)) => m[i + 7 + l.to_string().len()..]
            .trim_start_matches(':')
            .trim(),
        _ => m.trim(),
    };
    m.replace("Lua ", "").replace("runtime error: ", "")
}

fn quoted(m: &str) -> Option<&str> {
    let a = m.find('\'')?;
    let b = m[a + 1..].find('\'')?;
    Some(&m[a + 1..a + 1 + b])
}

/// 1-based column of the most telling token of the message on that line.
fn column(src: &str, line: u32, msg: &str) -> u32 {
    let text = src
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .unwrap_or("");
    let at = |needle: &str| -> Option<u32> {
        if needle.is_empty() {
            return None;
        }
        text.find(needle)
            .map(|i| text[..i].chars().count() as u32 + 1)
    };
    if msg.contains("near <eof>") || msg.contains("near '<eof>'") {
        return text.chars().count() as u32 + 1;
    }
    let mut cands: Vec<String> = vec![];
    if let Some(q) = quoted(msg) {
        cands.push(q.to_string());
    }
    if let Some((head, _)) = msg.split_once(':') {
        let f = head.split_whitespace().next().unwrap_or("");
        if !f.is_empty()
            && f.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            cands.push(format!("{f}("));
            cands.push(f.to_string());
        }
    }
    cands
        .iter()
        .find_map(|c| at(c))
        .unwrap_or_else(|| text.chars().take_while(|c| c.is_whitespace()).count() as u32 + 1)
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != *cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

fn nearest(name: &str) -> Option<&'static str> {
    STD_NAMES
        .iter()
        .map(|n| (distance(name, n), *n))
        .filter(|(d, n)| *d <= 2.max(n.len() / 3))
        .min()
        .map(|x| x.1)
}

fn hint_for(kind: ErrorKind, msg: &str) -> String {
    if kind == ErrorKind::Syntax {
        return "every if/for/while/function needs its `end`; `then` after if, `do` after \
                for/while; strings in quotes; comments start with --"
            .into();
    }
    if kind == ErrorKind::Assert {
        return "an assert of the script failed: fix the geometry or the params".into();
    }
    if msg.contains("attempt to call a nil value") {
        let name = quoted(msg).unwrap_or("");
        return match nearest(name) {
            Some(n) => format!("`{name}` is not a function; did you mean `{n}`?"),
            None => format!(
                "`{name}` is not a function; available: {}",
                STD_NAMES.join(", ")
            ),
        };
    }
    if msg.contains("arithmetic on a nil") || msg.contains("index a nil") {
        return "a variable is nil: check its spelling and that it was assigned before use".into();
    }
    if msg.contains("unit") {
        return "lengths are numbers in metres or text with a unit: len(\"1200 mm\")".into();
    }
    if msg.contains("shorter than 5 mm") {
        return "parts under 5 mm are not real parts: merge them or make them longer".into();
    }
    if msg.starts_with("constrain") {
        return "move the part ends onto the node, or fix the value of the constraint".into();
    }
    if msg.contains("bad argument") || msg.contains("error converting") {
        return "check the arguments: points are vec3(x, y, z), lengths numbers in metres, \
                profiles from profile.*, forms from extrude/sweep/revolve/loft"
            .into();
    }
    String::new()
}

pub(crate) fn map_error(e: &mlua::Error, src: &str, last_line: u32) -> ScriptError {
    let (raw, tb) = flatten(e);
    let line = line_in(&raw)
        .or_else(|| line_in(&tb))
        .unwrap_or(last_line)
        .max(1);
    let message = clean(&raw);
    let kind = if matches!(e, mlua::Error::SyntaxError { .. }) {
        ErrorKind::Syntax
    } else if message.starts_with("assert:") {
        ErrorKind::Assert
    } else if message.starts_with("param") {
        ErrorKind::Param
    } else {
        ErrorKind::Runtime
    };
    let col = column(src, line, &message);
    let hint = hint_for(kind, &message);
    ScriptError {
        kind,
        line,
        col,
        message,
        hint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_name_and_columns() {
        assert_eq!(nearest("extrud"), Some("extrude"));
        assert_eq!(nearest("zzzzzzzz"), None);
        let src = "local a = 1\n  local f = extrude(p, a)\n";
        assert_eq!(column(src, 2, "bad argument #1 to 'extrude': x"), 13);
        assert_eq!(line_in("script:12: oops"), Some(12));
        assert_eq!(clean("script:12: oops\nstack traceback:\n..."), "oops");
    }
}
