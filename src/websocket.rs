
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use rustls::{ClientConnection, StreamOwned};
use rustls::pki_types::ServerName;

const WSS_FRAME_READ_TIMEOUT: Duration = Duration::from_secs(30);

pub struct WsClient {
    stream: TcpStream,
    pending: Vec<u8>,
}

pub struct WssClient {
    stream: StreamOwned<ClientConnection, TcpStream>,
    pending: Vec<u8>,
}

fn read_upgrade_response<S: Read>(stream: &mut S) -> std::io::Result<Vec<u8>> {
    let mut resp_buf = [0u8; 4096];
    let mut total = 0;
    loop {
        let n = stream.read(&mut resp_buf[total..])?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "Connection closed during handshake",
            ));
        }
        total += n;
        if let Some(end) = resp_buf[..total].windows(4).position(|w| w == b"\r\n\r\n") {
            if !resp_buf.starts_with(b"HTTP/1.1 101") {
                let head = String::from_utf8_lossy(&resp_buf[..end]);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("WebSocket upgrade failed: {}", head.lines().next().unwrap_or("")),
                ));
            }
            return Ok(resp_buf[end + 4..total].to_vec());
        }
        if total >= resp_buf.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "HTTP response too large",
            ));
        }
    }
}

impl WssClient {
    pub fn connect(host: &str, port: u16, path: &str) -> std::io::Result<Self> {
        let server_name: ServerName<'static> = ServerName::try_from(host.to_owned())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid hostname"))?;
        let conn = ClientConnection::new(crate::tls_client::tls_config(), server_name)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("TLS init: {}", e)))?;
        let tcp = crate::tls_client::connect_tcp(host, port)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(10)))?;
        let mut stream = StreamOwned::new(conn, tcp);

        let key = generate_ws_key();
        let request = format!(
            "GET {} HTTP/1.1\r\n\
             Host: {}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {}\r\n\
             Sec-WebSocket-Version: 13\r\n\
             \r\n",
            path, host, key
        );
        stream.write_all(request.as_bytes())?;
        stream.flush()?;

        let pending = read_upgrade_response(&mut stream)?;
        Ok(Self { stream, pending })
    }

    fn read_exact_buffered(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        let from_pending = buf.len().min(self.pending.len());
        buf[..from_pending].copy_from_slice(&self.pending[..from_pending]);
        self.pending.drain(..from_pending);
        if from_pending < buf.len() {
            self.stream.read_exact(&mut buf[from_pending..])?;
        }
        Ok(())
    }

    pub fn send_text(&mut self, msg: &str) -> std::io::Result<()> {
        let payload = msg.as_bytes();
        let mask = generate_mask();
        let mut frame = Vec::new();
        frame.push(0x81);
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len <= 65535 {
            frame.push(0x80 | 126);
            frame.push((len >> 8) as u8);
            frame.push(len as u8);
        } else {
            frame.push(0x80 | 127);
            for i in (0..8).rev() {
                frame.push((len >> (i * 8)) as u8);
            }
        }
        frame.extend_from_slice(&mask);
        for (i, &b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }
        self.stream.write_all(&frame)?;
        self.stream.flush()
    }

    pub fn recv_text(&mut self) -> std::io::Result<String> {
        loop {
            let (opcode, payload) = self.read_frame()?;
            match opcode {
                0x1 => {
                    return String::from_utf8(payload).map_err(|e| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                    });
                }
                0x8 => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "WebSocket closed by server",
                    ));
                }
                0x9 => {
                    self.send_pong(&payload)?;
                }
                0xA => {}
                _ => {}
            }
        }
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.stream.sock.set_read_timeout(timeout)
    }

    fn send_pong(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let mask = generate_mask();
        let mut frame = Vec::new();
        frame.push(0x8A);
        let len = payload.len();
        frame.push(0x80 | len as u8);
        frame.extend_from_slice(&mask);
        for (i, &b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }
        self.stream.write_all(&frame)?;
        self.stream.flush()
    }

    fn read_frame(&mut self) -> std::io::Result<(u8, Vec<u8>)> {
        let mut first = [0u8; 1];
        self.read_exact_buffered(&mut first)?;
        let idle_timeout = self.stream.sock.read_timeout()?;
        self.stream.sock.set_read_timeout(Some(WSS_FRAME_READ_TIMEOUT))?;
        let frame = self.read_frame_rest(first[0]);
        self.stream.sock.set_read_timeout(idle_timeout)?;
        frame.map_err(|e| match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "WebSocket frame stalled",
            ),
            _ => e,
        })
    }

    fn read_frame_rest(&mut self, first: u8) -> std::io::Result<(u8, Vec<u8>)> {
        let mut second = [0u8; 1];
        self.read_exact_buffered(&mut second)?;
        let opcode = first & 0x0F;
        let masked = (second[0] & 0x80) != 0;
        let mut len = (second[0] & 0x7F) as u64;
        if len == 126 {
            let mut ext = [0u8; 2];
            self.read_exact_buffered(&mut ext)?;
            len = u16::from_be_bytes(ext) as u64;
        } else if len == 127 {
            let mut ext = [0u8; 8];
            self.read_exact_buffered(&mut ext)?;
            len = u64::from_be_bytes(ext);
        }
        if len > 16 * 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "WebSocket frame too large",
            ));
        }
        let mask_key = if masked {
            let mut mk = [0u8; 4];
            self.read_exact_buffered(&mut mk)?;
            Some(mk)
        } else {
            None
        };
        let mut payload = vec![0u8; len as usize];
        self.read_exact_buffered(&mut payload)?;
        if let Some(mk) = mask_key {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mk[i % 4];
            }
        }
        Ok((opcode, payload))
    }
}

