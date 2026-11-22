// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Argument parsing by hand: the command set is small and fixed.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::export::Format;

#[derive(Debug)]
pub enum Cmd {
    Help,
    Version,
    Open {
        file: PathBuf,
        exports: Vec<(Format, PathBuf)>,
    },
    Info {
        file: PathBuf,
    },
    Batch {
        jobs: PathBuf,
    },
    Run(crate::run::RunArgs),
    #[cfg(feature = "link")]
    Gen(crate::gen::GenArgs),
    #[cfg(feature = "link")]
    Publish(crate::publish::Args),
}

#[derive(Debug)]
pub struct Parsed {
    pub cmd: Cmd,
    pub json: bool,
    pub quiet: bool,
}

pub fn parse(argv: &[OsString]) -> Result<Parsed, String> {
    let (mut json, mut quiet) = (false, false);
    let mut pos: Vec<OsString> = vec![];
    let mut exports = vec![];
    let (mut out, mut level, mut allow_many) = (None, None, false);
    let (mut brief, mut force) = (None, false);
    let mut metrics = None;
    let (mut sheets, mut ifc) = (false, false);
    let mut kv: Vec<(String, String)> = vec![];
    let mut params = vec![];
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.to_str().unwrap_or_default() {
            "--json" => json = true,
            "--quiet" | "-q" => quiet = true,
            "--help" | "-h" => {
                return Ok(Parsed {
                    cmd: Cmd::Help,
                    json,
                    quiet,
                })
            }
            "--version" | "-V" => {
                return Ok(Parsed {
                    cmd: Cmd::Version,
                    json,
                    quiet,
                })
            }
            "--export" => {
                let need = "--export needs a format and an output path";
                let f = it.next().ok_or(need)?.to_string_lossy();
                let o = it.next().ok_or(need)?;
                exports.push((Format::parse(&f)?, PathBuf::from(o)));
            }
            "--out" | "-o" => out = Some(PathBuf::from(it.next().ok_or("--out needs a file")?)),
            "--level" => level = Some(it.next().ok_or("--level needs a name")?.to_string_lossy().into_owned()),
            "--allow-many" => allow_many = true,
            "--brief" => brief = Some(PathBuf::from(it.next().ok_or("--brief needs a file")?)),
            "--metrics" => metrics = Some(PathBuf::from(it.next().ok_or("--metrics needs a file")?)),
            "--force" => force = true,
            "--sheets" => sheets = true,
            "--ifc" => ifc = true,
            o @ ("--parent" | "--depth" | "--name" | "--author" | "--message" | "--project-id" | "--erp-url" | "--revision-id"
            | "--created-at" | "--imported-from") => {
                let v = it.next().ok_or(format!("{o} needs a value"))?.to_string_lossy().into_owned();
                kv.push((o[2..].to_string(), v));
            }
            "--param" => {
                let v = it.next().ok_or("--param needs name=value")?.to_string_lossy();
                let (k, v) = v.split_once('=').ok_or("--param needs name=value")?;
                params.push((k.to_string(), v.to_string()));
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => pos.push(a.clone()),
        }
    }
    let Some(name) = pos.first().map(|p| p.to_string_lossy().into_owned()) else {
        return Ok(Parsed {
            cmd: Cmd::Help,
            json,
            quiet,
        });
    };
    let one = |what: &str| -> Result<PathBuf, String> {
        match &pos[1..] {
            [p] => Ok(PathBuf::from(p)),
            [] => Err(format!("{name}: missing {what}")),
            _ => Err(format!("{name}: too many arguments")),
        }
    };
    let no_export = || {
        if exports.is_empty() {
            Ok(())
        } else {
            Err(format!("{name}: --export is only for open"))
        }
    };
    let cmd = match name.as_str() {
        "open" => {
            let file = one("file")?;
            if exports.is_empty() {
                return Err("open: at least one --export <fmt> <out> is required".into());
            }
            Cmd::Open { file, exports }
        }
        "info" => {
            no_export()?;
            Cmd::Info { file: one("file")? }
        }
        "batch" => {
            no_export()?;
            Cmd::Batch {
                jobs: one("jobs file")?,
            }
        }
        "run" => match &pos[1..] {
            [project, script] => Cmd::Run(crate::run::RunArgs {
                project: PathBuf::from(project),
                script: PathBuf::from(script),
                out: (out.ok_or("run: --out <new file> is required")?),
                params,
                level,
                allow_many,
            }),
            _ => return Err("run: expected <project> <script.lua>".into()),
        },
        "gen" => {
            if pos.len() > 1 {
                return Err("gen: unexpected argument, use --brief <file> --out <model.ddc>".into());
            }
            #[cfg(feature = "link")]
            {
                Cmd::Gen(crate::gen::GenArgs {
                    brief: brief.ok_or("gen: --brief <brief.json> is required")?,
                    out: out.ok_or("gen: --out <model.ddc> is required")?,
                    force,
                    metrics,
                })
            }
            #[cfg(not(feature = "link"))]
            {
                let _ = (&brief, force, &metrics);
                return Err("gen: this build has no ERP link (build with --features link)".into());
            }
        }
        "publish" => {
            #[cfg(feature = "link")]
            {
                let get = |k: &str| kv.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.clone());
                Cmd::Publish(crate::publish::Args {
                    file: one("file.ddc")?,
                    out: out.ok_or("publish: --out <rev.ddcrev> is required")?,
                    parent: get("parent").map(PathBuf::from),
                    sheets,
                    ifc,
                    force,
                    depth: get("depth"),
                    name: get("name"),
                    author: get("author"),
                    message: get("message"),
                    project_id: get("project-id"),
                    erp_url: get("erp-url"),
                    brief,
                    imported_from: get("imported-from"),
                    revision_id: get("revision-id"),
                    created_at: get("created-at"),
                })
            }
            #[cfg(not(feature = "link"))]
            {
                let _ = (&brief, force, sheets, ifc, &kv);
                return Err("publish: this build has no ERP link (build with --features link)".into());
            }
        }
        other => return Err(format!("unknown command '{other}'")),
    };
    Ok(Parsed { cmd, json, quiet })
}
