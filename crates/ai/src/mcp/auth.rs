//! The token and the checks that keep a web page or a foreign host away from the server.

use crate::secret::{Secret, SecretError, SecretStore};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

/// Entry of the token in the key store.
pub const TOKEN_ID: &str = "mcp";
const TOKEN_BYTES: usize = 32;

pub(crate) fn random_text(bytes: usize) -> Result<String, SecretError> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).map_err(|e| SecretError::Store(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(&buf))
}

pub fn generate_token() -> Result<Secret, SecretError> {
    random_text(TOKEN_BYTES).map(Secret::new)
}

/// A new token replaces the stored one.
pub fn issue_token(store: &dyn SecretStore) -> Result<Secret, SecretError> {
    let token = generate_token()?;
    store.set(TOKEN_ID, token.clone())?;
    Ok(token)
}

pub fn load_token(store: &dyn SecretStore) -> Result<Option<Secret>, SecretError> {
    store.get(TOKEN_ID)
}

/// Only for the settings page, where the person copies the token into their agent.
pub fn reveal(token: &Secret) -> String {
    token.expose().to_string()
}

pub(crate) fn token_matches(expected: &Secret, given: &[u8]) -> bool {
    ct_eq(expected.expose().as_bytes(), given)
}

/// The time does not depend on where the values differ; only their lengths can show.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u64;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= u64::from(x ^ y);
    }
    std::hint::black_box(diff) == 0
}

pub(crate) fn bearer(header: &str) -> Option<&str> {
    let (scheme, rest) = header.trim().split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
}

fn split_host_port(s: &str) -> Option<(&str, u16)> {
    if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        return Some((host, tail.strip_prefix(':')?.parse().ok()?));
    }
    let (host, port) = s.rsplit_once(':')?;
    if host.contains(':') {
        return None;
    }
    Some((host, port.parse().ok()?))
}

fn is_loopback_name(host: &str) -> bool {
    host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost")
}

/// DNS rebinding: the page's name resolves to loopback, but the browser still sends that name.
pub(crate) fn host_allowed(value: &str, port: u16) -> bool {
    split_host_port(value.trim()).is_some_and(|(h, p)| p == port && is_loopback_name(h))
}

/// A page served from another local port is as foreign as a remote one. No header: not a browser.
pub(crate) fn origin_allowed(value: &str, port: u16) -> bool {
    value
        .trim()
        .strip_prefix("http://")
        .is_some_and(|rest| host_allowed(rest, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::MemoryStore;

    #[test]
    fn comparison_needs_every_byte_and_the_length() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(
            !ct_eq(b"ab\0", b"ab"),
            "a zero byte must not pass for a missing one"
        );
        assert!(!ct_eq(b"", b"a"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn host_must_be_loopback_on_our_port() {
        for ok in [
            "127.0.0.1:7000",
            "[::1]:7000",
            "localhost:7000",
            "LOCALHOST:7000",
        ] {
            assert!(host_allowed(ok, 7000), "{ok}");
        }
        for bad in [
            "evil.example:7000",
            "127.0.0.1:7001",
            "127.0.0.1",
            "127.0.0.2:7000",
            "0.0.0.0:7000",
            "[::2]:7000",
            "localhost.evil.example:7000",
            "::1:7000",
            "",
        ] {
            assert!(!host_allowed(bad, 7000), "{bad}");
        }
    }

    #[test]
    fn origin_must_be_this_server_itself() {
        for ok in [
            "http://127.0.0.1:7000",
            "http://[::1]:7000",
            "http://localhost:7000",
        ] {
            assert!(origin_allowed(ok, 7000), "{ok}");
        }
        for bad in [
            "null",
            "https://127.0.0.1:7000",
            "http://127.0.0.1:3000",
            "http://evil.example",
            "http://evil.example:7000",
            "file://",
            "http://127.0.0.1:7000/x",
            "",
        ] {
            assert!(!origin_allowed(bad, 7000), "{bad}");
        }
    }

    #[test]
    fn bearer_scheme_is_case_insensitive_and_required() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("bearer  abc "), Some("abc"));
        assert_eq!(bearer("Basic abc"), None);
        assert_eq!(bearer("abc"), None);
    }

    #[test]
    fn a_new_token_has_full_length_replaces_the_old_one_and_stays_hidden() {
        let store = MemoryStore::new();
        assert_eq!(load_token(&store).unwrap(), None);
        let a = issue_token(&store).unwrap();
        assert_eq!(reveal(&a).len(), 43, "32 bytes without padding");
        let b = issue_token(&store).unwrap();
        assert_ne!(a, b);
        assert_eq!(load_token(&store).unwrap(), Some(b.clone()));
        assert_eq!(format!("{b:?}"), "***");
    }
}
