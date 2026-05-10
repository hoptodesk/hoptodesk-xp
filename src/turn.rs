
use crate::network::FramedStream;
use crate::websocket::WsClient;
use crate::wininet;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::config::mask_ip;

fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
        0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
        0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
        0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
        0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
        0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];

    let (mut h0, mut h1, mut h2, mut h3) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);

    let bit_len = (input.len() as u64) * 8;
    let mut msg = input.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 { msg.push(0); }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.chunks(64) {
        let mut m = [0u32; 16];
        for i in 0..16 {
            m[i] = u32::from_le_bytes([chunk[4*i], chunk[4*i+1], chunk[4*i+2], chunk[4*i+3]]);
        }
        let (mut a, mut b, mut c, mut d) = (h0, h1, h2, h3);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]).rotate_left(S[i]),
            );
            a = temp;
        }
        h0 = h0.wrapping_add(a); h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c); h3 = h3.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&h0.to_le_bytes());
    out[4..8].copy_from_slice(&h1.to_le_bytes());
    out[8..12].copy_from_slice(&h2.to_le_bytes());
    out[12..16].copy_from_slice(&h3.to_le_bytes());
    out
}

fn sha1(input: &[u8]) -> [u8; 20] {
    let (mut h0, mut h1, mut h2, mut h3, mut h4) =
        (0x67452301u32, 0xEFCDAB89u32, 0x98BADCFEu32, 0x10325476u32, 0xC3D2E1F0u32);

    let bit_len = (input.len() as u64) * 8;
    let mut msg = input.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 { msg.push(0); }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4*i], chunk[4*i+1], chunk[4*i+2], chunk[4*i+3]]);
        }
        for i in 16..80 {
            w[i] = (w[i-3] ^ w[i-8] ^ w[i-14] ^ w[i-16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e)
                .wrapping_add(k).wrapping_add(w[i]);
            e = d; d = c; c = b.rotate_left(30); b = a; a = temp;
        }
        h0 = h0.wrapping_add(a); h1 = h1.wrapping_add(b); h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d); h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut k = if key.len() > 64 { sha1(key).to_vec() } else { key.to_vec() };
    k.resize(64, 0);

    let mut ipad = vec![0x36u8; 64];
    let mut opad = vec![0x5Cu8; 64];
    for i in 0..64 { ipad[i] ^= k[i]; opad[i] ^= k[i]; }

    ipad.extend_from_slice(data);
    let inner_hash = sha1(&ipad);
    opad.extend_from_slice(&inner_hash);
    sha1(&opad)
}

const MAGIC: u32 = 0x2112A442;

const ATTR_USERNAME: u16 = 0x0006;
const ATTR_MSG_INTEGRITY: u16 = 0x0008;
const ATTR_ERROR_CODE: u16 = 0x0009;
const ATTR_XOR_PEER_ADDR: u16 = 0x0012;
const ATTR_REALM: u16 = 0x0014;
const ATTR_NONCE: u16 = 0x0015;
const ATTR_XOR_RELAYED_ADDR: u16 = 0x0016;
const ATTR_REQ_TRANSPORT: u16 = 0x0019;
const ATTR_XOR_MAPPED_ADDR: u16 = 0x0020;
const ATTR_CONNECTION_ID: u16 = 0x002A;

const METHOD_ALLOCATE: u16 = 0x003;
const METHOD_CREATE_PERM: u16 = 0x008;
const METHOD_CONN_BIND: u16 = 0x00B;
const METHOD_CONN_ATTEMPT: u16 = 0x00C;

const CLASS_REQUEST: u16 = 0;
const CLASS_INDICATION: u16 = 1;
const CLASS_SUCCESS: u16 = 2;
const CLASS_ERROR: u16 = 3;

fn stun_type(method: u16, class: u16) -> u16 {
    (method & 0xF)
        | ((class & 0x1) << 4)
        | ((method & 0x70) << 1)
        | ((class & 0x2) << 7)
        | ((method & 0xF80) << 2)
}

fn stun_method(msg_type: u16) -> u16 {
    (msg_type & 0xF) | ((msg_type >> 1) & 0x70) | ((msg_type >> 2) & 0xF80)
}

fn stun_class(msg_type: u16) -> u16 {
    ((msg_type >> 4) & 0x1) | ((msg_type >> 7) & 0x2)
}

fn new_tid() -> [u8; 12] {
    let bytes = crate::config::generate_random_bytes(12);
    let mut tid = [0u8; 12];
    if bytes.len() == 12 {
        tid.copy_from_slice(&bytes);
    }
    tid
}

