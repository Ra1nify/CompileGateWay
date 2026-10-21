//! Minimal HTTP/1.1 client.
//!
//! The CLI window uses it to call the local daemon's HTTP API; design doc §3
//! requires "the CLI is a thin HTTP client of the daemon and must behave exactly
//! like the WebUI".
//!
//! It talks only to `127.0.0.1`, so no TLS and no HTTP crate is needed: writing
//! this small piece by hand beats pulling in reqwest.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Response {
    pub status: u16,
    /// Text view of the body (non-UTF-8 bytes are replaced, so **use
    /// `body_bytes` to download files**).
    pub body: String,
    /// Raw body bytes.
    pub body_bytes: Vec<u8>,
    /// Response headers (names kept as-is; compare with `eq_ignore_ascii_case`).
    /// Only `cgw login` reads `Set-Cookie` today, but keeping them all is
    /// cleaner than keeping one field.
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Default source tag. Design doc §10: the daemon tags logs with the `source`
/// from the `X-Client` header, so every client should identify itself.
pub const DEFAULT_CLIENT: &str = "cli";

/// Send a request to the local port. `body` of `None` means no request body.
pub fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<Response, String> {
    request_to(&format!("127.0.0.1:{port}"), method, path, body)
}

/// Send a request to the given `host:port` (the CLI may target a daemon on
/// another machine). `Connection: close` + `read_to_end` saves parsing
/// Content-Length ourselves.
pub fn request_to(
    addr: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<Response, String> {
    request_as(addr, method, path, body, DEFAULT_CLIENT)
}

/// Same as above, but with an explicit `X-Client` (e.g. the Tauri CLI window
/// passes `cli`, the MCP adapter passes `mcp`).
pub fn request_as(
    addr: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
    client: &str,
) -> Result<Response, String> {
    request_full(addr, method, path, body, client, None)
}

/// Full form: `X-Client` plus an optional session cookie.
///
/// A cookie is needed when the daemon is on another machine (or the caller is
/// not a loopback source); local access is unauthenticated, so `None` is fine.
pub fn request_full(
    addr: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
    client: &str,
    cookie: Option<&str>,
) -> Result<Response, String> {
    let mut stream = TcpStream::connect(addr)
        .map_err(|err| crate::i18n::http_connect_failed(crate::i18n::current(), addr, &err.to_string()))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(120)));

    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: application/json\r\nX-Client: {client}\r\n"
    );
    if let Some(cookie) = cookie {
        if !cookie.is_empty() {
            head.push_str(&format!("Cookie: {cookie}\r\n"));
        }
    }
    if let Some(body) = body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", body.as_bytes().len()));
    }
    head.push_str("\r\n");

    stream
        .write_all(head.as_bytes())
        .map_err(|err| crate::i18n::http_send_failed(crate::i18n::current(), &err.to_string()))?;
    if let Some(body) = body {
        stream
            .write_all(body.as_bytes())
            .map_err(|err| {
                crate::i18n::http_send_body_failed(crate::i18n::current(), &err.to_string())
            })?;
    }
    stream
        .flush()
        .map_err(|err| crate::i18n::http_send_failed(crate::i18n::current(), &err.to_string()))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|err| crate::i18n::http_read_failed(crate::i18n::current(), &err.to_string()))?;
    parse(&raw)
}

fn parse(raw: &[u8]) -> Result<Response, String> {
    // Find the header separator by byte and **keep the body bytes as they are**:
    // when downloading a zip, any "decode to UTF-8 then re-encode" step corrupts
    // binary data (invalid bytes become U+FFFD).
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| crate::i18n::http_bad_response(crate::i18n::current()))?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let body_bytes = &raw[split + 4..];

    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| crate::i18n::http_bad_status_line(crate::i18n::current(), status_line))?;

    let chunked = lines.clone().any(|line| {
        line.split_once(':').is_some_and(|(key, value)| {
            key.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
        })
    });

    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            line.split_once(':')
                .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        })
        .collect();

    let body_bytes = if chunked {
        dechunk(body_bytes)
    } else {
        body_bytes.to_vec()
    };

    Ok(Response {
        status,
        body: String::from_utf8_lossy(&body_bytes).to_string(),
        body_bytes,
        headers,
    })
}

