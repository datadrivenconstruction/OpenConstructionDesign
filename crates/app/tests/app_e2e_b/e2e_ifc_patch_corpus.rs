// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! End-to-end: IFC patch through the edit paths on real files. For each IFC of `DDC_E2E_IFC`
//! (a file, a folder or a path list) and for the synthetic fixture: open the file, edit a text and
//! a length property (`imported_props::set_all`), move a wall, change a wall height, delete a
//! wall, write the patch to a temp copy and read the copy back. Edited values must read back, the
//! deleted GlobalId must be gone, statements outside the touched elements must stay byte for
//! byte, and every `#ref` must resolve. A failure is a product finding, not a test to relax;
//! doubts (candidates the patch cannot hold, zipped or unknown-schema files) are listed per file.

use crate::e2e_common;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use ddc_app::editor::{Editor, OpFn, Sel};
use ddc_app::imported_props;
use ddc_app::query;
use ddc_io::ifc::patch::{check_patched, patch_file, PatchError, DOUBT_SCHEMA, DOUBT_ZIP};
use ddc_io::ifc::{import_ifc_file, import_ifc_path};
use ddc_model::source::SqlValue as Sql;
use ddc_model::{Model, Target};
use e2e_common::temp_file;

/// Candidates tried per role before the file counts as not patchable for it.
const TRIES: usize = 6;
/// Wall coordinates, heights and lengths read back, metres.
const TOL: f64 = 1e-6;

#[derive(Default)]
struct Case {
    tag: String,
    /// Not failures: candidates the patch could not hold, roles without candidates, skips.
    doubts: Vec<String>,
    /// Failures of patch_corpus_props_move_delete.
    props: Vec<String>,
    /// Failures of patch_corpus_untouched_bytes_identical.
    bytes: Vec<String>,
    /// Failures of patch_corpus_refs_valid.
    refs: Vec<String>,
    skipped: Option<String>,
    /// Roles (delete, move, height, text, length) that were edited and patched.
    covered: usize,
}

// ---------------------------------------------------------------- STEP text

struct Stmt {
    range: (usize, usize),
    id: Option<u32>,
    ty: String,
    guid: Option<String>,
    refs: Vec<u32>,
}

fn find(b: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    if from > b.len() {
        return None;
    }
    b[from..]
        .windows(pat.len())
        .position(|w| w == pat)
        .map(|p| p + from)
}

fn skip_ws_comments(b: &[u8], mut i: usize) -> usize {
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b[i.min(b.len())..].starts_with(b"/*") {
            match find(b, i + 2, b"*/") {
                Some(e) => i = e + 2,
                None => return b.len(),
            }
        } else {
            return i;
        }
    }
}

/// `#n` outside strings and comments.
fn refs_of(b: &[u8]) -> Vec<u32> {
    let (mut v, mut i, mut in_s) = (vec![], 0, false);
    while i < b.len() {
        let c = b[i];
        if in_s {
            in_s = c != b'\'';
            i += 1;
        } else if c == b'\'' {
            in_s = true;
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = find(b, i + 2, b"*/").map_or(b.len(), |e| e + 2);
        } else if c == b'#' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if let Some(n) = std::str::from_utf8(&b[i + 1..j])
                .ok()
                .and_then(|t| t.parse().ok())
            {
                v.push(n);
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    v
}

fn parse_stmt(b: &[u8], range: (usize, usize)) -> Stmt {
    let c = &b[range.0..range.1];
    let mut s = Stmt {
        range,
        id: None,
        ty: String::new(),
        guid: None,
        refs: vec![],
    };
    let i = skip_ws_comments(c, 0);
    if c.get(i) != Some(&b'#') {
        return s;
    }
    let mut j = i + 1;
    while j < c.len() && c[j].is_ascii_digit() {
        j += 1;
    }
    let k = skip_ws_comments(c, j);
    if j == i + 1 || c.get(k) != Some(&b'=') {
        return s;
    }
    s.id = std::str::from_utf8(&c[i + 1..j])
        .ok()
        .and_then(|t| t.parse().ok());
    let t0 = skip_ws_comments(c, k + 1);
    let mut t1 = t0;
    while t1 < c.len() && (c[t1].is_ascii_alphanumeric() || c[t1] == b'_') {
        t1 += 1;
    }
    s.ty = String::from_utf8_lossy(&c[t0..t1]).to_uppercase();
    let u = skip_ws_comments(c, t1);
    if c.get(u) == Some(&b'(') && c.get(u + 1) == Some(&b'\'') {
        if let Some(e) = c[u + 2..].iter().position(|&x| x == b'\'') {
            let g = &c[u + 2..u + 2 + e];
            if g.len() == 22
                && g.iter()
                    .all(|x| x.is_ascii_alphanumeric() || *x == b'_' || *x == b'$')
            {
                s.guid = Some(String::from_utf8_lossy(g).into_owned());
            }
        }
    }
    s.refs = refs_of(&c[t1..]);
    s
}

/// Chunks of a STEP file split at `;` outside strings and comments, trimmed.
fn scan(b: &[u8]) -> Vec<Stmt> {
    let mut out = vec![];
    let push = |from: usize, to: usize, out: &mut Vec<Stmt>| {
        let (mut a, mut z) = (from, to);
        while a < z && b[a].is_ascii_whitespace() {
            a += 1;
        }
        while z > a && b[z - 1].is_ascii_whitespace() {
            z -= 1;
        }
        if a < z {
            out.push(parse_stmt(b, (a, z)));
        }
    };
    let (mut start, mut i, mut in_s) = (0usize, 0usize, false);
    while i < b.len() {
        let c = b[i];
        if in_s {
            in_s = c != b'\'';
        } else if c == b'\'' {
            in_s = true;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = find(b, i + 2, b"*/").map_or(b.len(), |e| e + 2);
            continue;
        } else if c == b';' {
            push(start, i, &mut out);
            start = i + 1;
        }
        i += 1;
    }
    push(start, b.len(), &mut out);
    out
}

fn bytes_of<'a>(b: &'a [u8], s: &Stmt) -> &'a [u8] {
    &b[s.range.0..s.range.1]
}