fn encode_xor_addr(addr: SocketAddr, _tid: &[u8; 12]) -> Vec<u8> {
    match addr {
        SocketAddr::V4(v4) => {
            let mut buf = vec![0u8; 8];
            buf[1] = 0x01;
            let port = v4.port() ^ ((MAGIC >> 16) as u16);
            buf[2..4].copy_from_slice(&port.to_be_bytes());
            let ip = u32::from_be_bytes(v4.ip().octets()) ^ MAGIC;
            buf[4..8].copy_from_slice(&ip.to_be_bytes());
            buf
        }
        _ => Vec::new(),
    }
}

fn decode_xor_addr(value: &[u8], _tid: &[u8; 12]) -> Option<SocketAddr> {
    if value.len() < 8 || value[1] != 0x01 { return None; }
    let port = u16::from_be_bytes([value[2], value[3]]) ^ ((MAGIC >> 16) as u16);
    let ip = u32::from_be_bytes([value[4], value[5], value[6], value[7]]) ^ MAGIC;
    let o = ip.to_be_bytes();
    Some(SocketAddr::from((std::net::Ipv4Addr::new(o[0], o[1], o[2], o[3]), port)))
}

struct StunBuilder {
    msg_type: u16,
    tid: [u8; 12],
    attrs: Vec<u8>,
}

impl StunBuilder {
    fn new(method: u16, class: u16) -> Self {
        Self { msg_type: stun_type(method, class), tid: new_tid(), attrs: Vec::new() }
    }

    fn add_attr(&mut self, attr_type: u16, value: &[u8]) {
        self.attrs.extend_from_slice(&attr_type.to_be_bytes());
        self.attrs.extend_from_slice(&(value.len() as u16).to_be_bytes());
        self.attrs.extend_from_slice(value);
        let pad = (4 - (value.len() % 4)) % 4;
        for _ in 0..pad { self.attrs.push(0); }
    }

    fn add_username(&mut self, u: &str) { self.add_attr(ATTR_USERNAME, u.as_bytes()); }
    fn add_realm(&mut self, r: &str) { self.add_attr(ATTR_REALM, r.as_bytes()); }
    fn add_nonce(&mut self, n: &str) { self.add_attr(ATTR_NONCE, n.as_bytes()); }
    fn add_transport_tcp(&mut self) { self.add_attr(ATTR_REQ_TRANSPORT, &[6, 0, 0, 0]); }
    fn add_connection_id(&mut self, id: u32) { self.add_attr(ATTR_CONNECTION_ID, &id.to_be_bytes()); }

    fn add_xor_peer_addr(&mut self, addr: SocketAddr) {
        self.add_attr(ATTR_XOR_PEER_ADDR, &encode_xor_addr(addr, &self.tid));
    }

    fn build(self, key: Option<&[u8]>) -> Vec<u8> {
        let body_len = self.attrs.len() + if key.is_some() { 24 } else { 0 };
        let mut msg = Vec::with_capacity(20 + body_len);
        msg.extend_from_slice(&self.msg_type.to_be_bytes());
        msg.extend_from_slice(&(body_len as u16).to_be_bytes());
        msg.extend_from_slice(&MAGIC.to_be_bytes());
        msg.extend_from_slice(&self.tid);
        msg.extend_from_slice(&self.attrs);
        if let Some(key) = key {
            let hmac = hmac_sha1(key, &msg);
            msg.extend_from_slice(&ATTR_MSG_INTEGRITY.to_be_bytes());
            msg.extend_from_slice(&20u16.to_be_bytes());
            msg.extend_from_slice(&hmac);
        }
        msg
    }
}

struct ParsedStun {
    msg_type: u16,
    tid: [u8; 12],
    attrs: Vec<(u16, Vec<u8>)>,
}

impl ParsedStun {
    fn class(&self) -> u16 { stun_class(self.msg_type) }
    fn method(&self) -> u16 { stun_method(self.msg_type) }

    fn get_attr(&self, t: u16) -> Option<&[u8]> {
        self.attrs.iter().find(|(at, _)| *at == t).map(|(_, v)| v.as_slice())
    }

    fn get_str(&self, t: u16) -> Option<String> {
        self.get_attr(t).and_then(|v| String::from_utf8(v.to_vec()).ok())
    }

    fn get_error(&self) -> Option<(u16, String)> {
        self.get_attr(ATTR_ERROR_CODE).and_then(|v| {
            if v.len() >= 4 {
                Some((v[2] as u16 * 100 + v[3] as u16, String::from_utf8_lossy(&v[4..]).into()))
            } else { None }
        })
    }