/// Split chunked transfer encoding (**byte-wise**).
///
/// The old version sliced `&tail[..size]` on an `&str`: `size` counts **bytes**
/// and may not fall on a char boundary, so a chunked response containing CJK
/// text panicked whenever a chunk split a multi-byte character. Working on bytes
/// fixes that and lets binary downloads use the same path.
fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let Some(at) = rest.windows(2).position(|window| window == b"\r\n") else {
            break;
        };
        let size_line = String::from_utf8_lossy(&rest[..at]).to_string();
        let size = usize::from_str_radix(
            size_line.trim().split(';').next().unwrap_or("0").trim(),
            16,
        )
        .unwrap_or(0);
        let tail = &rest[at + 2..];
        if size == 0 {
            break;
        }
        if tail.len() < size {
            out.extend_from_slice(tail);
            break;
        }
        out.extend_from_slice(&tail[..size]);
        rest = tail[size..]
            .strip_prefix(b"\r\n".as_slice())
            .unwrap_or(&tail[size..]);
    }
    out
}

/// Pretty-print JSON; anything else is returned unchanged.
pub fn pretty(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| body.to_string()),
        Err(_) => body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_length_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\n\r\n{\"ok\":true}\r\n";
        let response = parse(raw).unwrap();
        assert_eq!(response.status, 200);
        assert!(response.ok());
        assert_eq!(response.body.trim(), "{\"ok\":true}");
    }

    #[test]
    fn parses_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n7\r\n{\"a\":1}\r\n0\r\n\r\n";
        let response = parse(raw).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"a\":1}");
    }

    #[test]
    fn reports_error_status() {
        let raw = b"HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\n\r\n{\"error\":{}}";
        let response = parse(raw).unwrap();
        assert_eq!(response.status, 409);
        assert!(!response.ok());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"not http at all").is_err());
    }

    #[test]
    fn pretty_prints_json_and_passes_through_anything_else() {
        assert!(pretty("{\"a\":1}").contains("\n"));
        assert_eq!(pretty("plain text"), "plain text");
    }

    /// File downloads rely on `body_bytes`: invalid UTF-8 bytes in the body must
    /// not be polluted by the text view.
    #[test]
    fn keeps_binary_bodies_byte_exact() {
        let mut raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\n\r\n".to_vec();
        raw.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04, 0xff, 0x00, 0xfe]);
        let response = parse(&raw).unwrap();
        assert_eq!(response.body_bytes, vec![0x50, 0x4b, 0x03, 0x04, 0xff, 0x00, 0xfe]);
        // The text view holds replacement characters, so it must not be used to
        // write files
        assert!(response.body.contains('\u{fffd}'));
    }

    /// The old implementation panicked when a chunk boundary split a multi-byte
    /// character, slicing an `&str` by bytes.
    #[test]
    fn dechunks_across_a_multibyte_boundary() {
        // The 3-byte CJK char below is deliberately split mid-character: first
        // chunk 1 byte, second chunk 2 bytes
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n\xe4\r\n2\r\n\xb8\xad\r\n0\r\n\r\n";
        let response = parse(raw).unwrap();
        assert_eq!(response.body, "中");
        assert_eq!(response.body_bytes, "中".as_bytes().to_vec());
    }

    /// Chunk extensions (everything after `;` in the chunk header) must be ignored.
    #[test]
    fn dechunk_ignores_chunk_extensions() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;ext=1\r\nhello\r\n0\r\n\r\n";
        assert_eq!(parse(raw).unwrap().body, "hello");
    }
}
