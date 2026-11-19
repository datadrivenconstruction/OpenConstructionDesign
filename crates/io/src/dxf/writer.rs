//! DXF ASCII writer (LF line endings). Output is R2007-level structure (AC1021, UTF-8 text):
//! `$ACADVER`/`$HANDSEED`, handles (code 5) and owners (330) on tables, records, blocks and
//! entities, a BLOCK_RECORD for `*Model_Space`, `*Paper_Space` and every block, and the mandatory
//! default table records. Handles of modeled items are regenerated on every write; raw items
//! (unknown entities, unmodeled tables and sections) keep theirs and are never reused.
//! Newlines inside values cannot be represented in a group pair: they become a space
//! (MTEXT converts them to `\P` before this point).

use super::entities::{write_attdef, write_entity, Entity};
use super::{Block, DxfDocument, DxfError, Pair};
use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;

const VERSION: &str = "AC1021";
const MODEL: &str = "*Model_Space";
const PAPER: &str = "*Paper_Space";

pub(super) struct Out {
    buf: String,
    next: u64,
    /// Owner handle (330) of entities written now.
    pub owner: String,
    /// Below AC1021 text is code-page based: non-ASCII chars are written as `\U+XXXX`.
    ascii: bool,
}

/// Every non-ASCII char as `\U+XXXX`; chars above U+FFFF as a surrogate pair.
pub(super) fn escape_non_ascii(v: &str) -> String {
    let mut o = String::with_capacity(v.len() + 8);
    let mut b = [0u16; 2];
    for c in v.chars() {
        if c.is_ascii() {
            o.push(c);
        } else {
            for u in c.encode_utf16(&mut b) {
                let _ = write!(o, "\\U+{u:04X}");
            }
        }
    }
    o
}

impl Out {
    pub fn s(&mut self, code: i32, v: &str) {
        let v = if v.contains(['\n', '\r']) {
            v.replace(['\n', '\r'], " ")
        } else {
            v.to_string()
        };
        if self.ascii && !v.is_ascii() {
            let v = escape_non_ascii(&v);
            let _ = write!(self.buf, "{code}\n{v}\n");
        } else {
            let _ = write!(self.buf, "{code}\n{v}\n");
        }
    }
    pub fn i(&mut self, code: i32, v: i32) {
        let _ = write!(self.buf, "{code}\n{v}\n");
    }
    /// Shortest representation that parses back to the same f64.
    pub fn f(&mut self, code: i32, v: f64) {
        let _ = write!(self.buf, "{code}\n{v}\n");
    }
    pub fn extra(&mut self, pairs: &[Pair]) {
        for p in pairs {
            self.s(p.code, &p.value);
        }
    }
    /// Extras of modeled items: handle/owner-like pairs are regenerated, never copied.
    pub fn extra_m(&mut self, pairs: &[Pair]) {
        for p in pairs
            .iter()
            .filter(|p| !super::entities::is_structural(p.code))
        {
            self.s(p.code, &p.value);
        }
    }
    fn handle(&mut self) -> String {
        let h = format!("{:X}", self.next);
        self.next += 1;
        h
    }
    /// Starts an entity record: type, fresh handle, owner. Returns the handle.
    pub fn rec(&mut self, kind: &str) -> String {
        self.s(0, kind);
        let h = self.handle();
        self.s(5, &h);
        let o = self.owner.clone();
        self.s(330, &o);
        h
    }
    /// Raw record kept verbatim; a missing handle is added, the owner is pointed at the current one.
    pub fn unknown(&mut self, pairs: &[Pair]) {
        let Some(first) = pairs.first() else { return };
        self.s(first.code, &first.value);
        if !pairs.iter().any(|p| p.code == 5) {
            let h = self.handle();
            self.s(5, &h);
        }
        let owner = self.owner.clone();
        let mut in_group = false;
        let has_owner = pairs.iter().skip(1).any(|p| {
            if p.code == 102 {
                in_group = p.value.starts_with('{');
            }
            p.code == 330 && !in_group
        });
        if !has_owner {
            self.s(330, &owner);
        }
        in_group = false;
        for p in &pairs[1..] {
            if p.code == 102 {
                in_group = p.value.starts_with('{');
            }
            if p.code == 330 && !in_group {
                self.s(330, &owner);
            } else {
                self.s(p.code, &p.value);
            }
        }
    }
    fn section(&mut self, name: &str) {
        self.s(0, "SECTION");
        self.s(2, name);
    }
    fn end_section(&mut self) {
        self.s(0, "ENDSEC");
    }
    /// Opens a symbol table; returns its handle.
    fn table(&mut self, name: &str, count: usize) -> String {
        self.s(0, "TABLE");
        self.s(2, name);
        let h = self.handle();
        self.s(5, &h);
        self.s(330, "0");
        self.s(100, "AcDbSymbolTable");
        self.i(70, count as i32);
        h
    }
    fn record(&mut self, kind: &str, table: &str, sub: &str) {
        self.s(0, kind);
        let h = self.handle();
        self.s(if kind == "DIMSTYLE" { 105 } else { 5 }, &h);
        self.s(330, table);
        self.s(100, "AcDbSymbolTableRecord");
        self.s(100, sub);
    }
}

