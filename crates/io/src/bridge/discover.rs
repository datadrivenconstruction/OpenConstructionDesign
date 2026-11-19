//! Installed bridges and the trust root. A bridge is a signed package (docs/plugins/BRIDGE_PROTOCOL.md §1):
//! `<bridges dir>/<name>/bridge.json` lists every file with its SHA-256, `bridge.json.minisig` signs it.
//! Keys come only from [`OFFICIAL_KEYS`] compiled in and from the developer list the user confirmed
//! in the UI, stored in the config dir; the bridges dir (and `DDC_BRIDGE_DIR`) never supplies keys.

use std::path::{Path, PathBuf};

use super::package;
use super::sig::PublicKey;

/// Official release keys (minisign public-key lines). Empty in the open repo until the first
/// signed release; release builds add theirs here.
pub const OFFICIAL_KEYS: &[&str] = &[];
/// Key ids that no longer sign anything (compromised or retired).
pub const REVOKED_KEYS: &[[u8; 8]] = &[];
/// (bridge name, lowest accepted version): older signed packages are refused.
pub const MIN_VERSIONS: &[(&str, &str)] = &[];
/// (bridge name, version) pairs withdrawn after release.
pub const REVOKED_VERSIONS: &[(&str, &str)] = &[];

/// Minimum-version and revocation rules applied after a signature verifies.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub revoked_keys: Vec<[u8; 8]>,
    pub min_versions: Vec<(String, String)>,
    pub revoked_versions: Vec<(String, String)>,
}

impl Policy {
    pub fn builtin() -> Self {
        let own = |v: &[(&str, &str)]| {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        Self {
            revoked_keys: REVOKED_KEYS.to_vec(),
            min_versions: own(MIN_VERSIONS),
            revoked_versions: own(REVOKED_VERSIONS),
        }
    }

    pub fn check(&self, name: &str, version: &str) -> Result<(), String> {
        let v = package::parse_version(version).ok_or("bad version")?;
        if self
            .revoked_versions
            .iter()
            .any(|(n, x)| n == name && x == version)
        {
            return Err(format!("version {version} is revoked"));
        }
        for (n, min) in &self.min_versions {
            if n == name && package::parse_version(min).is_some_and(|m| v < m) {
                return Err(format!("version {version} is older than the minimum {min}"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    keys: Vec<PublicKey>,
    pub policy: Policy,
}

impl TrustStore {
    pub fn official() -> Self {
        Self::from_keys(
            OFFICIAL_KEYS
                .iter()
                .filter_map(|k| PublicKey::parse(k).ok())
                .collect(),
        )
    }

    /// Exactly `keys` with the built-in policy (official + developer-confirmed keys, or tests).
    pub fn from_keys(keys: Vec<PublicKey>) -> Self {
        Self {
            keys,
            policy: Policy::builtin(),
        }
    }

    /// Adds keys the user confirmed in developer mode (see [`load_developer_keys`]).
    pub fn add_developer(&mut self, keys: Vec<PublicKey>) {
        for k in keys {
            if !self.keys.contains(&k) {
                self.keys.push(k);
            }
        }
    }

    pub fn keys(&self) -> &[PublicKey] {
        &self.keys
    }

    /// Ok(trusted comment) if `sig` is a valid signature of `data` by a non-revoked key.
    pub fn verify(&self, data: &[u8], sig: &str) -> Result<String, String> {
        let mut last = "no trusted keys".to_string();
        for k in &self.keys {
            if self.policy.revoked_keys.contains(&k.key_id) {
                last = "signing key is revoked".into();
                continue;
            }
            match k.verify(data, sig) {
                Ok(c) => return Ok(c),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

#[derive(Clone, Debug)]
pub struct Bridge {
    pub name: String,
    pub version: String,
    /// Package folder; it is verified again and copied before every run.
    pub dir: PathBuf,
    pub formats: Vec<String>,
    /// Ok(trusted comment) or why the package must not run.
    pub trust: Result<String, String>,
}

/// `%APPDATA%\ddc-editor`, else `$XDG_CONFIG_HOME|~/.config/ddc-editor`. Never `DDC_BRIDGE_DIR`.
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ddc-editor"))
}

/// `DDC_BRIDGE_DIR`, else `<config dir>/bridges`.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("DDC_BRIDGE_DIR") {
        return Some(d.into());
    }
    Some(config_dir()?.join("bridges"))
}

/// Developer-confirmed keys file, `<config dir>/developer_keys.txt`.
pub fn developer_keys_path() -> Option<PathBuf> {
    Some(config_dir()?.join("developer_keys.txt"))
}

/// Keys from the developer list, one key line each. Empty when `path` lies inside `bridges`:
/// whoever can drop a package must not also be able to add its key.
pub fn load_developer_keys(path: &Path, bridges: Option<&Path>) -> Vec<PublicKey> {
    let inside = |b: &Path| {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let parent = path.parent().map(canon).unwrap_or_default();
        parent.starts_with(canon(b))
    };
    if bridges.is_some_and(inside) {
        return vec![];
    }
    let Ok(text) = package::read_small(path, 64 * 1024) else {
        return vec![];
    };
    String::from_utf8_lossy(&text)
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| PublicKey::parse(l).ok())
        .collect()
}

/// Appends `key_line` to the developer list after the user confirmed it in the UI.
pub fn confirm_developer_key(path: &Path, key_line: &str) -> Result<PublicKey, String> {
    let k = PublicKey::parse(key_line)?;
    let line = key_line
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
        .unwrap_or_default();
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{line}").map_err(|e| e.to_string())?;
    Ok(k)
}

/// Every package under `dir`; only the manifest signature and the folder listing are checked here
/// (cheap), file hashes are checked on the private copy right before each run.
pub fn discover(dir: &Path, trust: &TrustStore) -> Vec<Bridge> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut subs: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    subs.sort();
    for sub in subs {
        // unsigned manifests still show (as "signature not confirmed") when they parse
        let Ok(bytes) = package::read_small(&sub.join(package::MANIFEST), package::MAX_MANIFEST)
        else {
            continue;
        };
        let Ok(m) = package::parse_manifest(&bytes) else {
            continue;
        };
        let trust = package::verify_manifest(&sub, trust).map(|v| v.comment);
        out.push(Bridge {
            name: m.name,
            version: m.version,
            dir: sub,
            formats: m.formats,
            trust,
        });
    }
    out
}

/// The installed bridges and the keys they were checked against: the official keys plus those
/// the person confirmed in developer mode (kept in the settings folder, never the bridge folder).
pub fn installed() -> (Vec<Bridge>, TrustStore) {
    let dir = default_dir();
    let mut trust = TrustStore::official();
    if let Some(p) = developer_keys_path() {
        trust.add_developer(load_developer_keys(&p, dir.as_deref()));
    }
    let list = dir.map(|d| discover(&d, &trust)).unwrap_or_default();
    (list, trust)
}

/// First trusted bridge reading `ext` (without dot).
pub fn for_ext<'a>(bridges: &'a [Bridge], ext: &str) -> Option<&'a Bridge> {
    let ext = ext.to_ascii_lowercase();
    bridges
        .iter()
        .find(|b| b.trust.is_ok() && b.formats.contains(&ext))
}
