use crate::network::FramedStream;
use crate::protocol::message_proto;
use protobuf::Message as ProtoMsg;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const CHANNEL_BUFFER_SIZE: usize = 256;
const READ_BUF_SIZE: usize = 4096;
const STREAM_READ_TIMEOUT_MS: u64 = 50;

fn get_default_shell() -> String {
    if let Ok(comspec) = std::env::var("COMSPEC") {
        if !comspec.is_empty() && std::path::Path::new(&comspec).exists() {
            return comspec;
        }
    }
    let candidates = [
        r"C:\Windows\System32\cmd.exe",
        r"C:\WINNT\System32\cmd.exe",
        r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        r"C:\WINNT\System32\WindowsPowerShell\v1.0\powershell.exe",
    ];
    for p in &candidates {
        if std::path::Path::new(p).exists() {
            return (*p).to_string();
        }
    }
    "cmd.exe".to_string()
}

fn io_err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}

struct TerminalSession {
    pid: u32,
    child: Arc<Mutex<Option<Child>>>,
    input_tx: Option<SyncSender<Vec<u8>>>,
    output_rx: Receiver<Vec<u8>>,
    exiting: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    closed_sent: bool,
}

impl TerminalSession {
    fn shutdown(&mut self) {
        self.exiting.store(true, Ordering::SeqCst);
        self.input_tx.take();
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        for handle in [self.reader.take(), self.writer.take(), self.stderr_reader.take()] {
            if let Some(h) = handle {
                let _ = h.join();
            }
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn spawn_terminal(terminal_id: i32) -> io::Result<TerminalSession> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let shell = get_default_shell();
    crate::config::write_log(&format!(
        "[terminal] Spawning shell '{}' for terminal_id={}",
        shell, terminal_id
    ));

    let mut child = Command::new(&shell)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| {
            crate::config::write_log(&format!(
                "[terminal] Failed to spawn shell '{}': {}",
                shell, e
            ));
            e
        })?;

    let pid = child.id();
    let stdin = child.stdin.take().ok_or_else(|| io_err("no stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| io_err("no stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| io_err("no stderr"))?;

    let child_arc = Arc::new(Mutex::new(Some(child)));
    let exiting = Arc::new(AtomicBool::new(false));

    let (input_tx, input_rx) = mpsc::sync_channel::<Vec<u8>>(CHANNEL_BUFFER_SIZE);
    let (output_tx, output_rx) = mpsc::sync_channel::<Vec<u8>>(CHANNEL_BUFFER_SIZE);

    let writer = {
        let mut stdin = stdin;
        let exiting = exiting.clone();
        thread::spawn(move || {
            while !exiting.load(Ordering::SeqCst) {
                match input_rx.recv() {
                    Ok(buf) => {
                        if stdin.write_all(&buf).is_err() {
                            break;
                        }
                        let _ = stdin.flush();
                    }
                    Err(_) => break,
                }
            }
        })
    };

    let reader = {
        let mut stdout = stdout;
        let exiting = exiting.clone();
        let output_tx = output_tx.clone();
        thread::spawn(move || {
            let mut buf = vec![0u8; READ_BUF_SIZE];
            loop {
                if exiting.load(Ordering::SeqCst) {
                    break;
                }
                match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        match output_tx.try_send(buf[..n].to_vec()) {
                            Ok(_) => {}
                            Err(TrySendError::Full(v)) => {
                                let _ = output_tx.send(v);
                            }
                            Err(TrySendError::Disconnected(_)) => break,
                        }
                    }
                    Err(_) => break,
                }
            }
        })
    };

    let stderr_reader = {
        let mut stderr = stderr;
        let exiting = exiting.clone();
        thread::spawn(move || {
            let mut buf = vec![0u8; READ_BUF_SIZE];
            loop {
                if exiting.load(Ordering::SeqCst) {
                    break;
                }
                match stderr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = output_tx.try_send(buf[..n].to_vec());
                    }
                    Err(_) => break,
                }
            }
        })
    };

    Ok(TerminalSession {
        pid,
        child: child_arc,
        input_tx: Some(input_tx),
        output_rx,
        exiting,
        reader: Some(reader),
        writer: Some(writer),
        stderr_reader: Some(stderr_reader),
        closed_sent: false,
    })
}