impl WsClient {

    pub fn connect(host: &str, port: u16, path: &str) -> std::io::Result<Self> {
        let mut stream = crate::tls_client::connect_tcp_timeout(host, port, std::time::Duration::from_secs(30))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::ConnectionRefused, e))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;

        let key = generate_ws_key();

        let request = format!(
            "GET {} HTTP/1.1\r\n\
             Host: {}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {}\r\n\
             Sec-WebSocket-Version: 13\r\n\
             \r\n",
            path, format!("{}:{}", host, port), key
        );
        stream.write_all(request.as_bytes())?;
        stream.flush()?;

        let pending = read_upgrade_response(&mut stream)?;
        Ok(Self { stream, pending })
    }

    fn read_exact_buffered(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        let from_pending = buf.len().min(self.pending.len());
        buf[..from_pending].copy_from_slice(&self.pending[..from_pending]);
        self.pending.drain(..from_pending);
        if from_pending < buf.len() {
            self.stream.read_exact(&mut buf[from_pending..])?;
        }
        Ok(())
    }

    pub fn send_text(&mut self, msg: &str) -> std::io::Result<()> {
        let payload = msg.as_bytes();
        let mask = generate_mask();

        let mut frame = Vec::new();
        frame.push(0x81);

        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len <= 65535 {
            frame.push(0x80 | 126);
            frame.push((len >> 8) as u8);
            frame.push(len as u8);
        } else {
            frame.push(0x80 | 127);
            for i in (0..8).rev() {
                frame.push((len >> (i * 8)) as u8);
            }
        }

        frame.extend_from_slice(&mask);

        for (i, &b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }

        self.stream.write_all(&frame)?;
        self.stream.flush()
    }

    pub fn recv_text(&mut self) -> std::io::Result<String> {
        loop {
            let (opcode, payload) = self.read_frame()?;
            match opcode {
                0x1 => {

                    return String::from_utf8(payload).map_err(|e| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                    });
                }
                0x8 => {

                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "WebSocket closed by server",
                    ));
                }
                0x9 => {

                    self.send_pong(&payload)?;
                }
                0xA => {

                }
                _ => {

                }
            }
        }
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    fn send_pong(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let mask = generate_mask();
        let mut frame = Vec::new();
        frame.push(0x8A);
        let len = payload.len();
        frame.push(0x80 | len as u8);
        frame.extend_from_slice(&mask);
        for (i, &b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }
        self.stream.write_all(&frame)?;
        self.stream.flush()
    }

    fn read_frame(&mut self) -> std::io::Result<(u8, Vec<u8>)> {
        let mut first = [0u8; 1];
        self.read_exact_buffered(&mut first)?;
        let idle_timeout = self.stream.read_timeout()?;
        self.stream.set_read_timeout(Some(WSS_FRAME_READ_TIMEOUT))?;
        let frame = self.read_frame_rest(first[0]);
        self.stream.set_read_timeout(idle_timeout)?;
        frame.map_err(|e| match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "WebSocket frame stalled",
            ),
            _ => e,
        })
    }

    fn read_frame_rest(&mut self, first: u8) -> std::io::Result<(u8, Vec<u8>)> {
        let mut second = [0u8; 1];
        self.read_exact_buffered(&mut second)?;
        let opcode = first & 0x0F;
        let masked = (second[0] & 0x80) != 0;
        let mut len = (second[0] & 0x7F) as u64;
        if len == 126 {
            let mut ext = [0u8; 2];
            self.read_exact_buffered(&mut ext)?;
            len = u16::from_be_bytes(ext) as u64;
        } else if len == 127 {
            let mut ext = [0u8; 8];
            self.read_exact_buffered(&mut ext)?;
            len = u64::from_be_bytes(ext);
        }
        if len > 16 * 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "WebSocket frame too large",
            ));
        }
        let mask_key = if masked {
            let mut mk = [0u8; 4];
            self.read_exact_buffered(&mut mk)?;
            Some(mk)
        } else {
            None
        };
        let mut payload = vec![0u8; len as usize];
        self.read_exact_buffered(&mut payload)?;
        if let Some(mk) = mask_key {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mk[i % 4];
            }
        }
        Ok((opcode, payload))
    }
}

fn generate_ws_key() -> String {
    let bytes = crate::config::generate_random_bytes(16);
    base64_encode(&bytes)
}

fn generate_mask() -> [u8; 4] {
    let bytes = crate::config::generate_random_bytes(4);
    if bytes.len() == 4 {
        return [bytes[0], bytes[1], bytes[2], bytes[3]];
    }
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let state = (seed as u64).wrapping_mul(6364136223846793005);
    [
        (state >> 24) as u8,
        (state >> 16) as u8,
        (state >> 8) as u8,
        state as u8,
    ]
}

pub fn base64_encode_bytes(data: &[u8]) -> String {
    base64_encode(data)
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn serve_once(reply: Vec<u8>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = sock.read(&mut request);
            sock.write_all(&reply).unwrap();
            std::thread::sleep(Duration::from_millis(500));
        });
        port
    }

    #[test]
    fn ws_client_keeps_a_frame_that_arrives_with_the_upgrade_reply() {
        let mut reply = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n".to_vec();
        reply.extend_from_slice(&[0x81, 0x05]);
        reply.extend_from_slice(b"hello");
        let port = serve_once(reply);
        let mut ws = WsClient::connect("127.0.0.1", port, "/").unwrap();
        assert_eq!(ws.recv_text().unwrap(), "hello");
    }

    #[test]
    fn ws_client_refuses_a_reply_that_is_not_an_upgrade() {
        let port = serve_once(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_vec());
        assert!(WsClient::connect("127.0.0.1", port, "/").is_err());
    }
}