    fn get_xor_addr(&self, t: u16) -> Option<SocketAddr> {
        self.get_attr(t).and_then(|v| decode_xor_addr(v, &self.tid))
    }

    fn get_conn_id(&self) -> Option<u32> {
        self.get_attr(ATTR_CONNECTION_ID).and_then(|v| {
            if v.len() >= 4 { Some(u32::from_be_bytes([v[0], v[1], v[2], v[3]])) } else { None }
        })
    }
}

fn parse_stun(data: &[u8]) -> Option<ParsedStun> {
    if data.len() < 20 { return None; }
    let msg_type = u16::from_be_bytes([data[0], data[1]]);
    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    if u32::from_be_bytes([data[4], data[5], data[6], data[7]]) != MAGIC { return None; }
    if data.len() < 20 + msg_len { return None; }

    let mut tid = [0u8; 12];
    tid.copy_from_slice(&data[8..20]);

    let mut attrs = Vec::new();
    let mut pos = 20;
    while pos + 4 <= 20 + msg_len {
        let at = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let al = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        if pos + al > data.len() { break; }
        attrs.push((at, data[pos..pos + al].to_vec()));
        pos += al;
        pos = (pos + 3) & !3;
    }
    Some(ParsedStun { msg_type, tid, attrs })
}

fn send_stun(stream: &mut TcpStream, msg: &[u8]) -> io::Result<()> {
    stream.write_all(msg)?;
    stream.flush()
}

fn recv_stun(stream: &mut TcpStream) -> io::Result<ParsedStun> {
    let mut header = [0u8; 20];
    stream.read_exact(&mut header)?;
    let msg_len = u16::from_be_bytes([header[2], header[3]]) as usize;
    if msg_len > 65535 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "STUN message too large"));
    }
    let mut body = vec![0u8; msg_len];
    stream.read_exact(&mut body)?;
    let mut full = Vec::with_capacity(20 + msg_len);
    full.extend_from_slice(&header);
    full.extend_from_slice(&body);
    parse_stun(&full).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid STUN"))
}

pub struct TurnServer {
    pub addr: String,
    pub username: String,
    pub password: String,
}

pub fn get_public_ip() -> Option<SocketAddr> {
    let t0 = std::time::Instant::now();
    let servers = get_turn_servers_from_api();
    crate::config::write_log(&format!("[turn] get_public_ip: {} TURN server(s), api took {}ms", servers.len(), t0.elapsed().as_millis()));
    for server in &servers {
        let sock = match resolve_addr(&server.addr) {
            Ok(s) => s,
            Err(e) => {
                crate::config::write_log(&format!("[turn] Resolve {} failed: {}", mask_ip(&server.addr), e));
                continue;
            }
        };
        if let Ok(mut stream) = crate::tls_client::connect_tcp_timeout(&sock.ip().to_string(), sock.port(), Duration::from_secs(3)) {
            stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
            stream.set_nodelay(true).ok();

            let req = StunBuilder::new(0x001, CLASS_REQUEST);
            if send_stun(&mut stream, &req.build(None)).is_ok() {
                if let Ok(resp) = recv_stun(&mut stream) {
                    if let Some(addr) = resp.get_xor_addr(ATTR_XOR_MAPPED_ADDR) {
                        crate::config::write_log(&format!("[turn] Public IP: {} (total {}ms)", mask_ip(&addr), t0.elapsed().as_millis()));
                        return Some(addr);
                    }
                }
            }
        } else {
            crate::config::write_log(&format!("[turn] Connect to {} failed", mask_ip(&server.addr)));
        }
    }
    crate::config::write_log(&format!("[turn] get_public_ip failed after {}ms", t0.elapsed().as_millis()));
    None
}

pub fn get_turn_servers_from_api() -> Vec<TurnServer> {
    let body = match crate::signal::api_get() {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let json: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut servers = Vec::new();
    if let Some(arr) = json["turnservers"].as_array() {
        for s in arr {
            if s["protocol"].as_str().unwrap_or("") != "turn" { continue; }
            let host = s["host"].as_str().unwrap_or("");
            let port = s["port"].as_str().unwrap_or("");
            let user = s["username"].as_str().unwrap_or("");
            let pass = s["password"].as_str().unwrap_or("");
            if !host.is_empty() && !port.is_empty() {
                servers.push(TurnServer {
                    addr: format!("{}:{}", host, port),
                    username: user.to_string(),
                    password: pass.to_string(),
                });
            }
        }
    }
    servers
}

fn stun_key(username: &str, realm: &str, password: &str) -> [u8; 16] {
    md5(format!("{}:{}:{}", username, realm, password).as_bytes())
}

fn resolve_addr(addr: &str) -> io::Result<SocketAddr> {
    addr.parse().or_else(|_| {
        addr.to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "DNS resolve failed"))
    })
}

