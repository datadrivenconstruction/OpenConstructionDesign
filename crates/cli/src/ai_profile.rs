// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc ai-profile add|list|remove`: the assistant's connections without the window. The list is
//! the editor's own file (`ai_profiles.json`, no keys in it); the key goes to the system key store
//! under the profile id, as the settings window does, and is read from an environment variable,
//! never from the command line.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ddc_ai::profile::{blank, free_id, load_profiles, save_profiles};
use ddc_ai::{AuthKind, Profile, Protocol, Secret, SecretStore};
use serde_json::json;

use crate::{EXIT_FAILED, EXIT_OK, LOG_SCHEMA};

/// The variable the key is read from unless `--key-env` names another.
pub const KEY_ENV: &str = "DDC_AI_KEY";

#[derive(Clone, Debug, PartialEq)]
pub enum Args {
    List,
    Add(AddArgs),
    Remove { which: String },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AddArgs {
    pub name: String,
    pub preset: Option<String>,
    pub url: Option<String>,
    pub model: Option<String>,
    pub protocol: Option<String>,
    pub auth: Option<String>,
    pub price_in: Option<f64>,
    pub price_out: Option<f64>,
    pub context: Option<u32>,
    pub max_output: Option<u32>,
    pub key_env: Option<String>,
}

pub const USAGE: &str = "\
  ddc ai-profile list
  ddc ai-profile add --name <name> [--preset gateway|local] [--url <address>] [--model <id>]
      [--protocol chat|blocks] [--auth bearer|none|header:<name>|query:<name>]
      [--price-in <per 1M tokens>] [--price-out <per 1M tokens>] [--context <tokens>]
      [--max-output <tokens>] [--key-env <VAR>]
      The key is read from DDC_AI_KEY (or the variable named by --key-env) and kept in the
      system key store, never in the list file.
  ddc ai-profile remove <id or name>
      Removes the connection and its stored key.
";

pub(crate) fn value(it: &mut dyn Iterator<Item = String>, flag: &str) -> Result<String, String> {
    it.next().ok_or(format!("{flag} needs a value"))
}

pub(crate) fn number(it: &mut dyn Iterator<Item = String>, flag: &str) -> Result<f64, String> {
    // num-lint: command-line flag, invariant form
    value(it, flag)?
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("{flag} needs a number"))
}

/// `rest` is what follows `ai-profile`; gives the command, `--json` and `--quiet`.
pub fn parse(rest: &[OsString]) -> Result<(Args, bool, bool), String> {
    let (mut json, mut quiet) = (false, false);
    let mut words: Vec<String> = vec![];
    let mut a = AddArgs::default();
    let mut it = rest.iter().map(|s| s.to_string_lossy().into_owned());
    let it: &mut dyn Iterator<Item = String> = &mut it;
    while let Some(w) = it.next() {
        match w.as_str() {
            "--json" => json = true,
            "--quiet" | "-q" => quiet = true,
            "--name" => a.name = value(it, "--name")?,
            "--preset" => a.preset = Some(value(it, "--preset")?),
            "--url" => a.url = Some(value(it, "--url")?),
            "--model" => a.model = Some(value(it, "--model")?),
            "--protocol" => a.protocol = Some(value(it, "--protocol")?),
            "--auth" => a.auth = Some(value(it, "--auth")?),
            "--key-env" => a.key_env = Some(value(it, "--key-env")?),
            "--price-in" => a.price_in = Some(number(it, "--price-in")?),
            "--price-out" => a.price_out = Some(number(it, "--price-out")?),
            "--context" => a.context = Some(number(it, "--context")? as u32),
            "--max-output" => a.max_output = Some(number(it, "--max-output")? as u32),
            f if f.starts_with("--") => return Err(format!("unknown option {f} for ai-profile")),
            _ => words.push(w),
        }
    }
    let args = match words.first().map(String::as_str) {
        Some("list") => Args::List,
        Some("add") if a.name.trim().is_empty() => return Err("ai-profile add needs --name".into()),
        Some("add") => Args::Add(a),
        Some("remove") => Args::Remove {
            which: words
                .get(1)
                .cloned()
                .ok_or("ai-profile remove needs an id or a name")?,
        },
        _ => return Err(format!("ai-profile needs add, list or remove\n{USAGE}")),
    };
    Ok((args, json, quiet))
}

fn protocol_of(s: &str) -> Result<Protocol, String> {
    match s {
        "chat" => Ok(Protocol::ChatCompletions),
        "blocks" => Ok(Protocol::ContentBlocks),
        _ => Err(format!("unknown protocol {s}: chat or blocks")),
    }
}

