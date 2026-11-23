// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A raw HTTP client for the server's tests: it can send what a well-behaved library would not.

use crate::J;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const VERSION: &str = "2026-07-28";
const TIMEOUT: Duration = crate::limits::MCP_TEST_TIMEOUT.dur();

#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.0.eq_ignore_ascii_case(name))
            .map(|h| h.1.as_str())
    }

    pub fn json(&self) -> J {
        serde_json::from_slice(&self.body).unwrap_or(J::Null)
    }
}

#[derive(Clone)]
pub struct Client {
    pub port: u16,
    pub token: String,
    pub name: String,
}

impl Client {
    pub fn new(port: u16, token: impl Into<String>) -> Self {
        Client {
            port,
            token: token.into(),
            name: "test-agent".to_string(),
        }
    }

    pub fn host(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// Exactly these headers and this body on a new connection.
    pub fn raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> std::io::Result<Reply> {
        let mut s = TcpStream::connect(("127.0.0.1", self.port))?;
        s.set_read_timeout(Some(TIMEOUT))?;
        let mut head = format!("{method} {path} HTTP/1.1\r\n");
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        s.write_all(head.as_bytes())?;
        s.write_all(body)?;
        let mut data = Vec::new();
        s.read_to_end(&mut data)?;
        parse(&data)
    }

    /// The headers a proper client sends.
    pub fn base_headers(&self, body_len: usize) -> Vec<(String, String)> {
        vec![
            ("Host".to_string(), self.host()),
            (
                "Authorization".to_string(),
                format!("Bearer {}", self.token),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "Accept".to_string(),
                "application/json, text/event-stream".to_string(),
            ),
            ("Content-Length".to_string(), body_len.to_string()),
        ]
    }

    pub fn meta(&self) -> J {
        json!({
            "io.modelcontextprotocol/protocolVersion": VERSION,
            "io.modelcontextprotocol/clientInfo": {"name": self.name, "version": "1"},
            "io.modelcontextprotocol/clientCapabilities": {},
        })
    }

    /// A request of the current revision: metadata in the body, mirrored in the headers.
    pub fn request(&self, method: &str, mut params: J) -> std::io::Result<Reply> {
        params["_meta"] = self.meta();
        let body =
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
        let mut headers = self.base_headers(body.len());
        headers.push(("MCP-Protocol-Version".to_string(), VERSION.to_string()));
        headers.push(("Mcp-Method".to_string(), method.to_string()));
        if let Some(name) = params["name"].as_str() {
            headers.push(("Mcp-Name".to_string(), name.to_string()));
        }
        self.raw("POST", "/mcp", &headers, body.as_bytes())
    }

    /// `(parsed reply text, isError)`; panics on anything but a tool result.
    pub fn tool(&self, name: &str, args: J) -> (J, bool) {
        let reply = self
            .request("tools/call", json!({"name": name, "arguments": args}))
            .expect("the server answers");
        assert_eq!(
            reply.status,
            200,
            "{}: {}",
            name,
            String::from_utf8_lossy(&reply.body)
        );
        let v = reply.json();
        let text = v["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: no text in {v}"));
        let parsed: J = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("{name}: reply is not fenced JSON: {e}: {text}"));
        assert_eq!(parsed["untrusted"], "data", "{name}: not fenced: {text}");
        (parsed, v["result"]["isError"].as_bool().unwrap_or(false))
    }
}

fn parse(data: &[u8]) -> std::io::Result<Reply> {
    let bad = |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string());
    let at = data
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| bad("no end of head"))?;
    let head = std::str::from_utf8(&data[..at]).map_err(|_| bad("head is not text"))?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| bad("no status"))?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_string(), v.trim().to_string()))
        .collect();
    Ok(Reply {
        status,
        headers,
        body: data[at + 4..].to_vec(),
    })
}