fn turn_allocate(
    stream: &mut TcpStream,
    username: &str,
    password: &str,
) -> Result<(SocketAddr, String, String), String> {

    let mut req = StunBuilder::new(METHOD_ALLOCATE, CLASS_REQUEST);
    req.add_transport_tcp();
    send_stun(stream, &req.build(None)).map_err(|e| format!("send: {}", e))?;

    let resp = recv_stun(stream).map_err(|e| format!("recv: {}", e))?;

    if resp.class() == CLASS_SUCCESS {
        let relay = resp.get_xor_addr(ATTR_XOR_RELAYED_ADDR)
            .ok_or("No relay addr in success")?;
        let realm = resp.get_str(ATTR_REALM).unwrap_or_default();
        let nonce = resp.get_str(ATTR_NONCE).unwrap_or_default();
        return Ok((relay, realm, nonce));
    }

    if resp.class() != CLASS_ERROR {
        return Err("Unexpected STUN response class".into());
    }
    let (code, reason) = resp.get_error().unwrap_or((0, "unknown".into()));
    if code != 401 {
        return Err(format!("Allocate error {}: {}", code, reason));
    }

    let realm = resp.get_str(ATTR_REALM).ok_or("No REALM in 401")?;
    let nonce = resp.get_str(ATTR_NONCE).ok_or("No NONCE in 401")?;

    let key = stun_key(username, &realm, password);
    let mut req2 = StunBuilder::new(METHOD_ALLOCATE, CLASS_REQUEST);
    req2.add_transport_tcp();
    req2.add_username(username);
    req2.add_realm(&realm);
    req2.add_nonce(&nonce);
    send_stun(stream, &req2.build(Some(&key))).map_err(|e| format!("send: {}", e))?;

    let resp2 = recv_stun(stream).map_err(|e| format!("recv: {}", e))?;
    if resp2.class() == CLASS_ERROR {
        let (code, reason) = resp2.get_error().unwrap_or((0, "unknown".into()));
        return Err(format!("Allocate error {}: {}", code, reason));
    }

    let relay = resp2.get_xor_addr(ATTR_XOR_RELAYED_ADDR)
        .ok_or("No XOR-RELAYED-ADDRESS")?;
    Ok((relay, realm, nonce))
}

fn turn_create_permission(
    stream: &mut TcpStream,
    peer_addr: SocketAddr,
    username: &str, password: &str, realm: &str, nonce: &str,
) -> Result<(), String> {
    let key = stun_key(username, realm, password);
    let mut req = StunBuilder::new(METHOD_CREATE_PERM, CLASS_REQUEST);
    req.add_xor_peer_addr(peer_addr);
    req.add_username(username);
    req.add_realm(realm);
    req.add_nonce(nonce);
    send_stun(stream, &req.build(Some(&key))).map_err(|e| format!("send: {}", e))?;

    let resp = recv_stun(stream).map_err(|e| format!("recv: {}", e))?;
    if resp.class() == CLASS_ERROR {
        let (code, reason) = resp.get_error().unwrap_or((0, "unknown".into()));
        return Err(format!("CreatePermission error {}: {}", code, reason));
    }
    Ok(())
}

fn wait_connection_attempt(stream: &mut TcpStream, timeout: Duration) -> Result<u32, String> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("set_timeout: {}", e))?;
    let deadline = std::time::Instant::now() + timeout;

    loop {
        if std::time::Instant::now() >= deadline {
            return Err("Timeout waiting for ConnectionAttempt".into());
        }
        match recv_stun(stream) {
            Ok(msg) => {
                if msg.method() == METHOD_CONN_ATTEMPT && msg.class() == CLASS_INDICATION {
                    return msg.get_conn_id()
                        .ok_or_else(|| "ConnectionAttempt missing CONNECTION-ID".into());
                }

            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock
                || e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(format!("recv: {}", e)),
        }
    }
}

