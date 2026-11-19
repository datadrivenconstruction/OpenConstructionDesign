// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Just enough HTTP/1.1 for one request per connection, with bounds on everything a client sends.

use crate::J;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use std::io::{self, ErrorKind, Read, Write};
use web_time::Instant;

#[derive(Debug)]
pub(crate) enum ReadError {
    TooLarge,
    Bad,
    Timeout,
    Closed,
}

#[derive(Debug, Default)]
pub(crate) struct Head {
    pub method: String,
    pub target: String,
    headers: Vec<(String, String)>,
}

impl Head {
    /// The first value; `name` in lower case.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.0 == name)
            .map(|h| h.1.as_str())
    }

    pub fn count(&self, name: &str) -> usize {
        self.headers.iter().filter(|h| h.0 == name).count()
    }
}

fn parse_head(text: &str) -> Result<Head, ReadError> {
    let mut lines = text.split("\r\n");
    let first = lines.next().ok_or(ReadError::Bad)?;
    let mut parts = first.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ReadError::Bad);
    };
    if !version.starts_with("HTTP/1.") || method.is_empty() || target.is_empty() {
        return Err(ReadError::Bad);
    }
    let mut head = Head {
        method: method.to_string(),
        target: target.to_string(),
        headers: vec![],
    };
    for line in lines {
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(ReadError::Bad);
        }
        let (name, value) = line.split_once(':').ok_or(ReadError::Bad)?;
        if name.is_empty() || name.contains(' ') {
            return Err(ReadError::Bad);
        }
        head.headers
            .push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    Ok(head)
}

fn read_some(r: &mut impl Read, buf: &mut [u8], deadline: Instant) -> Result<usize, ReadError> {
    loop {
        if Instant::now() >= deadline {
            return Err(ReadError::Timeout);
        }
        match r.read(buf) {
            Ok(n) => return Ok(n),
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(ReadError::Closed),
        }
    }
}

/// The head and whatever of the body came with it. The deadline covers the whole head, so a client
/// sending a byte at a time cannot keep the connection.
pub(crate) fn read_head(
    r: &mut impl Read,
    max: usize,
    deadline: Instant,
) -> Result<(Head, Vec<u8>), ReadError> {
    let mut data = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    loop {
        let n = read_some(r, &mut chunk, deadline)?;
        if n == 0 {
            return Err(if data.is_empty() {
                ReadError::Closed
            } else {
                ReadError::Bad
            });
        }
        let from = data.len().saturating_sub(3);
        data.extend_from_slice(&chunk[..n]);
        if let Some(at) = data[from..]
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| p + from)
        {
            if at > max {
                return Err(ReadError::TooLarge);
            }
            let text = std::str::from_utf8(&data[..at]).map_err(|_| ReadError::Bad)?;
            let head = parse_head(text)?;
            return Ok((head, data[at + 4..].to_vec()));
        }
        if data.len() > max {
            return Err(ReadError::TooLarge);
        }
    }
}

