//! Minimal blocking HTTP/1.1 + pure-Rust TLS client for `solo studio`.
//!
//! Uses `std::net::TcpStream` + `rustls` (`rustls_rustcrypto::provider()` and
//! `rustls_native_certs::load_native_certs()`). Plain `http://` is permitted
//! only for loopback addresses (`127.0.0.1`, `localhost`, `[::1]`) used in
//! local `wrangler dev` tests; all remote hosts require `https://`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub is_https: bool,
    pub host: String,
    pub port: u16,
    pub host_header: String,
    pub path_and_query: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[cfg(test)]
impl HttpResponse {
    pub fn text(&self) -> Result<&str, String> {
        std::str::from_utf8(&self.body)
            .map_err(|e| format!("response body is not valid UTF-8: {e}"))
    }
}

pub fn is_loopback_host(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.eq_ignore_ascii_case("localhost") || bare == "127.0.0.1" || bare == "::1"
}

pub fn parse_url(raw: &str) -> Result<ParsedUrl, String> {
    let s = raw.trim();
    let (is_https, rest) = if let Some(r) = s.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = s.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("URL must start with https:// or http://: {raw}"));
    };

    let (authority, path_and_query) = match rest.find(['/', '?']) {
        Some(idx) => {
            let (auth, pq) = rest.split_at(idx);
            let pq = if pq.starts_with('?') {
                format!("/{pq}")
            } else {
                pq.to_string()
            };
            (auth, pq)
        }
        None => (rest, "/".to_string()),
    };

    if authority.is_empty() || authority.contains('@') {
        return Err(format!("invalid URL authority: {raw}"));
    }

    let (host, port) = if authority.starts_with('[') {
        let end = authority
            .find(']')
            .ok_or_else(|| format!("unclosed IPv6 host in URL: {raw}"))?;
        let h = &authority[..=end];
        let after = &authority[end + 1..];
        let p = if let Some(p_str) = after.strip_prefix(':') {
            p_str
                .parse::<u16>()
                .map_err(|_| format!("invalid port in URL: {raw}"))?
        } else if after.is_empty() {
            if is_https { 443 } else { 80 }
        } else {
            return Err(format!("invalid authority in URL: {raw}"));
        };
        (h.to_string(), p)
    } else if let Some((h, p_str)) = authority.rsplit_once(':') {
        let p = p_str
            .parse::<u16>()
            .map_err(|_| format!("invalid port in URL: {raw}"))?;
        (h.to_string(), p)
    } else {
        (authority.to_string(), if is_https { 443 } else { 80 })
    };

    if host.is_empty() {
        return Err(format!("missing host in URL: {raw}"));
    }
    if !is_https && !is_loopback_host(&host) {
        return Err(format!(
            "refusing plaintext http:// for non-loopback host '{host}'; use https://"
        ));
    }

    let default_port = if is_https { 443 } else { 80 };
    let host_header = if port == default_port {
        host.clone()
    } else {
        format!("{host}:{port}")
    };

    Ok(ParsedUrl {
        is_https,
        host,
        port,
        host_header,
        path_and_query,
    })
}