/// Handles carried by raw items (header, unmodeled tables, sections, unknown entities).
/// Modeled items get fresh handles on every write, so only these survive a write.
fn raw_handles(doc: &DxfDocument) -> HashSet<u64> {
    let mut set = HashSet::new();
    let mut see = |ps: &[Pair]| {
        for p in ps.iter().filter(|p| p.code == 5 || p.code == 105) {
            if let Ok(v) = u64::from_str_radix(p.value.trim(), 16) {
                set.insert(v);
            }
        }
    };
    for h in &doc.header {
        see(&h.values);
    }
    for t in doc.other_tables.iter().filter(|t| t.name != "BLOCK_RECORD") {
        see(&t.header);
        t.entries.iter().for_each(|e| see(e));
    }
    for s in &doc.other_sections {
        see(&s.pairs);
    }
    let all = doc
        .entities
        .iter()
        .chain(doc.blocks.iter().flat_map(|b| &b.entities));
    for e in all {
        if let Entity::Unknown(p) = e {
            see(p);
        }
    }
    set
}

/// First handle above every handle in raw items and in an existing `$HANDSEED`.
fn first_free_handle(doc: &DxfDocument) -> u64 {
    let max = raw_handles(doc).into_iter().max().unwrap_or(0);
    (max + 1).max(0x10)
}

/// Pointer/owner codes 330..=369 whose target is not a surviving raw handle are dropped: the
/// handles of modeled items change on write, so such a reference would point at nothing (or at an
/// unrelated new item). A dictionary entry loses its name (code 3) together with its target.
fn drop_dangling(pairs: &[Pair], known: &HashSet<u64>) -> Vec<Pair> {
    let mut out: Vec<Pair> = Vec::with_capacity(pairs.len());
    for p in pairs {
        if (330..=369).contains(&p.code) {
            let v = p.value.trim();
            let live = u64::from_str_radix(v, 16).is_ok_and(|h| h == 0 || known.contains(&h));
            if !live {
                if (p.code == 350 || p.code == 360) && out.last().is_some_and(|l| l.code == 3) {
                    out.pop();
                }
                continue;
            }
        }
        out.push(p.clone());
    }
    out
}

fn has_name<'a>(mut names: impl Iterator<Item = &'a str>, n: &str) -> bool {
    names.any(|x| x.eq_ignore_ascii_case(n))
}

fn raw<'a>(doc: &'a DxfDocument, name: &str) -> Option<&'a super::RawTable> {
    doc.other_tables.iter().find(|t| t.name == name)
}

fn write_raw_table(w: &mut Out, t: &super::RawTable) {
    w.s(0, "TABLE");
    w.s(2, &t.name);
    w.extra(&t.header);
    for e in &t.entries {
        w.extra(e);
    }
    w.s(0, "ENDTAB");
}

