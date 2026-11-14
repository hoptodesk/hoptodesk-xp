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
use std::time::{Duration, Instant};

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

extern "system" {
    fn GetOEMCP() -> u32;
    fn IsDBCSLeadByteEx(code_page: u32, test_char: u8) -> i32;
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        src: *const u8,
        src_len: i32,
        dst: *mut u16,
        dst_len: i32,
    ) -> i32;
    fn WideCharToMultiByte(
        code_page: u32,
        flags: u32,
        src: *const u16,
        src_len: i32,
        dst: *mut u8,
        dst_len: i32,
        default_char: *const u8,
        used_default: *mut i32,
    ) -> i32;
}

const CP_UTF8: u32 = 65001;

fn shell_code_page() -> u32 {
    unsafe { GetOEMCP() }
}

fn complete_prefix_len(code_page: u32, bytes: &[u8]) -> usize {
    let mut i = 0;
    while i < bytes.len() {
        if unsafe { IsDBCSLeadByteEx(code_page, bytes[i]) } != 0 {
            if i + 1 >= bytes.len() {
                return i;
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    bytes.len()
}

fn shell_bytes_to_utf8(code_page: u32, pending: &mut Vec<u8>, bytes: &[u8]) -> Vec<u8> {
    if code_page == CP_UTF8 {
        return bytes.to_vec();
    }
    pending.extend_from_slice(bytes);
    let end = complete_prefix_len(code_page, pending);
    let chunk: Vec<u8> = pending.drain(..end).collect();
    if chunk.is_empty() {
        return Vec::new();
    }
    unsafe {
        let len = MultiByteToWideChar(code_page, 0, chunk.as_ptr(), chunk.len() as i32, std::ptr::null_mut(), 0);
        if len <= 0 {
            return chunk;
        }
        let mut wide = vec![0u16; len as usize];
        MultiByteToWideChar(code_page, 0, chunk.as_ptr(), chunk.len() as i32, wide.as_mut_ptr(), len);
        String::from_utf16_lossy(&wide).into_bytes()
    }
}

fn utf8_to_shell_bytes(code_page: u32, text: &str) -> Vec<u8> {
    if code_page == CP_UTF8 || text.is_ascii() {
        return text.as_bytes().to_vec();
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let len = WideCharToMultiByte(
            code_page,
            0,
            wide.as_ptr(),
            wide.len() as i32,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null_mut(),
        );
        if len <= 0 {
            return text.as_bytes().to_vec();
        }
        let mut out = vec![0u8; len as usize];
        WideCharToMultiByte(
            code_page,
            0,
            wide.as_ptr(),
            wide.len() as i32,
            out.as_mut_ptr(),
            len,
            std::ptr::null(),
            std::ptr::null_mut(),
        );
        out
    }
}

fn spawn_output_reader<R: Read + Send + 'static>(
    mut source: R,
    exiting: Arc<AtomicBool>,
    output_tx: SyncSender<Vec<u8>>,
    code_page: u32,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = vec![0u8; READ_BUF_SIZE];
        let mut pending = Vec::new();
        loop {
            if exiting.load(Ordering::SeqCst) {
                break;
            }
            match source.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let text = shell_bytes_to_utf8(code_page, &mut pending, &buf[..n]);
                    if text.is_empty() {
                        continue;
                    }
                    match output_tx.try_send(text) {
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
}

#[derive(PartialEq, Eq)]
enum EscState {
    Normal,
    Esc,
    Csi,
}

struct TerminalSession {
    pid: u32,
    child: Arc<Mutex<Option<Child>>>,
    input_tx: Option<SyncSender<Vec<u8>>>,
    output_rx: Receiver<Vec<u8>>,
    output_tx_echo: Option<SyncSender<Vec<u8>>>,
    line_buffer: Vec<char>,
    cursor: usize,
    csi: String,
    esc_state: EscState,
    exiting: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    closed_sent: bool,
    code_page: u32,
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

    let is_cmd = shell.to_lowercase().ends_with("cmd.exe");
    let mut command = Command::new(&shell);
    if is_cmd {
        command.arg("/q");
    }
    let mut child = command
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

    let code_page = shell_code_page();
    let reader = spawn_output_reader(stdout, exiting.clone(), output_tx.clone(), code_page);
    let stderr_reader = spawn_output_reader(stderr, exiting.clone(), output_tx.clone(), code_page);

    Ok(TerminalSession {
        pid,
        child: child_arc,
        input_tx: Some(input_tx),
        output_rx,
        output_tx_echo: Some(output_tx),
        line_buffer: Vec::new(),
        cursor: 0,
        csi: String::new(),
        esc_state: EscState::Normal,
        exiting,
        reader: Some(reader),
        writer: Some(writer),
        stderr_reader: Some(stderr_reader),
        closed_sent: false,
        code_page,
    })
}

fn send_ctrl_c(pid: u32) {
    extern "system" {
        fn FreeConsole() -> i32;
        fn AttachConsole(dwProcessId: u32) -> i32;
        fn GenerateConsoleCtrlEvent(dwCtrlEvent: u32, dwProcessGroupId: u32) -> i32;
        fn SetConsoleCtrlHandler(
            HandlerRoutine: Option<unsafe extern "system" fn(u32) -> i32>,
            Add: i32,
        ) -> i32;
    }
    const CTRL_C_EVENT: u32 = 0;
    unsafe {
        FreeConsole();
        if AttachConsole(pid) != 0 {
            HOST_CTRL_HANDLER_ONCE.call_once(|| {
                SetConsoleCtrlHandler(Some(host_console_ctrl_handler), 1);
            });
            HOST_CTRL_EVENT_HANDLED.store(false, Ordering::SeqCst);
            if GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) != 0 {
                let deadline = Instant::now() + HOST_CTRL_HANDLER_WAIT;
                while !HOST_CTRL_EVENT_HANDLED.load(Ordering::SeqCst) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(2));
                }
            }
            FreeConsole();
        }
    }
}

const HOST_CTRL_HANDLER_WAIT: Duration = Duration::from_secs(2);
static HOST_CTRL_HANDLER_ONCE: std::sync::Once = std::sync::Once::new();
static HOST_CTRL_EVENT_HANDLED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn host_console_ctrl_handler(ctrl_type: u32) -> i32 {
    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;
    if ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_BREAK_EVENT {
        HOST_CTRL_EVENT_HANDLED.store(true, Ordering::SeqCst);
        1
    } else {
        0
    }
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

fn line_move_left(s: &mut TerminalSession, echo: &mut String) {
    if s.cursor > 0 {
        s.cursor -= 1;
        echo.push('\u{8}');
    }
}

fn line_move_right(s: &mut TerminalSession, echo: &mut String) {
    if s.cursor < s.line_buffer.len() {
        echo.push(s.line_buffer[s.cursor]);
        s.cursor += 1;
    }
}

fn line_move_home(s: &mut TerminalSession, echo: &mut String) {
    while s.cursor > 0 {
        s.cursor -= 1;
        echo.push('\u{8}');
    }
}

fn line_move_end(s: &mut TerminalSession, echo: &mut String) {
    while s.cursor < s.line_buffer.len() {
        echo.push(s.line_buffer[s.cursor]);
        s.cursor += 1;
    }
}

fn line_delete_at(s: &mut TerminalSession, echo: &mut String) {
    if s.cursor < s.line_buffer.len() {
        s.line_buffer.remove(s.cursor);
        let tail: String = s.line_buffer[s.cursor..].iter().collect();
        let tail_len = s.line_buffer.len() - s.cursor;
        echo.push_str(&tail);
        echo.push(' ');
        for _ in 0..(tail_len + 1) {
            echo.push('\u{8}');
        }
    }
}

fn line_backspace(s: &mut TerminalSession, echo: &mut String) {
    if s.cursor > 0 {
        s.cursor -= 1;
        echo.push('\u{8}');
        line_delete_at(s, echo);
    }
}

fn line_insert(s: &mut TerminalSession, ch: char, echo: &mut String) {
    s.line_buffer.insert(s.cursor, ch);
    let tail: String = s.line_buffer[s.cursor..].iter().collect();
    let tail_len = s.line_buffer.len() - s.cursor;
    echo.push_str(&tail);
    s.cursor += 1;
    for _ in 0..(tail_len - 1) {
        echo.push('\u{8}');
    }
}

fn line_clear(s: &mut TerminalSession, echo: &mut String) {
    line_move_end(s, echo);
    let n = s.line_buffer.len();
    s.line_buffer.clear();
    s.cursor = 0;
    for _ in 0..n {
        echo.push_str("\u{8} \u{8}");
    }
}

fn apply_csi(s: &mut TerminalSession, final_byte: char, echo: &mut String) {
    match final_byte {
        'D' => line_move_left(s, echo),
        'C' => line_move_right(s, echo),
        'H' => line_move_home(s, echo),
        'F' => line_move_end(s, echo),
        '~' => match s.csi.as_str() {
            "1" | "7" => line_move_home(s, echo),
            "4" | "8" => line_move_end(s, echo),
            "3" => line_delete_at(s, echo),
            _ => {}
        },
        _ => {}
    }
    s.csi.clear();
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
            let input_str = String::from_utf8_lossy(&raw).to_string();
            crate::config::write_log(&format!(
                "[terminal] Data: id={} {} byte(s)",
                id, raw.len()
            ));
            if let Some(session) = sessions.get_mut(&id) {
                let mut echo = String::new();
                let mut to_send: Vec<Vec<u8>> = Vec::new();
                let mut ctrl_c = false;
                let pid = session.pid;
                let code_page = session.code_page;
                for ch in input_str.chars() {
                    if session.esc_state == EscState::Esc {
                        if ch == '[' {
                            session.csi.clear();
                            session.esc_state = EscState::Csi;
                        } else {
                            session.esc_state = EscState::Normal;
                        }
                        continue;
                    }
                    if session.esc_state == EscState::Csi {
                        let code = ch as u32;
                        if (0x40..=0x7E).contains(&code) {
                            apply_csi(session, ch, &mut echo);
                            session.esc_state = EscState::Normal;
                        } else {
                            session.csi.push(ch);
                        }
                        continue;
                    }
                    if ch == '\x1b' {
                        session.esc_state = EscState::Esc;
                        continue;
                    }
                    if ch == '\r' || ch == '\n' {
                        let line: String = session.line_buffer.iter().collect();
                        session.line_buffer.clear();
                        session.cursor = 0;
                        echo.push_str("\r\n");
                        let mut payload = utf8_to_shell_bytes(code_page, &line);
                        payload.extend_from_slice(b"\r\n");
                        to_send.push(payload);
                    } else if ch == '\x7f' || ch == '\x08' {
                        line_backspace(session, &mut echo);
                    } else if ch == '\x15' {
                        line_clear(session, &mut echo);
                    } else if ch == '\x03' {
                        session.line_buffer.clear();
                        session.cursor = 0;
                        ctrl_c = true;
                    } else if ch == '\t' {
                    } else if ch >= ' ' {
                        line_insert(session, ch, &mut echo);
                    }
                }
                if !echo.is_empty() {
                    if let Some(tx) = session.output_tx_echo.as_ref() {
                        let _ = tx.try_send(echo.into_bytes());
                    }
                }
                if let Some(tx) = session.input_tx.as_ref() {
                    for payload in to_send {
                        if let Err(e) = tx.try_send(payload) {
                            crate::config::write_log(&format!(
                                "[terminal] Data: id={} send error: {:?}", id, e
                            ));
                            break;
                        }
                    }
                }
                if ctrl_c {
                    send_ctrl_c(pid);
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
    let mut last_keepalive = std::time::Instant::now();
    let mut last_cm_check = std::time::Instant::now();

    loop {
        if stop.load(Ordering::Relaxed) {
            crate::config::write_log("[terminal] Stop flag set, exiting loop");
            break;
        }

        if last_cm_check.elapsed() >= Duration::from_millis(500) {
            last_cm_check = std::time::Instant::now();
            if crate::server::cm_disconnect_requested() {
                crate::config::write_log("[terminal] CM user disconnected session");
                crate::server::send_session_close(stream);
                break;
            }
        }

        if last_keepalive.elapsed() >= Duration::from_secs(3) {
            last_keepalive = std::time::Instant::now();
            let mut td = message_proto::TestDelay::new();
            td.from_client = false;
            let mut msg = message_proto::Message::new();
            msg.set_test_delay(td);
            stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
        }

        match stream.recv_msg() {
            Ok(data) => {
                if let Ok(msg) = message_proto::Message::parse_from_bytes(&data) {
                    match msg.union {
                        Some(message_proto::message::Union::TerminalAction(ta)) => {
                            handle_action(stream, &mut sessions, ta)?;
                        }
                        Some(message_proto::message::Union::TestDelay(_td)) => {
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