fn turn_connection_bind(
    turn_addr: &SocketAddr,
    conn_id: u32,
    username: &str, password: &str, realm: &str, nonce: &str,
) -> Result<TcpStream, String> {
    let mut data_conn = crate::tls_client::connect_tcp_timeout(&turn_addr.ip().to_string(), turn_addr.port(), Duration::from_secs(5))
        .map_err(|e| format!("data connect: {}", e))?;
    data_conn.set_nodelay(true).ok();

    let key = stun_key(username, realm, password);
    let mut req = StunBuilder::new(METHOD_CONN_BIND, CLASS_REQUEST);
    req.add_connection_id(conn_id);
    req.add_username(username);
    req.add_realm(realm);
    req.add_nonce(nonce);
    send_stun(&mut data_conn, &req.build(Some(&key))).map_err(|e| format!("send: {}", e))?;

    let resp = recv_stun(&mut data_conn).map_err(|e| format!("recv: {}", e))?;
    if resp.class() == CLASS_ERROR {
        let (code, reason) = resp.get_error().unwrap_or((0, "unknown".into()));
        return Err(format!("ConnectionBind error {}: {}", code, reason));
    }
    Ok(data_conn)
}

fn try_turn_server(
    server: &TurnServer,
    peer_addr: SocketAddr,
    peer_id: &str,
    my_id: &str,
    ws_host: &str,
    ws_port: u16,
) -> Result<FramedStream, String> {
    crate::config::write_log(&format!("[turn] Trying TURN server: {}", mask_ip(&server.addr)));

    let turn_sock = resolve_addr(&server.addr)
        .map_err(|e| format!("resolve: {}", e))?;
    let mut ctrl = crate::tls_client::connect_tcp_timeout(&turn_sock.ip().to_string(), turn_sock.port(), Duration::from_secs(5))
        .map_err(|e| format!("connect: {}", e))?;
    ctrl.set_nodelay(true).ok();
    ctrl.set_read_timeout(Some(Duration::from_secs(10))).ok();

    let (relay_addr, realm, nonce) = turn_allocate(
        &mut ctrl, &server.username, &server.password,
    )?;
    crate::config::write_log(&format!("[turn] Allocated relay: {}", mask_ip(&relay_addr)));

    turn_create_permission(
        &mut ctrl, peer_addr,
        &server.username, &server.password, &realm, &nonce,
    )?;
    crate::config::write_log(&format!("[turn] Permission created for {}", mask_ip(&peer_addr)));

    let relay_msg = serde_json::json!({
        "protocol": "one-to-one",
        "endpoint": peer_id,
        "addr": relay_addr.to_string(),
    });

    let temp_id = format!("{}_{}", my_id, std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis());
    let path = format!("/?user={}", temp_id);

    let mut ws = WsClient::connect(ws_host, ws_port, &path)
        .map_err(|e| format!("WS connect: {}", e))?;
    ws.send_text(&relay_msg.to_string())
        .map_err(|e| format!("Send RelayConnection: {}", e))?;
    crate::config::write_log(&format!("[turn] Sent RelayConnection to {}", peer_id));

    let conn_id = wait_connection_attempt(&mut ctrl, Duration::from_secs(15))?;
    crate::config::write_log(&format!("[turn] ConnectionAttempt, id={}", conn_id));

    let data_conn = turn_connection_bind(
        &turn_sock, conn_id,
        &server.username, &server.password, &realm, &nonce,
    )?;
    crate::config::write_log(&format!("[turn] ConnectionBind OK"));

    let ready_msg = serde_json::json!({
        "protocol": "one-to-one",
        "endpoint": peer_id,
    });
    ws.send_text(&ready_msg.to_string()).ok();
    crate::config::write_log(&format!("[turn] Sent RelayReady"));

    Ok(FramedStream::from_tcp(data_conn))
}

pub fn connect_via_turn(
    peer_addr: &str,
    peer_id: &str,
    my_id: &str,
    ws_host: &str,
    ws_port: u16,
) -> Result<FramedStream, String> {
    let servers = get_turn_servers_from_api();
    if servers.is_empty() {
        return Err("No TURN servers available".into());
    }
    crate::config::write_log(&format!("[turn] {} TURN server(s) available", servers.len()));

    let peer_sock: SocketAddr = peer_addr.parse()
        .map_err(|e| format!("parse peer addr '{}': {}", peer_addr, e))?;

    let mut last_err = String::new();
    for server in &servers {
        match try_turn_server(server, peer_sock, peer_id, my_id, ws_host, ws_port) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                crate::config::write_log(&format!("[turn] Server {} failed: {}", mask_ip(&server.addr), e));
                last_err = e;
            }
        }
    }

    Err(format!("All TURN servers failed: {}", last_err))
}