/// Class prefixes of what hangs on a relationship and goes with it.
const DESCEND: [&str; 7] = [
    "IFCPROPERTY",
    "IFCELEMENTQUANTITY",
    "IFCQUANTITY",
    "IFCMATERIAL",
    "IFCCOMPLEXPROPERTY",
    "IFCPHYSICAL",
    "IFCCLASSIFICATION",
];

/// Statements an edit of the `touched` elements may change: the elements, what they own
/// (placement, shape, property sets), the openings and fillings of a host, and every
/// relationship that names one of them.
fn allowed_ids(s: &[Stmt], touched: &[String]) -> HashSet<u32> {
    let by_id: HashMap<u32, &Stmt> = s.iter().filter_map(|x| Some((x.id?, x))).collect();
    let rooted: HashSet<u32> = s
        .iter()
        .filter(|x| x.guid.is_some())
        .filter_map(|x| x.id)
        .collect();
    let id_of_guid: HashMap<&str, u32> = s
        .iter()
        .filter_map(|x| Some((x.guid.as_deref()?, x.id?)))
        .collect();
    let mut a: HashSet<u32> = touched
        .iter()
        .filter_map(|g| id_of_guid.get(g.as_str()).copied())
        .collect();
    loop {
        let mut add = vec![];
        for r in s
            .iter()
            .filter(|r| r.ty == "IFCRELVOIDSELEMENT" || r.ty == "IFCRELFILLSELEMENT")
        {
            if r.refs.iter().any(|x| a.contains(x)) {
                add.extend(
                    r.refs
                        .iter()
                        .copied()
                        .filter(|x| rooted.contains(x) && !a.contains(x)),
                );
            }
        }
        if add.is_empty() {
            break;
        }
        a.extend(add);
    }
    let mut allowed: HashSet<u32> = HashSet::new();
    let mut stack: Vec<u32> = a.iter().copied().collect();
    for r in s
        .iter()
        .filter(|r| r.ty.starts_with("IFCREL") && r.refs.iter().any(|x| a.contains(x)))
    {
        if let Some(i) = r.id {
            allowed.insert(i);
        }
        for x in &r.refs {
            if by_id
                .get(x)
                .is_some_and(|t| DESCEND.iter().any(|p| t.ty.starts_with(p)))
            {
                stack.push(*x);
            }
        }
    }
    while let Some(i) = stack.pop() {
        if !allowed.insert(i) {
            continue;
        }
        if let Some(t) = by_id.get(&i) {
            stack.extend(t.refs.iter().copied());
        }
    }
    allowed
}

fn bare_lf(b: &[u8]) -> usize {
    (0..b.len())
        .filter(|&i| b[i] == b'\n' && (i == 0 || b[i - 1] != b'\r'))
        .count()
}

fn head(b: &[u8]) -> String {
    String::from_utf8_lossy(&b[..b.len().min(60)]).into_owned()
}

