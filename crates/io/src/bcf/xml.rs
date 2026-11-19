//! A small XML reader and writer for BCF parts. Elements, attributes, text, CDATA and the five
//! predefined entities plus numeric references; namespaces are dropped from names. A DOCTYPE is
//! refused (no entity expansion, nothing to resolve), depth and size are capped.

use super::BcfError;

const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 400_000;

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub kids: Vec<Node>,
    pub text: String,
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn kid(&self, name: &str) -> Option<&Node> {
        self.kids.iter().find(|k| k.name == name)
    }

    pub fn kids_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.kids.iter().filter(move |k| k.name == name)
    }

    /// Trimmed text of the first child with this name.
    pub fn kid_text(&self, name: &str) -> Option<&str> {
        self.kid(name).map(|k| k.text.trim())
    }

    /// Every descendant (not self) in document order.
    pub fn descendants<'a>(&'a self, out: &mut Vec<&'a Node>) {
        for k in &self.kids {
            out.push(k);
            k.descendants(out);
        }
    }
}

fn bad(msg: &str) -> BcfError {
    BcfError::Xml(msg.to_string())
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

fn decode(s: &str) -> Result<String, BcfError> {
    if !s.contains('&') {
        return Ok(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        let tail = &rest[p + 1..];
        let end = tail.find(';').ok_or_else(|| bad("unterminated entity"))?;
        let ent = &tail[..end];
        match ent {
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "amp" => out.push('&'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                let code = ent
                    .strip_prefix("#x")
                    .or_else(|| ent.strip_prefix("#X"))
                    .map(|h| u32::from_str_radix(h, 16))
                    .or_else(|| ent.strip_prefix('#').map(|d| d.parse::<u32>()))
                    .ok_or_else(|| bad("unknown entity"))?
                    .map_err(|_| bad("bad character reference"))?;
                out.push(char::from_u32(code).ok_or_else(|| bad("bad character reference"))?);
            }
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

pub(super) fn parse(src: &str) -> Result<Node, BcfError> {
    let s = src.strip_prefix('\u{feff}').unwrap_or(src);
    let b = s.as_bytes();
    let mut i = 0;
    let mut stack: Vec<Node> = vec![];
    let mut root: Option<Node> = None;
    let mut count = 0usize;
    while i < b.len() {
        if b[i] != b'<' {
            let j = s[i..].find('<').map_or(b.len(), |p| i + p);
            let chunk = &s[i..j];
            match stack.last_mut() {
                Some(top) => top.text.push_str(&decode(chunk)?),
                None if chunk.trim().is_empty() => {}
                None => return Err(bad("text outside the root element")),
            }
            i = j;
            continue;
        }
        let rest = &s[i..];
        if rest.starts_with("<!--") {
            let e = rest.find("-->").ok_or_else(|| bad("unterminated comment"))?;
            i += e + 3;
        } else if rest.starts_with("<![CDATA[") {
            let e = rest.find("]]>").ok_or_else(|| bad("unterminated CDATA"))?;
            let top = stack.last_mut().ok_or_else(|| bad("CDATA outside the root"))?;
            top.text.push_str(&rest[9..e]);
            i += e + 3;
        } else if rest.starts_with("<?") {
            let e = rest.find("?>").ok_or_else(|| bad("unterminated declaration"))?;
            i += e + 2;
        } else if rest.starts_with("<!") {
            return Err(bad("DOCTYPE is not supported"));
        } else if rest.starts_with("</") {
            let e = rest.find('>').ok_or_else(|| bad("unterminated end tag"))?;
            let name = local(rest[2..e].trim());
            let node = stack.pop().ok_or_else(|| bad("end tag without start"))?;
            if node.name != name {
                return Err(bad("mismatched end tag"));
            }
            close(node, &mut stack, &mut root)?;
            i += e + 1;
        } else {
            let (node, used, empty) = start_tag(rest)?;
            count += 1;
            if count > MAX_NODES {
                return Err(bad("too many elements"));
            }
            i += used;
            if empty {
                close(node, &mut stack, &mut root)?;
            } else {
                if stack.len() >= MAX_DEPTH {
                    return Err(bad("elements nested too deep"));
                }
                stack.push(node);
            }
        }
    }
    if !stack.is_empty() {
        return Err(bad("unclosed element"));
    }
    root.ok_or_else(|| bad("no root element"))
}

fn close(node: Node, stack: &mut [Node], root: &mut Option<Node>) -> Result<(), BcfError> {
    match stack.last_mut() {
        Some(parent) => parent.kids.push(node),
        None if root.is_none() => *root = Some(node),
        None => return Err(bad("more than one root element")),
    }
    Ok(())
}

/// Parses `<name a="b" ...>` or `.../>` at the start of `t`; returns the node, bytes used and
/// whether the tag was self-closing.
fn start_tag(t: &str) -> Result<(Node, usize, bool), BcfError> {
    let b = t.as_bytes();
    let mut i = 1;
    let name_start = i;
    while i < b.len() && !is_ws(b[i]) && b[i] != b'/' && b[i] != b'>' {
        i += 1;
    }
    if i == name_start || i >= b.len() {
        return Err(bad("bad start tag"));
    }
    let mut node = Node {
        name: local(&t[name_start..i]).to_string(),
        ..Node::default()
    };
    loop {
        while i < b.len() && is_ws(b[i]) {
            i += 1;
        }
        match b.get(i) {
            None => return Err(bad("unterminated start tag")),
            Some(b'>') => return Ok((node, i + 1, false)),
            Some(b'/') => {
                return if b.get(i + 1) == Some(&b'>') {
                    Ok((node, i + 2, true))
                } else {
                    Err(bad("bad start tag"))
                };
            }
            Some(_) => {}
        }
        let k0 = i;
        while i < b.len() && !is_ws(b[i]) && b[i] != b'=' && b[i] != b'>' && b[i] != b'/' {
            i += 1;
        }
        let key = local(&t[k0..i]).to_string();
        while i < b.len() && is_ws(b[i]) {
            i += 1;
        }
        if b.get(i) != Some(&b'=') || key.is_empty() {
            return Err(bad("bad attribute"));
        }
        i += 1;
        while i < b.len() && is_ws(b[i]) {
            i += 1;
        }
        let q = *b.get(i).ok_or_else(|| bad("bad attribute"))?;
        if q != b'"' && q != b'\'' {
            return Err(bad("attribute value is not quoted"));
        }
        let v0 = i + 1;
        let len = t[v0..].find(q as char).ok_or_else(|| bad("unterminated attribute"))?;
        let val = decode(&t[v0..v0 + len])?;
        i = v0 + len + 1;
        // namespace declarations carry nothing for us
        if key != "xmlns" && !t[k0..].starts_with("xmlns:") {
            node.attrs.push((key, val));
        }
    }
}

/// XML-safe text: markup characters escaped, characters XML 1.0 forbids dropped.
pub(super) fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            '\t' | '\n' => o.push(c),
            c if (c as u32) < 0x20 || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => o.push(c),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_elements_attributes_and_entities() {
        let n = parse(
            "\u{feff}<?xml version=\"1.0\"?><!-- c --><a:Root xmlns:a=\"u\" k='v&amp;w'>\
             <Kid Guid=\"g\">x &lt; y &#65;&#x42;</Kid><E/><![CDATA[<raw>]]></a:Root>",
        )
        .unwrap();
        assert_eq!(n.name, "Root");
        assert_eq!(n.attr("k"), Some("v&w"));
        assert!(n.attr("xmlns").is_none() && n.attrs.len() == 1);
        assert_eq!(n.kid("Kid").unwrap().text, "x < y AB");
        assert_eq!(n.kid("Kid").unwrap().attr("Guid"), Some("g"));
        assert!(n.kid("E").is_some());
        assert_eq!(n.text, "<raw>");
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        for s in [
            "",
            "<a>",
            "<a></b>",
            "<a><b></a></b>",
            "<a x=1/>",
            "<a x=\"1/>",
            "<a>&bogus;</a>",
            "<a>&#xZZ;</a>",
            "<a>&amp</a>",
            "<!DOCTYPE a [<!ENTITY x \"y\">]><a/>",
            "<a/><b/>",
            "text<a/>",
            "<",
            "<a",
            "<a/",
            "<a><!-- x </a>",
            "<a>&#1114112;</a>",
        ] {
            assert!(parse(s).is_err(), "{s:?}");
        }
    }

    #[test]
    fn deep_nesting_is_refused() {
        let s = "<a>".repeat(200) + &"</a>".repeat(200);
        assert!(parse(&s).is_err());
    }

    #[test]
    fn multibyte_text_survives() {
        let n = parse("<a>\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} \u{1f600}</a>").unwrap();
        assert_eq!(n.text, "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} \u{1f600}");
    }

    #[test]
    fn escaping_round_trips_through_the_reader() {
        let raw = "a<b>&\"c'\nd\u{1}e";
        let n = parse(&format!("<a>{}</a>", esc(raw))).unwrap();
        assert_eq!(n.text, "a<b>&\"c'\nde");
    }
}