fn write_tables(w: &mut Out, doc: &DxfDocument, brs: &[(String, String)]) {
    w.section("TABLES");

    // VPORT
    if let Some(t) = raw(doc, "VPORT") {
        write_raw_table(w, t);
    } else {
        let th = w.table("VPORT", 1);
        w.record("VPORT", &th, "AcDbViewportTableRecord");
        w.s(2, "*Active");
        w.i(70, 0);
        for (c, v) in [
            (10, 0.0),
            (20, 0.0),
            (11, 1.0),
            (21, 1.0),
            (12, 0.0),
            (22, 0.0),
        ] {
            w.f(c, v);
        }
        for (c, v) in [
            (13, 0.0),
            (23, 0.0),
            (14, 10.0),
            (24, 10.0),
            (15, 10.0),
            (25, 10.0),
        ] {
            w.f(c, v);
        }
        for (c, v) in [
            (16, 0.0),
            (26, 0.0),
            (36, 1.0),
            (17, 0.0),
            (27, 0.0),
            (37, 0.0),
        ] {
            w.f(c, v);
        }
        w.f(40, 100.0);
        w.f(41, 1.0);
        w.s(0, "ENDTAB");
    }

    // LTYPE: ByBlock, ByLayer, Continuous are mandatory.
    let names = || doc.ltypes.iter().map(|t| t.name.as_str());
    let missing: Vec<(&str, &str)> = [
        ("ByBlock", ""),
        ("ByLayer", ""),
        ("Continuous", "Solid line"),
    ]
    .into_iter()
    .filter(|(n, _)| !has_name(names(), n))
    .collect();
    let th = w.table("LTYPE", missing.len() + doc.ltypes.len());
    for (n, d) in &missing {
        w.record("LTYPE", &th, "AcDbLinetypeTableRecord");
        w.s(2, n);
        w.i(70, 0);
        w.s(3, d);
        w.i(72, 65);
        w.i(73, 0);
        w.f(40, 0.0);
    }
    for t in &doc.ltypes {
        w.record("LTYPE", &th, "AcDbLinetypeTableRecord");
        w.s(2, &t.name);
        w.i(70, t.flags);
        w.s(3, &t.description);
        w.i(72, 65);
        w.i(73, t.pattern.len() as i32);
        w.f(40, t.pattern_length);
        for e in &t.pattern {
            w.f(49, *e);
            w.i(74, 0);
        }
        w.extra_m(&t.extra);
    }
    w.s(0, "ENDTAB");

    // LAYER: layer "0" is mandatory.
    let zero = has_name(doc.layers.iter().map(|l| l.name.as_str()), "0");
    let th = w.table("LAYER", doc.layers.len() + usize::from(!zero));
    if !zero {
        w.record("LAYER", &th, "AcDbLayerTableRecord");
        w.s(2, "0");
        w.i(70, 0);
        w.i(62, 7);
        w.s(6, "Continuous");
    }
    for l in &doc.layers {
        w.record("LAYER", &th, "AcDbLayerTableRecord");
        w.s(2, &l.name);
        w.i(70, l.flags);
        w.i(62, l.color);
        w.s(6, &l.linetype);
        w.extra_m(&l.extra);
    }
    w.s(0, "ENDTAB");

    // STYLE: Standard is mandatory.
    let std_style = has_name(doc.styles.iter().map(|s| s.name.as_str()), "Standard");
    let th = w.table("STYLE", doc.styles.len() + usize::from(!std_style));
    if !std_style {
        w.record("STYLE", &th, "AcDbTextStyleTableRecord");
        w.s(2, "Standard");
        w.i(70, 0);
        w.f(40, 0.0);
        w.f(41, 1.0);
        w.f(50, 0.0);
        w.s(3, "txt");
        w.s(4, "");
    }
    for t in &doc.styles {
        w.record("STYLE", &th, "AcDbTextStyleTableRecord");
        w.s(2, &t.name);
        w.i(70, t.flags);
        w.f(40, t.height);
        w.f(41, t.width_factor);
        w.f(50, t.oblique);
        w.s(3, &t.font);
        w.s(4, &t.big_font);
        w.extra_m(&t.extra);
    }
    w.s(0, "ENDTAB");

    // VIEW, UCS: empty when not preserved.
    for n in ["VIEW", "UCS"] {
        if let Some(t) = raw(doc, n) {
            write_raw_table(w, t);
        } else {
            w.table(n, 0);
            w.s(0, "ENDTAB");
        }
    }

    // APPID: ACAD is mandatory.
    if let Some(t) = raw(doc, "APPID") {
        write_raw_table(w, t);
    } else {
        let th = w.table("APPID", 1);
        w.record("APPID", &th, "AcDbRegAppTableRecord");
        w.s(2, "ACAD");
        w.i(70, 0);
        w.s(0, "ENDTAB");
    }

    // DIMSTYLE: Standard is mandatory.
    if let Some(t) = raw(doc, "DIMSTYLE") {
        write_raw_table(w, t);
    } else {
        w.table("DIMSTYLE", 1);
        w.s(100, "AcDbDimStyleTable");
        w.i(71, 1);
        w.record("DIMSTYLE", "0", "AcDbDimStyleTableRecord");
        w.s(2, "Standard");
        w.i(70, 0);
        w.s(0, "ENDTAB");
    }

    // BLOCK_RECORD: one per block incl. the two spaces.
    let th = w.table("BLOCK_RECORD", brs.len());
    for (name, h) in brs {
        w.s(0, "BLOCK_RECORD");
        w.s(5, h);
        w.s(330, &th);
        w.s(100, "AcDbSymbolTableRecord");
        w.s(100, "AcDbBlockTableRecord");
        w.s(2, name);
    }
    w.s(0, "ENDTAB");

    const KNOWN: [&str; 9] = [
        "VPORT",
        "LTYPE",
        "LAYER",
        "STYLE",
        "VIEW",
        "UCS",
        "APPID",
        "DIMSTYLE",
        "BLOCK_RECORD",
    ];
    for t in doc
        .other_tables
        .iter()
        .filter(|t| !KNOWN.contains(&t.name.as_str()))
    {
        write_raw_table(w, t);
    }
    w.end_section();
}

