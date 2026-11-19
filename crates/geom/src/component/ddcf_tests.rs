use super::ddcf::{self, check, fmt, upgrade, Meta, Version};
use super::Family;
use proptest::prelude::*;

fn golden() -> String {
    include_str!("ddcf_golden.ddcf").replace("\r\n", "\n")
}

fn plain(mut f: Family) -> Family {
    f.meta = Meta::default();
    f
}

const V1: &str = "# lead\n\nfamily   T   \ncategory furniture\nextrude Box rect(0, 0,\n   1m, 1m)   z(0, 1m)  # box\nparam W  length  type  1m\n\n# about types\ntype \"A  B\" W=2m\nhost floor\n";

const V1_CANON: &str = "# lead\nfamily T\ncategory furniture\nhost floor\nparam W length type 1m\n# about types\ntype \"A  B\" W=2m\nextrude Box rect(0, 0, 1m, 1m) z(0, 1m) # box\n";

#[test]
fn ddcf_v1_text_still_parses() {
    let f = Family::parse(V1).unwrap();
    assert_eq!(f.meta, Meta::default());
    assert_eq!(f.name, "T");
    assert_eq!(f.types[0].name, "A  B");
}

#[test]
fn ddcf_header_is_read() {
    let f = check(&golden()).unwrap();
    assert_eq!(f.meta.format, 2);
    let v = Version {
        major: 1,
        minor: 2,
        patch: 0,
    };
    assert_eq!(f.meta.version, Some(v));
    assert_eq!(v.to_string(), "1.2.0");
    assert_eq!(f.meta.uid.as_deref(), Some("0123456789abcdef0123456789abcdef"));
    assert_eq!(f.ext.children.len(), 1);
    // an explicit `ddcf 1` is the old format
    assert_eq!(Family::parse("ddcf 1\nfamily A\n").unwrap().meta.format, 1);
}

#[test]
fn ddcf_golden_is_canonical() {
    let g = golden();
    assert_eq!(fmt(&g).unwrap(), g);
    assert_eq!(upgrade(&g).unwrap(), g);
}

#[test]
fn ddcf_fmt_canonical_order_and_spacing() {
    let c = fmt(V1).unwrap();
    assert_eq!(c, V1_CANON);
    assert_eq!(fmt(&c).unwrap(), c);
    assert_eq!(check(&c).unwrap(), check(V1).unwrap());
}

#[test]
fn ddcf_upgrade_v1_to_v2() {
    let up = upgrade(V1).unwrap();
    let l: Vec<&str> = up.lines().collect();
    assert_eq!(&l[..4], &["# lead", "ddcf 2", "family T", "version 1.0.0"]);
    let uid = l[4].strip_prefix("uid ").unwrap();
    assert!(ddcf::uid_ok(uid) && uid.len() == 32, "{uid}");
    // pure, idempotent, independent of spacing
    assert_eq!(upgrade(V1).unwrap(), up);
    assert_eq!(upgrade(&up).unwrap(), up);
    assert_eq!(upgrade(V1_CANON).unwrap(), up);
    // without the header lines it is the canonical v1 text
    let rest: Vec<&str> = l
        .iter()
        .enumerate()
        .filter(|(k, _)| ![1, 3, 4].contains(k))
        .map(|(_, x)| *x)
        .collect();
    assert_eq!(rest.join("\n") + "\n", V1_CANON);
    let f = check(&up).unwrap();
    assert_eq!(f.meta.format, 2);
    assert_eq!(plain(f), Family::parse(V1).unwrap());
    // different content, different uid
    let other = upgrade(&V1.replace("2m", "3m")).unwrap();
    assert_ne!(other.lines().nth(4), up.lines().nth(4));
    // an old `ddcf 1` line is replaced
    let up1 = upgrade("ddcf 1\nfamily A\n").unwrap();
    assert!(up1.starts_with("ddcf 2\nfamily A\nversion 1.0.0\nuid "), "{up1}");
}

fn err(t: &str) -> (usize, usize, String) {
    let e = check(t).unwrap_err();
    assert_eq!(e.to_string(), format!("{}:{}: {}", e.line, e.col, e.msg));
    (e.line, e.col, e.msg)
}