/// `'guid'` somewhere in the file.
fn has_quoted(b: &[u8], guid: &str) -> bool {
    let q = format!("'{guid}'");
    find(b, 0, q.as_bytes()).is_some()
}

/// Statements outside the touched elements stay byte for byte, in order; line ends stay.
fn check_bytes(src: &[u8], out: &[u8], touched: &[String], edited: bool) -> Vec<String> {
    let (s, o) = (scan(src), scan(out));
    let in_out: HashSet<&[u8]> = o.iter().map(|x| bytes_of(out, x)).collect();
    let in_src: HashSet<&[u8]> = s.iter().map(|x| bytes_of(src, x)).collect();
    let allowed = allowed_ids(&s, touched);
    let mut v = vec![];
    let mut bad: Vec<u32> = vec![];
    let mut lost = 0usize;
    for x in s.iter().filter(|x| !in_out.contains(bytes_of(src, x))) {
        lost += 1;
        match x.id {
            None => v.push(format!(
                "header or trailer chunk changed: {:?}",
                head(bytes_of(src, x))
            )),
            Some(id) if !allowed.contains(&id) => bad.push(id),
            _ => {}
        }
    }
    if !bad.is_empty() {
        v.push(format!(
            "{} statements outside the touched elements changed, first ids {:?}",
            bad.len(),
            &bad[..bad.len().min(8)]
        ));
    }
    let seq_s: Vec<&[u8]> = s
        .iter()
        .map(|x| bytes_of(src, x))
        .filter(|b| in_out.contains(b))
        .collect();
    let seq_o: Vec<&[u8]> = o
        .iter()
        .map(|x| bytes_of(out, x))
        .filter(|b| in_src.contains(b))
        .collect();
    if seq_s != seq_o {
        v.push("order of the untouched statements changed".into());
    }
    if edited && lost == 0 && o.len() == s.len() {
        v.push("edits made but no statement differs".into());
    }
    if (bare_lf(src) == 0) != (bare_lf(out) == 0) || src.contains(&b'\r') != out.contains(&b'\r')
    {
        v.push("line endings changed".into());
    }
    if !edited && src != out {
        v.push("no edits but the output differs from the source".into());
    }
    v
}

/// (ids defined twice, ids referenced and never defined).
fn id_faults(b: &[u8]) -> (HashSet<u32>, HashSet<u32>) {
    let s = scan(b);
    let (mut seen, mut dup) = (HashSet::new(), HashSet::new());
    for x in &s {
        if let Some(i) = x.id {
            if !seen.insert(i) {
                dup.insert(i);
            }
        }
    }
    let missing = s
        .iter()
        .flat_map(|x| x.refs.iter().copied())
        .filter(|r| !seen.contains(r))
        .collect();
    (dup, missing)
}

/// The output adds no repeated id and no dangling reference the source did not have.
fn check_refs(src: &[u8], out: &[u8]) -> Vec<String> {
    let ((sd, sm), (od, om)) = (id_faults(src), id_faults(out));
    let mut v = vec![];
    let new_dup: Vec<&u32> = od.difference(&sd).collect();
    if !new_dup.is_empty() {
        v.push(format!(
            "ids defined twice: {:?}",
            &new_dup[..new_dup.len().min(8)]
        ));
    }
    let new_missing: Vec<&u32> = om.difference(&sm).collect();
    if !new_missing.is_empty() {
        v.push(format!(
            "{} references without a statement, first {:?}",
            new_missing.len(),
            &new_missing[..new_missing.len().min(8)]
        ));
    }
    v
}

// ---------------------------------------------------------------- editing

enum Ap {
    /// Nothing was written; there is nothing to undo.
    Nothing,
    /// A step was written but is not what was asked for.
    Wrong(String),
    Done,
}

enum Probe {
    Ok,
    Skip(String),
    Bad(String),
}

/// The source and where the patch goes.
struct Cx<'a> {
    key: &'a str,
    src: &'a Path,
    out: &'a Path,
}

fn probe(ed: &Editor, cx: &Cx) -> Probe {
    match patch_file(&ed.model, cx.key, cx.src, cx.out, None) {
        Ok(p) => {
            if !p.will_not.is_empty() {
                return Probe::Bad(format!("the file cannot hold {:?}", p.will_not));
            }
            if let Some(e) = &p.expect {
                if let Err(w) = check_patched(cx.out, e, None) {
                    return Probe::Bad(format!("read-back: {w}"));
                }
            }
            Probe::Ok
        }
        Err(PatchError::Doubt { key: k, detail }) if k == DOUBT_ZIP || k == DOUBT_SCHEMA => {
            Probe::Skip(format!("{k}: {detail}"))
        }
        Err(e) => Probe::Bad(e.to_string()),
    }
}

