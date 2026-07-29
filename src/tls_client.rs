
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::wininet::{parse_url, url_encode};

pub fn tls_config() -> Arc<ClientConfig> {
    TLS_CONFIG.clone()
}

fn make_tls_config() -> Arc<ClientConfig> {
    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = rustls::crypto::ring::default_provider();
    Arc::new(
        ClientConfig::builder_with_provider(provider.into())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(root_store)
            .with_no_client_auth(),
    )
}

lazy_static::lazy_static! {
    static ref TLS_CONFIG: Arc<ClientConfig> = make_tls_config();
    static ref DNS_CACHE: std::sync::Mutex<std::collections::HashMap<String, (Vec<std::net::SocketAddr>, std::time::Instant)>> =
        std::sync::Mutex::new(std::collections::HashMap::new());
}

const DNS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

fn resolve_with_cache(host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, String> {
    use std::net::ToSocketAddrs;
    let key = format!("{}:{}", host, port);
    if let Ok(cache) = DNS_CACHE.lock() {
        if let Some((addrs, at)) = cache.get(&key) {
            if at.elapsed() < DNS_CACHE_TTL && !addrs.is_empty() {
                return Ok(addrs.clone());
            }
        }
    }
    match (host, port).to_socket_addrs() {
        Ok(iter) => {
            let addrs: Vec<std::net::SocketAddr> = iter.collect();
            if addrs.is_empty() {
                return Err("DNS resolve: no addresses".to_string());
            }
            if let Ok(mut cache) = DNS_CACHE.lock() {
                cache.insert(key, (addrs.clone(), std::time::Instant::now()));
            }
            Ok(addrs)
        }
        Err(e) => {
            if let Ok(cache) = DNS_CACHE.lock() {
                if let Some((addrs, _)) = cache.get(&key) {
                    if !addrs.is_empty() {
                        crate::config::write_log(&format!(
                            "[dns] Resolve failed ({}), using stale cache for {}",
                            e, key
                        ));
                        return Ok(addrs.clone());
                    }
                }
            }
            Err(format!("DNS resolve failed: {}", e))
        }
    }
}

pub struct ProxySettings {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub proxy_type: String,
}

pub fn get_proxy_settings() -> Option<ProxySettings> {
    let cfg = crate::config::Config2::load();
    let proxy = cfg.get_option("socks-proxy");
    if proxy.is_empty() {
        return None;
    }
    let username = cfg.get_option("socks-username");
    let password = cfg.get_option("socks-password");
    let proxy_type = cfg.get_option("socks-proxy-type");
    let (proxy_host, proxy_port) = if let Some(colon) = proxy.rfind(':') {
        let h = &proxy[..colon];
        let p = proxy[colon + 1..].parse::<u16>().unwrap_or(1080);
        (h.to_string(), p)
    } else {
        (proxy.clone(), 1080)
    };
    Some(ProxySettings {
        host: proxy_host,
        port: proxy_port,
        username,
        password,
        proxy_type: if proxy_type.is_empty() { "auto".into() } else { proxy_type },
    })
}

pub fn connect_tcp(target_host: &str, target_port: u16) -> Result<TcpStream, String> {
    connect_tcp_timeout(target_host, target_port, std::time::Duration::from_secs(30))
}

pub fn connect_tcp_timeout(target_host: &str, target_port: u16, timeout: std::time::Duration) -> Result<TcpStream, String> {
    if let Some(proxy) = get_proxy_settings() {
        crate::config::write_log(&format!(
            "[proxy] Connecting to {}:{} via proxy {}:{} (type={})",
            target_host, target_port, proxy.host, proxy.port, proxy.proxy_type
        ));
        let proxy_addr: std::net::SocketAddr = format!("{}:{}", proxy.host, proxy.port)
            .parse()
            .or_else(|_| {

                use std::net::ToSocketAddrs;
                (proxy.host.as_str(), proxy.port)
                    .to_socket_addrs()
                    .map_err(|e| format!("Proxy DNS resolve failed: {}", e))?
                    .next()
                    .ok_or_else(|| "Proxy DNS resolve: no addresses".to_string())
            })?;
        let mut tcp = TcpStream::connect_timeout(&proxy_addr, timeout)
            .map_err(|e| format!("Proxy connect failed: {}", e))?;
        tcp.set_read_timeout(Some(std::time::Duration::from_secs(30))).ok();

        match proxy.proxy_type.as_str() {
            "socks5" => socks5_handshake(&mut tcp, target_host, target_port, &proxy.username, &proxy.password)?,
            "http" => http_connect(&mut tcp, target_host, target_port, &proxy.username, &proxy.password)?,
            _ => {

                if socks5_handshake(&mut tcp, target_host, target_port, &proxy.username, &proxy.password).is_err() {
                    drop(tcp);
                    tcp = TcpStream::connect_timeout(&proxy_addr, timeout)
                        .map_err(|e| format!("Proxy reconnect failed: {}", e))?;
                    tcp.set_read_timeout(Some(std::time::Duration::from_secs(30))).ok();
                    http_connect(&mut tcp, target_host, target_port, &proxy.username, &proxy.password)?;
                }
            }
        }
        crate::config::write_log(&format!("[proxy] Tunnel established to {}:{}", target_host, target_port));
        Ok(tcp)
    } else {

        if let Ok(addr) = format!("{}:{}", target_host, target_port).parse::<std::net::SocketAddr>() {
            return TcpStream::connect_timeout(&addr, timeout)
                .map_err(|e| format!("TCP connect failed: {}", e));
        }
        let addrs = resolve_with_cache(target_host, target_port)?;
        let mut last_err = String::new();
        for addr in &addrs {
            match TcpStream::connect_timeout(addr, timeout) {
                Ok(tcp) => return Ok(tcp),
                Err(e) => {
                    last_err = format!("TCP connect to {} failed: {}", addr, e);
                    crate::config::write_log(&format!("[tcp] {}", last_err));
                }
            }
        }
        Err(last_err)
    }
}

fn socks5_handshake(tcp: &mut TcpStream, target_host: &str, target_port: u16, username: &str, password: &str) -> Result<(), String> {

    let auth_method = if username.is_empty() { 0x00u8 } else { 0x02u8 };
    tcp.write_all(&[0x05, 0x01, auth_method])
        .map_err(|e| format!("SOCKS5 greeting write: {}", e))?;
    tcp.flush().map_err(|e| format!("SOCKS5 flush: {}", e))?;

    let mut resp = [0u8; 2];
    tcp.read_exact(&mut resp)
        .map_err(|e| format!("SOCKS5 greeting read: {}", e))?;
    if resp[0] != 0x05 {
        return Err(format!("SOCKS5: bad version {}", resp[0]));
    }
    if resp[1] == 0xFF {
        return Err("SOCKS5: no acceptable auth method".to_string());
    }

    if resp[1] == 0x02 {
        let mut auth = vec![0x01];
        auth.push(username.len() as u8);
        auth.extend_from_slice(username.as_bytes());
        auth.push(password.len() as u8);
        auth.extend_from_slice(password.as_bytes());
        tcp.write_all(&auth).map_err(|e| format!("SOCKS5 auth write: {}", e))?;
        tcp.flush().map_err(|e| format!("SOCKS5 auth flush: {}", e))?;

        let mut auth_resp = [0u8; 2];
        tcp.read_exact(&mut auth_resp).map_err(|e| format!("SOCKS5 auth read: {}", e))?;
        if auth_resp[1] != 0x00 {
            return Err("SOCKS5: authentication failed".to_string());
        }
    }

    let mut req = vec![0x05, 0x01, 0x00];

    req.push(0x03);
    req.push(target_host.len() as u8);
    req.extend_from_slice(target_host.as_bytes());
    req.push((target_port >> 8) as u8);
    req.push((target_port & 0xFF) as u8);
    tcp.write_all(&req).map_err(|e| format!("SOCKS5 connect write: {}", e))?;
    tcp.flush().map_err(|e| format!("SOCKS5 connect flush: {}", e))?;

    let mut hdr = [0u8; 4];
    tcp.read_exact(&mut hdr).map_err(|e| format!("SOCKS5 connect read: {}", e))?;
    if hdr[0] != 0x05 {
        return Err(format!("SOCKS5: bad response version {}", hdr[0]));
    }
    if hdr[1] != 0x00 {
        let err_msg = match hdr[1] {
            0x01 => "general failure",
            0x02 => "connection not allowed",
            0x03 => "network unreachable",
            0x04 => "host unreachable",
            0x05 => "connection refused",
            0x06 => "TTL expired",
            0x07 => "command not supported",
            0x08 => "address type not supported",
            _ => "unknown error",
        };
        return Err(format!("SOCKS5 connect failed: {} ({})", err_msg, hdr[1]));
    }

    match hdr[3] {
        0x01 => { let mut skip = [0u8; 6]; tcp.read_exact(&mut skip).ok(); }
        0x03 => {
            let mut len = [0u8; 1];
            tcp.read_exact(&mut len).ok();
            let mut skip = vec![0u8; len[0] as usize + 2];
            tcp.read_exact(&mut skip).ok();
        }
        0x04 => { let mut skip = [0u8; 18]; tcp.read_exact(&mut skip).ok(); }
        _ => {}
    }
    Ok(())
}

fn http_connect(tcp: &mut TcpStream, target_host: &str, target_port: u16, username: &str, password: &str) -> Result<(), String> {
    let mut connect_req = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n",
        target_host, target_port, target_host, target_port
    );
    if !username.is_empty() {
        let creds = crate::websocket::base64_encode_bytes(
            format!("{}:{}", username, password).as_bytes()
        );
        connect_req.push_str(&format!("Proxy-Authorization: Basic {}\r\n", creds));
    }
    connect_req.push_str("\r\n");

    tcp.write_all(connect_req.as_bytes())
        .map_err(|e| format!("Proxy write failed: {}", e))?;
    tcp.flush().map_err(|e| format!("Proxy flush failed: {}", e))?;

    let mut resp_buf = [0u8; 1024];
    let mut total = 0;
    loop {
        let n = tcp.read(&mut resp_buf[total..])
            .map_err(|e| format!("Proxy read failed: {}", e))?;
        if n == 0 {
            return Err("Proxy closed connection".to_string());
        }
        total += n;
        if let Ok(s) = std::str::from_utf8(&resp_buf[..total]) {
            if s.contains("\r\n\r\n") {
                if !s.starts_with("HTTP/1.1 200") && !s.starts_with("HTTP/1.0 200") {
                    return Err(format!("Proxy CONNECT failed: {}", s.lines().next().unwrap_or("")));
                }
                return Ok(());
            }
        }
        if total >= resp_buf.len() {
            return Err("Proxy response too large".to_string());
        }
    }
}

