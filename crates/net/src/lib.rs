// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One blocking HTTP transport for the crates that call out (ERP link, AI providers).
//!
//! - [`Transport`]: send one [`Request`], get the [`Response`] whatever its status.
//! - `native` (desktop): ureq on the OS TLS stack, with the [`Policy`] applied.
//! - `web` (browser): the page's request object; the browser owns TLS, proxies and redirects.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
pub mod native;
#[cfg(target_arch = "wasm32")]
pub mod web;

/// A request body: none, bytes, or a reader of a known length (sent with that exact length).
pub enum Body {
    Empty,
    Bytes(Vec<u8>),
    Reader(Box<dyn Read + Send>, u64),
}

pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl Request {
    pub fn new(method: &str, url: impl Into<String>) -> Self {
        Request { method: method.to_string(), url: url.into(), headers: Vec::new(), body: Body::Empty }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn body(mut self, body: Body) -> Self {
        self.body = body;
        self
    }
}

/// An answer of any status; the body is read by the caller.
pub struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
    body: Box<dyn Read + Send>,
}

impl Response {
    pub fn new(status: u16, headers: Vec<(String, String)>, body: Box<dyn Read + Send>) -> Self {
        Response { status, headers, body }
    }

    /// The first header of that name, case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn into_reader(self) -> Box<dyn Read + Send> {
        self.body
    }
}

/// The request never got an answer (address, TLS, timeout, policy); the text is for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetError(pub String);

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NetError {}

/// Where a request may go and how long it may take. The browser applies its own rules, so the
/// web transport honours only what a page can.
#[derive(Clone, Debug)]
pub struct Policy {
    pub https_only: bool,
    /// Resolve only to this computer's loopback addresses, never through a proxy.
    pub loopback_only: bool,
    pub redirects: u32,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            https_only: true,
            loopback_only: false,
            redirects: 0,
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(120),
            write_timeout: Duration::from_secs(120),
        }
    }
}

pub trait Transport: Send + Sync {
    fn send(&self, req: Request) -> Result<Response, NetError>;
}

/// The platform's transport for this policy.
pub fn transport(policy: &Policy) -> Result<Arc<dyn Transport>, NetError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Ok(Arc::new(native::Native::new(policy)?))
    }
    #[cfg(target_arch = "wasm32")]
    {
        Ok(Arc::new(web::Web::new(policy)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_case_insensitive() {
        let r = Response::new(200, vec![("X-DDC-Link".into(), "1".into())], Box::new(std::io::empty()));
        assert_eq!(r.header("x-ddc-link"), Some("1"));
        assert_eq!(r.header("missing"), None);
    }
}