fn write_block(w: &mut Out, b: &Block, record: &str) {
    let paper = b.name.eq_ignore_ascii_case(PAPER);
    w.owner = record.to_string();
    w.s(0, "BLOCK");
    let h = w.handle();
    w.s(5, &h);
    w.s(330, record);
    w.s(100, "AcDbEntity");
    if paper {
        w.i(67, 1);
    }
    w.s(8, &b.layer);
    w.s(100, "AcDbBlockBegin");
    w.s(2, &b.name);
    w.i(70, b.flags);
    w.f(10, b.base[0]);
    w.f(20, b.base[1]);
    w.f(30, b.base[2]);
    w.s(3, &b.name);
    w.extra_m(&b.extra);
    for a in &b.attdefs {
        write_attdef(w, a);
    }
    for e in &b.entities {
        write_entity(w, e);
    }
    w.s(0, "ENDBLK");
    let h = w.handle();
    w.s(5, &h);
    w.s(330, record);
    w.s(100, "AcDbEntity");
    if paper {
        w.i(67, 1);
    }
    w.s(8, &b.layer);
    w.s(100, "AcDbBlockEnd");
}

fn source_version(doc: &DxfDocument) -> Option<&str> {
    doc.header
        .iter()
        .find(|v| v.name == "$ACADVER")
        .and_then(|v| v.values.first())
        .map(|p| p.value.trim())
        .filter(|v| v.starts_with("AC") && *v >= "AC1015" && *v <= "AC9999")
}