#[test]
fn ddcf_error_positions() {
    let (l, c, m) = err("ddcf 2\nfamily A\nversion 1.x\n");
    assert_eq!((l, c), (3, 9), "{m}");
    assert!(m.contains("major.minor.patch"), "{m}");
    let (l, c, m) = err("family A\nddcf 2\n");
    assert_eq!((l, c), (2, 1), "{m}");
    assert!(m.contains("first statement"), "{m}");
    let (l, c, m) = err("ddcf 3\nfamily A\n");
    assert_eq!((l, c), (1, 6), "{m}");
    assert!(m.contains("unsupported format"), "{m}");
    let (l, c, m) = err("family A\nversion 1.0.0\n");
    assert_eq!((l, c), (2, 1), "{m}");
    assert!(m.contains("'ddcf 2'"), "{m}");
    let (l, c, m) = err("ddcf 2\nfamily A\nuid XYZ\n");
    assert_eq!((l, c), (3, 5), "{m}");
    assert!(m.contains("hex"), "{m}");
    let (l, c, m) = err("ddcf 2\nfamily A\nversion 1.0.0\nversion 1.0.1\n");
    assert_eq!((l, c), (4, 1), "{m}");
    let (l, c, m) = err("family A\n  param W lenth type 1m\n");
    assert_eq!((l, c), (2, 11), "{m}");
    assert!(m.contains("unknown kind"), "{m}");
    let (l, c, m) = err("family A\n\nfoo bar\n");
    assert_eq!((l, c), (3, 1), "{m}");
    let (l, c, m) = err("family A\nparam W length type 1m\nparam W length type 2m\n");
    assert_eq!((l, c), (3, 7), "{m}");
    let (l, c, m) = err("family A\nparam X length type = Y\nparam Y length type = X\n");
    assert!((l == 2 || l == 3) && c == 7, "{l}:{c}: {m}");
    assert!(m.contains("cycle"), "{m}");
    // inside a nested block: the absolute line of the inner error
    let (l, c, m) = err("family A\nbegin\nfamily B\nparam Q lenth type 1m\nend\n");
    assert_eq!((l, c), (4, 9), "{m}");
    // fmt refuses invalid text with the same position
    assert_eq!(fmt("family A\nfoo\n").unwrap_err().line, 2);
}

/// One random edit of the golden body (lines between the header and the nested block).
fn edit(body: &mut Vec<String>, op: u8, a: u32, b: u32) {
    let n = body.len();
    let k = a as usize % n.max(1);
    match op {
        // more whitespace (golden quotes hold no spaces)
        0 => body[k] = body[k].replace(' ', "  \t "),
        // blank or comment line
        1 => {
            let at = a as usize % (n + 1);
            let line = if b % 2 == 0 { String::new() } else { format!("# note {b}") };
            body.insert(at, line);
        }
        // trailing comment
        2 if !body[k].contains('#') && !body[k].trim().is_empty() => {
            body[k].push_str(&format!("   # t{b}"))
        }
        // new default of a plain length parameter
        3 if body[k].starts_with("param ") && !body[k].contains('=') && body[k].ends_with("mm") => {
            let w: Vec<&str> = body[k].split_whitespace().collect();
            if w.len() == 5 && w[1] != "Width" {
                body[k] = format!("{} {} {} {} {}mm", w[0], w[1], w[2], w[3], 1 + b % 5000);
            }
        }
        // swap neighbours from different sections
        4 if k + 1 < n => {
            let r = |s: &str| ddcf::rank(s.split_whitespace().next().unwrap_or("#"));
            let (x, y) = (r(&body[k]), r(&body[k + 1]));
            let stmt = |s: &str| !s.trim().is_empty() && !s.trim_start().starts_with('#');
            let body_rank = |r: u8| (10..=18).contains(&r);
            if stmt(&body[k]) && stmt(&body[k + 1]) && x != y && body_rank(x) && body_rank(y) {
                body.swap(k, k + 1);
            }
        }
        // continue a statement on the next line after its first '('
        5 => {
            if let Some(p) = body[k].find('(') {
                if !body[k].trim_start().starts_with('#') {
                    body[k].insert_str(p + 1, "\n    ");
                }
            }
        }
        _ => {}
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    #[test]
    fn ddcf_fmt_random_edits(
        ops in prop::collection::vec((0u8..6, any::<u32>(), any::<u32>()), 0..16)
    ) {
        let g = golden();
        let lines: Vec<String> = g.lines().map(str::to_string).collect();
        let (h, e) = (8, lines.iter().position(|l| l == "begin").unwrap());
        let mut body = lines[h..e].to_vec();
        for (op, a, b) in ops {
            edit(&mut body, op, a, b);
        }
        let text = [&lines[..h], &body[..], &lines[e..]].concat().join("\n");
        let f = check(&text).map_err(|e| TestCaseError::fail(format!("{e}\n{text}")))?;
        let c = fmt(&text).unwrap();
        prop_assert_eq!(check(&c).unwrap(), f, "{}", c);
        prop_assert_eq!(fmt(&c).unwrap(), c.clone());
        for l in c.lines() {
            prop_assert!(!l.trim().is_empty() && l == l.trim(), "{:?}", l);
        }
        prop_assert_eq!(upgrade(&c).unwrap(), c);
    }
}

/// Statement heads the parser accepts: `super::HEADS` (which the parser itself gates on), the
/// sketch statements and the nested-family `begin`.
fn parser_heads() -> Vec<String> {
    let mut heads: Vec<String> = super::HEADS.iter().map(|s| s.to_string()).collect();
    heads.extend(super::ext::STMTS.iter().map(|s| s.to_string()));
    heads.push("begin".into());
    heads.sort();
    heads.dedup();
    assert!(heads.len() >= 30, "operator list shrank: {heads:?}");
    heads
}

/// Every listed head reaches its own arm (never the unknown-statement gate), and a head outside
/// the lists is refused.
#[test]
fn parser_heads_match_the_parser() {
    for h in super::HEADS {
        let e = Family::parse(&format!("family A\n{h} x\n")).err().map(|e| e.to_string());
        assert!(!e.is_some_and(|m| m.contains("unknown statement")), "'{h}' is gated out");
    }
    for h in super::SOLID_HEADS {
        assert!(super::HEADS.contains(h), "{h}");
    }
    let e = Family::parse("family A\nfrobnicate 1\n").unwrap_err().to_string();
    assert!(e.contains("unknown statement"), "{e}");
}

#[test]
fn ddcf_every_operator_has_a_rank() {
    let heads = parser_heads();
    for h in ["family", "param", "extrude", "plant", "limit", "status", "uid", "refline"] {
        assert!(heads.iter().any(|x| x == h), "{h} not found in {heads:?}");
    }
    for h in &heads {
        assert_ne!(ddcf::rank(h), ddcf::UNRANKED, "'{h}' has no canonical section");
    }
    assert_eq!(ddcf::rank("no-such-statement"), ddcf::UNRANKED);
}

/// R14: every statement of the closed set has a line in the provenance log.
#[test]
fn provenance_covers_operators() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../legal/PROVENANCE_LOG.md");
    let log = std::fs::read_to_string(path).expect("legal/PROVENANCE_LOG.md");
    let row = log
        .lines()
        .find(|l| l.starts_with('|') && l.contains("ddcf 2"))
        .expect("a provenance row for the ddcf 2 format");
    for h in parser_heads() {
        assert!(row.contains(&format!("`{h}`")), "'{h}' missing from the ddcf 2 provenance row");
    }
}

