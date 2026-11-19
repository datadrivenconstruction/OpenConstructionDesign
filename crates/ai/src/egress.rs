//! Outbound address policy, checked before every request.

use crate::provider::ProviderError;
use std::net::{Ipv4Addr, Ipv6Addr};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EgressGuard {
    pub local_only: bool,
    pub consented_host: Option<String>,
}

impl EgressGuard {
    /// Local addresses always pass: nothing leaves the machine. Anything else needs
    /// `local_only` off and `consented_host` to match: `[scheme://]host[:port]`, where a
    /// value without a scheme means https, so moving an address to http asks again.
    pub fn check(&self, url: &str) -> Result<(), ProviderError> {
        self.check_send(url, false)
    }

    /// `check` for a request that carries a key: plain http off the machine is refused
    /// whatever was consented to, because the key would be readable on the network.
    pub fn check_send(&self, url: &str, sends_secret: bool) -> Result<(), ProviderError> {
        let t = parse_target(url).map_err(ProviderError::Blocked)?;
        if t.local {
            return Ok(());
        }
        if self.local_only {
            return Err(ProviderError::Blocked(format!("local only: {}", t.host)));
        }
        if sends_secret && t.scheme != "https" {
            return Err(ProviderError::Blocked(format!(
                "key over plain http: {}",
                t.host
            )));
        }
        match self.consented_host.as_deref() {
            Some(c) if consent_matches(c, &t) => Ok(()),
            _ => Err(ProviderError::Blocked(format!(
                "no consent for {}://{}",
                t.scheme, t.host
            ))),
        }
    }
}

/// The value to keep in `consented_host` after a person agreed to send to `url`.
pub fn consent_key(url: &str) -> Result<String, ProviderError> {
    let t = parse_target(url).map_err(ProviderError::Blocked)?;
    Ok(format!("{}://{}", t.scheme, t.authority))
}

fn consent_matches(consent: &str, t: &Target) -> bool {
    let c = consent.trim().to_ascii_lowercase();
    let (scheme, rest) = c.split_once("://").unwrap_or(("https", c.as_str()));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let bare = authority
        .strip_prefix('[')
        .and_then(|a| a.strip_suffix(']'))
        .unwrap_or(authority);
    scheme == t.scheme && (authority == t.authority || bare == t.host)
}

/// True for localhost, 127.0.0.0/8 and ::1. Unparsable addresses are not local.
pub fn is_local_url(url: &str) -> bool {
    parse_target(url).map(|t| t.local).unwrap_or(false)
}

struct Target {
    scheme: String,
    host: String,
    authority: String,
    local: bool,
}

