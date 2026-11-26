// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Signed bridge package: the manifest lists every file with its SHA-256 and is signed as a whole.
//! Before each run the listed files (and only those) are copied into a private per-run folder,
//! locked against writes, hashed and compared with the manifest; the copy is what runs, so a swap
//! of the installed files after the check cannot change the executed bytes, and the loader finds
//! no unsigned library next to the executable.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use web_time::Instant;

use sha2::{Digest, Sha256};

use super::discover::TrustStore;

pub const MANIFEST: &str = "bridge.json";
pub const SIGNATURE: &str = "bridge.json.minisig";
pub const MAX_MANIFEST: u64 = 1 << 20;
pub const MAX_FILES: usize = 256;
pub const MAX_FILE: u64 = 1 << 30;
pub const MAX_TOTAL: u64 = 2 << 30;

#[derive(Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub sha256: [u8; 32],
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub exe: String,
    pub formats: Vec<String>,
    pub files: Vec<FileEntry>,
}

/// A package whose manifest signature, trusted comment, policy and folder listing passed.
#[derive(Clone, Debug)]
pub struct Verified {
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub comment: String,
}

fn token_ok(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Dotted numeric version, 1–4 parts ("1.2.3").
pub fn parse_version(v: &str) -> Option<Vec<u64>> {
    let p: Vec<u64> = v
        .split('.')
        .map(|x| {
            (!x.is_empty() && x.len() <= 9 && x.bytes().all(|c| c.is_ascii_digit()))
                .then(|| x.parse().ok())
                .flatten()
        })
        .collect::<Option<_>>()?;
    (1..=4).contains(&p.len()).then_some(p)
}

/// Bare file name, no device names, not the manifest or its signature.
fn file_name_ok(n: &str) -> bool {
    let stem = n.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    token_ok(n)
        && !n.starts_with('.')
        && !n.ends_with('.')
        && !device
        && !n.eq_ignore_ascii_case(MANIFEST)
        && !n.eq_ignore_ascii_case(SIGNATURE)
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    let j: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| "manifest is not JSON")?;
    let s = |k: &str| j[k].as_str().ok_or(format!("manifest: no '{k}'"));
    let (name, version, exe) = (s("name")?, s("version")?, s("exe")?);
    if !token_ok(name) {
        return Err("manifest: bad name".into());
    }
    if parse_version(version).is_none() {
        return Err("manifest: bad version".into());
    }
    if j["protocol"].as_u64() != Some(u64::from(ddc_bridge_proto::PROTOCOL)) {
        return Err("manifest: unsupported protocol".into());
    }
    let mut formats = vec![];
    for f in j["formats"].as_array().ok_or("manifest: no 'formats'")? {
        let f = f
            .as_str()
            .ok_or("manifest: bad format")?
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if f.is_empty() || f.len() > 16 || !f.bytes().all(|c| c.is_ascii_alphanumeric()) {
            return Err("manifest: bad format".into());
        }
        formats.push(f);
    }
    let arr = j["files"].as_array().ok_or("manifest: no 'files'")?;
    if arr.is_empty() || arr.len() > MAX_FILES {
        return Err("manifest: file count".into());
    }
    let (mut files, mut total) = (Vec::<FileEntry>::new(), 0u64);
    for f in arr {
        let path = f["path"].as_str().unwrap_or_default();
        if !file_name_ok(path) {
            return Err(format!("manifest: bad file name '{path}'"));
        }
        if files.iter().any(|x| x.path.eq_ignore_ascii_case(path)) {
            return Err(format!("manifest: '{path}' listed twice"));
        }
        let sha256 = f["sha256"]
            .as_str()
            .and_then(hex32)
            .ok_or("manifest: sha256 must be 64 lower-case hex digits")?;
        let size = f["size"]
            .as_u64()
            .filter(|&n| n <= MAX_FILE)
            .ok_or("manifest: file size")?;
        total += size;
        files.push(FileEntry {
            path: path.into(),
            sha256,
            size,
        });
    }
    if total > MAX_TOTAL {
        return Err("manifest: package too large".into());
    }
    if !files.iter().any(|f| f.path == exe) {
        return Err("manifest: 'exe' is not among 'files'".into());
    }
    Ok(Manifest {
        name: name.into(),
        version: version.into(),
        exe: exe.into(),
        formats,
        files,
    })
}

/// The trusted comment a signature must carry: binds name, version and formats to the key.
pub fn trusted_comment(m: &Manifest) -> String {
    format!(
        "ddc-bridge name={} version={} formats={}",
        m.name,
        m.version,
        m.formats.join(",")
    )
}

