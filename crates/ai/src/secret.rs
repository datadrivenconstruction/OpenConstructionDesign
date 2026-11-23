// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Key storage. Keys live only in the OS vault or, for tests and systems without one,
//! in process memory. A `Secret` cannot be printed or serialized.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

pub const SERVICE: &str = "ddc-editor";

/// Every secret made in this process, so a last filter can wipe it from text bound for the
/// journal or a log even when a caller forgot to. Short values are not worth matching.
static KNOWN: Mutex<Vec<String>> = Mutex::new(Vec::new());
const MIN_KNOWN: usize = 8;
const MAX_KNOWN: usize = crate::limits::SECRETS_KNOWN.us();

thread_local! {
    /// The same list per thread: a secret made on this thread is never pushed out of it by
    /// secrets other threads make (parallel tests, a busy worker), so its scrub is certain.
    static MINE: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn push_bounded(k: &mut Vec<String>, v: &str) {
    if k.iter().any(|x| x == v) {
        return;
    }
    if k.len() >= MAX_KNOWN {
        k.remove(0);
    }
    k.push(v.to_string());
}

fn remember(v: &str) {
    if v.len() < MIN_KNOWN {
        return;
    }
    MINE.with(|m| push_bounded(&mut m.borrow_mut(), v));
    push_bounded(&mut KNOWN.lock().unwrap_or_else(|e| e.into_inner()), v);
}

/// A query parameter name that usually carries a credential (`key`, `api-key`, `sig`, ...).
/// `max_tokens` and the like are not: only the exact names and the `_key`/`_token` endings.
pub fn is_credential_param(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase().replace('-', "_");
    const EXACT: [&str; 10] = [
        "key", "apikey", "token", "sig", "signature", "secret", "password", "passwd", "pwd",
        "auth",
    ];
    const ENDS: [&str; 6] = ["_key", "_token", "_secret", "_signature", "_sig", "_password"];
    !n.is_empty() && (EXACT.contains(&n.as_str()) || ENDS.iter().any(|e| n.ends_with(e)))
}

fn ends_value(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`')
}

/// `user:pass@` after every `scheme://` becomes `***@`.
fn redact_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| matches!(c, '/' | '?' | '#') || ends_value(c))
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rfind('@') {
            Some(a) if a > 0 => {
                out.push_str("***");
                out.push_str(&authority[a..]);
            }
            _ => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// The value of every credential-like `?name=` / `&name=` becomes `***`.
fn redact_query(text: &str) -> String {
    redact_query_by(text, &is_credential_param)
}

/// The value of every `?name=` / `&name=` named `param` (any case) becomes `***`: the key
/// parameter of a profile may carry any name, and the address holds it percent-encoded.
pub fn redact_query_param(text: &str, param: &str) -> String {
    let want = param.trim();
    if want.is_empty() {
        return text.to_string();
    }
    redact_query_by(text, &|n: &str| n.trim().eq_ignore_ascii_case(want))
}

fn redact_query_by(text: &str, is_secret: &dyn Fn(&str) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(p) = rest.find(|c: char| c == '?' || c == '&') {
        out.push_str(&rest[..=p]);
        let tail = &rest[p + 1..];
        let name_end = tail
            .find(|c: char| matches!(c, '=' | '&' | '#' | '?') || ends_value(c))
            .unwrap_or(tail.len());
        if tail[name_end..].starts_with('=') && is_secret(&tail[..name_end]) {
            out.push_str(&tail[..=name_end]);
            let v = &tail[name_end + 1..];
            let v_end = v
                .find(|c: char| matches!(c, '&' | '#') || ends_value(c))
                .unwrap_or(v.len());
            if v_end > 0 {
                out.push_str("***");
            }
            rest = &v[v_end..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

/// `text` with the keys an address can carry masked: userinfo and credential-like query
/// values. Works on any text, so a URL inside an error or a log line is caught too.
pub fn redact_url_secrets(text: &str) -> String {
    if !text.contains("://") && !text.contains('=') {
        return text.to_string();
    }
    redact_query(&redact_userinfo(text))
}

/// True where `default_store` keeps keys only in memory: they are gone when the app closes.
pub fn store_is_session_only() -> bool {
    !cfg!(any(windows, target_os = "macos"))
}

/// `text` with every known key and token replaced by `***`, and the keys an address can
/// carry masked as well.
pub fn scrub_known(text: &str) -> String {
    let mut out = redact_url_secrets(text);
    let mut wipe = |v: &str| {
        if out.contains(v) {
            out = out.replace(v, "***");
        }
    };
    MINE.with(|m| m.borrow().iter().for_each(|v| wipe(v.as_str())));
    let k = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    k.iter().for_each(|v| wipe(v.as_str()));
    drop(k);
    out
}

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        remember(&value);
        Secret(value)
    }

    /// Only the HTTP layer reads the value.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("no secure key storage on this system")]
    Unavailable,
    #[error("key storage error: {0}")]
    Store(String),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError>;
    fn set(&self, id: &str, k: Secret) -> Result<(), SecretError>;
    fn remove(&self, id: &str) -> Result<(), SecretError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OsStore;

#[cfg(any(windows, target_os = "macos"))]
impl SecretStore for OsStore {
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        match entry(id)?.get_password() {
            Ok(v) => Ok(Some(Secret::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(store_err(e)),
        }
    }

    fn set(&self, id: &str, k: Secret) -> Result<(), SecretError> {
        entry(id)?.set_password(&k.0).map_err(store_err)
    }

    fn remove(&self, id: &str) -> Result<(), SecretError> {
        match entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(store_err(e)),
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn entry(id: &str) -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new(SERVICE, id).map_err(store_err)
}

#[cfg(any(windows, target_os = "macos"))]
fn store_err(e: keyring::Error) -> SecretError {
    SecretError::Store(e.to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
impl SecretStore for OsStore {
    fn get(&self, _id: &str) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::Unavailable)
    }

    fn set(&self, _id: &str, _k: Secret) -> Result<(), SecretError> {
        Err(SecretError::Unavailable)
    }

    fn remove(&self, _id: &str) -> Result<(), SecretError> {
        Err(SecretError::Unavailable)
    }
}

#[derive(Debug, Default)]
pub struct MemoryStore {
    keys: Mutex<HashMap<String, Secret>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn map(&self) -> Result<MutexGuard<'_, HashMap<String, Secret>>, SecretError> {
        self.keys
            .lock()
            .map_err(|_| SecretError::Store("poisoned".to_string()))
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self.map()?.get(id).cloned())
    }

    fn set(&self, id: &str, k: Secret) -> Result<(), SecretError> {
        self.map()?.insert(id.to_string(), k);
        Ok(())
    }

    fn remove(&self, id: &str) -> Result<(), SecretError> {
        self.map()?.remove(id);
        Ok(())
    }
}

/// The vault where the platform has one, session memory otherwise.
pub fn default_store() -> Arc<dyn SecretStore> {
    if cfg!(any(windows, target_os = "macos")) {
        Arc::new(OsStore)
    } else {
        Arc::new(MemoryStore::new())
    }
}

/// Auth schemes whose single word after them is the credential.
const SCHEMES: [&str; 2] = ["bearer", "basic"];
/// Common key prefixes (generic, not tied to any service) and the start of a signed token.
const KEY_PREFIXES: [&str; 4] = ["sk-", "pk-", "rk-", "eyj"];

/// `Bearer abc` gives the scheme and the credential; anything else gives no scheme.
pub fn split_scheme(v: &str) -> (Option<&str>, &str) {
    if let Some((w, rest)) = v.trim().split_once(' ') {
        let rest = rest.trim();
        let one_word = !rest.is_empty() && !rest.contains(char::is_whitespace);
        if one_word && SCHEMES.iter().any(|s| w.eq_ignore_ascii_case(s)) {
            return (Some(w), rest);
        }
    }
    (None, v.trim())
}

fn is_uuid(w: &str) -> bool {
    let parts: Vec<&str> = w.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn opaque_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '/' | '=')
}

/// Bits per char of `v`, from its own char counts.
fn entropy(v: &str) -> f64 {
    let mut seen = [0u32; 128];
    let mut total = 0u32;
    for b in v.bytes().filter(u8::is_ascii) {
        seen[usize::from(b)] += 1;
        total += 1;
    }
    if total == 0 {
        return 0.0;
    }
    let t = f64::from(total);
    seen.iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / t;
            -p * p.log2()
        })
        .sum()
}

/// A value that is a key by its form alone: a scheme with a token (bearer, basic), a known
/// key prefix or a signed token.
pub fn value_is_key_shaped(value: &str) -> bool {
    let v = value.trim();
    if let (Some(_), rest) = split_scheme(v) {
        return rest.len() >= 8;
    }
    let l = v.to_ascii_lowercase();
    v.len() >= 16 && KEY_PREFIXES.iter().any(|p| l.starts_with(p))
}

/// A value that reads like a key: a scheme with a token (bearer, basic), a known key prefix
/// or a signed token, a long opaque mix of letters and digits, or a shorter one as dense as a
/// random string. Request ids (UUIDs), dates, versions and media types are not keys.
pub fn value_looks_secret(value: &str) -> bool {
    let v = value.trim();
    if value_is_key_shaped(v) {
        return true;
    }
    // a short token after a scheme has a space, so it is not opaque either
    if v.is_empty() || !v.chars().all(opaque_char) || is_uuid(v) {
        return false;
    }
    let digit = v.chars().any(|c| c.is_ascii_digit());
    let upper = v.chars().any(|c| c.is_ascii_uppercase());
    let lower = v.chars().any(|c| c.is_ascii_lowercase());
    let long = v.len() >= 32 && digit && (upper || lower);
    let dense = v.len() >= 20 && digit && upper && lower && entropy(v) >= 3.5;
    long || dense
}

/// Content ranges of the quoted strings in `text` (escapes skipped, not decoded); the last
/// one may run to the end of a cut file.
fn quoted(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        if b[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < b.len() && b[j] != b'"' {
            j += if b[j] == b'\\' { 2 } else { 1 };
        }
        let end = j.min(b.len());
        out.push((start, end));
        i = end + 1;
    }
    out
}

/// A string right after a credential-named one (`["X-Api-Key", "..."]`, `"token": "..."`)
/// becomes `***`. The key mode field keeps its value (`"auth": "Bearer"`).
fn mask_named_strings(text: &str) -> String {
    let spans = quoted(text);
    let mut hide = Vec::new();
    for w in spans.windows(2) {
        let (name, value) = (&text[w[0].0..w[0].1], w[1]);
        let sep = text[w[0].1 + 1..value.0 - 1].trim();
        let pair = sep == "," || (sep == ":" && !name.eq_ignore_ascii_case("auth"));
        // a header or field name, not an address or a profile name that happens to say "key"
        let ident = name.len() <= 64
            && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
        if pair && ident && value.1 > value.0 && crate::profile::is_secret_header(name) {
            hide.push(value);
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (s, e) in hide {
        out.push_str(&text[at..s]);
        out.push_str("***");
        at = e;
    }
    out.push_str(&text[at..]);
    out
}

fn word_char(c: char) -> bool {
    opaque_char(c) || c == '~'
}

/// A word as written, or `***` when it reads like a key. In an address only the key-shaped
/// parts of the path go, so its host and path can still be read.
fn mask_word(w: &str) -> String {
    if w.contains('/') && w.contains('.') {
        let parts: Vec<&str> = w
            .split('/')
            .map(|s| if value_looks_secret(s) { "***" } else { s })
            .collect();
        return parts.join("/");
    }
    if value_looks_secret(w) {
        "***".to_string()
    } else {
        w.to_string()
    }
}

/// Key-shaped words and the word after a scheme become `***`.
fn mask_key_words(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut rest, mut after_scheme) = (text, false);
    while let Some(c) = rest.chars().next() {
        let n: usize = rest
            .chars()
            .take_while(|c| word_char(*c))
            .map(char::len_utf8)
            .sum();
        if n == 0 {
            out.push(c);
            // the credential is the word after the scheme and its spaces
            after_scheme &= c == ' ';
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let (w, tail) = rest.split_at(n);
        if after_scheme {
            out.push_str("***");
        } else {
            out.push_str(&mask_word(w));
        }
        after_scheme = !after_scheme && SCHEMES.iter().any(|s| w.eq_ignore_ascii_case(s));
        rest = tail;
    }
    out
}

/// `text` (a file that could not be read as a list) with every key-like part masked: known
/// keys, address keys, a value after a credential-named string, the word after a scheme and
/// key-shaped words. Names and plain addresses stay readable.
pub fn scrub_key_like(text: &str) -> String {
    mask_key_words(&mask_named_strings(&scrub_known(text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_stays_scrubbed_on_its_thread_whatever_other_threads_register() {
        let _mine = Secret::new("fixture-own-thread-q1");
        let flood = std::thread::spawn(|| {
            for i in 0..(MAX_KNOWN * 3) {
                let _ = Secret::new(format!("fixture-flood-{i:04}"));
            }
        });
        flood.join().unwrap();
        let out = scrub_known("sent fixture-own-thread-q1 to host");
        assert_eq!(out, "sent *** to host");
    }

    #[test]
    fn a_made_secret_is_wiped_from_any_text_but_short_values_are_left() {
        let _ = Secret::new("canary-key-0042-abcdef");
        let _ = Secret::new("short");
        let out = scrub_known("fail: key canary-key-0042-abcdef was rejected; short stays");
        assert_eq!(out, "fail: key *** was rejected; short stays");
    }

    #[test]
    fn secret_never_shows_in_debug_or_display() {
        let s = Secret::new("sk-value-123");
        assert_eq!(format!("{s:?}"), "***");
        assert_eq!(format!("{s}"), "***");
        assert!(!format!("{:?}", Some(s)).contains("value"));
    }

    #[test]
    fn memory_store_round_trip_and_remove() {
        let m = MemoryStore::new();
        assert_eq!(m.get("p1").unwrap(), None);
        m.set("p1", Secret::new("a")).unwrap();
        assert_eq!(m.get("p1").unwrap().unwrap().expose(), "a");
        m.remove("p1").unwrap();
        m.remove("p1").unwrap();
        assert_eq!(m.get("p1").unwrap(), None);
    }

    #[test]
    fn address_keys_are_masked_in_any_text() {
        let line = "GET https://u:SECRETSECRET@h.test/v1?key=SECRETSECRET&max_tokens=5 failed";
        let out = redact_url_secrets(line);
        assert!(!out.contains("SECRETSECRET"), "{out}");
        assert_eq!(out, "GET https://***@h.test/v1?key=***&max_tokens=5 failed");
        for q in ["?api-key=S3CR3TS3CR3T", "?access_token=S3CR3TS3CR3T", "&sig=S3CR3TS3CR3T"] {
            let out = scrub_known(&format!("https://h.test/x{q}#f"));
            assert!(!out.contains("S3CR3TS3CR3T"), "{out}");
        }
        assert_eq!(redact_url_secrets("a & b = c, ?model=m"), "a & b = c, ?model=m");
        let done = "https://***@h.test/?key=***";
        assert_eq!(redact_url_secrets(done), done);
    }

    #[test]
    fn credential_params_are_named_not_guessed() {
        for n in ["key", "API-KEY", "api_key", "token", "access_token", "sig", "Signature"] {
            assert!(is_credential_param(n), "{n}");
        }
        for n in ["max_tokens", "model", "keys", "version", ""] {
            assert!(!is_credential_param(n), "{n}");
        }
    }

    #[test]
    fn store_is_session_only_matches_the_default_store() {
        assert_eq!(store_is_session_only(), !cfg!(any(windows, target_os = "macos")));
    }

    #[test]
    fn memory_store_debug_hides_keys() {
        let m = MemoryStore::new();
        m.set("p1", Secret::new("sk-value-123")).unwrap();
        assert!(!format!("{m:?}").contains("sk-value"));
    }

    // Fixtures only: shaped like keys, never real ones.
    const FAKE_SK: &str = "sk-fixture-r114b-not-a-real-key";
    const FAKE_JWT: &str = "eyJfixture.eyJub3RyZWFs.r114bSig";
    const FAKE_DENSE: &str = "Qz7fixtureR114bXk2Lp9W";
    const FAKE_LONG: &str = "fixture0r114b0token0not0real0key0abc";

    #[test]
    fn key_shaped_values_are_secret_and_ordinary_ones_are_not() {
        for v in [
            format!("Bearer {FAKE_DENSE}"),
            format!("basic {FAKE_LONG}"),
            FAKE_SK.to_string(),
            FAKE_JWT.to_string(),
            FAKE_DENSE.to_string(),
            FAKE_LONG.to_string(),
        ] {
            assert!(value_looks_secret(&v), "missed a fixture of {} chars", v.len());
        }
        for v in [
            "1",
            "2023-06-01",
            "application/json",
            "text/event-stream",
            "123e4567-e89b-12d3-a456-426614174000",
            "basic tier",
            "desktop-editor",
            "DDC Editor/2.0",
            "",
        ] {
            assert!(!value_looks_secret(v), "{v}");
        }
    }

    #[test]
    fn key_like_text_is_masked_and_names_and_addresses_stay() {
        let text = format!(
            "{{\"name\":\"Office server\",\"base_url\":\"https://models.example.test/v1?key=\
             {FAKE_LONG}\",\"auth\":\"Bearer\",\"extra_headers\":[[\"X-Api-Key\",\"short-1\"],\
             [\"x-trace\",\"Bearer {FAKE_DENSE}\"],[\"x-version\",\"2023-06-01\"],\
             [\"x-sig\",\"{FAKE_JWT}\"]],\"note\":\"{FAKE_SK}"
        );
        let out = scrub_key_like(&text);
        for fake in [FAKE_SK, FAKE_JWT, FAKE_DENSE, FAKE_LONG, "short-1"] {
            assert!(!out.contains(fake), "a fixture of {} chars stayed", fake.len());
        }
        for kept in [
            "Office server",
            "https://models.example.test/v1",
            "\"auth\":\"Bearer\"",
            "[\"x-version\",\"2023-06-01\"]",
            "X-Api-Key",
        ] {
            assert!(out.contains(kept), "{kept} is gone: {out}");
        }
    }
}