fn tls_request(
    host: &str,
    port: u16,
    request_bytes: &[u8],
) -> Result<String, String> {
    let server_name: ServerName<'static> = ServerName::try_from(host.to_owned())
        .map_err(|_| format!("Invalid hostname: {}", host))?;
    let conn = ClientConnection::new(TLS_CONFIG.clone(), server_name)
        .map_err(|e| format!("TLS init failed: {}", e))?;
    let tcp = connect_tcp_timeout(host, port, std::time::Duration::from_secs(10))?;
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(15)))
        .ok();
    let mut stream = StreamOwned::new(conn, tcp);

    stream
        .write_all(request_bytes)
        .map_err(|e| format!("TLS write failed: {}", e))?;
    stream.flush().map_err(|e| format!("TLS flush failed: {}", e))?;

    let mut response = Vec::with_capacity(4096);
    let mut buf = [0u8; 4096];
    let mut header_end_pos: Option<usize> = None;

    loop {
        let n = stream.read(&mut buf)
            .map_err(|e| format!("TLS read failed: {}", e))?;
        if n == 0 { break; }
        response.extend_from_slice(&buf[..n]);
        if header_end_pos.is_none() {
            header_end_pos = find_header_end(&response);
        }
        if header_end_pos.is_some() { break; }
        if response.len() > 64 * 1024 {
            return Err("Response headers too large".to_string());
        }
    }

    let hdr_end = header_end_pos
        .ok_or_else(|| format!("No HTTP headers received ({} bytes read)", response.len()))?;
    let header_str = std::str::from_utf8(&response[..hdr_end])
        .map_err(|_| "Invalid HTTP response headers".to_string())?;
    let status_line = header_str.lines().next().unwrap_or("");

    crate::config::write_log(&format!("[tls] {} from {} ({} hdr bytes so far)",
        status_line, host, response.len()));

    if !status_line.contains(" 200 ") && !status_line.contains(" 201 ") {
        return Err(format!("HTTP error: {}", status_line));
    }

    let header_lower = header_str.to_lowercase();
    let body_start = hdr_end + 4;

    if header_lower.contains("transfer-encoding: chunked") {

        loop {
            let body_so_far = &response[body_start..];
            if body_ends_with_final_chunk(body_so_far) { break; }
            let n = stream.read(&mut buf)
                .map_err(|e| format!("TLS read (chunked) failed: {}", e))?;
            if n == 0 { break; }
            response.extend_from_slice(&buf[..n]);
        }
        decode_chunked(&response[body_start..])
    } else if let Some(cl) = parse_content_length(&header_lower) {

        while response.len() - body_start < cl {
            let n = stream.read(&mut buf)
                .map_err(|e| format!("TLS read (body) failed: {}", e))?;
            if n == 0 { break; }
            response.extend_from_slice(&buf[..n]);
        }
        String::from_utf8(response[body_start..].to_vec())
            .map_err(|e| format!("UTF-8 error: {}", e))
    } else {

        loop {
            let n = match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::ConnectionReset => break,
                Err(e) => return Err(format!("TLS read failed: {}", e)),
            };
            response.extend_from_slice(&buf[..n]);
        }
        String::from_utf8(response[body_start..].to_vec())
            .map_err(|e| format!("UTF-8 error: {}", e))
    }
}