/// Edits with the first candidate whose patch the file can hold; the others are undone.
fn role<C>(
    ed: &mut Editor,
    c: &mut Case,
    name: &str,
    cands: &[C],
    cx: &Cx,
    apply: &dyn Fn(&mut Editor, &C) -> Ap,
) -> Option<usize> {
    if c.skipped.is_some() {
        return None;
    }
    if cands.is_empty() {
        c.doubts.push(format!("{name}: no candidate, not covered"));
        return None;
    }
    let mut why = vec![];
    for (i, cand) in cands.iter().take(TRIES).enumerate() {
        match apply(ed, cand) {
            Ap::Nothing => {
                why.push(format!("candidate {i}: not applicable"));
                continue;
            }
            Ap::Wrong(w) => {
                why.push(format!("candidate {i}: {w}"));
                ed.undo();
                continue;
            }
            Ap::Done => {}
        }
        match probe(ed, cx) {
            Probe::Ok => {
                for w in why {
                    c.doubts.push(format!("{name} {w}"));
                }
                c.covered += 1;
                return Some(i);
            }
            Probe::Skip(s) => {
                c.skipped = Some(s);
                ed.undo();
                return None;
            }
            Probe::Bad(s) => {
                why.push(format!("candidate {i}: {s}"));
                ed.undo();
            }
        }
    }
    c.props.push(format!(
        "{name}: none of {} candidates could be patched: {}",
        cands.len().min(TRIES),
        why.join("; ")
    ));
    None
}

fn walls_of(m: &Model, key: &str) -> Vec<(i64, String)> {
    let Ok(mut st) = m.sql().prepare(
        "SELECT w.id, e.guid FROM wall w JOIN ext_ref e \
         ON e.kind='wall' AND e.ref=w.id AND e.source=?1 ORDER BY w.id",
    ) else {
        return vec![];
    };
    let rows = st.query_map([key], |r| Ok((r.get(0)?, r.get(1)?)));
    match rows {
        Ok(r) => r.filter_map(Result::ok).collect(),
        Err(_) => vec![],
    }
}

/// ax, ay, bx, by, height.
fn wall_row(m: &Model, id: i64) -> Option<[f64; 5]> {
    m.sql()
        .query_row(
            "SELECT ax,ay,bx,by,height FROM wall WHERE id=?1",
            [id],
            |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?]),
        )
        .ok()
}

struct PropC {
    table: &'static str,
    id: i64,
    key: String,
    unit: String,
    text: Option<String>,
    num: Option<f64>,
    guid: String,
}

fn table_of(k: &str) -> Option<&'static str> {
    ["wall", "slab", "opening", "room", "col", "beam"]
        .into_iter()
        .find(|t| *t == k)
}

fn prop_cands(m: &Model, key: &str, numeric: bool, used: &HashSet<i64>) -> Vec<PropC> {
    const DERIVED: [&str; 4] = ["IFC.", "Material.", "Classification.", "type."];
    let Ok(mut st) = m.sql().prepare(
        "SELECT p.kind,p.ref,p.key,p.value,p.unit,e.guid FROM prop p JOIN ext_ref e \
         ON e.kind=p.kind AND e.ref=p.ref AND e.source=?1 \
         WHERE p.kind IN ('wall','slab','opening','room','col','beam') AND p.key LIKE '%.%' \
         AND typeof(p.value)=?2 AND (?3=0 OR p.unit='m') \
         ORDER BY p.kind<>'wall', p.ref, p.key",
    ) else {
        return vec![];
    };
    let ty = if numeric { "real" } else { "text" };
    let Ok(rows) = st.query_map(rusqlite::params![key, ty, numeric as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Sql>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
        ))
    }) else {
        return vec![];
    };
    let mut out = vec![];
    for (kind, id, k, v, unit, guid) in rows.filter_map(Result::ok) {
        let Some(table) = table_of(&kind) else {
            continue;
        };
        if DERIVED.iter().any(|d| k.starts_with(d))
            || (table == "wall" && used.contains(&id))
            || m.element_by_guid(key, &guid).ok().flatten().is_none()
        {
            continue;
        }
        let (text, num) = match v {
            Sql::Text(t) => (Some(t), None),
            Sql::Real(f) => (None, Some(f)),
            _ => continue,
        };
        out.push(PropC {
            table,
            id,
            key: k,
            unit,
            text,
            num,
            guid,
        });
        if out.len() >= TRIES {
            break;
        }
    }
    out
}