fn auth_of(s: &str) -> Result<AuthKind, String> {
    match s.split_once(':') {
        None if s == "bearer" => Ok(AuthKind::Bearer),
        None if s == "none" => Ok(AuthKind::None),
        Some(("header", n)) if !n.trim().is_empty() => Ok(AuthKind::Header {
            name: n.trim().into(),
        }),
        Some(("query", n)) if !n.trim().is_empty() => Ok(AuthKind::Query {
            name: n.trim().into(),
        }),
        _ => Err(format!(
            "unknown auth {s}: bearer, none, header:<name> or query:<name>"
        )),
    }
}

/// The profile `a` describes, with a free id among `existing`.
pub fn build(a: &AddArgs, existing: &[Profile]) -> Result<Profile, String> {
    let ids: Vec<String> = existing.iter().map(|p| p.id.clone()).collect();
    let mut p = blank(free_id(&ids), a.name.trim().to_string());
    match a.preset.as_deref() {
        None => {}
        Some("gateway") => p.base_url = ddc_ai::gateway::GATEWAY_URL.into(),
        Some("local") => {
            p.base_url = "http://localhost:11434/v1".into();
            p.auth = AuthKind::None;
        }
        Some(o) => return Err(format!("unknown preset {o}: gateway or local")),
    }
    if let Some(u) = &a.url {
        p.base_url = u.trim().to_string();
    }
    if let Some(m) = &a.model {
        p.model = m.trim().to_string();
    }
    if let Some(s) = &a.protocol {
        p.protocol = protocol_of(s)?;
    }
    if let Some(s) = &a.auth {
        p.auth = auth_of(s)?;
    }
    p.price_in_per_mtok = a.price_in.or(p.price_in_per_mtok);
    p.price_out_per_mtok = a.price_out.or(p.price_out_per_mtok);
    p.context_tokens = a.context.unwrap_or(p.context_tokens);
    p.max_output_tokens = a.max_output.unwrap_or(p.max_output_tokens);
    let url = p.base_url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(
            "the connection needs an http:// or https:// address (--url or --preset)".into(),
        );
    }
    if ddc_ai::profile::split_url_secret(url).is_some() {
        return Err(
            "the address carries a key: pass the key through the environment instead".into(),
        );
    }
    if p.model.is_empty() {
        return Err("the connection needs --model".into());
    }
    if existing.iter().any(|o| o.name == p.name) {
        return Err(format!(
            "a connection named {} exists; remove it first",
            p.name
        ));
    }
    Ok(p)
}