fn send_response(stream: &mut FramedStream, resp: message_proto::TerminalResponse) -> io::Result<()> {
    let mut msg = message_proto::Message::new();
    msg.set_terminal_response(resp);
    let bytes = msg.write_to_bytes().map_err(io_err)?;
    stream.send_msg(&bytes)
}

fn send_opened(stream: &mut FramedStream, terminal_id: i32, success: bool, message: &str, pid: u32) -> io::Result<()> {
    let mut opened = message_proto::TerminalOpened::new();
    opened.terminal_id = terminal_id;
    opened.success = success;
    opened.message = message.to_string();
    opened.pid = pid;
    opened.service_id = String::new();
    let mut resp = message_proto::TerminalResponse::new();
    resp.set_opened(opened);
    send_response(stream, resp)
}

fn send_data(stream: &mut FramedStream, terminal_id: i32, data: Vec<u8>) -> io::Result<()> {
    let mut td = message_proto::TerminalData::new();
    td.terminal_id = terminal_id;
    td.data = data.into();
    td.compressed = false;
    let mut resp = message_proto::TerminalResponse::new();
    resp.set_data(td);
    send_response(stream, resp)
}

fn send_closed(stream: &mut FramedStream, terminal_id: i32, exit_code: i32) -> io::Result<()> {
    let mut tc = message_proto::TerminalClosed::new();
    tc.terminal_id = terminal_id;
    tc.exit_code = exit_code;
    let mut resp = message_proto::TerminalResponse::new();
    resp.set_closed(tc);
    send_response(stream, resp)
}

fn send_error(stream: &mut FramedStream, terminal_id: i32, message: &str) -> io::Result<()> {
    let mut te = message_proto::TerminalError::new();
    te.terminal_id = terminal_id;
    te.message = message.to_string();
    let mut resp = message_proto::TerminalResponse::new();
    resp.set_error(te);
    send_response(stream, resp)
}

fn normalize_crlf(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + 8);
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b == b'\r' {
            out.extend_from_slice(b"\r\n");
            if input.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
        } else if b == b'\n' {
            out.extend_from_slice(b"\r\n");
        } else {
            out.push(b);
        }
        i += 1;
    }
    out
}

