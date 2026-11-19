// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! SQL text that comes from a file (the op log's inverses, a staged copy's schema) is the file
//! author's choice: it runs only as one statement of an allowed kind. `ATTACH` or `VACUUM INTO`
//! after a `;` would write any file on disk.

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    /// A bare word or number, upper-cased.
    Word(String),
    /// A string literal or a quoted identifier, without its quotes.
    Quoted(String),
    Semi,
    Punct(char),
}

/// Tokens of `sql` as SQLite splits them, comments dropped; None when a literal, a quoted name
/// or a comment is left open.
pub(crate) fn tokens(sql: &str) -> Option<Vec<Tok>> {
    let c: Vec<char> = sql.chars().collect();
    let word = |x: char| x.is_ascii_alphanumeric() || x == '_' || x == '$' || !x.is_ascii();
    let mut out = vec![];
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0c') {
            i += 1;
        } else if ch == '-' && c.get(i + 1) == Some(&'-') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            let end = (i + 2..c.len().saturating_sub(1)).find(|&k| c[k] == '*' && c[k + 1] == '/')?;
            i = end + 2;
        } else if let Some(close) = match ch {
            '\'' | '"' | '`' => Some(ch),
            '[' => Some(']'),
            _ => None,
        } {
            let mut s = String::new();
            i += 1;
            loop {
                let x = *c.get(i)?;
                i += 1;
                if x == close {
                    // a doubled quote is the quote itself; `[...]` has no escape
                    if close != ']' && c.get(i) == Some(&close) {
                        s.push(close);
                        i += 1;
                        continue;
                    }
                    break;
                }
                s.push(x);
            }
            out.push(Tok::Quoted(s));
        } else if word(ch) {
            let st = i;
            while i < c.len() && word(c[i]) {
                i += 1;
            }
            out.push(Tok::Word(c[st..i].iter().collect::<String>().to_ascii_uppercase()));
        } else {
            out.push(if ch == ';' { Tok::Semi } else { Tok::Punct(ch) });
            i += 1;
        }
    }
    Some(out)
}

fn is_word(t: Option<&Tok>, w: &str) -> bool {
    matches!(t, Some(Tok::Word(x)) if x == w)
}

/// One row edit (INSERT, REPLACE, UPDATE, DELETE), at most a `;` at its end.
pub(crate) fn is_row_edit(sql: &str) -> bool {
    let Some(mut t) = tokens(sql) else {
        return false;
    };
    while t.last() == Some(&Tok::Semi) {
        t.pop();
    }
    let kind = ["INSERT", "UPDATE", "DELETE", "REPLACE"].iter().any(|w| is_word(t.first(), w));
    let barred = ["ATTACH", "DETACH", "VACUUM", "PRAGMA"];
    kind && t.len() > 1
        && !t.iter().any(|x| *x == Tok::Semi || barred.iter().any(|w| is_word(Some(x), w)))
}

