// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use ddc_cli::args::{parse, Cmd};
use ddc_cli::{cmd, run, EXIT_FAILED, HELP};

fn main() {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let p = match parse(&argv) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}\nrun `ddc --help` for the commands and options");
            std::process::exit(EXIT_FAILED);
        }
    };
    let (text, code) = match p.cmd {
        Cmd::Help => (HELP.to_string(), 0),
        Cmd::Version => (format!("ddc {}\n", env!("CARGO_PKG_VERSION")), 0),
        Cmd::Open { file, exports } => {
            let job = cmd::Job {
                id: None,
                file,
                info: false,
                exports,
            };
            cmd::open(&job, p.json, p.quiet)
        }
        Cmd::Info { file } => cmd::info(&file, p.json, p.quiet),
        Cmd::Batch { jobs } => cmd::batch(&jobs, p.json, p.quiet),
        Cmd::Run(a) => run::run(&a, p.json, p.quiet),
        #[cfg(feature = "link")]
        Cmd::Gen(a) => ddc_cli::gen::run(&a, p.json, p.quiet),
        #[cfg(feature = "link")]
        Cmd::Publish(a) => ddc_cli::publish::run(&a, p.json, p.quiet),
    };
    println!("{}", text.trim_end());
    std::process::exit(code);
}