/// R1: geom and every workspace crate it reaches by path know nothing of the model or app.
#[test]
fn geom_does_not_depend_on_model() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut todo = vec![root.clone()];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(dir) = todo.pop() {
        let dir = dir.canonicalize().unwrap_or(dir);
        if !seen.insert(dir.clone()) {
            continue;
        }
        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).expect("Cargo.toml");
        for line in toml.lines() {
            let l = line.split('#').next().unwrap_or("");
            for bad in ["ddc-model", "ddc_model", "ddc-app", "ddc_app", "ddc-ai", "ddc_ai"] {
                assert!(!l.contains(bad), "{}: {line}", dir.display());
            }
            if let Some((_, p)) = l.split_once("path = \"") {
                let p = p.split('"').next().unwrap_or("");
                assert!(!p.ends_with("model") && !p.ends_with("app"), "{}: {line}", dir.display());
                todo.push(dir.join(p));
            }
        }
    }
    assert!(seen.len() >= 2, "{seen:?}");
}

fn eval_all(t: &str) -> Vec<String> {
    let f = Family::parse(t).unwrap();
    let mut vals: Vec<Vec<(String, super::Value)>> = f.types.iter().map(|t| t.values.clone()).collect();
    vals.push(vec![]);
    let mut out = vec![];
    for v in &vals {
        let r = f.resolve(v, &[]).unwrap();
        for lod in [super::Lod::Coarse, super::Lod::Medium, super::Lod::Fine] {
            out.push(format!("{:?}", f.build(&r, lod).unwrap()));
        }
    }
    out
}