/// UNC / network paths are refused: their content can change under a lock we cannot hold.
fn is_remote(p: &Path) -> bool {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy();
    s.starts_with(r"\\?\UNC\") || (s.starts_with(r"\\") && !s.starts_with(r"\\?\"))
}

/// Regular local file (no link, pipe, device or network share).
fn regular(p: &Path) -> Result<(), String> {
    let m = std::fs::symlink_metadata(p).map_err(|_| format!("{}: missing", p.display()))?;
    if !m.file_type().is_file() {
        return Err(format!("{}: not a regular file", p.display()));
    }
    if is_remote(p) {
        return Err(format!("{}: network location", p.display()));
    }
    Ok(())
}

/// Whole file of at most `max` bytes, regular files only.
pub fn read_small(p: &Path, max: u64) -> Result<Vec<u8>, String> {
    regular(p)?;
    let f = File::open(p).map_err(|e| e.to_string())?;
    if !f.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("{}: not a regular file", p.display()));
    }
    let mut out = vec![];
    f.take(max + 1)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() as u64 > max {
        return Err(format!("{}: too large", p.display()));
    }
    Ok(out)
}

/// Checks the signed manifest and that the folder holds nothing but listed files.
pub fn verify_manifest(dir: &Path, trust: &TrustStore) -> Result<Verified, String> {
    if is_remote(dir) {
        return Err("package is on a network location".into());
    }
    let bytes = read_small(&dir.join(MANIFEST), MAX_MANIFEST)?;
    let sig = read_small(&dir.join(SIGNATURE), 4096).map_err(|_| "no signature file")?;
    let sig = String::from_utf8(sig).map_err(|_| "signature is not text")?;
    let comment = trust.verify(&bytes, &sig)?;
    let m = parse_manifest(&bytes)?;
    if comment != trusted_comment(&m) {
        return Err("trusted comment does not match the manifest".into());
    }
    trust.policy.check(&m.name, &m.version)?;
    for e in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        let known = n == MANIFEST || n == SIGNATURE || m.files.iter().any(|f| f.path == n);
        if !known {
            return Err(format!("unsigned file in package: {n}"));
        }
    }
    Ok(Verified {
        dir: dir.to_path_buf(),
        manifest: m,
        comment,
    })
}

/// Private per-run copy; removed on drop (after the bridge process has exited).
#[derive(Debug)]
pub struct Staged {
    pub dir: PathBuf,
    pub exe: PathBuf,
    locks: Vec<File>,
}

impl Drop for Staged {
    fn drop(&mut self) {
        self.locks.clear();
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

/// Read-only handle that forbids writers for as long as it is open (Windows share mode).
fn open_locked(p: &Path) -> std::io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0x1); // FILE_SHARE_READ only
    }
    o.open(p)
}

fn sha256_of(f: &mut File, deadline: Option<Instant>) -> Result<([u8; 32], u64), String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err("verification timed out".into());
        }
        let k = f.read(&mut buf).map_err(|e| e.to_string())?;
        if k == 0 {
            return Ok((h.finalize().into(), n));
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
}

fn unique_name() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(N.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    format!("ddc-bridge-run-{}-{:016x}", std::process::id(), h.finish())
}

/// Folder only the current user can open (Windows: protected DACL with one ACE for the user's SID;
/// elsewhere mode 0700). Fails if the name exists, so a pre-planted folder is never reused.
#[cfg(windows)]
fn create_private_dir(p: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let err = std::io::Error::last_os_error;
    // SAFETY: plain Win32 calls; every buffer outlives its use and every allocation is freed.
    unsafe {
        let mut tok: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return Err(err());
        }
        let mut buf = vec![0u64; 128];
        let mut len = 0u32;
        let ok = GetTokenInformation(
            tok,
            TokenUser,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut len,
        );
        CloseHandle(tok);
        if ok == 0 {
            return Err(err());
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut sid_w: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid_w) == 0 {
            return Err(err());
        }
        let mut n = 0;
        while *sid_w.add(n) != 0 {
            n += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_w, n));
        LocalFree(sid_w.cast());
        let sddl: Vec<u16> = format!("D:P(A;OICI;FA;;;{sid})\0").encode_utf16().collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(err());
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let w: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
        let ok = CreateDirectoryW(w.as_ptr(), &sa);
        let e = err();
        LocalFree(sd);
        if ok == 0 {
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(p)
}

/// No permission bits here (browser build): a plain folder, which there is no disk for anyway.
#[cfg(not(any(unix, windows)))]
fn create_private_dir(p: &Path) -> std::io::Result<()> {
    std::fs::create_dir(p)
}

