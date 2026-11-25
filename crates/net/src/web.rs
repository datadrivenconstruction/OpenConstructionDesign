// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Browser transport: a synchronous request on the page's request object, so callers keep one
//! blocking API. Bytes come back through the `x-user-defined` charset (one char per byte).

use std::io::Read;

use wasm_bindgen::JsValue;

use crate::{Body, NetError, Policy, Request, Response, Transport};

pub struct Web {
    https_only: bool,
}

impl Web {
    pub fn new(p: &Policy) -> Self {
        Web { https_only: p.https_only && !p.loopback_only }
    }
}

fn js(e: JsValue) -> NetError {
    NetError(e.as_string().unwrap_or_else(|| "the browser refused the request".into()))
}

/// Headers the browser sets itself and refuses from a page.
fn forbidden(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "content-length" | "host" | "connection" | "user-agent")
}

fn parse_headers(raw: &str) -> Vec<(String, String)> {
    raw.lines().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect()
}

fn send_bytes(x: &web_sys::XmlHttpRequest, b: &[u8]) -> Result<(), NetError> {
    let arr = js_sys::Uint8Array::from(b);
    x.send_with_opt_buffer_source(Some(&arr)).map_err(js)
}

impl Transport for Web {
    fn send(&self, req: Request) -> Result<Response, NetError> {
        if self.https_only && !req.url.starts_with("https://") {
            return Err(NetError("only https addresses are allowed".into()));
        }
        let x = web_sys::XmlHttpRequest::new().map_err(js)?;
        x.open_with_async(&req.method, &req.url, false).map_err(js)?;
        x.override_mime_type("text/plain; charset=x-user-defined").map_err(js)?;
        for (k, v) in req.headers.iter().filter(|(k, _)| !forbidden(k)) {
            x.set_request_header(k, v).map_err(js)?;
        }
        match req.body {
            Body::Empty => x.send().map_err(js)?,
            Body::Bytes(b) => send_bytes(&x, &b)?,
            Body::Reader(mut rd, _) => {
                let mut b = Vec::new();
                rd.read_to_end(&mut b).map_err(|e| NetError(e.to_string()))?;
                send_bytes(&x, &b)?;
            }
        }
        let status = x.status().map_err(js)?;
        if status == 0 {
            return Err(NetError("no answer (network, address or cross-origin policy)".into()));
        }
        let headers = parse_headers(&x.get_all_response_headers().map_err(js)?);
        let text = x.response_text().map_err(js)?.unwrap_or_default();
        let bytes: Vec<u8> = text.chars().map(|c| (c as u32 & 0xff) as u8).collect();
        Ok(Response::new(status, headers, Box::new(std::io::Cursor::new(bytes))))
    }
}