fn prop_value(m: &Model, table: &str, id: i64, key: &str) -> Option<Sql> {
    m.sql()
        .query_row(
            "SELECT value FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
            rusqlite::params![table, id, key],
            |r| r.get::<_, Sql>(0),
        )
        .ok()
}

fn import_op(ed: &mut Editor, path: &Path) -> Result<(), String> {
    let p = path.to_path_buf();
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        import_ifc_path(m, &p)
            .map(|_| ())
            .map_err(|e| ddc_model::Error::Invalid(e.to_string()))
    })))
    .map_err(|e| e.to_string())
}

fn first_source(m: &Model) -> Option<String> {
    m.sources().ok()?.into_iter().next().map(|s| s.source)
}

fn new_text(p: &PropC) -> String {
    format!("{} tst21", p.text.clone().unwrap_or_default())
}

fn new_length(p: &PropC) -> f64 {
    p.num.unwrap_or_default() + 0.125
}

fn run(src: &Path, tag: &str) -> Case {
    let mut c = Case {
        tag: tag.into(),
        ..Default::default()
    };
    let mut ed = Editor::new_in_memory().unwrap();
    ed.model = Model::open(":memory:").unwrap();
    if let Err(e) = import_op(&mut ed, src) {
        c.props.push(format!("import failed: {e}"));
        return c;
    }
    let Some(key) = first_source(&ed.model) else {
        c.props.push("import left no source".into());
        return c;
    };
    let out = temp_file("tst21_out.ifc");
    let back = temp_file("tst21_back.ifc");
    let cx = Cx {
        key: &key,
        src,
        out: &out,
    };
    let mut touched: Vec<String> = vec![];
    let mut used: HashSet<i64> = HashSet::new();
    let walls = walls_of(&ed.model, &key);
    let (mut deleted, mut text, mut length) = (None, None, None);

    // delete: from the last wall
    let cands: Vec<(i64, String)> = walls.iter().rev().cloned().collect();
    let del = |ed: &mut Editor, w: &(i64, String)| {
        ed.selection = vec![Sel::Wall(w.0)];
        ed.delete_selection();
        if ed.pending_delete.is_some() {
            ed.confirm_delete();
        }
        if wall_row(&ed.model, w.0).is_none() {
            Ap::Done
        } else {
            Ap::Nothing
        }
    };
    if let Some(i) = role(&mut ed, &mut c, "delete", &cands, &cx, &del) {
        deleted = Some(cands[i].1.clone());
        touched.push(cands[i].1.clone());
        used.insert(cands[i].0);
    }

    // move: from the first wall
    let cands: Vec<(i64, String)> = walls
        .iter()
        .filter(|w| !used.contains(&w.0))
        .cloned()
        .collect();
    let mv = |ed: &mut Editor, w: &(i64, String)| {
        let id = w.0;
        let r = ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.move_elements(&[Target::Wall(id)], (1.0, 0.5))
        })));
        if r.is_ok() {
            Ap::Done
        } else {
            Ap::Nothing
        }
    };
    if let Some(i) = role(&mut ed, &mut c, "move", &cands, &cx, &mv) {
        touched.push(cands[i].1.clone());
        used.insert(cands[i].0);
    }

    // height: from the middle wall
    let mid = walls.len() / 2;
    let cands: Vec<(i64, String)> = walls[mid..]
        .iter()
        .chain(walls[..mid].iter())
        .filter(|w| !used.contains(&w.0))
        .cloned()
        .collect();
    let ht = |ed: &mut Editor, w: &(i64, String)| {
        let id = w.0;
        let Some(h0) = wall_row(&ed.model, id).map(|r| r[4]) else {
            return Ap::Nothing;
        };
        let Ok(q) = query::parse("-> height=height*1.1") else {
            return Ap::Nothing;
        };
        let Ok(edits) = query::assign_edits(ed, &[Sel::Wall(id)], &q.set) else {
            return Ap::Nothing;
        };
        if edits.is_empty() {
            return Ap::Nothing;
        }
        let r = ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.apply_edits(&edits))));
        if r.is_err() {
            return Ap::Nothing;
        }
        let h = wall_row(&ed.model, id).map_or(f64::NAN, |r| r[4]);
        if (h - h0 * 1.1).abs() < 1e-9 {
            Ap::Done
        } else {
            Ap::Wrong(format!("height {h} want {}", h0 * 1.1))
        }
    };
    if let Some(i) = role(&mut ed, &mut c, "height", &cands, &cx, &ht) {
        touched.push(cands[i].1.clone());
        used.insert(cands[i].0);
    }

    // text property
    let cands = prop_cands(&ed.model, &key, false, &used);
    let set_text = |ed: &mut Editor, p: &PropC| {
        let v = Sql::Text(new_text(p));
        match imported_props::set_all(ed, &[(p.table, p.id)], &p.key, v, &p.unit) {
            Ok(_) => Ap::Done,
            Err(_) => Ap::Nothing,
        }
    };
    if let Some(i) = role(&mut ed, &mut c, "text property", &cands, &cx, &set_text) {
        touched.push(cands[i].guid.clone());
        text = Some(i);
    }
    let text = text.map(|i| (cands[i].guid.clone(), cands[i].key.clone(), new_text(&cands[i])));

    // length property
    let cands = prop_cands(&ed.model, &key, true, &used);
    let set_len = |ed: &mut Editor, p: &PropC| {
        let v = Sql::Real(new_length(p));
        match imported_props::set_all(ed, &[(p.table, p.id)], &p.key, v, &p.unit) {
            Ok(_) => Ap::Done,
            Err(_) => Ap::Nothing,
        }
    };
    if let Some(i) = role(&mut ed, &mut c, "length property", &cands, &cx, &set_len) {
        touched.push(cands[i].guid.clone());
        length = Some(i);
    }
    let length =
        length.map(|i| (cands[i].guid.clone(), cands[i].key.clone(), new_length(&cands[i])));

    if let Some(s) = c.skipped.clone() {
        c.doubts.push(format!("skipped: {s}"));
        let _ = std::fs::remove_file(&out);
        return c;
    }

    // the finished patch
    let p = match patch_file(&ed.model, &key, src, &out, None) {
        Ok(p) => p,
        Err(PatchError::Doubt { key: k, detail }) if k == DOUBT_ZIP || k == DOUBT_SCHEMA => {
            c.doubts.push(format!("skipped: {k}: {detail}"));
            return c;
        }
        Err(e) => {
            c.props.push(format!("patch failed: {e}"));
            return c;
        }
    };
    if let Some(e) = &p.expect {
        if let Err(w) = check_patched(&out, e, None) {
            c.props.push(format!("patch read-back: {w}"));
        }
    }
    if !p.will_not.is_empty() {
        c.props
            .push(format!("the finished patch cannot hold {:?}", p.will_not));
    }
    let (Ok(sb), Ok(ob)) = (std::fs::read(src), std::fs::read(&out)) else {
        c.props.push("cannot read the source or the output".into());
        return c;
    };
    let edited = c.covered > 0;
    c.bytes.extend(check_bytes(&sb, &ob, &touched, edited));
    c.refs.extend(check_refs(&sb, &ob));

    // reopen the output
    if std::fs::copy(&out, &back).is_err() {
        c.props.push("cannot copy the output".into());
        return c;
    }
    let mut m2 = Model::open(":memory:").unwrap();
    if let Err(e) = import_ifc_file(&mut m2, &back) {
        c.props.push(format!("the output does not open: {e}"));
        return c;
    }
    let Some(key2) = first_source(&m2) else {
        c.props.push("the output left no source".into());
        return c;
    };
    let mut f: Vec<String> = vec![];
    for (id, guid) in &walls {
        let found = m2.element_by_guid(&key2, guid).ok().flatten();
        if deleted.as_deref() == Some(guid.as_str()) {
            if found.is_some() {
                f.push(format!("deleted wall {guid} is still in the output model"));
            }
            if has_quoted(&ob, guid) {
                f.push(format!("deleted GlobalId {guid} is still in the file"));
            }
            continue;
        }
        let (Some(now), Some((_, id2))) = (wall_row(&ed.model, *id), found) else {
            f.push(format!("wall {guid} is missing in the output"));
            continue;
        };
        match wall_row(&m2, id2) {
            Some(r) => {
                if (0..5).any(|k| (now[k] - r[k]).abs() > TOL) {
                    f.push(format!("wall {guid}: {now:?} became {r:?}"));
                }
            }
            None => f.push(format!("wall {guid} has no row in the output model")),
        }
    }
    let n_now = walls.len() - usize::from(deleted.is_some());
    let n_back = m2
        .sql()
        .query_row("SELECT count(*) FROM wall", [], |r| r.get::<_, i64>(0))
        .unwrap_or(-1);
    if n_back != n_now as i64 {
        f.push(format!("{n_back} walls in the output, {n_now} edited"));
    }
    if let Some((guid, k, new)) = &text {
        match m2.element_by_guid(&key2, guid).ok().flatten() {
            Some((t, i)) => match prop_value(&m2, &t, i, k) {
                Some(Sql::Text(x)) if &x == new => {}
                other => f.push(format!("{k} reads {other:?}, want {new:?}")),
            },
            None => f.push(format!("{k}: element {guid} is missing")),
        }
    }
    if let Some((guid, k, new)) = &length {
        match m2.element_by_guid(&key2, guid).ok().flatten() {
            Some((t, i)) => match prop_value(&m2, &t, i, k) {
                Some(Sql::Real(x)) if (x - new).abs() <= TOL * new.abs().max(1.0) => {}
                other => f.push(format!("{k} reads {other:?}, want {new}")),
            },
            None => f.push(format!("{k}: element {guid} is missing")),
        }
    }
    c.props.extend(f);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&back);
    c
}

