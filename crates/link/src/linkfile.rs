//! `ddc-link/1` (contract §7, `schemas/link.schema.json`): the file the ERP hands over.

use serde::Deserialize;

use crate::{LinkError, Secret};

/// Where the ERP is: `https://host[:port][/path]`, or `http://` for a loopback host only.
/// No credentials, query or fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErpUrl {
    /// Scheme, authority and path, without a trailing `/`.
    pub base: String,
    /// `host[:port]`, lower case: the key-store id is built from it.
    pub authority: String,
    pub loopback: bool,
}

impl ErpUrl {
    pub fn parse(s: &str) -> Result<Self, LinkError> {
        let bad = |why: &str| LinkError::BadLink(format!("erp_url {why}"));
        let (rest, https) = if let Some(r) = s.strip_prefix("https://") {
            (r, true)
        } else if let Some(r) = s.strip_prefix("http://") {
            (r, false)
        } else {
            return Err(bad("must start with https://"));
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if path.chars().any(|c| c == '?' || c == '#' || c == '@' || c.is_whitespace()) {
            return Err(bad("may not carry credentials, a query or a fragment"));
        }
        let (host, port) = if let Some(h) = authority.strip_prefix('[') {
            let end = h.find(']').ok_or_else(|| bad("has an unclosed IPv6 address"))?;
            let port = h[end + 1..].strip_prefix(':');
            if !h[end + 1..].is_empty() && port.is_none() {
                return Err(bad("has text after the address"));
            }
            (&authority[..end + 2], port)
        } else {
            match authority.split_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (authority, None),
            }
        };
        if let Some(p) = port {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) || p.parse::<u16>().is_err() {
                return Err(bad("has a bad port"));
            }
        }
        let loopback = host.eq_ignore_ascii_case("localhost") || host == "[::1]" || is_127(host);
        if https {
            if host.is_empty() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
                return Err(bad("has a bad host"));
            }
        } else if !loopback {
            return Err(bad("must use https unless the ERP runs on this computer"));
        }
        Ok(ErpUrl {
            base: s.trim_end_matches('/').to_string(),
            authority: authority.to_ascii_lowercase(),
            loopback,
        })
    }
}

