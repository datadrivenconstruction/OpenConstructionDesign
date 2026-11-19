// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Build id for the About page: short commit and commit date, from the environment when the
//! builder sets them, else from git. Absent values leave the page showing `dev`.
//! `DDC_BUILD` = `<short commit>[+dirty], <build day>` names a non-release build in logs and UI;
//! the dirty flag is taken when the script reruns (the checked-out commit moves).
//! Also writes the build record kept in the executable image (see `write_build_record`).

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const RECORD_MAGIC: [u8; 16] = [
    0x89, 0xfb, 0xe4, 0x14, 0x1d, 0xdc, 0xe3, 0x0a, 0x73, 0xaf, 0xbf, 0x39, 0xd4, 0x3a, 0x79, 0xae,
];
const RECORD_KEY_ENV: &str = "DDC_BUILD_KEY";

fn hmac_sha256(key: &[u8; 32], msg: &[u8]) -> [u8; 32] {
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for (i, b) in key.iter().enumerate() {
        ipad[i] ^= b;
        opad[i] ^= b;
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(msg)
        .finalize();
    let outer = Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer);
    out
}

fn hex_bytes<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.trim();
    if s.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Record key: `DDC_BUILD_KEY` names the file, else `<APPDATA or ~/.config>/ddc-build/key`.
fn record_key() -> Option<[u8; 32]> {
    let path = match std::env::var_os(RECORD_KEY_ENV) {
        Some(p) => PathBuf::from(p),
        None => std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?
            .join("ddc-build")
            .join("key"),
    };
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    hex_bytes::<32>(&std::fs::read_to_string(path).ok()?)
}

/// 94 bytes: magic, version 1, flags (bit 0 keyed, bit 1 uncommitted changes), commit (20),
/// build time (u64 LE seconds), build id (16), HMAC-SHA256 over version..build id.
fn write_build_record() {
    println!("cargo:rerun-if-env-changed={RECORD_KEY_ENV}");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let commit = git(&["rev-parse", "HEAD"])
        .and_then(|h| hex_bytes::<20>(&h))
        .unwrap_or([0u8; 20]);
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some();
    let time = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or_else(|| {
            let now = SystemTime::now().duration_since(UNIX_EPOCH);
            now.map(|d| d.as_secs()).unwrap_or(0)
        });
    let id_src = format!(
        "{commit:?}|{time}|{}|{}|{}",
        std::process::id(),
        std::env::var("PROFILE").unwrap_or_default(),
        std::env::var("TARGET").unwrap_or_default()
    );
    let build_id = Sha256::digest(id_src.as_bytes());
    let key = record_key();
    let mut rec = Vec::with_capacity(94);
    rec.extend_from_slice(&RECORD_MAGIC);
    rec.push(1);
    rec.push(u8::from(key.is_some()) | (u8::from(dirty) << 1));
    rec.extend_from_slice(&commit);
    rec.extend_from_slice(&time.to_le_bytes());
    rec.extend_from_slice(&build_id[..16]);
    let tag = key
        .map(|k| hmac_sha256(&k, &rec[16..]))
        .unwrap_or([0u8; 32]);
    rec.extend_from_slice(&tag);
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set for build scripts"));
    std::fs::write(out.join("build_record.bin"), rec).expect("write build record");
}

/// Build day `YYYY-MM-DD` (UTC) from `SOURCE_DATE_EPOCH`, else the clock.
fn build_day() -> String {
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    // civil date from days since 1970-01-01 (proleptic Gregorian)
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Rerun only when the checked-out commit moves: HEAD, the branch ref it names and
/// packed-refs. Works in linked worktrees (per-worktree HEAD, shared refs).
fn watch_git() {
    let Some(git_dir) = git(&["rev-parse", "--git-dir"]) else { return };
    let common = git(&["rev-parse", "--git-common-dir"]).unwrap_or_else(|| git_dir.clone());
    let git_dir = std::path::PathBuf::from(git_dir);
    let common = std::path::PathBuf::from(common);
    let head = git_dir.join("HEAD");
    if head.exists() {
        println!("cargo:rerun-if-changed={}", head.display());
    }
    if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
        let p = common.join(&r);
        if p.exists() {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    }
    let packed = common.join("packed-refs");
    if packed.exists() {
        println!("cargo:rerun-if-changed={}", packed.display());
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=DDC_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=DDC_BUILD_DATE");
    println!("cargo:rerun-if-changed=build.rs");
    watch_git();
    write_build_record();
    let env = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
    let commit = env("DDC_BUILD_COMMIT").or_else(|| git(&["rev-parse", "--short=8", "HEAD"]));
    let date = env("DDC_BUILD_DATE")
        .or_else(|| git(&["log", "-1", "--format=%cs"]));
    let dirty = env("DDC_BUILD_COMMIT").is_none()
        && git(&["status", "--porcelain", "--untracked-files=no"]).is_some();
    println!(
        "cargo:rustc-env=DDC_BUILD={}{}, {}",
        commit.as_deref().unwrap_or("dev"),
        if dirty { "+dirty" } else { "" },
        build_day()
    );
    if let Some(c) = commit {
        println!("cargo:rustc-env=DDC_BUILD_COMMIT={c}");
    }
    if let Some(d) = date {
        println!("cargo:rustc-env=DDC_BUILD_DATE={d}");
    }
}