// ---------------------------------------------------------------- inputs

/// 4 walls in millimetres, each with an own property set (text and length), one shared material
/// layer set, a comment, a class the importer skips and a statement over two lines.
fn fixture() -> String {
    let mut s = String::from(
        "ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');
FILE_NAME('f.ifc','2020-01-01T00:00:00',(''),(''),'','','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#2=IFCUNITASSIGNMENT((#1));
#3=IFCPROJECT('0000000000000000000001',#4,'P',$,$,$,$,(#7),#2);
#4=IFCOWNERHISTORY(#5,#6,$,.ADDED.,$,$,$,0);
#5=IFCPERSONANDORGANIZATION(#8,#9,$);
#6=IFCAPPLICATION(#9,'1','a','a');
#8=IFCPERSON($,$,'a',$,$,$,$,$);
#9=IFCORGANIZATION($,'o',$,$,$);
#10=IFCCARTESIANPOINT((0.,0.,0.));
#11=IFCAXIS2PLACEMENT3D(#10,$,$);
#7=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-5,#11,$);
#17=IFCGEOMETRICREPRESENTATIONSUBCONTEXT('Body','Model',*,*,*,*,#7,$,.MODEL_VIEW.,$);
#18=IFCGEOMETRICREPRESENTATIONSUBCONTEXT('Axis','Model',*,*,*,*,#7,$,.MODEL_VIEW.,$);
#12=IFCLOCALPLACEMENT($,#11);
#13=IFCCARTESIANPOINT((0.,0.,3000.));
#14=IFCAXIS2PLACEMENT3D(#13,$,$);
#15=IFCLOCALPLACEMENT(#12,#14);
#16=IFCBUILDINGSTOREY('0000000000000000000002',#4,'L1',$,$,#15,$,$,.ELEMENT.,3000.);
#21=IFCDIRECTION((0.,0.,1.));
#22=IFCDIRECTION((0.,1.,0.));
#34=IFCCARTESIANPOINT((2000.,0.));
#35=IFCAXIS2PLACEMENT2D(#34,$);
#37=IFCAXIS2PLACEMENT3D(#10,$,$);
#50=IFCMATERIAL('Brick');
#52=IFCMATERIALLAYER(#50,250.,$);
#54=IFCMATERIALLAYERSET((#52),'Ext 250');
#55=IFCMATERIALLAYERSETUSAGE(#54,.AXIS2.,.POSITIVE.,-125.);
/* four walls along +Y, 4000 long, 5000 apart */
",
    );
    let mut ids = vec![];
    for i in 0..4usize {
        let b = 100 + 20 * i;
        let x = 1000 + 5000 * i;
        let g = |k: usize| format!("{:022}", 1000 + i * 10 + k);
        s += &format!(
            "#{b}=IFCCARTESIANPOINT(({x}.,2000.,0.));
#{p1}=IFCAXIS2PLACEMENT3D(#{b},#21,#22);
#{p2}=IFCLOCALPLACEMENT(#15,#{p1});
#{p3}=IFCCARTESIANPOINT((0.,0.));
#{p4}=IFCCARTESIANPOINT((4000.,0.));
#{p5}=IFCPOLYLINE((#{p3},#{p4}));
#{p6}=IFCSHAPEREPRESENTATION(#18,'Axis','Curve2D',(#{p5}));
#{p7}=IFCRECTANGLEPROFILEDEF(.AREA.,$,#35,4000.,250.);
#{p8}=IFCEXTRUDEDAREASOLID(#{p7},#37,#21,2800.);
#{p9}=IFCSHAPEREPRESENTATION(#17,'Body','SweptSolid',(#{p8}));
#{p10}=IFCPRODUCTDEFINITIONSHAPE($,$,(#{p6},#{p9}));
#{p11}=IFCWALLSTANDARDCASE('{g0}',#4,'W{i}',$,$,#{p2},#{p10},$,$);
#{p12}=IFCPROPERTYSINGLEVALUE('Fire',$,IFCLABEL('EI 30'),$);
#{p13}=IFCPROPERTYSINGLEVALUE('Thick',$,IFCLENGTHMEASURE(250.),$);
#{p14}=IFCPROPERTYSET('{g1}',#4,'Own',$,(#{p12},#{p13}));
#{p15}=IFCRELDEFINESBYPROPERTIES('{g2}',#4,$,$,(#{p11}),#{p14});
",
            p1 = b + 1,
            p2 = b + 2,
            p3 = b + 3,
            p4 = b + 4,
            p5 = b + 5,
            p6 = b + 6,
            p7 = b + 7,
            p8 = b + 8,
            p9 = b + 9,
            p10 = b + 10,
            p11 = b + 11,
            p12 = b + 12,
            p13 = b + 13,
            p14 = b + 14,
            p15 = b + 15,
            g0 = g(0),
            g1 = g(1),
            g2 = g(2),
        );
        ids.push(format!("#{}", b + 11));
    }
    let list = ids.join(",");
    s += &format!(
        "#56=IFCRELASSOCIATESMATERIAL('0000000000000000000004',#4,$,$,({list}),#55);
#57=IFCRELCONTAINEDINSPATIALSTRUCTURE('0000000000000000000005',#4,$,$,({list}),#16);
#900=IFCANNOTATION('0000000000000000000900',$,'kept',$,$,$,$);
#901=IFCPROPERTYSINGLEVALUE('Spread',
   'over two lines',IFCLABEL('x'),$);
ENDSEC;
END-ISO-10303-21;
"
    );
    s
}

