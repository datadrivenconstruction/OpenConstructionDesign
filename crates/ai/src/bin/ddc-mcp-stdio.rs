// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Lets an agent that speaks only stdio talk to the running editor.
//! Port: `--port N` or `DDC_MCP_PORT`. Token: `DDC_MCP_TOKEN`, otherwise the OS key store.

use ddc_ai::mcp::load_token;
use ddc_ai::mcp::stdio::{run, StdioConfig};
use ddc_ai::secret::default_store;
use ddc_ai::Secret;
use std::io;
use std::process::ExitCode;

fn port_from(args: &[String]) -> Option<u16> {
    let at = args.iter().position(|a| a == "--port");
    let text = match at {
        Some(i) => args.get(i + 1).cloned(),
        None => std::env::var("DDC_MCP_PORT").ok(),
    };
    text?.parse().ok().filter(|p| *p != 0)
}

fn token() -> Result<Secret, String> {
    if let Ok(t) = std::env::var("DDC_MCP_TOKEN") {
        if !t.is_empty() {
            return Ok(Secret::new(t));
        }
    }
    let store = default_store();
    match load_token(store.as_ref()) {
        Ok(Some(t)) => Ok(t),
        Ok(None) => Err(
            "no token: turn the server on in the editor's settings, or set DDC_MCP_TOKEN"
                .to_string(),
        ),
        Err(e) => Err(format!("cannot read the token: {e}")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(port) = port_from(&args) else {
        eprintln!("usage: ddc-mcp-stdio --port N (the port shown in the editor's settings)");
        return ExitCode::from(2);
    };
    let token = match token() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let cfg = StdioConfig { port, token };
    let stdin = io::stdin();
    let stdout = io::stdout();
    match run(&cfg, &mut stdin.lock(), &mut stdout.lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
