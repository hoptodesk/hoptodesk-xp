
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub fn encode_frame(data: &[u8], buf: &mut Vec<u8>) {
    let n = data.len();
    if n <= 0x3F {
        buf.push((n << 2) as u8);
    } else if n <= 0x3FFF {
        let v = ((n << 2) as u16) | 0x1;
        buf.push(v as u8);
        buf.push((v >> 8) as u8);
    } else if n <= 0x3FFFFF {
        let v = ((n << 2) as u32) | 0x2;
        buf.push(v as u8);
        buf.push((v >> 8) as u8);
        buf.push((v >> 16) as u8);
    } else if n <= 0x3FFFFFFF {
        let v = ((n << 2) as u32) | 0x3;
        buf.push(v as u8);
        buf.push((v >> 8) as u8);
        buf.push((v >> 16) as u8);
        buf.push((v >> 24) as u8);
    }
    buf.extend_from_slice(data);
}

pub fn read_frame(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut first = [0u8; 1];
    stream.read_exact(&mut first)?;

    let head_len = ((first[0] & 0x3) + 1) as usize;
    let mut header = [0u8; 4];
    header[0] = first[0];

    if head_len > 1 {
        stream.read_exact(&mut header[1..head_len])?;
    }

    let mut n = header[0] as usize;
    if head_len > 1 {
        n |= (header[1] as usize) << 8;
    }
    if head_len > 2 {
        n |= (header[2] as usize) << 16;
    }
    if head_len > 3 {
        n |= (header[3] as usize) << 24;
    }
    n >>= 2;

    if n > 16 * 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Frame too large"));
    }

    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

struct Encrypt {
    key: [u8; 32],
    send_seq: u64,
    recv_seq: u64,
}

impl Encrypt {
    fn new(key: [u8; 32]) -> Self {
        Self { key, send_seq: 0, recv_seq: 0 }
    }

    fn enc(&mut self, data: &[u8]) -> Vec<u8> {
        self.send_seq += 1;
        let mut nonce = [0u8; 24];
        nonce[..8].copy_from_slice(&self.send_seq.to_le_bytes());
        crate::crypto::secretbox_seal(&self.key, &nonce, data)
    }

    fn dec(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
        self.recv_seq += 1;
        let mut nonce = [0u8; 24];
        nonce[..8].copy_from_slice(&self.recv_seq.to_le_bytes());
        crate::crypto::secretbox_open(&self.key, &nonce, data)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "decryption error"))
    }
}

pub struct FramedStream {
    stream: TcpStream,
    encrypt: Option<Encrypt>,
}

impl FramedStream {
    pub fn from_tcp(stream: TcpStream) -> Self {
        Self { stream, encrypt: None }
    }

    pub fn connect(addr: &str, timeout: Duration) -> io::Result<Self> {
        let stream = if let Some(pos) = addr.rfind(':') {
            let host = &addr[..pos];
            let port: u16 = addr[pos + 1..].parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("bad port: {}", e))
            })?;
            // Use proxy-aware TCP connect
            crate::tls_client::connect_tcp_timeout(host, port, timeout)
                .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e))?
        } else {
            TcpStream::connect(addr)?
        };
        stream.set_nodelay(true).ok();
        Ok(Self { stream, encrypt: None })
    }

    pub fn set_key(&mut self, key: [u8; 32]) {
        self.encrypt = Some(Encrypt::new(key));
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_write_timeout(timeout)
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.stream.local_addr()
    }

    pub fn peer_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.stream.peer_addr()
    }

    pub fn send_msg(&mut self, data: &[u8]) -> io::Result<()> {
        let payload = if let Some(ref mut enc) = self.encrypt {
            enc.enc(data)
        } else {
            data.to_vec()
        };
        let mut buf = Vec::with_capacity(payload.len() + 4);
        encode_frame(&payload, &mut buf);
        self.stream.write_all(&buf)?;
        self.stream.flush()
    }

    pub fn recv_msg(&mut self) -> io::Result<Vec<u8>> {
        let raw = read_frame(&mut self.stream)?;
        if let Some(ref mut enc) = self.encrypt {
            enc.dec(&raw)
        } else {
            Ok(raw)
        }
    }

    pub fn tcp_stream(&self) -> &TcpStream {
        &self.stream
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            encrypt: self.encrypt.as_ref().map(|e| Encrypt {
                key: e.key,
                send_seq: e.send_seq,
                recv_seq: e.recv_seq,
            }),
        })
    }
}

pub fn new_listener(addr: &str, port: u16) -> io::Result<std::net::TcpListener> {
    let bind_addr = format!("{}:{}", addr, port);
    let listener = std::net::TcpListener::bind(&bind_addr)?;
    Ok(listener)
}

pub fn get_local_ip() -> String {

    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if sock.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = sock.local_addr() {
                return addr.ip().to_string();
            }
        }
    }
    "0.0.0.0".to_string()
}