fn is_ifc(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ifc") || e.eq_ignore_ascii_case("ifczip"))
}

/// `DDC_E2E_IFC`: files, folders (their IFC files) or a path list.
fn corpus_files() -> Vec<PathBuf> {
    let Some(v) = std::env::var_os("DDC_E2E_IFC") else {
        return vec![];
    };
    let mut out = vec![];
    for p in std::env::split_paths(&v) {
        if p.is_dir() {
            let mut inside: Vec<PathBuf> = std::fs::read_dir(&p)
                .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
                .unwrap_or_default();
            inside.retain(|f| f.is_file() && is_ifc(f));
            inside.sort();
            out.extend(inside);
        } else if p.is_file() {
            out.push(p);
        } else {
            eprintln!("tst21: {} not found, skipped", p.display());
        }
    }
    out
}

fn cases() -> &'static Vec<Case> {
    static CASES: OnceLock<Vec<Case>> = OnceLock::new();
    CASES.get_or_init(|| {
        let synth = temp_file("tst21_synthetic.ifc");
        std::fs::write(&synth, fixture()).unwrap();
        let mut v = vec![run(&synth, "synthetic")];
        let _ = std::fs::remove_file(&synth);
        for f in corpus_files() {
            v.push(run(&f, &f.display().to_string()));
        }
        v
    })
}

fn verdict(what: &str, pick: fn(&Case) -> &Vec<String>) {
    let mut bad = vec![];
    for c in cases() {
        for d in &c.doubts {
            eprintln!("tst21 doubt: {}: {d}", c.tag);
        }
        for f in pick(c) {
            bad.push(format!("{}: {f}", c.tag));
        }
    }
    assert!(bad.is_empty(), "{what}:\n{}", bad.join("\n"));
}

#[test]
fn patch_corpus_props_move_delete() {
    verdict("edited values, move, delete", |c| &c.props);
}

#[test]
fn patch_corpus_untouched_bytes_identical() {
    verdict("untouched statements", |c| &c.bytes);
}

#[test]
fn patch_corpus_refs_valid() {
    verdict("references", |c| &c.refs);
}

#[test]
fn patch_corpus_synthetic_covers_every_edit() {
    let c = &cases()[0];
    assert!(c.skipped.is_none(), "{:?}", c.skipped);
    assert!(c.doubts.is_empty(), "{:?}", c.doubts);
    assert_eq!(c.covered, 5, "delete, move, height, text, length");
}
