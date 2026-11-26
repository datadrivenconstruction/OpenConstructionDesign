// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Bundle files the browser build loads on demand (feature `external-assets`): locale catalogs other
//! than en/ru (`i18n/<code>.json`) and the per-script fonts (`fonts/<file>`). The first lookup asks for
//! a path through the hook; the page fetches it, checks the manifest hash and hands the bytes to `add`.
//! Until then lookups fall back along the language chain and text draws in the Latin faces.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};

type Hook = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct Reg {
    files: BTreeMap<String, &'static [u8]>,
    asked: BTreeSet<String>,
    hook: Option<Hook>,
}

fn reg() -> &'static Mutex<Reg> {
    static R: OnceLock<Mutex<Reg>> = OnceLock::new();
    R.get_or_init(Default::default)
}

pub fn locale_path(code: &str) -> String {
    format!("i18n/{code}.json")
}

pub fn font_path(file: &str) -> String {
    format!("fonts/{file}")
}

/// Bytes of a bundle file; `None` until it arrives (asked for once).
pub fn get(path: &str) -> Option<&'static [u8]> {
    let hook = {
        let mut r = reg().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = r.files.get(path) {
            return Some(*b);
        }
        if !r.asked.insert(path.to_owned()) {
            return None;
        }
        r.hook.clone()
    };
    // outside the lock: the hook may hand the bytes back synchronously
    if let Some(h) = hook {
        h(path);
    }
    None
}

/// A fetched bundle file; kept for the session (a few per language).
pub fn add(path: &str, bytes: Vec<u8>) {
    let mut r = reg().lock().unwrap_or_else(|e| e.into_inner());
    r.files.insert(path.to_owned(), Box::leak(bytes.into_boxed_slice()));
    r.asked.remove(path);
}

/// Where requests go; paths asked before the hook was set are sent to it at once.
pub fn set_hook(hook: impl Fn(&str) + Send + Sync + 'static) {
    let (hook, pending): (Hook, Vec<String>) = {
        let mut r = reg().lock().unwrap_or_else(|e| e.into_inner());
        let h: Hook = Arc::new(hook);
        r.hook = Some(h.clone());
        (h, r.asked.iter().cloned().collect())
    };
    for p in pending {
        hook(&p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_once_then_serves_what_arrives() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let path = "i18n/test-asks-once.json";
        assert!(get(path).is_none(), "nothing before the hook");
        let s = seen.clone();
        set_hook(move |p| s.lock().unwrap().push(p.to_owned()));
        assert!(seen.lock().unwrap().iter().any(|p| p == path), "pending path sent when the hook is set");
        let before = seen.lock().unwrap().len();
        assert!(get(path).is_none());
        assert_eq!(seen.lock().unwrap().len(), before, "a path is asked for once");
        add(path, b"{}".to_vec());
        assert_eq!(get(path), Some(&b"{}"[..]));
    }
}