/// R1: evaluation is a pure function of the text: two parses of the same text build the same
/// bytes at every level of detail, for every type.
#[test]
fn eval_is_pure_same_text_same_bytes() {
    let extra = "family K\nparam R length type 100mm\nparam H length type 50mm\nparam T length type 5mm\n\
                 revolve Bowl poly(0,0, R,0, R,H, R-T,H, R-T,T, 0,T) axis(0, 0)\n\
                 sweep Rail rect(-10mm, -10mm, 10mm, 10mm) path(0m,0m,0m, 1m,0m,0m, 1m,1m,0m)\n\
                 blend Cone circle(0m, 0m, 200mm) circle(0m, 0m, 100mm) z(0m, 300mm)\n";
    let noise = [
        "family N1\nparam W length type 3m\nextrude Slab rect(0m, 0m, W, W) z(0m, 100mm)\n",
        "family N2\nparam D length type 7mm\nblend Cone circle(0m, 0m, D) circle(0m, 0m, D) z(0m, 1m)\n",
    ];
    let texts = [golden(), extra.to_string()];
    // baseline: each text evaluated alone, in a fresh state
    let alone: Vec<Vec<String>> = texts.iter().map(|t| eval_all(t)).collect();
    // reversed order, with unrelated families evaluated and kept alive in between
    let mut keep = vec![];
    for (i, text) in texts.iter().enumerate().rev() {
        for n in noise.iter().rev() {
            let f = Family::parse(n).unwrap();
            let r = f.resolve(&[], &[]).unwrap();
            keep.push(format!("{:?}", f.build(&r, super::Lod::Fine).unwrap()));
        }
        assert_eq!(eval_all(text), alone[i], "text {i} differs after unrelated evaluations");
    }
    assert!(!keep.is_empty());
    for text in [golden(), extra.to_string()] {
        let run = |t: String| -> Vec<String> {
            let f = Family::parse(&t).unwrap();
            let mut vals: Vec<Vec<(String, super::Value)>> =
                f.types.iter().map(|t| t.values.clone()).collect();
            vals.push(vec![]);
            let mut out = vec![];
            for v in &vals {
                let r = f.resolve(v, &[]).unwrap();
                for lod in [super::Lod::Coarse, super::Lod::Medium, super::Lod::Fine] {
                    out.push(format!("{:?}", f.build(&r, lod).unwrap()));
                }
            }
            out
        };
        let a = run(text.clone());
        let b = run(text.chars().collect());
        assert!(a.len() >= 3);
        assert_eq!(a, b);
    }
}

/// R4: one evaluator per solid kind: each solid statement makes its own `Shape` variant and
/// `build::shape` has exactly one arm for it.
#[test]
fn one_evaluator_per_kind() {
    let comp = include_str!("../component.rs");
    let at = comp.find("pub enum Shape {").expect("enum Shape");
    let body = &comp[at..at + comp[at..].find("\n}").expect("end of Shape")];
    // variant names are the capitalised identifiers met at brace/paren depth 0 of the enum body,
    // independent of indentation and line breaks
    let mut variants: Vec<String> = vec![];
    let (mut depth, mut word, mut in_comment) = (0i32, String::new(), false);
    for c in body["pub enum Shape {".len()..].chars().chain(std::iter::once('\n')) {
        if in_comment {
            in_comment = c != '\n';
            continue;
        }
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        if depth == 0 && word.starts_with(|c: char| c.is_ascii_uppercase()) {
            variants.push(word.clone());
        }
        word.clear();
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            '/' => in_comment = true,
            _ => {}
        }
    }
    let solids: Vec<String> = super::SOLID_HEADS.iter().map(|s| s.to_string()).collect();
    let mut names: Vec<String> = variants.iter().map(|v| v.to_lowercase()).collect();
    names.sort();
    let mut want = solids.clone();
    want.sort();
    assert!(names.len() >= 7, "Shape variants not found: {variants:?}");
    assert_eq!(names, want, "one solid statement per Shape variant");
    let build = include_str!("build.rs");
    let at = build.find("fn shape(").expect("build::shape");
    let rest = &build[at..];
    let shape_fn = &rest[..rest.find("\nfn ").unwrap_or(rest.len())];
    for v in &variants {
        let pat = format!("Shape::{v}");
        let n = shape_fn
            .match_indices(&pat)
            .filter(|(i, _)| !shape_fn[i + pat.len()..].starts_with(|c: char| c.is_ascii_alphanumeric()))
            .count();
        assert_eq!(n, 1, "Shape::{v} has {n} evaluator arms in build::shape");
    }
}

#[test]
fn ddcf_replace_uid_top_level_only() {
    let g = golden();
    let nested = g.replace("begin\nfamily Foot\n", "begin\nddcf 2\nfamily Foot\nuid aaaaaaaa\n");
    assert_ne!(nested, g);
    let out = ddcf::replace_uid(&nested, "bbbbbbbbbbbbbbbb").unwrap();
    let f = check(&out).unwrap();
    assert_eq!(f.meta.uid.as_deref(), Some("bbbbbbbbbbbbbbbb"));
    assert!(out.contains("\nuid aaaaaaaa\n"), "nested uid kept");
    assert_eq!(plain(f), plain(check(&nested).unwrap()));
    assert_eq!(ddcf::replace_uid("family A\n", "bbbbbbbb"), None);
}