fn body_ends_with_final_chunk(data: &[u8]) -> bool {

    if data.len() >= 5 && &data[data.len()-5..] == b"0\r\n\r\n" { return true; }

    for i in 0..data.len().saturating_sub(6) {
        if &data[i..i+7] == b"\r\n0\r\n\r\n" { return true; }
    }
    false
}

fn parse_content_length(header_lower: &str) -> Option<usize> {
    for line in header_lower.lines() {
        if line.starts_with("content-length:") {
            return line["content-length:".len()..].trim().parse().ok();
        }
    }
    None
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
}

fn decode_chunked(data: &[u8]) -> Result<String, String> {
    let mut result = Vec::new();
    let mut pos = 0;
    loop {

        let line_end = match find_crlf(data, pos) {
            Some(i) => i,
            None => break,
        };
        let size_str = std::str::from_utf8(&data[pos..line_end])
            .map_err(|_| "Invalid chunk size")?
            .trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| format!("Invalid chunk size: {}", size_str))?;
        if size == 0 {
            break;
        }
        let chunk_start = line_end + 2;
        let chunk_end = chunk_start + size;
        if chunk_end > data.len() {
            return Err("Truncated chunk".to_string());
        }
        result.extend_from_slice(&data[chunk_start..chunk_end]);
        pos = chunk_end + 2;
    }
    String::from_utf8(result).map_err(|e| format!("UTF-8 error: {}", e))
}