/// Strict on purpose: credentials, backslashes and whitespace are how a URL shows
/// one host to a check and another to the HTTP client.
fn parse_target(url: &str) -> Result<Target, String> {
    let bad = || "malformed address".to_string();
    if url
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return Err(bad());
    }
    let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(format!("unsupported scheme {scheme}"));
    }
    let end = rest
        .find(|c: char| matches!(c, '/' | '?' | '#'))
        .unwrap_or(rest.len());
    let authority = rest[..end].to_ascii_lowercase();
    if authority.is_empty() || authority.contains('@') {
        return Err(bad());
    }
    let (host, port): (&str, Option<&str>) = if let Some(r) = authority.strip_prefix('[') {
        let (h, tail) = r.split_once(']').ok_or_else(bad)?;
        if h.parse::<Ipv6Addr>().is_err() {
            return Err(bad());
        }
        match tail.strip_prefix(':') {
            Some(p) => (h, Some(p)),
            None if tail.is_empty() => (h, None),
            None => return Err(bad()),
        }
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority.as_str(), None),
        }
    };
    if host.is_empty() || port.map_or(false, |p| p.parse::<u16>().is_err()) {
        return Err(bad());
    }
    let local = host == "localhost"
        || host
            .parse::<Ipv4Addr>()
            .map_or(false, |ip| ip.octets()[0] == 127)
        || host
            .parse::<Ipv6Addr>()
            .map_or(false, |ip| ip.is_loopback());
    let host = host.to_string();
    Ok(Target {
        scheme: scheme.to_ascii_lowercase(),
        host,
        authority,
        local,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn consent(host: &str) -> EgressGuard {
        EgressGuard {
            local_only: false,
            consented_host: Some(host.to_string()),
        }
    }

    #[test]
    fn local_addresses_pass_even_when_local_only() {
        let g = EgressGuard {
            local_only: true,
            consented_host: None,
        };
        for u in [
            "http://localhost:11434/v1",
            "http://127.0.0.1/x",
            "http://127.9.8.7:80",
            "http://[::1]:8080/v1",
            "HTTP://LOCALHOST/v1",
        ] {
            assert_eq!(g.check(u), Ok(()), "{u}");
            assert!(is_local_url(u), "{u}");
        }
    }

    #[test]
    fn local_only_rejects_remote_even_with_consent() {
        let g = EgressGuard {
            local_only: true,
            consented_host: Some("api.example.com".to_string()),
        };
        assert!(matches!(
            g.check("https://api.example.com/v1"),
            Err(ProviderError::Blocked(_))
        ));
    }

    #[test]
    fn remote_needs_matching_consent() {
        assert!(EgressGuard::default()
            .check("https://api.example.com/v1")
            .is_err());
        assert!(consent("other.example.com")
            .check("https://api.example.com/v1")
            .is_err());
        assert_eq!(
            consent("API.example.com").check("https://api.example.com/v1"),
            Ok(())
        );
        assert_eq!(
            consent("api.example.com:8443").check("https://api.example.com:8443/v1"),
            Ok(())
        );
        assert!(consent("api.example.com:8443")
            .check("https://api.example.com:9000/v1")
            .is_err());
    }

    #[test]
    fn lookalike_hosts_are_not_local() {
        let g = EgressGuard {
            local_only: true,
            consented_host: None,
        };
        for u in [
            "http://localhost.evil.example/v1",
            "http://127.0.0.1.evil.example/v1",
            "http://127.0.0.1@evil.example/v1",
            "http://evil.example#@127.0.0.1/",
            "http://evil.example\\@127.0.0.1/",
            "http://evil.example/127.0.0.1",
            "http://2130706433/",
            "http://[::ffff:127.0.0.1]/",
            "http://128.0.0.1/",
            "http://0.0.0.0:11434/",
            "http://[::]:11434/",
            "ftp://localhost/",
            "localhost:8080/v1",
            "http:// localhost/",
            "http://localhost:notaport/",
            "http://[::1/",
            "http://",
        ] {
            assert!(g.check(u).is_err(), "{u}");
            assert!(!is_local_url(u), "{u}");
        }
    }

    #[test]
    fn consent_is_bound_to_the_scheme() {
        // A bare host means https: moving the same address to http asks again.
        assert!(consent("api.example.com")
            .check("http://api.example.com/v1")
            .is_err());
        assert!(consent("https://api.example.com")
            .check("http://api.example.com/v1")
            .is_err());
        assert!(consent("http://api.example.com")
            .check("https://api.example.com/v1")
            .is_err());
        assert_eq!(
            consent("HTTPS://API.example.com/").check("https://api.example.com/v1"),
            Ok(())
        );
        assert_eq!(
            consent("http://10.0.0.5:8080").check("http://10.0.0.5:8080/v1"),
            Ok(())
        );
        assert!(consent("http://10.0.0.5:8080")
            .check("http://10.0.0.5:9000/v1")
            .is_err());
        assert_eq!(
            consent("[2001:db8::1]").check("https://[2001:db8::1]:8443/v1"),
            Ok(())
        );
    }

    #[test]
    fn a_key_never_leaves_the_machine_over_plain_http() {
        let g = consent("http://api.example.com");
        assert_eq!(g.check_send("http://api.example.com/v1", false), Ok(()));
        let refused = g.check_send("http://api.example.com/v1", true);
        assert!(
            matches!(refused, Err(ProviderError::Blocked(ref m)) if m.contains("plain http")),
            "{refused:?}"
        );
        assert_eq!(
            consent("api.example.com").check_send("https://api.example.com/v1", true),
            Ok(())
        );
        for u in [
            "http://localhost:11434/v1",
            "http://127.0.0.1/v1",
            "http://[::1]:80/v1",
        ] {
            assert_eq!(EgressGuard::default().check_send(u, true), Ok(()), "{u}");
        }
    }

    #[test]
    fn consent_key_is_accepted_for_its_own_address_only() {
        let key = consent_key("HTTP://Box.example:8080/v1").unwrap();
        assert_eq!(key, "http://box.example:8080");
        assert_eq!(consent(&key).check("http://box.example:8080/v1"), Ok(()));
        assert!(consent(&key).check("https://box.example:8080/v1").is_err());
        assert!(consent_key("ftp://box.example").is_err());
    }
}