/// A stored `CREATE TABLE table(...)` or `CREATE [UNIQUE] INDEX name ON table(...)`, unqualified
/// (it lands in the schema it runs in) and nothing after it: no trigger, view or second statement.
pub(crate) fn is_plain_ddl(sql: &str, table: &str) -> bool {
    let Some(t) = tokens(sql) else {
        return false;
    };
    if t.iter().any(|x| *x == Tok::Semi || is_word(Some(x), "SELECT")) {
        return false;
    }
    let name_is = |x: Option<&Tok>, n: &str| match x {
        Some(Tok::Word(w) | Tok::Quoted(w)) => w.eq_ignore_ascii_case(n),
        _ => false,
    };
    let any_name = |x: Option<&Tok>| matches!(x, Some(Tok::Word(_) | Tok::Quoted(_)));
    let dot = |x: Option<&Tok>| x == Some(&Tok::Punct('.'));
    let open = |x: Option<&Tok>| x == Some(&Tok::Punct('('));
    if !is_word(t.first(), "CREATE") {
        return false;
    }
    let mut i = 1;
    let index = if is_word(t.get(i), "TABLE") {
        false
    } else {
        if is_word(t.get(i), "UNIQUE") {
            i += 1;
        }
        if !is_word(t.get(i), "INDEX") {
            return false;
        }
        true
    };
    i += 1;
    if is_word(t.get(i), "IF") {
        if !(is_word(t.get(i + 1), "NOT") && is_word(t.get(i + 2), "EXISTS")) {
            return false;
        }
        i += 3;
    }
    if dot(t.get(i + 1)) {
        return false;
    }
    if index {
        if !any_name(t.get(i)) || !is_word(t.get(i + 1), "ON") {
            return false;
        }
        i += 2;
    }
    name_is(t.get(i), table) && !dot(t.get(i + 1)) && open(t.get(i + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sec_row_edits_only_one_statement() {
        for ok in [
            "DELETE FROM wall WHERE id=?1",
            "  insert or replace into prop(rowid, kind) VALUES (?1,?2);",
            "UPDATE prop SET v='a;b' WHERE id=?1",
            "UPDATE prop SET v='x'';ATTACH ''y'' AS z' WHERE id=?1",
            "/* note; */ DELETE FROM wall -- tail; here\n WHERE id=?1",
            "UPDATE \"prop\" SET [v]=?2 WHERE `id`=?1",
        ] {
            assert!(is_row_edit(ok), "{ok}");
        }
        for bad in [
            "AtTaCh DATABASE 'x' AS z",
            "DeLeTe FROM wall; AtTaCh 'x' AS z",
            "DELETE FROM wall WHERE v='a'; ATTACH 'x' AS z",
            "DELETE FROM wall WHERE v=\"a\";VACUUM INTO 'x'",
            "WITH g AS (SELECT 1) DELETE FROM wall",
            "UPDATE prop SET v='open",
            "DELETE FROM wall /* open",
            "UPDATE prop SET v=1 WHERE id IN (SELECT 1); PRAGMA writable_schema=ON",
            "DELETE ; ;",
            ";",
            "",
        ] {
            assert!(!is_row_edit(bad), "{bad}");
        }
    }

    #[test]
    fn sec_stored_ddl_is_one_create_of_its_table() {
        for ok in [
            "CREATE TABLE ad4_extra(id INTEGER PRIMARY KEY, v TEXT NOT NULL)",
            "CREATE TABLE \"ad4_extra\" (v TEXT DEFAULT 'a;b' CHECK (v <> ';'))",
            "CREATE TABLE IF NOT EXISTS AD4_EXTRA(v)",
            "CREATE INDEX ad4_extra_v ON ad4_extra(v)",
            "CREATE UNIQUE INDEX IF NOT EXISTS u ON ad4_extra(v) WHERE v IS NOT NULL",
        ] {
            assert!(is_plain_ddl(ok, "ad4_extra"), "{ok}");
        }
        for bad in [
            "CREATE TABLE ad4_extra(v); ATTACH DATABASE 'x' AS z",
            "CREATE TABLE ad4_extra(v);AtTaCh 'x' AS z",
            "CREATE TABLE ad4_extra(v TEXT DEFAULT 'a');",
            "CREATE TABLE other(v)",
            "CREATE TABLE stg.ad4_extra(v)",
            "CREATE TABLE z.ad4_extra(v)",
            "CREATE TEMP TABLE ad4_extra(v)",
            "CREATE VIRTUAL TABLE ad4_extra USING fts5(v)",
            "CREATE TABLE ad4_extra AS SELECT 1",
            "CREATE TRIGGER t AFTER INSERT ON ad4_extra BEGIN DELETE FROM wall; END",
            "CREATE VIEW ad4_extra AS SELECT 1",
            "CREATE INDEX z.i ON ad4_extra(v)",
            "CREATE INDEX i ON other(v)",
            "CREATE TABLE ad4_extra(v DEFAULT 'open",
            "WITH x AS (SELECT 1) DELETE FROM ad4_extra",
            "",
        ] {
            assert!(!is_plain_ddl(bad, "ad4_extra"), "{bad}");
        }
    }
}