pub(crate) fn read_body(
    r: &mut impl Read,
    mut got: Vec<u8>,
    len: usize,
    deadline: Instant,
) -> Result<Vec<u8>, ReadError> {
    let mut chunk = [0u8; 16 * 1024];
    while got.len() < len {
        let n = read_some(r, &mut chunk, deadline)?;
        if n == 0 {
            return Err(ReadError::Bad);
        }
        got.extend_from_slice(&chunk[..n]);
    }
    got.truncate(len);
    Ok(got)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

#[derive(Debug)]
pub(crate) struct Resp {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn empty(status: u16) -> Self {
        Resp {
            status,
            headers: vec![],
            body: vec![],
        }
    }

    pub fn json(status: u16, value: &J) -> Self {
        Resp {
            status,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: value.to_string().into_bytes(),
        }
    }

    pub fn with(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    /// No CORS headers ever: a browser must not be able to read an answer from here.
    pub fn write(&self, w: &mut impl Write) -> io::Result<()> {
        let mut head = format!("HTTP/1.1 {} {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Length: {}\r\n", self.status, reason(self.status), self.body.len());
        for (name, value) in &self.headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        w.write_all(head.as_bytes())?;
        w.write_all(&self.body)?;
        w.flush()
    }
}

const SENTINEL_START: &str = "=?base64?";
const SENTINEL_END: &str = "?=";

/// The form of `Mcp-Name` on the wire.
pub(crate) fn encode_header_value(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes().all(|b| (0x20..=0x7e).contains(&b))
        && !s.starts_with(' ')
        && !s.ends_with(' ')
        && !(s.starts_with(SENTINEL_START) && s.ends_with(SENTINEL_END));
    if plain {
        s.to_string()
    } else {
        format!(
            "{SENTINEL_START}{}{SENTINEL_END}",
            STANDARD.encode(s.as_bytes())
        )
    }
}

pub(crate) fn decode_header_value(s: &str) -> Option<String> {
    match s
        .strip_prefix(SENTINEL_START)
        .and_then(|r| r.strip_suffix(SENTINEL_END))
    {
        Some(b64) => String::from_utf8(STANDARD.decode(b64).ok()?).ok(),
        None => Some(s.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn head_splits_from_the_body_in_any_chunking() {
        let raw = b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:1\r\nContent-Length: 2\r\n\r\nhi";
        let (head, rest) = read_head(&mut &raw[..], 1024, soon()).unwrap();
        assert_eq!(
            (head.method.as_str(), head.target.as_str()),
            ("POST", "/mcp")
        );
        assert_eq!(head.get("host"), Some("127.0.0.1:1"));
        assert_eq!(head.get("content-length"), Some("2"));
        assert_eq!(rest, b"hi");
        struct Drip<'a>(&'a [u8]);
        impl Read for Drip<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0.is_empty() || buf.is_empty() {
                    return Ok(0);
                }
                buf[0] = self.0[0];
                self.0 = &self.0[1..];
                Ok(1)
            }
        }
        let (head, rest) = read_head(&mut Drip(raw), 1024, soon()).unwrap();
        assert_eq!(head.count("host"), 1);
        assert!(rest.is_empty());
        assert_eq!(read_body(&mut Drip(b"hi"), rest, 2, soon()).unwrap(), b"hi");
    }

    #[test]
    fn bad_heads_are_refused() {
        for raw in [
            &b"GET /\r\n\r\n"[..],
            b"GET / HTTP/2\r\n\r\n",
            b"GET / HTTP/1.1\r\nNoColon\r\n\r\n",
            b"GET / HTTP/1.1\r\nA: b\r\n c\r\n\r\n",
            b"GET / HTTP/1.1\r\n: b\r\n\r\n",
            b"\xff\xfe / HTTP/1.1\r\n\r\n",
        ] {
            assert!(
                matches!(read_head(&mut &raw[..], 1024, soon()), Err(ReadError::Bad)),
                "{raw:?}"
            );
        }
        assert!(matches!(
            read_head(&mut &b""[..], 1024, soon()),
            Err(ReadError::Closed)
        ));
    }

    #[test]
    fn a_long_head_is_refused_without_waiting_for_its_end() {
        let raw = vec![b'a'; 5000];
        assert!(matches!(
            read_head(&mut &raw[..], 1024, soon()),
            Err(ReadError::TooLarge)
        ));
    }

    #[test]
    fn a_drained_deadline_stops_reading() {
        let raw = b"GET / HTTP/1.1\r\n\r\n";
        assert!(matches!(
            read_head(&mut &raw[..], 1024, Instant::now()),
            Err(ReadError::Timeout)
        ));
    }

    #[test]
    fn a_response_has_length_and_closes_and_never_allows_cross_origin() {
        let mut out = vec![];
        Resp::json(200, &json!({"a": 1})).write(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.contains("Content-Length: 7\r\n"));
        assert!(!text.to_ascii_lowercase().contains("access-control"));
        assert!(text.ends_with("\r\n\r\n{\"a\":1}"));
    }

    #[test]
    fn header_values_round_trip_through_the_sentinel() {
        assert_eq!(encode_header_value("get_context"), "get_context");
        for s in ["Hello, 世界", " padded ", "a\nb", "=?base64?literal?="] {
            let wire = encode_header_value(s);
            assert!(wire.starts_with("=?base64?"), "{wire}");
            assert_eq!(decode_header_value(&wire).as_deref(), Some(s));
        }
        assert_eq!(decode_header_value("=?base64?!!!?="), None);
    }
}