fn is_127(h: &str) -> bool {
    let parts: Vec<&str> = h.split('.').collect();
    parts.len() == 4
        && parts[0] == "127"
        && parts.iter().all(|p| (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Open an existing revision (`revision_id`).
    Open,
    /// Start an empty model for the project.
    New,
    /// Generate a model from the project brief.
    Generate,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    schema: String,
    erp_url: String,
    project_id: String,
    #[serde(default)]
    project_name: Option<String>,
    code: Secret,
    expires_at: String,
    action: Action,
    #[serde(default)]
    revision_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LinkFile {
    pub erp: ErpUrl,
    pub project_id: String,
    pub project_name: Option<String>,
    pub code: Secret,
    pub expires_at: String,
    pub action: Action,
    pub revision_id: Option<String>,
}

pub(crate) fn check_uuid(s: &str) -> Result<(), LinkError> {
    let ok = s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        });
    if ok {
        Ok(())
    } else {
        Err(LinkError::BadLink(format!("'{s}' is not a lower-case UUID")))
    }
}

impl LinkFile {
    pub fn parse(text: &str) -> Result<Self, LinkError> {
        // serde's message can quote the offending value: keep only its position
        let r: Raw = serde_json::from_str(text).map_err(|e| {
            LinkError::BadLink(format!("not a ddc-link/1 document (line {}, column {})", e.line(), e.column()))
        })?;
        if r.schema != "ddc-link/1" {
            return Err(LinkError::BadLink(format!("schema is '{}', this editor reads ddc-link/1", r.schema)));
        }
        let erp = ErpUrl::parse(&r.erp_url)?;
        check_uuid(&r.project_id)?;
        let c = r.code.expose();
        if !(32..=128).contains(&c.len()) || !c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Err(LinkError::BadLink("the code is not 32-128 url-safe characters".into()));
        }
        if let Some(rev) = &r.revision_id {
            check_uuid(rev)?;
        }
        if r.action == Action::Open && r.revision_id.is_none() {
            return Err(LinkError::BadLink("action 'open' needs a revision_id".into()));
        }
        if r.project_name.as_ref().is_some_and(|n| n.chars().count() > 200) {
            return Err(LinkError::BadLink("project_name is over 200 characters".into()));
        }
        Ok(LinkFile {
            erp,
            project_id: r.project_id,
            project_name: r.project_name,
            code: r.code,
            expires_at: r.expires_at,
            action: r.action,
            revision_id: r.revision_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../io/contract/examples/link.ddclink.json");

    #[test]
    fn contract_example_parses() {
        let l = LinkFile::parse(EXAMPLE).unwrap();
        assert_eq!(l.erp.base, "http://127.0.0.1:8000");
        assert_eq!(l.erp.authority, "127.0.0.1:8000");
        assert!(l.erp.loopback);
        assert_eq!(l.action, Action::Open);
        assert_eq!(l.revision_id.as_deref(), Some("68143a44-27fb-47bb-aa7e-c1229f23a2dc"));
        assert_eq!(format!("{:?}", l.code), "***");
    }

    #[test]
    fn erp_url_rules() {
        for ok in ["https://erp.example.org", "https://erp.example.org:8443/oe/", "http://localhost:5173", "http://127.0.0.1", "http://[::1]:8000"] {
            ErpUrl::parse(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "http://erp.example.org",
            "ftp://x",
            "https://user:pw@erp.example.org",
            "https://erp.example.org/?a=1",
            "https://erp.example.org/#f",
            "https://erp.example.org:99999",
            "http://128.0.0.1",
            "http://127.0.0.1.evil.org",
            "https://",
        ] {
            assert!(ErpUrl::parse(bad).is_err(), "{bad} must be refused");
        }
        assert_eq!(ErpUrl::parse("https://ERP.Example.org:8443/x/").unwrap().authority, "erp.example.org:8443");
        assert_eq!(ErpUrl::parse("https://erp.example.org/x/").unwrap().base, "https://erp.example.org/x");
    }

    #[test]
    fn refuses_what_the_schema_refuses() {
        let v: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
        let with = |k: &str, x: serde_json::Value| {
            let mut v = v.clone();
            v[k] = x;
            v.to_string()
        };
        assert!(LinkFile::parse(&with("schema", "ddc-link/2".into())).is_err());
        assert!(LinkFile::parse(&with("code", "short".into())).is_err());
        assert!(LinkFile::parse(&with("code", "a/b+cdefghijklmnopqrstuvwxyz0123456789".into())).is_err());
        assert!(LinkFile::parse(&with("project_id", "746402CB-14A8-430C-B3B9-9C6908FA75D7".into())).is_err());
        assert!(LinkFile::parse(&with("action", "delete".into())).is_err());
        assert!(LinkFile::parse(&with("extra", 1.into())).is_err(), "additionalProperties false");
        let mut no_rev = v.clone();
        no_rev.as_object_mut().unwrap().remove("revision_id");
        assert!(LinkFile::parse(&no_rev.to_string()).is_err(), "open needs revision_id");
        no_rev["action"] = "new".into();
        assert_eq!(LinkFile::parse(&no_rev.to_string()).unwrap().action, Action::New);
    }

    #[test]
    fn parse_error_does_not_quote_the_code() {
        let text = EXAMPLE.replace("\"open\"", "\"1nFlnKIs1m7gyR_tiXqflOszmc8d6c4CwYzlKK_GhO8\"");
        let e = LinkFile::parse(&text).unwrap_err().to_string();
        assert!(!e.contains("1nFlnKIs1m7gyR"), "{e}");
    }
}
