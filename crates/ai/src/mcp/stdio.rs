// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The bridge for agents that speak only stdio: one JSON-RPC message per line, forwarded to the
//! running editor. It adds no authority: it needs the same token and passes the same checks.

use super::wire::encode_header_value;
use crate::secret::Secret;
use crate::J;
use serde_json::json;
use std::io::{self, BufRead, Read, Write};
use std::time::Duration;

const MAX_LINE: usize = crate::limits::MCP_LINE.us();
const MAX_REPLY: u64 = crate::limits::MCP_REPLY.n();
const CONNECT_TIMEOUT: Duration = crate::limits::MCP_CONNECT_TIMEOUT.dur();
const KEY_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
/// Outside the range the protocol reserves: errors of this bridge, not of the editor.
const BRIDGE_ERROR: i64 = -31000;

pub struct StdioConfig {
    pub port: u16,
    pub token: Secret,
}

struct Bridge {
    agent: ureq::Agent,
    url: String,
    bearer: String,
    session: Option<String>,
    version: Option<String>,
}

fn bridge_error(id: Option<&J>, message: &str) -> J {
    let mut out = json!({"jsonrpc": "2.0", "error": {"code": BRIDGE_ERROR, "message": message}});
    if let Some(i) = id {
        out["id"] = i.clone();
    }
    out
}

fn read_reply(resp: ureq::Response) -> Option<J> {
    let mut body = Vec::new();
    resp.into_reader()
        .take(MAX_REPLY)
        .read_to_end(&mut body)
        .ok()?;
    serde_json::from_slice(&body).ok()
}

impl Bridge {
    fn new(cfg: &StdioConfig) -> Self {
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(crate::limits::MCP_READ_TIMEOUT.get_dur())
            .try_proxy_from_env(false)
            .build();
        Bridge {
            agent,
            url: format!("http://127.0.0.1:{}/mcp", cfg.port),
            bearer: format!("Bearer {}", cfg.token.expose()),
            session: None,
            version: None,
        }
    }

    /// The reply line, if the message wants one.
    fn forward(&mut self, msg: &J) -> Option<J> {
        let id = msg.get("id").filter(|i| !i.is_null());
        let method = msg["method"].as_str();
        let mut req = self
            .agent
            .post(&self.url)
            .set("Authorization", &self.bearer)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream");
        if let Some(version) = msg["params"]["_meta"][KEY_VERSION].as_str() {
            req = req.set("MCP-Protocol-Version", version);
            if let Some(m) = method {
                req = req.set("Mcp-Method", m);
            }
            if method == Some("tools/call") {
                if let Some(name) = msg["params"]["name"].as_str() {
                    req = req.set("Mcp-Name", &encode_header_value(name));
                }
            }
        } else {
            if let Some(v) = &self.version {
                req = req.set("MCP-Protocol-Version", v);
            }
            if let Some(s) = &self.session {
                req = req.set("Mcp-Session-Id", s);
            }
        }
        match req.send_string(&msg.to_string()) {
            Ok(resp) => {
                let session = resp.header("mcp-session-id").map(str::to_string);
                let status = resp.status();
                let reply = if status == 202 {
                    None
                } else {
                    read_reply(resp)
                };
                if method == Some("initialize") {
                    self.session = session.or(self.session.take());
                    self.version = reply
                        .as_ref()
                        .and_then(|r| r["result"]["protocolVersion"].as_str())
                        .map(str::to_string);
                }
                match (id, reply) {
                    (None, _) => None,
                    (Some(_), Some(r)) => Some(r),
                    (Some(i), None) => Some(bridge_error(Some(i), "the editor sent no answer")),
                }
            }
            Err(ureq::Error::Status(code, resp)) => {
                let i = id?;
                match read_reply(resp) {
                    Some(r) if r.get("error").is_some() => Some(r),
                    _ => Some(bridge_error(
                        Some(i),
                        &format!("the editor refused the request: HTTP {code}"),
                    )),
                }
            }
            Err(_) => id.map(|i| {
                bridge_error(
                    Some(i),
                    "the editor is not reachable: is the server turned on in its settings?",
                )
            }),
        }
    }
}

/// `None` at the end of input; `Some(None)` for a line over the limit, which is skipped whole.
fn read_line(input: &mut dyn BufRead, max: usize) -> io::Result<Option<Option<Vec<u8>>>> {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            if line.is_empty() && !too_long {
                return Ok(None);
            }
            break;
        }
        let end = chunk.iter().position(|b| *b == b'\n');
        let take = end.map_or(chunk.len(), |p| p + 1);
        if line.len() + take > max {
            too_long = true;
            line.clear();
        } else if !too_long {
            line.extend_from_slice(&chunk[..take]);
        }
        input.consume(take);
        if end.is_some() {
            break;
        }
    }
    Ok(Some(if too_long { None } else { Some(line) }))
}

/// Ends when the input ends. Only protocol messages go to the output.
pub fn run(cfg: &StdioConfig, input: &mut dyn BufRead, output: &mut dyn Write) -> io::Result<()> {
    let mut bridge = Bridge::new(cfg);
    while let Some(line) = read_line(input, MAX_LINE)? {
        let reply = match line {
            None => Some(bridge_error(None, "the message is too large")),
            Some(bytes) => {
                if bytes.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                match serde_json::from_slice::<J>(&bytes) {
                    Ok(msg) if msg.is_object() => bridge.forward(&msg),
                    _ => Some(bridge_error(None, "the line is not one JSON-RPC message")),
                }
            }
        };
        if let Some(r) = reply {
            // serialized here so that the reply cannot hold a line break
            writeln!(output, "{r}")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_lines_are_skipped_whole_and_reading_goes_on() {
        let raw = b"abc\nxxxxxxxxxx\nde";
        let mut input: &[u8] = raw;
        assert_eq!(
            read_line(&mut input, 5).unwrap(),
            Some(Some(b"abc\n".to_vec()))
        );
        assert_eq!(read_line(&mut input, 5).unwrap(), Some(None));
        assert_eq!(
            read_line(&mut input, 5).unwrap(),
            Some(Some(b"de".to_vec()))
        );
        assert_eq!(read_line(&mut input, 5).unwrap(), None);
    }
}
