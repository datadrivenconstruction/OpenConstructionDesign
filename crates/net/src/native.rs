// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Desktop transport: ureq on the OS TLS stack.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use crate::{Body, NetError, Policy, Request, Response, Transport};

pub struct Native {
    agent: ureq::Agent,
}

pub fn loopback_only(netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
    let v: Vec<SocketAddr> = netloc.to_socket_addrs()?.filter(|a| a.ip().is_loopback()).collect();
    if v.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "the address does not resolve to this computer",
        ));
    }
    Ok(v)
}

impl Native {
    pub fn new(p: &Policy) -> Result<Self, NetError> {
        let tls = native_tls::TlsConnector::new().map_err(|e| NetError(format!("tls: {e}")))?;
        let b = ureq::AgentBuilder::new()
            .tls_connector(Arc::new(tls))
            .redirects(p.redirects)
            .timeout_connect(p.connect_timeout)
            .timeout_read(p.read_timeout)
            .timeout_write(p.write_timeout)
            .try_proxy_from_env(!p.loopback_only)
            .https_only(p.https_only);
        Ok(Native { agent: if p.loopback_only { b.resolver(loopback_only).build() } else { b.build() } })
    }
}

fn answer(r: ureq::Response) -> Response {
    let headers = r.headers_names().into_iter().filter_map(|n| r.header(&n).map(|v| (n.clone(), v.to_string()))).collect();
    Response::new(r.status(), headers, Box::new(r.into_reader()))
}

impl Transport for Native {
    fn send(&self, req: Request) -> Result<Response, NetError> {
        let mut r = self.agent.request(&req.method, &req.url);
        for (k, v) in &req.headers {
            r = r.set(k, v);
        }
        let res = match req.body {
            Body::Empty => r.call(),
            Body::Bytes(b) => r.send_bytes(&b),
            Body::Reader(rd, len) => r.set("Content-Length", &len.to_string()).send(rd),
        };
        match res {
            Ok(r) | Err(ureq::Error::Status(_, r)) => Ok(answer(r)),
            Err(ureq::Error::Transport(t)) => Err(NetError(t.to_string())),
        }
    }
}