/// Copies the listed files into a fresh private folder, locks and hashes the copies.
pub fn stage(v: &Verified, deadline: Option<Instant>) -> Result<Staged, String> {
    let dir = std::env::temp_dir().join(unique_name());
    create_private_dir(&dir).map_err(|e| format!("private folder: {e}"))?;
    let mut st = Staged {
        exe: dir.join(&v.manifest.exe),
        dir,
        locks: vec![],
    };
    for f in &v.manifest.files {
        let src = v.dir.join(&f.path);
        regular(&src)?;
        let from = File::open(&src).map_err(|e| format!("{}: {e}", f.path))?;
        if !from.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err(format!("{}: not a regular file", f.path));
        }
        let dst = st.dir.join(&f.path);
        let mut to = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dst)
            .map_err(|e| format!("{}: {e}", f.path))?;
        std::io::copy(&mut from.take(f.size + 1), &mut to).map_err(|e| e.to_string())?;
        drop(to);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o700));
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err("verification timed out".into());
        }
    }
    // hash what will run, through handles that keep writers out until the run ends
    for f in &v.manifest.files {
        let mut h = open_locked(&st.dir.join(&f.path)).map_err(|e| e.to_string())?;
        let (sum, n) = sha256_of(&mut h, deadline)?;
        if n != f.size || sum != f.sha256 {
            return Err(format!("{}: does not match the signed manifest", f.path));
        }
        st.locks.push(h);
    }
    Ok(st)
}

/// Verify + stage: what [`super::import_trusted`] and the background import run.
pub fn prepare(
    dir: &Path,
    trust: &TrustStore,
    deadline: Option<Instant>,
) -> Result<Staged, String> {
    let v = verify_manifest(dir, trust)?;
    stage(&v, deadline)
}

/// Manifest text for `files` in `dir` (signing tools and tests): name, version, exe, formats.
pub fn manifest_for(
    dir: &Path,
    name: &str,
    version: &str,
    exe: &str,
    formats: &[&str],
    files: &[&str],
) -> std::io::Result<String> {
    let mut list = vec![];
    for f in files {
        let b = std::fs::read(dir.join(f))?;
        list.push(serde_json::json!({
            "path": f,
            "sha256": hex(&Sha256::digest(&b)),
            "size": b.len(),
        }));
    }
    Ok(serde_json::json!({
        "name": name,
        "version": version,
        "exe": exe,
        "protocol": ddc_bridge_proto::PROTOCOL,
        "formats": formats,
        "files": list,
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_rules() {
        let ok = |files: &str| {
            format!(
                r#"{{"name":"s","version":"1.2","exe":"a.exe","protocol":1,"formats":["RVT"],"files":{files}}}"#
            )
        };
        let f = |p: &str| format!(r#"{{"path":"{p}","sha256":"{}","size":1}}"#, "0".repeat(64));
        let m = parse_manifest(ok(&format!("[{}]", f("a.exe"))).as_bytes()).unwrap();
        assert_eq!(m.formats, vec!["rvt"]);
        assert_eq!(
            trusted_comment(&m),
            "ddc-bridge name=s version=1.2 formats=rvt"
        );
        for bad in [
            "../a.exe",
            "sub/a.exe",
            "NUL.dll",
            "bridge.json",
            ".hidden",
            "a b",
        ] {
            let j = ok(&format!("[{},{}]", f("a.exe"), f(bad)));
            assert!(parse_manifest(j.as_bytes()).is_err(), "{bad}");
        }
        assert!(parse_manifest(ok(&format!("[{}]", f("b.exe"))).as_bytes()).is_err());
        assert!(
            parse_manifest(ok(&format!("[{},{}]", f("a.exe"), f("A.EXE"))).as_bytes()).is_err()
        );
        assert_eq!(parse_version("1.10.0"), Some(vec![1, 10, 0]));
        assert!(parse_version("1.x").is_none() && parse_version("").is_none());
        assert!(parse_version("1.2.3.4.5").is_none());
    }

    #[test]
    fn private_dir_is_owner_only() {
        let d = std::env::temp_dir().join(unique_name());
        create_private_dir(&d).unwrap();
        assert!(
            create_private_dir(&d).is_err(),
            "existing name is never reused"
        );
        #[cfg(windows)]
        {
            let out = std::process::Command::new("icacls")
                .arg(&d)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            let aces = text.lines().filter(|l| l.contains(":(")).count();
            assert_eq!(aces, 1, "{text}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(&d).unwrap().permissions().mode();
            assert_eq!(m & 0o777, 0o700);
        }
        std::fs::remove_dir(&d).unwrap();
    }
}
