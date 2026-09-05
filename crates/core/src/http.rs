//! Just enough HTTP/1.1 for two programs that trust each other: one request
//! per connection, small JSON bodies, `Connection: close`. No framework, no
//! TLS (Tailscale is the transport), no keep-alive.

use anyhow::{anyhow, bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Larger bodies are refused; nothing BroLink sends comes close.
const MAX_BODY: usize = 64 * 1024;
const MAX_HEADER: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    pub fn json<T: serde::Serialize>(status: u16, value: &T) -> Self {
        Self {
            status,
            body: serde_json::to_string(value).unwrap_or_else(|_| "{}".into()),
        }
    }

    pub fn parse<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.body)
            .with_context(|| format!("HTTP {}: {}", self.status, self.body))
    }
}

/// Read one request from `stream`. `None` for an empty connection (a port
/// scan, a health probe) so the caller can drop it quietly.
pub fn read_request(stream: &mut TcpStream) -> Result<Option<Request>> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let path = parts.next().unwrap_or("").to_string();
    if method.is_empty() || !path.starts_with('/') {
        bail!("bad request line: {line:?}");
    }
    let mut content_length = 0usize;
    let mut header_bytes = line.len();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            bail!("connection closed inside headers");
        }
        header_bytes += line.len();
        if header_bytes > MAX_HEADER {
            bail!("headers too large");
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().context("content-length")?;
            }
        }
    }
    if content_length > MAX_BODY {
        bail!("body too large ({content_length} bytes)");
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    Ok(Some(Request {
        method,
        path,
        body: String::from_utf8(body).context("body is not UTF-8")?,
    }))
}

pub fn write_response(stream: &mut TcpStream, resp: &Response) -> Result<()> {
    let reason = match resp.status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "Status",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        resp.status,
        reason,
        resp.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(resp.body.as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// Accept forever, one thread per connection. `handler` sees the peer
/// address so it can decide who is allowed to ask.
pub fn serve<F>(listener: TcpListener, handler: F)
where
    F: Fn(SocketAddr, &Request) -> Response + Send + Sync + 'static,
{
    let handler = std::sync::Arc::new(handler);
    for conn in listener.incoming() {
        let Ok(mut stream) = conn else { continue };
        let handler = handler.clone();
        std::thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
            let peer = match stream.peer_addr() {
                Ok(p) => p,
                Err(_) => return,
            };
            let resp = match read_request(&mut stream) {
                Ok(Some(req)) => handler(peer, &req),
                Ok(None) => return,
                Err(e) => Response::json(
                    400,
                    &serde_json::json!({ "ok": false, "error": e.to_string() }),
                ),
            };
            let _ = write_response(&mut stream, &resp);
        });
    }
}

/// One request, one reply, within `timeout` for connect and for the read.
pub fn request(
    addr: impl ToSocketAddrs,
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<Response> {
    let addr = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow!("no address"))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let body = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("bad status line: {line:?}"))?;
    let mut content_length: Option<usize> = None;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().ok();
            }
        }
    }
    let mut body = Vec::new();
    match content_length {
        Some(n) if n <= MAX_BODY => {
            body.resize(n, 0);
            reader.read_exact(&mut body)?;
        }
        Some(n) => bail!("reply too large ({n} bytes)"),
        None => {
            reader.take(MAX_BODY as u64).read_to_end(&mut body)?;
        }
    }
    Ok(Response {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// GET `path` and decode the JSON reply.
pub fn get_json<T: serde::de::DeserializeOwned>(
    addr: impl ToSocketAddrs,
    path: &str,
    timeout: Duration,
) -> Result<T> {
    let r = request(addr, "GET", path, None, timeout)?;
    if r.status != 200 {
        bail!("HTTP {}: {}", r.status, r.body);
    }
    r.parse()
}

/// POST `value` as JSON to `path` and decode the reply.
pub fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
    addr: impl ToSocketAddrs,
    path: &str,
    value: &B,
    timeout: Duration,
) -> Result<T> {
    let body = serde_json::to_string(value)?;
    let r = request(addr, "POST", path, Some(&body), timeout)?;
    if r.status != 200 {
        bail!("HTTP {}: {}", r.status, r.body);
    }
    r.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_and_server_agree_over_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            serve(listener, |peer, req| {
                assert!(peer.ip().is_loopback());
                match (req.method.as_str(), req.path.as_str()) {
                    ("GET", "/v1/status") => {
                        Response::json(200, &serde_json::json!({ "name": "PC" }))
                    }
                    ("POST", "/v1/echo") => Response {
                        status: 200,
                        body: req.body.clone(),
                    },
                    _ => Response::json(404, &serde_json::json!({ "ok": false })),
                }
            });
        });
        let t = Duration::from_secs(2);
        let v: serde_json::Value = get_json(addr, "/v1/status", t).unwrap();
        assert_eq!(v["name"], "PC");
        let echoed: serde_json::Value =
            post_json(addr, "/v1/echo", &serde_json::json!({"pin": "1234"}), t).unwrap();
        assert_eq!(echoed["pin"], "1234");
        let err = get_json::<serde_json::Value>(addr, "/nope", t).unwrap_err();
        assert!(err.to_string().contains("404"), "{err}");
    }

    #[test]
    fn garbage_is_refused_not_panicked() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || serve(listener, |_, _| Response::json(200, &Ack::ok())));
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"not http at all\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 400"), "{out}");
    }

    use crate::api::Ack;
}