pub fn write_string(doc: &DxfDocument) -> String {
    let ver = source_version(doc).unwrap_or(VERSION);
    let ascii = ver < "AC1021";
    let mut w = Out {
        buf: String::new(),
        next: first_free_handle(doc),
        owner: String::new(),
        ascii,
    };

    // BLOCK_RECORD handles: model space, paper space, then each block (case-insensitive names).
    let mut brs: Vec<(String, String)> = vec![];
    for n in [MODEL, PAPER] {
        let h = w.handle();
        brs.push((n.to_string(), h));
    }
    for b in &doc.blocks {
        if !has_name(brs.iter().map(|(n, _)| n.as_str()), &b.name) {
            let h = w.handle();
            brs.push((b.name.clone(), h));
        }
    }
    let record_of = |name: &str| {
        brs.iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, h)| h.clone())
            .unwrap_or_default()
    };
    let model = record_of(MODEL);

    write_tables(&mut w, doc, &brs);

    w.section("BLOCKS");
    let mut written: HashSet<String> = HashSet::new();
    for n in [MODEL, PAPER] {
        if !has_name(doc.blocks.iter().map(|b| b.name.as_str()), n) {
            write_block(&mut w, &Block::new(n), &record_of(n));
            written.insert(n.to_ascii_lowercase());
        }
    }
    for b in &doc.blocks {
        if written.insert(b.name.to_ascii_lowercase()) {
            write_block(&mut w, b, &record_of(&b.name));
        }
    }
    w.end_section();

    w.owner = model;
    w.section("ENTITIES");
    for e in &doc.entities {
        write_entity(&mut w, e);
    }
    w.end_section();

    let known = raw_handles(doc);
    for s in &doc.other_sections {
        w.section(&s.name);
        if s.name == "OBJECTS" {
            w.extra(&drop_dangling(&s.pairs, &known));
        } else {
            w.extra(&s.pairs);
        }
        w.end_section();
    }
    if !doc.other_sections.iter().any(|s| s.name == "OBJECTS") {
        let (root, group) = (w.handle(), w.handle());
        w.section("OBJECTS");
        w.s(0, "DICTIONARY");
        w.s(5, &root);
        w.s(330, "0");
        w.s(100, "AcDbDictionary");
        w.i(281, 1);
        w.s(3, "ACAD_GROUP");
        w.s(350, &group);
        w.s(0, "DICTIONARY");
        w.s(5, &group);
        w.s(330, &root);
        w.s(100, "AcDbDictionary");
        w.i(281, 1);
        w.end_section();
    }
    w.s(0, "EOF");

    // Header last: `$HANDSEED` must exceed every handle handed out above.
    let mut h = Out {
        buf: String::new(),
        next: 0,
        owner: String::new(),
        ascii,
    };
    h.section("HEADER");
    h.s(9, "$ACADVER");
    h.s(1, ver);
    h.s(9, "$HANDSEED");
    h.s(5, &format!("{:X}", w.next));
    if !doc.header.iter().any(|v| v.name == "$DWGCODEPAGE") {
        h.s(9, "$DWGCODEPAGE");
        h.s(3, "ANSI_1252");
    }
    for v in doc
        .header
        .iter()
        .filter(|v| v.name != "$ACADVER" && v.name != "$HANDSEED")
    {
        h.s(9, &v.name);
        h.extra(&v.values);
    }
    h.end_section();
    h.buf + &w.buf
}

fn entity_strings(es: &[Entity]) -> Vec<String> {
    let mut out = vec![];
    for e in es {
        match e {
            Entity::Text(t) => {
                out.push(t.common.layer.clone());
                out.push(t.text.clone());
                out.extend(t.style.clone());
            }
            Entity::MText(t) => {
                out.push(t.common.layer.clone());
                out.push(t.text.clone());
            }
            Entity::Line(x) => out.push(x.common.layer.clone()),
            Entity::Insert(x) => {
                out.push(x.common.layer.clone());
                out.push(x.block.clone());
            }
            _ => {}
        }
    }
    out
}

/// Every non-empty string of `want` must be in `got` (as a multiset). Extra strings in `got`
/// are fine: the writer adds the mandatory records (layer "0", ByBlock/ByLayer/Continuous,
/// style "Standard").
fn all_come_back(what: &str, want: &[String], got: &[String], fold_breaks: bool) -> Result<(), String> {
    use super::text::decode_unicode;
    let mut left: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for g in got {
        *left.entry(decode_unicode(g)).or_default() += 1;
    }
    for w in want.iter().filter(|w| !w.is_empty()) {
        let mut d = decode_unicode(w);
        if fold_breaks {
            d = d.replace(['\n', '\r'], " ");
        }
        match left.get_mut(&d) {
            Some(n) if *n > 0 => *n -= 1,
            _ => return Err(format!("{what} changed on write: {w:?} did not come back")),
        }
    }
    Ok(())
}