fn find_crlf(data: &[u8], start: usize) -> Option<usize> {
    for i in start..data.len().saturating_sub(1) {
        if data[i] == b'\r' && data[i + 1] == b'\n' {
            return Some(i);
        }
    }
    None
}

fn build_request(method: &str, host: &str, path: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut req = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        method, path, host
    );
    if !body.is_empty() {
        req.push_str(&format!("Content-Type: {}\r\n", content_type));
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    let mut out = req.into_bytes();
    out.extend_from_slice(body);
    out
}

pub fn http_get(url: &str) -> Result<String, String> {
    let parsed = parse_url(url)?;
    let req = build_request("GET", &parsed.host, &parsed.path, "", &[]);
    tls_request(&parsed.host, parsed.port, &req)
}

pub fn http_post_form(url: &str, params: &[(&str, &str)]) -> Result<String, String> {
    let parsed = parse_url(url)?;
    let body: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let req = build_request(
        "POST",
        &parsed.host,
        &parsed.path,
        "application/x-www-form-urlencoded",
        body.as_bytes(),
    );
    tls_request(&parsed.host, parsed.port, &req)
}

pub fn http_post_multipart(
    url: &str,
    fields: &[(&str, &str)],
    file_field: &str,
    file_name: &str,
    file_data: &[u8],
) -> Result<String, String> {
    let parsed = parse_url(url)?;
    let boundary = "----HopToDesk7b3a4c";
    let mut body = Vec::new();

    for (k, v) in fields {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n",
                k, v
            )
            .as_bytes(),
        );
    }

    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n",
            file_field, file_name
        )
        .as_bytes(),
    );
    body.extend_from_slice(file_data);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

    let content_type = format!("multipart/form-data; boundary={}", boundary);
    let req = build_request("POST", &parsed.host, &parsed.path, &content_type, &body);
    tls_request(&parsed.host, parsed.port, &req)
}