/// The command against the list in `dir` and the key store `keys`; `env` reads a variable.
pub fn run_in(
    a: &Args,
    dir: &Path,
    keys: &dyn SecretStore,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(String, serde_json::Value), String> {
    let mut list = load_profiles(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    match a {
        Args::List => {
            let rows: Vec<_> = list
                .iter()
                .map(|p| {
                    let stored = matches!(keys.get(&p.id), Ok(Some(_)));
                    json!({"id": p.id, "name": p.name, "url": p.base_url, "model": p.model,
                           "key_stored": stored,
                           "price_in": p.price_in_per_mtok, "price_out": p.price_out_per_mtok})
                })
                .collect();
            let text = list
                .iter()
                .zip(&rows)
                .map(|(p, r)| {
                    let key = if r["key_stored"] == true {
                        "key stored"
                    } else if p.auth == AuthKind::None {
                        "no key"
                    } else {
                        "no key stored"
                    };
                    format!(
                        "{}  {}  {}  {}  ({key})\n",
                        p.id, p.name, p.base_url, p.model
                    )
                })
                .collect::<String>();
            let text = if text.is_empty() {
                "no connections\n".to_string()
            } else {
                text
            };
            Ok((text, json!({"profiles": rows})))
        }
        Args::Add(add) => {
            let p = build(add, &list)?;
            let var = add.key_env.as_deref().unwrap_or(KEY_ENV);
            let key = env(var).filter(|k| !k.trim().is_empty());
            if p.auth != AuthKind::None
                && key.is_none()
                && !ddc_ai::egress::is_local_url(&p.base_url)
            {
                return Err(format!("set {var} to the key of this connection"));
            }
            let mut note = String::new();
            if let (Some(k), true) = (key, p.auth != AuthKind::None) {
                if let Err(e) = keys.set(&p.id, Secret::new(k.trim())) {
                    return Err(format!("the key was not stored: {e}"));
                }
                note = ", key stored".into();
            }
            list.push(p.clone());
            save_profiles(dir, &list).map_err(|e| e.to_string())?;
            Ok((
                format!("ok: added {} ({}){note}\n", p.name, p.id),
                json!({"added": p.id, "name": p.name, "key_stored": !note.is_empty()}),
            ))
        }
        Args::Remove { which } => {
            let i = list
                .iter()
                .position(|p| p.id == *which || p.name == *which)
                .ok_or(format!("no connection {which}"))?;
            let p = list.remove(i);
            save_profiles(dir, &list).map_err(|e| e.to_string())?;
            keys.remove(&p.id)
                .map_err(|e| format!("the stored key was not removed: {e}"))?;
            Ok((
                format!("ok: removed {} ({})\n", p.name, p.id),
                json!({"removed": p.id}),
            ))
        }
    }
}

pub fn data_dir() -> Result<PathBuf, String> {
    ddc_ai::profile::default_data_dir()
        .ok_or_else(|| "the system gives no folder for settings".to_string())
}

pub fn run(a: &Args, json_out: bool, _quiet: bool) -> (String, i32) {
    let store = ddc_ai::secret::default_store();
    let env = |k: &str| std::env::var(k).ok();
    let r = data_dir().and_then(|d| run_in(a, &d, &*store, &env));
    let code = if r.is_ok() { EXIT_OK } else { EXIT_FAILED };
    if json_out {
        let j = match &r {
            Ok((_, v)) => {
                json!({"schema": LOG_SCHEMA, "command": "ai-profile", "exit": code, "result": v})
            }
            Err(e) => {
                json!({"schema": LOG_SCHEMA, "command": "ai-profile", "exit": code, "error": e})
            }
        };
        return (j.to_string(), code);
    }
    match r {
        Ok((t, _)) => (t, code),
        Err(e) => (
            format!("error: {}\n", ddc_ai::secret::scrub_known(&e)),
            code,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ai::MemoryStore;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    fn args(v: &[&str]) -> Args {
        parse(&os(v)).unwrap().0
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_cli_aiprof_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn add_keeps_the_key_in_the_store_and_out_of_the_file() {
        let (dir, keys) = (tmp("add"), MemoryStore::new());
        let env = |k: &str| (k == KEY_ENV).then(|| "sk-test-secret-123".to_string());
        let a = args(&[
            "add",
            "--name",
            "gw",
            "--preset",
            "gateway",
            "--model",
            "vendor/m",
            "--price-in",
            "3",
        ]);
        run_in(&a, &dir, &keys, &env).unwrap();
        let file = std::fs::read_to_string(ddc_ai::profile::profiles_path(&dir)).unwrap();
        assert!(!file.contains("sk-test-secret-123"), "{file}");
        let list = load_profiles(&dir).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].base_url, ddc_ai::gateway::GATEWAY_URL);
        assert_eq!(list[0].price_in_per_mtok, Some(3.0));
        assert_eq!(
            keys.get(&list[0].id).unwrap(),
            Some(Secret::new("sk-test-secret-123"))
        );
        let (text, _) = run_in(&args(&["list"]), &dir, &keys, &env).unwrap();
        assert!(
            text.contains("gw") && text.contains("key stored") && !text.contains("sk-test"),
            "{text}"
        );
        let again = run_in(&a, &dir, &keys, &env);
        assert!(again.is_err(), "the same name twice");
        run_in(&args(&["remove", "gw"]), &dir, &keys, &env).unwrap();
        assert!(load_profiles(&dir).unwrap().is_empty());
        assert_eq!(
            keys.get(&list[0].id).unwrap(),
            None,
            "the key leaves with the connection"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_without_a_key_or_with_a_key_in_the_address_is_refused() {
        let (dir, keys) = (tmp("nokey"), MemoryStore::new());
        let none = |_: &str| None;
        let a = args(&[
            "add",
            "--name",
            "x",
            "--url",
            "https://models.example.test/v1",
            "--model",
            "m",
        ]);
        assert!(run_in(&a, &dir, &keys, &none)
            .unwrap_err()
            .contains(KEY_ENV));
        let a = args(&[
            "add",
            "--name",
            "x",
            "--url",
            "https://h.example.test/v1?key=sk-abcdefabcdef",
            "--model",
            "m",
        ]);
        assert!(run_in(&a, &dir, &keys, &|_| Some("k".into())).is_err());
        let a = args(&["add", "--name", "l", "--preset", "local", "--model", "m"]);
        run_in(&a, &dir, &keys, &none).unwrap();
        assert!(parse(&os(&["add"])).is_err(), "a name is needed");
        assert!(parse(&os(&["nope"])).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