fn build_tls_config() -> Result<Arc<ClientConfig>, String> {
    let mut root_store = RootCertStore::empty();
    let cert_res = rustls_native_certs::load_native_certs();
    for cert in cert_res.certs {
        let _ = root_store.add(cert);
    }
    if root_store.is_empty() {
        let msg = cert_res
            .errors
            .into_iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "no native TLS root certificates available{}",
            if msg.is_empty() {
                String::new()
            } else {
                format!(": {msg}")
            }
        ));
    }
    let provider = Arc::new(rustls_rustcrypto::provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS configuration error: {e}"))?
        .with_root_certificates(root_store)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

fn connect_tcp(host: &str, port: u16) -> Result<TcpStream, String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = (bare, port)
        .to_socket_addrs()
        .map_err(|e| format!("failed to resolve {host}:{port}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("no addresses resolved for {host}:{port}"));
    }
    let mut last_err = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
                let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
                return Ok(stream);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(format!(
        "failed to connect to {host}:{port}: {}",
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown error".into())
    ))
}

pub fn request(
    method: &str,
    url: &str,
    bearer_token: Option<&str>,
    body: Option<&[u8]>,
    content_type: Option<&str>,
) -> Result<HttpResponse, String> {
    let parsed = parse_url(url)?;
    let mut req_buf = String::new();
    req_buf.push_str(&format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: sws-cli/{}\r\nAccept: application/json\r\nConnection: close\r\n",
        parsed.path_and_query,
        parsed.host_header,
        env!("CARGO_PKG_VERSION")
    ));
    if let Some(tok) = bearer_token {
        req_buf.push_str(&format!("Authorization: Bearer {tok}\r\n"));
    }
    if let Some(ct) = content_type {
        req_buf.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    if let Some(b) = body {
        req_buf.push_str(&format!("Content-Length: {}\r\n", b.len()));
    } else if matches!(method, "POST" | "PUT" | "PATCH") {
        req_buf.push_str("Content-Length: 0\r\n");
    }
    req_buf.push_str("\r\n");

    let tcp = connect_tcp(&parsed.host, parsed.port)?;
    if parsed.is_https {
        let config = build_tls_config()?;
        let bare = parsed.host.trim_start_matches('[').trim_end_matches(']');
        let server_name = ServerName::try_from(bare.to_owned())
            .map_err(|e| format!("invalid TLS server name '{}': {e}", parsed.host))?;
        let conn = ClientConnection::new(config, server_name)
            .map_err(|e| format!("TLS handshake setup failed: {e}"))?;
        let mut tls = StreamOwned::new(conn, tcp);
        tls.write_all(req_buf.as_bytes())
            .map_err(|e| format!("HTTP write failed: {e}"))?;
        if let Some(b) = body {
            tls.write_all(b)
                .map_err(|e| format!("HTTP body write failed: {e}"))?;
        }
        tls.flush().map_err(|e| format!("HTTP flush failed: {e}"))?;
        read_http_response(tls)
    } else {
        let mut stream = tcp;
        stream
            .write_all(req_buf.as_bytes())
            .map_err(|e| format!("HTTP write failed: {e}"))?;
        if let Some(b) = body {
            stream
                .write_all(b)
                .map_err(|e| format!("HTTP body write failed: {e}"))?;
        }
        stream
            .flush()
            .map_err(|e| format!("HTTP flush failed: {e}"))?;
        read_http_response(stream)
    }
}

fn read_http_response<R: Read>(stream: R) -> Result<HttpResponse, String> {
    let mut reader = BufReader::new(stream);
    let mut header_bytes = 0usize;
    let mut status_line = String::new();
    let n = reader
        .read_line(&mut status_line)
        .map_err(|e| format!("failed to read HTTP status line: {e}"))?;
    if n == 0 {
        return Err("empty HTTP response from server".to_string());
    }
    header_bytes += n;
    let status = parse_status_line(&status_line)?;

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("failed to read HTTP header: {e}"))?;
        if n == 0 {
            break;
        }
        header_bytes += n;
        if header_bytes > MAX_HEADER_BYTES {
            return Err("HTTP response headers exceed limit".to_string());
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }

    let is_chunked = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked")
    });
    let content_length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok());

    let body = if is_chunked {
        read_chunked(&mut reader)?
    } else if let Some(len) = content_length {
        if len > MAX_RESPONSE_BYTES {
            return Err(format!("response body exceeds {MAX_RESPONSE_BYTES} byte limit"));
        }
        let mut buf = vec![0u8; len];
        reader
            .read_exact(&mut buf)
            .map_err(|e| format!("failed to read HTTP response body: {e}"))?;
        buf
    } else {
        let mut buf = Vec::new();
        let mut limited = reader.take((MAX_RESPONSE_BYTES + 1) as u64);
        limited
            .read_to_end(&mut buf)
            .map_err(|e| format!("failed to read HTTP response body: {e}"))?;
        if buf.len() > MAX_RESPONSE_BYTES {
            return Err(format!("response body exceeds {MAX_RESPONSE_BYTES} byte limit"));
        }
        buf
    };

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn parse_status_line(line: &str) -> Result<u16, String> {
    let mut parts = line.split_whitespace();
    let proto = parts
        .next()
        .ok_or_else(|| format!("malformed HTTP status line: {line}"))?;
    if !proto.starts_with("HTTP/1.") {
        return Err(format!("unsupported HTTP version in status line: {proto}"));
    }
    let code_str = parts
        .next()
        .ok_or_else(|| format!("missing status code in HTTP status line: {line}"))?;
    code_str
        .parse::<u16>()
        .map_err(|_| format!("invalid HTTP status code: {code_str}"))
}

fn read_chunked<R: BufRead>(reader: &mut R) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("failed to read chunk size: {e}"))?;
        if n == 0 {
            return Err("unexpected EOF in chunked HTTP body".to_string());
        }
        let size_hex = line
            .trim()
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| format!("invalid chunk size '{size_hex}'"))?;
        if size == 0 {
            // Drain trailing CRLF / trailers.
            let mut trailer = String::new();
            while reader.read_line(&mut trailer).unwrap_or(0) > 0 {
                if trailer.trim().is_empty() {
                    break;
                }
                trailer.clear();
            }
            break;
        }
        if out.len().saturating_add(size) > MAX_RESPONSE_BYTES {
            return Err(format!("chunked response exceeds {MAX_RESPONSE_BYTES} byte limit"));
        }
        let start = out.len();
        out.resize(start + size, 0);
        reader
            .read_exact(&mut out[start..])
            .map_err(|e| format!("failed to read chunk data: {e}"))?;
        let mut crlf = [0u8; 2];
        reader
            .read_exact(&mut crlf)
            .map_err(|e| format!("failed to read chunk terminator: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parse_url_allows_https_and_loopback_http_only() {
        let u = parse_url("https://studio.example:8443/api/health?x=1").unwrap();
        assert!(u.is_https);
        assert_eq!(u.host, "studio.example");
        assert_eq!(u.port, 8443);
        assert_eq!(u.host_header, "studio.example:8443");
        assert_eq!(u.path_and_query, "/api/health?x=1");

        let local = parse_url("http://127.0.0.1:8804/api/sites/ws-a/s1/sync").unwrap();
        assert!(!local.is_https);
        assert_eq!(local.port, 8804);

        let err = parse_url("http://studio.example/api/health").unwrap_err();
        assert!(err.contains("refusing plaintext http://"), "{err}");
    }

    #[test]
    fn read_http_response_handles_chunked_and_content_length() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n7\r\n{\"ok\": \r\n5\r\ntrue}\r\n0\r\n\r\n";
        let resp = read_http_response(Cursor::new(&raw[..])).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.text().unwrap(), "{\"ok\": true}");
    }
}