fn handle_action(
    stream: &mut FramedStream,
    sessions: &mut HashMap<i32, TerminalSession>,
    action: message_proto::TerminalAction,
) -> io::Result<()> {
    match action.union {
        Some(message_proto::terminal_action::Union::Open(open)) => {
            let id = open.terminal_id;
            crate::config::write_log(&format!(
                "[terminal] Open: id={} rows={} cols={}",
                id, open.rows, open.cols
            ));
            if sessions.contains_key(&id) {
                crate::config::write_log(&format!("[terminal] Open: id={} already exists, replacing", id));
                sessions.remove(&id);
            }
            match spawn_terminal(id) {
                Ok(session) => {
                    let pid = session.pid;
                    sessions.insert(id, session);
                    send_opened(stream, id, true, "Terminal opened", pid)?;
                }
                Err(e) => {
                    crate::config::write_log(&format!("[terminal] Spawn failed: {}", e));
                    send_opened(stream, id, false, &format!("Failed to spawn shell: {}", e), 0)?;
                }
            }
        }
        Some(message_proto::terminal_action::Union::Data(data)) => {
            let id = data.terminal_id;
            let raw = data.data.to_vec();
            let bytes = normalize_crlf(&raw);
            crate::config::write_log(&format!(
                "[terminal] Data: id={} {} byte(s) (raw {}): hex={}",
                id,
                bytes.len(),
                raw.len(),
                bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>()
            ));
            if let Some(session) = sessions.get_mut(&id) {
                if let Some(tx) = session.input_tx.as_ref() {
                    if let Err(e) = tx.try_send(bytes) {
                        crate::config::write_log(&format!("[terminal] Data: id={} send error: {:?}", id, e));
                    }
                }
            } else {
                send_error(stream, id, "Terminal not open")?;
            }
        }
        Some(message_proto::terminal_action::Union::Resize(resize)) => {
            crate::config::write_log(&format!(
                "[terminal] Resize: id={} rows={} cols={} (no-op for pipe shell)",
                resize.terminal_id, resize.rows, resize.cols
            ));
        }
        Some(message_proto::terminal_action::Union::Close(close)) => {
            let id = close.terminal_id;
            crate::config::write_log(&format!("[terminal] Close: id={}", id));
            if let Some(mut session) = sessions.remove(&id) {
                session.shutdown();
                send_closed(stream, id, 0)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn pump_outputs(
    stream: &mut FramedStream,
    sessions: &mut HashMap<i32, TerminalSession>,
) -> io::Result<()> {
    let ids: Vec<i32> = sessions.keys().copied().collect();
    for id in ids {
        let mut combined: Vec<u8> = Vec::new();
        let mut child_dead = false;
        {
            let session = sessions.get_mut(&id).unwrap();
            loop {
                match session.output_rx.try_recv() {
                    Ok(buf) => combined.extend_from_slice(&buf),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        child_dead = true;
                        break;
                    }
                }
            }
            if !child_dead {
                if let Ok(mut guard) = session.child.lock() {
                    if let Some(child) = guard.as_mut() {
                        match child.try_wait() {
                            Ok(Some(_status)) => child_dead = true,
                            Ok(None) => {}
                            Err(_) => child_dead = true,
                        }
                    } else {
                        child_dead = true;
                    }
                }
            }
        }

        if !combined.is_empty() {
            send_data(stream, id, combined)?;
        }

        if child_dead {
            if let Some(mut session) = sessions.remove(&id) {
                if !session.closed_sent {
                    session.closed_sent = true;
                    let _ = send_closed(stream, id, 0);
                }
                session.shutdown();
            }
        }
    }
    Ok(())
}

pub fn run_terminal_loop(stream: &mut FramedStream, stop: &Arc<AtomicBool>) -> io::Result<()> {
    crate::config::write_log("[terminal] Entering terminal loop");
    stream
        .set_read_timeout(Some(Duration::from_millis(STREAM_READ_TIMEOUT_MS)))
        .ok();

    let mut sessions: HashMap<i32, TerminalSession> = HashMap::new();

    loop {
        if stop.load(Ordering::Relaxed) {
            crate::config::write_log("[terminal] Stop flag set, exiting loop");
            break;
        }

        match stream.recv_msg() {
            Ok(data) => {
                if let Ok(msg) = message_proto::Message::parse_from_bytes(&data) {
                    match msg.union {
                        Some(message_proto::message::Union::TerminalAction(ta)) => {
                            handle_action(stream, &mut sessions, ta)?;
                        }
                        Some(message_proto::message::Union::TestDelay(td)) => {
                            let mut resp = message_proto::TestDelay::new();
                            resp.time = td.time;
                            resp.last_delay = td.last_delay;
                            resp.from_client = false;
                            let mut reply = message_proto::Message::new();
                            reply.set_test_delay(resp);
                            stream.send_msg(&reply.write_to_bytes().map_err(io_err)?)?;
                        }
                        Some(message_proto::message::Union::Misc(misc)) => {
                            if let Some(message_proto::misc::Union::CloseReason(reason)) = misc.union {
                                crate::config::write_log(&format!("[terminal] Peer closed: {}", reason));
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => {
            }
            Err(e) => {
                crate::config::write_log(&format!("[terminal] Peer disconnected: {}", e));
                break;
            }
        }

        pump_outputs(stream, &mut sessions)?;
    }

    crate::config::write_log(&format!("[terminal] Loop exiting, killing {} session(s)", sessions.len()));
    sessions.clear();
    Ok(())
}