/// Compares decoded layer, linetype, style, block and text strings of `doc` with its read-back
/// `back`, matched by name per table and per block; a code-page mangled string shows up as a
/// missing one. Records the writer adds on its own are allowed.
pub fn check_text_round_trip(doc: &DxfDocument, back: &DxfDocument) -> Result<(), String> {
    use super::text::decode_unicode;
    let names = |v: Vec<&String>| v.into_iter().cloned().collect::<Vec<_>>();
    all_come_back(
        "layer",
        &names(doc.layers.iter().map(|l| &l.name).collect()),
        &names(back.layers.iter().map(|l| &l.name).collect()),
        false,
    )?;
    for l in &doc.layers {
        let key = decode_unicode(&l.name);
        if let Some(b) = back.layers.iter().find(|b| decode_unicode(&b.name) == key) {
            all_come_back("layer linetype", &[l.linetype.clone()], &[b.linetype.clone()], false)?;
        }
    }
    all_come_back(
        "linetype",
        &names(doc.ltypes.iter().map(|l| &l.name).collect()),
        &names(back.ltypes.iter().map(|l| &l.name).collect()),
        false,
    )?;
    all_come_back(
        "style",
        &names(doc.styles.iter().map(|l| &l.name).collect()),
        &names(back.styles.iter().map(|l| &l.name).collect()),
        false,
    )?;
    all_come_back(
        "block",
        &names(doc.blocks.iter().map(|b| &b.name).collect()),
        &names(back.blocks.iter().map(|b| &b.name).collect()),
        false,
    )?;
    for blk in &doc.blocks {
        let key = decode_unicode(&blk.name);
        if let Some(b) = back.blocks.iter().find(|b| decode_unicode(&b.name) == key) {
            all_come_back("block text", &entity_strings(&blk.entities), &entity_strings(&b.entities), true)?;
        }
    }
    all_come_back("text", &entity_strings(&doc.entities), &entity_strings(&back.entities), true)
}

pub fn write_file(doc: &DxfDocument, path: &Path) -> Result<(), DxfError> {
    std::fs::write(path, write_string(doc)).map_err(|e| DxfError(e.to_string()))
}

#[cfg(test)]
mod ascii_tests {
    use super::*;
    use crate::dxf::{read_str, text::decode_unicode, Layer};

    const KIT: &str = "\u{41a}\u{443}\u{445}\u{43d}\u{44f}";

    fn doc(ver: &str) -> DxfDocument {
        let mut d = read_str(&format!("0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\n{ver}\n0\nENDSEC\n0\nEOF\n"))
            .unwrap();
        d.layers.push(Layer {
            name: KIT.into(),
            flags: 0,
            color: 7,
            linetype: "CONTINUOUS".into(),
            extra: vec![],
        });
        let mut t = crate::dxf::entities::Text::default();
        t.common.layer = KIT.into();
        t.text = format!("{KIT} \u{1F600}");
        t.height = 2.5;
        d.entities.push(Entity::Text(t));
        d
    }

    #[test]
    fn round_trip_check_allows_writer_defaults_and_catches_mojibake() {
        for v in ["AC1015", "AC1018", "AC1021"] {
            let d = doc(v);
            let back = read_str(&write_string(&d)).unwrap();
            // the writer adds layer "0", ByBlock/ByLayer/Continuous and "Standard"
            assert!(back.layers.len() + back.ltypes.len() + back.styles.len()
                > d.layers.len() + d.ltypes.len() + d.styles.len());
            assert_eq!(check_text_round_trip(&d, &back), Ok(()), "{v}");
            let mut bad = back.clone();
            for l in bad.layers.iter_mut().filter(|l| decode_unicode(&l.name) == KIT) {
                l.name = "?????".into();
            }
            assert!(check_text_round_trip(&d, &bad).is_err(), "{v}");
        }
    }

    #[test]
    fn pre2007_export_escapes_cyrillic_layer() {
        let out = write_string(&doc("AC1018"));
        assert!(out.contains("\\U+041A\\U+0443\\U+0445\\U+043D\\U+044F"));
        let back = read_str(&out).unwrap();
        assert_eq!(decode_unicode(&back.layers.last().unwrap().name), KIT);
    }

    #[test]
    fn pre2007_export_round_trips_text() {
        let d = doc("AC1015");
        let back = read_str(&write_string(&d)).unwrap();
        let Entity::Text(t) = &back.entities[0] else { panic!("text") };
        assert_eq!(decode_unicode(&t.text), format!("{KIT} \u{1F600}"));
        assert!(check_text_round_trip(&d, &back).is_ok());
        let mut bad = back.clone();
        if let Entity::Text(t) = &mut bad.entities[0] {
            t.text = "????".into();
        }
        assert!(check_text_round_trip(&d, &bad).is_err());
    }

    #[test]
    fn ac1021_export_keeps_utf8() {
        let out = write_string(&doc("AC1021"));
        assert!(out.contains(KIT));
        assert!(!out.contains("\\U+"));
    }

    #[test]
    fn output_is_pure_ascii_below_ac1021() {
        for v in ["AC1015", "AC1018"] {
            assert!(write_string(&doc(v)).is_ascii());
        }
        assert!(!write_string(&doc("AC1024")).is_ascii());
    }
}
