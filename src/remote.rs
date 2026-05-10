
use crate::{client, config, signal, turn};
use crate::protocol::message_proto;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

fn is_direct_ip(target: &str) -> bool {

    if let Some(colon) = target.rfind(':') {
        let host = &target[..colon];
        let port = &target[colon + 1..];

        if port.parse::<u16>().is_err() {
            return false;
        }

        host.contains('.') || host.contains(':')
    } else {
        false
    }
}

struct JobWriteState {
    dest_path: String,
    files: Vec<String>,
    offsets: Vec<u64>,
    total_size: u64,
    file_num: i32,
    done_count: usize,
}

#[link(name = "user32")]
extern "system" {
    fn SetTimer(
        hwnd: *mut std::ffi::c_void,
        id: usize,
        interval: u32,
        callback: Option<unsafe extern "system" fn(*mut std::ffi::c_void, u32, usize, u32)>,
    ) -> usize;
    fn GetClientRect(hwnd: *mut std::ffi::c_void, rect: *mut [i32; 4]) -> i32;
    fn InvalidateRect(hwnd: *mut std::ffi::c_void, rect: *const std::ffi::c_void, erase: i32) -> i32;
    fn CreateWindowExA(
        ex_style: u32, class: *const u8, title: *const u8, style: u32,
        x: i32, y: i32, w: i32, h: i32,
        parent: *mut std::ffi::c_void, menu: *mut std::ffi::c_void,
        instance: *mut std::ffi::c_void, param: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn MoveWindow(hwnd: *mut std::ffi::c_void, x: i32, y: i32, w: i32, h: i32, repaint: i32) -> i32;
    fn ShowWindow(hwnd: *mut std::ffi::c_void, cmd: i32) -> i32;
    fn DestroyWindow(hwnd: *mut std::ffi::c_void) -> i32;
    fn GetWindowLongA(hwnd: *mut std::ffi::c_void, index: i32) -> i32;
    fn SetWindowLongA(hwnd: *mut std::ffi::c_void, index: i32, value: i32) -> i32;
    fn PostMessageA(hwnd: *mut std::ffi::c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn SetWindowTextA(hwnd: *mut std::ffi::c_void, text: *const u8) -> i32;
    fn FillRect(hdc: *mut std::ffi::c_void, rect: *const [i32; 4], brush: *mut std::ffi::c_void) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn GetDC(hwnd: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn ReleaseDC(hwnd: *mut std::ffi::c_void, hdc: *mut std::ffi::c_void) -> i32;
    fn StretchDIBits(
        hdc: *mut std::ffi::c_void,
        x_dest: i32, y_dest: i32, w_dest: i32, h_dest: i32,
        x_src: i32, y_src: i32, w_src: i32, h_src: i32,
        bits: *const u8,
        bmi: *const u8,
        usage: u32,
        rop: u32,
    ) -> i32;
    fn CreateSolidBrush(color: u32) -> *mut std::ffi::c_void;
    fn DeleteObject(obj: *mut std::ffi::c_void) -> i32;
    fn SetStretchBltMode(hdc: *mut std::ffi::c_void, mode: i32) -> i32;
}

static mut REMOTE_CLIENT_STATE: Option<Arc<Mutex<client::ClientState>>> = None;
static mut REMOTE_CLIENT_STOP: Option<Arc<AtomicBool>> = None;
static mut REMOTE_HWND: sciter::types::HWINDOW = std::ptr::null_mut();
static mut VIDEO_HWND: *mut std::ffi::c_void = std::ptr::null_mut();
static mut LAST_FRAME_SEQ: u64 = 0;
static mut IS_FILE_TRANSFER_MODE: bool = false;
static mut FT_JOBS: Option<HashMap<i32, JobWriteState>> = None;

static mut CONNECT_TARGET_ID: Option<String> = None;
static mut CONNECT_MY_ID: Option<String> = None;
static mut VIDEO_OFFSET_X: i32 = 0;
static mut VIDEO_OFFSET_Y: i32 = 0;
static mut VIDEO_SCALE_W: i32 = 0;
static mut VIDEO_SCALE_H: i32 = 0;
static mut PASSWORD_DIALOG_SHOWN: bool = false;
static mut RESET_TICK_COUNT: bool = false;
static mut FT_NEEDS_REMOTE_DIR_REFRESH: bool = false;
static mut INITIAL_PASSWORD_EMPTY: bool = false;

const WM_CLOSE: u32 = 0x0010;

unsafe fn close_remote_window() {
    if let Some(stop) = REMOTE_CLIENT_STOP.as_ref() {
        stop.store(true, Ordering::Relaxed);
    }
    PostMessageA(REMOTE_HWND as *mut std::ffi::c_void, WM_CLOSE, 0, 0);
}

pub fn run_connect_process_ex(target_id: &str, peer_password: &str, is_file_transfer: bool, switch_uuid: Option<&str>) {
    if let Some(uuid) = switch_uuid {
        unsafe { SWITCH_UUID = Some(uuid.to_string()); }
    }
    run_connect_process(target_id, peer_password, is_file_transfer);
}

pub static mut SWITCH_UUID: Option<String> = None;

pub fn run_connect_process(target_id: &str, peer_password: &str, is_file_transfer: bool) {
    let cfg = config::Config::load();
    let my_id = cfg.id.clone();
    let password = peer_password.to_string();

    unsafe {
        INITIAL_PASSWORD_EMPTY = password.is_empty();
    }

    let client_state = Arc::new(Mutex::new(client::ClientState::default()));
    let client_stop = Arc::new(AtomicBool::new(false));

    if is_file_transfer {
        if let Ok(mut s) = client_state.lock() {
            s.file_transfer_mode = true;
        }
    }

    let target = target_id.to_string();
    let cs = client_state.clone();
    let stop = client_stop.clone();
    let my_id2 = my_id.clone();
    let pw2 = password.clone();
    let ft = is_file_transfer;

    thread::spawn(move || {
        if let Ok(mut s) = cs.lock() {
            s.status = "connecting".into();
        }

        if is_direct_ip(&target) {
            crate::config::write_log(&format!("[connect] Direct IP connection to {}", crate::config::mask_ip(&target)));
            if ft {
                client::connect_to_peer_ft(&target, &my_id2, &target, &pw2, cs.clone(), stop.clone());
            } else {
                client::connect_to_peer(&target, &my_id2, &target, &pw2, cs.clone(), stop.clone());
            }
            return;
        }

        let signal_state = Arc::new(Mutex::new(signal::SignalState::default()));

        match signal::send_connect_request(&my_id2, &target, &signal_state) {
            Ok(peer) => {
                crate::config::write_log(&format!("[connect] Got peer: addr={} public={}", crate::config::mask_ip(&peer.addr), crate::config::mask_ip(&peer.public_addr)));

                let mut unique_addrs = Vec::new();
                for addr in [&peer.addr, &peer.public_addr] {
                    if !addr.is_empty() && !unique_addrs.contains(&addr.to_string()) {
                        unique_addrs.push(addr.to_string());
                    }
                }

                let mut connected = false;
                for addr in &unique_addrs {
                    crate::config::write_log(&format!("[connect] Trying direct TCP to {}...", crate::config::mask_ip(&addr)));
                    if ft {
                        client::connect_to_peer_ft(addr, &my_id2, &target, &pw2, cs.clone(), stop.clone());
                    } else {
                        client::connect_to_peer(addr, &my_id2, &target, &pw2, cs.clone(), stop.clone());
                    }
                    if let Ok(s) = cs.lock() {
                        if s.status == "connected" || s.status == "closed" || s.status == "error" {
                            connected = true;
                            break;
                        }
                    }
                }

                if !connected {
                    crate::config::write_log(&format!("[connect] Direct failed, trying TURN relay..."));
                    if let Ok(mut s) = cs.lock() {
                        s.status = "connecting".into();
                        s.error.clear();
                    }

                    let relay_addr = if !peer.public_addr.is_empty() {
                        &peer.public_addr
                    } else {
                        &peer.addr
                    };

                    let (ws_host, ws_port) = {
                        let ss = signal_state.lock().unwrap();
                        (ss.ws_host.clone(), ss.ws_port)
                    };

                    match turn::connect_via_turn(relay_addr, &target, &my_id2, &ws_host, ws_port) {
                        Ok(stream) => {
                            crate::config::write_log(&format!("[connect] Connected via TURN relay!"));
                            if ft {
                                client::run_client_on_stream_ft(
                                    stream, &my_id2, &target, &pw2, cs.clone(), stop.clone(),
                                );
                            } else {
                                client::run_client_on_stream(
                                    stream, &my_id2, &target, &pw2, cs.clone(), stop.clone(),
                                );
                            }
                        }
                        Err(e) => {
                            crate::config::write_log(&format!("[connect] TURN relay failed: {}", e));
                            if let Ok(mut s) = cs.lock() {
                                s.status = "error".into();
                                s.error = "Could not connect (direct + relay failed)".into();
                            }
                        }
                    }
                }
            }
            Err(e) => {
                crate::config::write_log(&format!("[connect] ConnectRequest failed: {}", e));
                if let Ok(mut s) = cs.lock() {
                    s.status = "error".into();
                    s.error = e;
                }
            }
        }
    });

    sciter::set_options(sciter::RuntimeOptions::GfxLayer(sciter::GFX_LAYER::CPU)).ok();

    let mut frame = sciter::Window::new();

    let html_template = include_str!("ui/remote.html");
    let home_dir = crate::file_transfer::get_home_dir();
    let html = html_template
        .replace("__HOME_DIR__", &home_dir.replace('\\', "\\\\"))
        .replace("__IS_FILE_TRANSFER__", if is_file_transfer { "true" } else { "false" });
    frame.load_html(html.as_bytes(), Some("this://app/remote.html"));
    frame.set_title("HopToDesk");

    let hwnd = frame.get_hwnd();
    crate::set_window_icon(hwnd);
    unsafe {
        REMOTE_CLIENT_STATE = Some(client_state);
        REMOTE_CLIENT_STOP = Some(client_stop);
        REMOTE_HWND = hwnd;
        LAST_FRAME_SEQ = 0;
        IS_FILE_TRANSFER_MODE = is_file_transfer;
        FT_JOBS = Some(HashMap::new());
        CONNECT_TARGET_ID = Some(target_id.to_string());
        CONNECT_MY_ID = Some(my_id.clone());
        PASSWORD_DIALOG_SHOWN = false;
        VIDEO_HWND = std::ptr::null_mut();
        INITIAL_PASSWORD_EMPTY = password.is_empty();

        SetTimer(hwnd as *mut std::ffi::c_void, 1, 33, Some(remote_timer_callback));
    }

    frame.run_app();

    unsafe {
        if let Some(stop) = REMOTE_CLIENT_STOP.as_ref() {
            stop.store(true, Ordering::Relaxed);
        }
    }

    std::process::exit(0);
}

unsafe extern "system" fn remote_timer_callback(
    _hwnd: *mut std::ffi::c_void,
    _msg: u32,
    _id: usize,
    _time: u32,
) {
    static mut TICK_COUNT: u32 = 0;
    if RESET_TICK_COUNT {
        TICK_COUNT = 0;
        RESET_TICK_COUNT = false;
    }
    TICK_COUNT += 1;

    let client_state = match REMOTE_CLIENT_STATE.as_ref() {
        Some(s) => s,
        None => { crate::config::write_log(&format!("[remote/timer] no client_state")); return; },
    };
    let hwnd = REMOTE_HWND;
    if hwnd.is_null() {
        crate::config::write_log(&format!("[remote/timer] null hwnd"));
        return;
    }

    let root = match sciter::Element::from_window(hwnd) {
        Ok(r) => r,
        Err(e) => {
            if TICK_COUNT % 30 == 1 {
                crate::config::write_log(&format!("[remote/timer] from_window failed: {:?}", e));
            }
            return;
        },
    };

    if IS_FILE_TRANSFER_MODE {
        process_file_responses(client_state, &root);
    }

    if IS_FILE_TRANSFER_MODE {
        let (status, error) = {
            let cs = match client_state.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            (cs.status.clone(), cs.error.clone())
        };

        if status == "error" && !PASSWORD_DIALOG_SHOWN {
            let err_lower = error.to_lowercase();
            let is_password_error = err_lower.contains("wrong password")
                || err_lower.contains("password empty")
                || err_lower.contains("wrong_password")
                || err_lower.contains("password_empty")
                || err_lower.contains("connection rejected");
            if is_password_error {
                PASSWORD_DIALOG_SHOWN = true;
                let title = if err_lower.contains("wrong password") { "Wrong Password" } else { "Password Required" };
                let msg = if err_lower.contains("wrong password") { "Do you want to enter again?" } else { "Please enter the password" };
                let _ = root.call_method("showPasswordDialog", &[
                    sciter::Value::from(title),
                    sciter::Value::from(msg),
                ]);
            }
        }

        if let Ok(Some(mut el)) = root.find_first("#retry-password") {
            let txt = el.get_text();
            if !txt.is_empty() {
                let _ = el.set_text("");
                retry_connection_with_password(&txt);

                return;
            }
        }

        if status == "error" || status == "closed" {
            if !PASSWORD_DIALOG_SHOWN {
                if let Ok(Some(mut overlay)) = root.find_first("#status-overlay") {
                    let text = if status == "error" { &error } else { "Connection closed" };
                    let _ = overlay.set_text(text);
                    let _ = overlay.set_style_attribute("display", "block");
                }
            }

            if (status == "error" && !PASSWORD_DIALOG_SHOWN && TICK_COUNT > 90)
                || (status == "closed" && TICK_COUNT > 30)
            {
                crate::config::write_log(&format!("[remote/timer] FT connection ended ({}), closing window", status));
                close_remote_window();
                return;
            }
        }

        if status == "connecting" && TICK_COUNT > 1800 {
            crate::config::write_log("[remote/timer] FT connection timeout (60s), closing window");
            close_remote_window();
            return;
        }

        if FT_NEEDS_REMOTE_DIR_REFRESH && status == "connected" {
            FT_NEEDS_REMOTE_DIR_REFRESH = false;
            if let Ok(Some(mut el)) = root.find_first("#ft-read-remote-dir") {
                let _ = el.set_text("|0");
                crate::config::write_log("[remote/timer] Re-requesting remote dir after retry");
            }
        }

        poll_ft_flags(client_state, &root);

        if let Ok(Some(mut el)) = root.find_first("#disconnect-flag") {
            let txt = el.get_text();
            if !txt.is_empty() {
                let _ = el.set_text("");
                close_remote_window();
                return;
            }
        }

        return;
    }

    if let Ok(Some(mut el)) = root.find_first("#mouse-event") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            handle_mouse_input(&txt, client_state);
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#key-event") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            handle_key_input(&txt, client_state);
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#disconnect-flag") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            close_remote_window();
            return;
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#transfer-flag") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            if let Some(ref target) = CONNECT_TARGET_ID {
                let exe = std::env::current_exe().unwrap_or_default();
                crate::config::write_log(&format!("[remote] Opening FT window for {}", target));
                let _ = std::process::Command::new(&exe)
                    .args(["--connect", target, "--file-transfer"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#screenshot-flag") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");

            if let Ok(cs) = client_state.lock() {
                let w = cs.frame_width;
                let h = cs.frame_height;
                if w > 0 && h > 0 && !cs.frame_data.is_empty() {
                    let desktop = std::env::var("USERPROFILE")
                        .unwrap_or_else(|_| "C:\\".to_string());
                    let desktop = format!("{}\\Desktop", desktop);
                    let timestamp = {
                        use std::time::{SystemTime, UNIX_EPOCH};
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
                    };
                    let path = format!("{}\\screenshot_{}.bmp", desktop, timestamp);
                    match save_frame_as_bmp(&path, w, h, &cs.frame_data) {
                        Ok(()) => crate::config::write_log(&format!("[remote] Screenshot saved: {}", path)),
                        Err(e) => crate::config::write_log(&format!("[remote] Screenshot error: {}", e)),
                    }
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#switch-sides-flag") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            crate::config::write_log("[remote] Switch Sides requested");

            let uuid_bytes = crate::config::generate_random_bytes(16);

            let uuid_str = format!(
                "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                uuid_bytes[0], uuid_bytes[1], uuid_bytes[2], uuid_bytes[3],
                uuid_bytes[4], uuid_bytes[5], uuid_bytes[6], uuid_bytes[7],
                uuid_bytes[8], uuid_bytes[9], uuid_bytes[10], uuid_bytes[11],
                uuid_bytes[12], uuid_bytes[13], uuid_bytes[14], uuid_bytes[15]
            );

            client::send_switch_sides_request_bytes(client_state, &uuid_bytes);

            if let Ok(mut cs) = client_state.lock() {
                cs.status = "closed".to_string();
            }

            if let Some(ref target) = CONNECT_TARGET_ID {
                let exe = std::env::current_exe().unwrap_or_default();
                crate::config::write_log(&format!("[remote] Spawning switch sides connection to {} with UUID {}", target, uuid_str));
                let _ = std::process::Command::new(&exe)
                    .args(["--connect", target, "--switch_uuid", &uuid_str])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }

            close_remote_window();

            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(500));
                std::process::exit(0);
            });
            return;
        }
    }

    let (status, error, peer_name, frame_seq, frame_w, frame_h) = {
        let cs = match client_state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        (
            cs.status.clone(),
            cs.error.clone(),
            cs.peer_name.clone(),
            cs.frame_seq,
            cs.frame_width,
            cs.frame_height,
        )
    };

    if TICK_COUNT % 30 == 1 && status != "connected" {
        crate::config::write_log(&format!("[remote/timer] status={} seq={} frame={}x{}", status, frame_seq, frame_w, frame_h));
    }

    if status == "login" && INITIAL_PASSWORD_EMPTY && !PASSWORD_DIALOG_SHOWN && TICK_COUNT > 5 {
        PASSWORD_DIALOG_SHOWN = true;
        let _ = root.call_method("showPasswordDialog", &[
            sciter::Value::from("Password Required"),
            sciter::Value::from("Enter password or wait for remote to accept"),
        ]);
    }

    if status == "error" && !PASSWORD_DIALOG_SHOWN {
        let err_lower = error.to_lowercase();
        let is_password_error = err_lower.contains("wrong password")
            || err_lower.contains("password empty")
            || err_lower.contains("wrong_password")
            || err_lower.contains("password_empty")
            || err_lower.contains("connection rejected");
        if is_password_error {
            PASSWORD_DIALOG_SHOWN = true;
            let title = if err_lower.contains("wrong password") { "Wrong Password" } else { "Password Required" };
            let msg = if err_lower.contains("wrong password") { "Do you want to enter again?" } else { "Please enter the password" };
            let _ = root.call_method("showPasswordDialog", &[
                sciter::Value::from(title),
                sciter::Value::from(msg),
            ]);
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#retry-password") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            retry_connection_with_password(&txt);
            return;
        }
    }

    if status == "closed" || (status == "error" && TICK_COUNT > 30 && !PASSWORD_DIALOG_SHOWN) {
        crate::config::write_log(&format!("[remote/timer] Connection ended ({}), closing window", status));
        close_remote_window();
        return;
    }

    if status == "connecting" && TICK_COUNT > 1800 {
        crate::config::write_log("[remote/timer] Connection timeout (60s), closing window");
        close_remote_window();
        return;
    }

    if frame_w > 0 && frame_h > 0 {
        if let Ok(Some(mut el)) = root.find_first("#frame-width") {
            let _ = el.set_text(&frame_w.to_string());
        }
        if let Ok(Some(mut el)) = root.find_first("#frame-height") {
            let _ = el.set_text(&frame_h.to_string());
        }
        if let Ok(Some(mut el)) = root.find_first("#video-offset-x") {
            let _ = el.set_text(&VIDEO_OFFSET_X.to_string());
        }
        if let Ok(Some(mut el)) = root.find_first("#video-offset-y") {
            let _ = el.set_text(&VIDEO_OFFSET_Y.to_string());
        }
        if let Ok(Some(mut el)) = root.find_first("#video-scale-w") {
            let _ = el.set_text(&VIDEO_SCALE_W.to_string());
        }
        if let Ok(Some(mut el)) = root.find_first("#video-scale-h") {
            let _ = el.set_text(&VIDEO_SCALE_H.to_string());
        }
    }

    if status == "connected" {

        let _ = root.call_method("showConnected", &[]);

        if let Ok(Some(mut el)) = root.find_first("#peer-name") {
            let current = el.get_text();
            if current.contains("Connecting") || current.contains("Authenticating") {
                let _ = el.set_text(&peer_name);

                let target_id = CONNECT_TARGET_ID.as_deref().unwrap_or("");
                let formatted_id = crate::format_id(target_id);
                let alias = {
                    let pcfg = crate::config::PeerConfig::load(target_id);
                    pcfg.alias.clone()
                };
                let title = if alias.is_empty() {
                    format!("HopToDesk - {}", formatted_id)
                } else {
                    format!("HopToDesk - {} ({})", formatted_id, alias)
                };
                let title_cstr = format!("{}\0", title);
                SetWindowTextA(REMOTE_HWND as *mut std::ffi::c_void, title_cstr.as_ptr() as *const u8);
            }
        }
    }

    {
        let messages: Vec<(String, String)> = {
            if let Ok(state) = client_state.lock() {
                if let Ok(mut q) = state.chat_messages.lock() {
                    q.drain(..).collect()
                } else { Vec::new() }
            } else { Vec::new() }
        };
        for (sender, text) in &messages {
            let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            let sender_escaped = sender.replace('&', "&amp;").replace('<', "&lt;");
            let _ = root.call_method("chatReceived", &[
                sciter::Value::from(sender_escaped.as_str()),
                sciter::Value::from(escaped.as_str()),
            ]);
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#chat-send-flag") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            client::send_chat_message(client_state, &txt);
        }
    }

    if let Ok(Some(mut overlay)) = root.find_first("#status-overlay") {
        match status.as_str() {
            "connecting" => {
                let _ = overlay.set_text("Connecting...");
                let _ = overlay.set_style_attribute("display", "block");
            }
            "login" => {
                let _ = overlay.set_text("Authenticating...");
                let _ = overlay.set_style_attribute("display", "block");
            }
            "connected" => {

                if frame_seq == 0 {
                    let _ = overlay.set_text("Connected, waiting for display...");
                    let _ = overlay.set_style_attribute("display", "block");
                }
            }
            "error" => {
                let _ = overlay.set_text(&error);
                let _ = overlay.set_style_attribute("display", "block");
                let _ = overlay.set_style_attribute("color", "#DC2626");
            }
            "closed" => {
                let _ = overlay.set_text("Connection closed");
                let _ = overlay.set_style_attribute("display", "block");
            }
            _ => {}
        }
    }

    if frame_seq > LAST_FRAME_SEQ && frame_w > 0 && frame_h > 0 {
        LAST_FRAME_SEQ = frame_seq;

        let frame_data = {
            let cs = match client_state.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            let expected = (cs.frame_width * cs.frame_height * 4) as usize;
            if cs.frame_data.len() != expected {
                crate::config::write_log(&format!("[remote/timer] frame size mismatch: got {} expected {}", cs.frame_data.len(), expected));
                return;
            }
            cs.frame_data.clone()
        };

        if VIDEO_HWND.is_null() && !IS_FILE_TRANSFER_MODE {
            let mut rect = [0i32; 4];
            GetClientRect(hwnd as *mut std::ffi::c_void, &mut rect);
            let header_h = 33;
            let gwl_style = -16i32;
            let style = GetWindowLongA(hwnd as *mut std::ffi::c_void, gwl_style);
            SetWindowLongA(hwnd as *mut std::ffi::c_void, gwl_style, style | 0x02000000);
            let child = CreateWindowExA(
                0,
                b"Static\0".as_ptr(), std::ptr::null(),
                0x40000000 | 0x10000000 | 0x04000000 | 0x08000000,
                0, header_h, rect[2], rect[3] - header_h,
                hwnd as *mut std::ffi::c_void,
                std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(),
            );
            VIDEO_HWND = child;
            crate::config::write_log("[remote] Video child window created on first frame");
        }

        if let Ok(Some(mut el)) = root.find_first("#status-overlay") {
            let _ = el.set_style_attribute("display", "none");
        }

        if let Ok(Some(mut el)) = root.find_first("#password-overlay") {
            let _ = el.set_style_attribute("display", "none");
        }

        render_frame_gdi(hwnd, &frame_data, frame_w, frame_h);
    }
}

unsafe fn retry_connection_with_password(password: &str) {
    crate::config::write_log(&format!("[remote] Retrying connection with new password"));

    if let Some(stop) = REMOTE_CLIENT_STOP.as_ref() {
        stop.store(true, Ordering::Relaxed);
    }

    let target_id = match CONNECT_TARGET_ID.as_ref() {
        Some(t) => t.clone(),
        None => { crate::config::write_log(&format!("[remote] No target_id for retry")); return; }
    };
    let my_id = match CONNECT_MY_ID.as_ref() {
        Some(m) => m.clone(),
        None => { crate::config::write_log(&format!("[remote] No my_id for retry")); return; }
    };
    let is_ft = IS_FILE_TRANSFER_MODE;
    let pw = password.to_string();

    let client_state = Arc::new(Mutex::new(client::ClientState::default()));
    let client_stop = Arc::new(AtomicBool::new(false));

    if is_ft {
        if let Ok(mut s) = client_state.lock() {
            s.file_transfer_mode = true;
        }
    }

    REMOTE_CLIENT_STATE = Some(client_state.clone());
    REMOTE_CLIENT_STOP = Some(client_stop.clone());
    VIDEO_HWND = std::ptr::null_mut();
    LAST_FRAME_SEQ = 0;
    INITIAL_PASSWORD_EMPTY = false;
    PASSWORD_DIALOG_SHOWN = false;
    RESET_TICK_COUNT = true;
    if IS_FILE_TRANSFER_MODE {
        FT_NEEDS_REMOTE_DIR_REFRESH = true;
    }
    FT_JOBS = Some(HashMap::new());

    let cs = client_state;
    let stop = client_stop;

    thread::spawn(move || {
        if let Ok(mut s) = cs.lock() {
            s.status = "connecting".into();
        }

        let signal_state = Arc::new(Mutex::new(signal::SignalState::default()));

        match signal::send_connect_request(&my_id, &target_id, &signal_state) {
            Ok(peer) => {
                crate::config::write_log(&format!("[connect/retry] Got peer: addr={} public={}", crate::config::mask_ip(&peer.addr), crate::config::mask_ip(&peer.public_addr)));

                let mut unique_addrs = Vec::new();
                for addr in [&peer.addr, &peer.public_addr] {
                    if !addr.is_empty() && !unique_addrs.contains(&addr.to_string()) {
                        unique_addrs.push(addr.to_string());
                    }
                }

                let mut connected = false;
                for addr in &unique_addrs {
                    crate::config::write_log(&format!("[connect/retry] Trying direct TCP to {}...", crate::config::mask_ip(&addr)));
                    if is_ft {
                        client::connect_to_peer_ft(addr, &my_id, &target_id, &pw, cs.clone(), stop.clone());
                    } else {
                        client::connect_to_peer(addr, &my_id, &target_id, &pw, cs.clone(), stop.clone());
                    }
                    if let Ok(s) = cs.lock() {
                        if s.status == "connected" || s.status == "closed" || s.status == "error" {
                            connected = true;
                            break;
                        }
                    }
                }

                if !connected {
                    crate::config::write_log(&format!("[connect/retry] Direct failed, trying TURN relay..."));
                    if let Ok(mut s) = cs.lock() {
                        s.status = "connecting".into();
                        s.error.clear();
                    }

                    let relay_addr = if !peer.public_addr.is_empty() {
                        &peer.public_addr
                    } else {
                        &peer.addr
                    };

                    let (ws_host, ws_port) = {
                        let ss = signal_state.lock().unwrap();
                        (ss.ws_host.clone(), ss.ws_port)
                    };

                    match turn::connect_via_turn(relay_addr, &target_id, &my_id, &ws_host, ws_port) {
                        Ok(stream) => {
                            crate::config::write_log(&format!("[connect/retry] Connected via TURN relay!"));
                            if is_ft {
                                client::run_client_on_stream_ft(
                                    stream, &my_id, &target_id, &pw, cs.clone(), stop.clone(),
                                );
                            } else {
                                client::run_client_on_stream(
                                    stream, &my_id, &target_id, &pw, cs.clone(), stop.clone(),
                                );
                            }
                        }
                        Err(e) => {
                            crate::config::write_log(&format!("[connect/retry] TURN relay failed: {}", e));
                            if let Ok(mut s) = cs.lock() {
                                s.status = "error".into();
                                s.error = "Could not connect (direct + relay failed)".into();
                            }
                        }
                    }
                }
            }
            Err(e) => {
                crate::config::write_log(&format!("[connect/retry] ConnectRequest failed: {}", e));
                if let Ok(mut s) = cs.lock() {
                    s.status = "error".into();
                    s.error = e;
                }
            }
        }
    });
}

unsafe fn process_file_responses(
    client_state: &Arc<Mutex<client::ClientState>>,
    root: &sciter::Element,
) {
    let responses: Vec<message_proto::FileResponse> = {
        if let Ok(state) = client_state.lock() {
            if let Ok(mut q) = state.file_responses.lock() {
                q.drain(..).collect()
            } else {
                return;
            }
        } else {
            return;
        }
    };

    if !responses.is_empty() {
        crate::config::write_log(&format!("[remote/ft] Processing {} file responses", responses.len()));
    }

    for fr in responses {
        match fr.union {
            Some(message_proto::file_response::Union::Dir(fd)) => {
                crate::config::write_log(&format!("[remote/ft] Dir response: id={} path={} entries={}", fd.id, fd.path, fd.entries.len()));
                for (i, e) in fd.entries.iter().take(5).enumerate() {
                    crate::config::write_log(&format!("[remote/ft]   [{}] name={} type={:?} size={}", i, e.name, e.entry_type, e.size));
                }

                let jobs = FT_JOBS.as_mut().unwrap();
                if let Some(job) = jobs.get_mut(&fd.id) {
                    let base = &job.dest_path;
                    let base_path = std::path::Path::new(base);

                    let mut file_paths = Vec::new();
                    for entry in fd.entries.iter() {
                        let entry_type = entry.entry_type.enum_value_or_default();
                        if entry_type == message_proto::FileType::File {
                            if entry.name.is_empty() {

                                file_paths.push(base.clone());
                            } else {

                                let dest = base_path.join(&entry.name);
                                file_paths.push(dest.to_string_lossy().to_string());
                            }
                        } else if entry.name.ends_with('/') {

                            let dir_dest = base_path.join(&entry.name);
                            let _ = std::fs::create_dir_all(&dir_dest);
                            crate::config::write_log(&format!("[remote/ft] Created empty dir: '{}'", dir_dest.display()));
                            file_paths.push(String::new());
                        } else {

                            file_paths.push(String::new());
                        }
                    }
                    crate::config::write_log(&format!("[remote/ft] Populated {} file paths for job {}", file_paths.len(), fd.id));
                    job.files = file_paths;
                    job.offsets = vec![0u64; fd.entries.len()];
                }

                let val = crate::file_transfer::file_directory_to_value(&fd);
                let result = root.call_method("updateFolderFiles", &[val]);
                crate::config::write_log(&format!("[remote/ft] updateFolderFiles call result: {:?}", result));
            }
            Some(message_proto::file_response::Union::Block(block)) => {

                let jobs = FT_JOBS.as_mut().unwrap();
                if let Some(job) = jobs.get_mut(&block.id) {
                    job.file_num = block.file_num;
                    let fnum = block.file_num as usize;

                    let write_path = if !job.files.is_empty() && fnum < job.files.len() && !job.files[fnum].is_empty() {
                        job.files[fnum].clone()
                    } else {

                        job.dest_path.clone()
                    };

                    while job.offsets.len() <= fnum {
                        job.offsets.push(0);
                    }
                    let offset = job.offsets[fnum];

                    match crate::file_transfer::write_file_block(&write_path, &block.data, offset) {
                        Ok(()) => {
                            job.offsets[fnum] += block.data.len() as u64;

                            let total_finished: u64 = job.offsets.iter().sum();
                            let _ = root.call_method("jobProgress", &[
                                sciter::Value::from(block.id),
                                sciter::Value::from(block.file_num),
                                sciter::Value::from(0i32),
                                sciter::Value::from(total_finished as i32),
                            ]);
                        }
                        Err(e) => {
                            crate::config::write_log(&format!("[remote/ft] write_file_block error: path='{}' err={}", write_path, e));
                        }
                    }
                } else {
                    crate::config::write_log(&format!("[remote/ft] Block for unknown job id={}", block.id));
                }
            }
            Some(message_proto::file_response::Union::Done(done)) => {

                let mut all_done = true;
                if let Some(jobs) = FT_JOBS.as_mut() {
                    if let Some(job) = jobs.get_mut(&done.id) {
                        job.done_count += 1;

                        if job.files.is_empty() || job.done_count >= job.files.len() {
                            all_done = true;
                        } else {
                            all_done = false;
                        }
                    }
                    if all_done {
                        jobs.remove(&done.id);
                    }
                }

                if all_done {
                    let _ = root.call_method("jobDone", &[
                        sciter::Value::from(done.id),
                        sciter::Value::from(done.file_num),
                    ]);
                }
            }
            Some(message_proto::file_response::Union::Error(err)) => {
                crate::config::write_log(&format!("[remote/ft] ERROR from server: id={} file_num={} error='{}'", err.id, err.file_num, err.error));
                let _ = root.call_method("jobError", &[
                    sciter::Value::from(err.id),
                    sciter::Value::from(err.error.as_str()),
                    sciter::Value::from(err.file_num),
                ]);
            }
            Some(message_proto::file_response::Union::Digest(digest)) => {
                crate::config::write_log(&format!("[remote/ft] Digest: id={} file_num={} file_size={} is_upload={}", digest.id, digest.file_num, digest.file_size, digest.is_upload));
                let _ = root.call_method("overrideFileConfirm", &[
                    sciter::Value::from(digest.id),
                    sciter::Value::from(digest.file_num),
                    sciter::Value::from(""),
                    sciter::Value::from(digest.is_upload),
                    sciter::Value::from(digest.is_identical),
                ]);
            }
            _ => {}
        }
    }
}

unsafe fn poll_ft_flags(
    client_state: &Arc<Mutex<client::ClientState>>,
    root: &sciter::Element,
) {

    if let Ok(Some(mut el)) = root.find_first("#ft-read-local-dir") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = txt.splitn(2, '|').collect();
            let path = parts.get(0).unwrap_or(&"");
            let show_hidden = parts.get(1).map(|s| *s == "1").unwrap_or(false);

            let fd = if path.is_empty() {
                crate::file_transfer::get_drives()
            } else {
                crate::file_transfer::read_dir_to_proto(path, show_hidden)
                    .unwrap_or_else(|_| {
                        let mut fd = crate::protocol::message_proto::FileDirectory::new();
                        fd.path = path.to_string();
                        fd
                    })
            };
            let val = crate::file_transfer::file_directory_to_value(&fd);
            let _ = root.call_method("updateLocalFolderFiles", &[val]);
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ft-read-remote-dir") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = txt.splitn(2, '|').collect();
            let path = parts.get(0).unwrap_or(&"").to_string();
            let show_hidden = parts.get(1).map(|s| *s == "1").unwrap_or(false);

            crate::config::write_log(&format!("[remote/ft] ReadRemoteDir: path='{}' show_hidden={}", path, show_hidden));
            let mut rd = crate::protocol::message_proto::ReadDir::new();
            rd.path = path;
            rd.include_hidden = show_hidden;
            let mut fa = crate::protocol::message_proto::FileAction::new();
            fa.set_read_dir(rd);
            client::send_file_action(client_state, fa);
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ft-send-files") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            crate::config::write_log(&format!("[remote/ft] SendFiles raw: '{}'", txt));
            let parts: Vec<&str> = txt.splitn(6, '|').collect();
            if parts.len() >= 6 {
                let id: i32 = parts[0].parse().unwrap_or(0);
                let path = parts[1];
                let to = parts[2];
                let file_num: i32 = parts[3].parse().unwrap_or(0);
                let show_hidden = parts[4] == "1";
                let is_remote = parts[5] == "1";

                crate::config::write_log(&format!("[remote/ft] SendFiles: id={} path='{}' to='{}' file_num={} is_remote={}", id, path, to, file_num, is_remote));

                if is_remote {

                    crate::config::write_log(&format!("[remote/ft] DOWNLOAD from remote: id={} path='{}'", id, path));
                    let jobs = FT_JOBS.as_mut().unwrap();
                    jobs.insert(id, JobWriteState {
                        dest_path: to.to_string(),
                        files: Vec::new(),
                        offsets: Vec::new(),
                        total_size: 0,
                        file_num: 0,
                        done_count: 0,
                    });
                    let mut send_req = crate::protocol::message_proto::FileTransferSendRequest::new();
                    send_req.id = id;
                    send_req.path = path.to_string();
                    send_req.include_hidden = show_hidden;
                    send_req.file_num = file_num;
                    let mut fa = crate::protocol::message_proto::FileAction::new();
                    fa.set_send(send_req);
                    client::send_file_action(client_state, fa);
                } else {

                    crate::config::write_log(&format!("[remote/ft] UPLOAD to remote: id={} path='{}' to='{}'", id, path, to));

                    let files = match crate::file_transfer::get_recursive_files(path, show_hidden) {
                        Ok(f) => f,
                        Err(e) => {
                            crate::config::write_log(&format!("[remote/ft] UPLOAD enumerate error: {}", e));
                            Vec::new()
                        }
                    };
                    if files.is_empty() {
                        crate::config::write_log("[remote/ft] UPLOAD no files to send, skipping");
                    } else {

                    let mut file_entries: Vec<_> = files.iter()
                        .filter(|f| {
                            let t = f.entry_type.enum_value_or_default();
                            t == message_proto::FileType::File || t == message_proto::FileType::Dir
                        })
                        .cloned()
                        .map(|mut f| { f.name = f.name.replace('\\', "/"); f })
                        .collect();
                    crate::config::write_log(&format!("[remote/ft] UPLOAD got {} entries (recursive)", file_entries.len()));

                    if file_entries.len() == 1 && !file_entries[0].name.is_empty() && std::path::Path::new(path).is_file() {
                        crate::config::write_log(&format!("[remote/ft] UPLOAD single file but name='{}', clearing to match convention", file_entries[0].name));
                        file_entries[0].name = String::new();
                    }

                    let total_size: u64 = file_entries.iter().map(|f| f.size).sum();
                    let mut recv = crate::protocol::message_proto::FileTransferReceiveRequest::new();
                    recv.id = id;
                    recv.path = to.to_string();
                    recv.files = file_entries.clone().into();
                    recv.file_num = file_num;
                    recv.total_size = total_size;
                    let mut fa = crate::protocol::message_proto::FileAction::new();
                    fa.set_receive(recv);
                    client::send_file_action(client_state, fa);

                    let source_path = std::path::Path::new(path).to_path_buf();

                    let cs = client_state.clone();
                    let job_id = id;
                    let file_entries_for_thread = file_entries.clone();
                    std::thread::spawn(move || {
                        use protobuf::Message as ProtoMsg;

                        std::thread::sleep(std::time::Duration::from_millis(300));

                        for (file_idx, fe) in file_entries_for_thread.iter().enumerate() {

                            if fe.name.ends_with('/') {
                                crate::config::write_log(&format!("[remote/ft] UPLOAD dir marker {}/{}: '{}'", file_idx + 1, file_entries_for_thread.len(), fe.name));
                                continue;
                            }

                            let local_file_path = if fe.name.is_empty() {
                                source_path.clone()
                            } else {
                                source_path.join(&fe.name)
                            };
                            let local_path_str = local_file_path.to_string_lossy().to_string();
                            crate::config::write_log(&format!("[remote/ft] UPLOAD file {}/{}: '{}'", file_idx + 1, file_entries_for_thread.len(), local_path_str));

                            let mut offset: u64 = 0;
                            let mut blk_id: u32 = 0;
                            loop {
                                match crate::file_transfer::read_file_block(&local_path_str, offset) {
                                    Ok(data) => {
                                        if data.is_empty() {
                                            break;
                                        }
                                        let data_len = data.len() as u64;
                                        let mut block = crate::protocol::message_proto::FileTransferBlock::new();
                                        block.id = job_id;
                                        block.file_num = file_idx as i32;
                                        block.data = data.into();
                                        block.blk_id = blk_id;
                                        let mut fr = crate::protocol::message_proto::FileResponse::new();
                                        fr.set_block(block);
                                        let mut msg = crate::protocol::message_proto::Message::new();
                                        msg.set_file_response(fr);
                                        if let Ok(bytes) = msg.write_to_bytes() {
                                            if let Ok(state) = cs.lock() {
                                                if let Some(ref stream) = state.input_stream {
                                                    if let Ok(mut s) = stream.lock() {
                                                        let _ = s.send_msg(&bytes);
                                                    }
                                                }
                                            }
                                        }
                                        offset += data_len;
                                        blk_id += 1;
                                    }
                                    Err(e) => {
                                        crate::config::write_log(&format!("[remote/ft] UPLOAD read error file {}: {}", file_idx, e));
                                        break;
                                    }
                                }
                            }

                        }

                        let mut done = crate::protocol::message_proto::FileTransferDone::new();
                        done.id = job_id;
                        done.file_num = (file_entries_for_thread.len() - 1) as i32;
                        let mut fr = crate::protocol::message_proto::FileResponse::new();
                        fr.set_done(done);
                        let mut msg = crate::protocol::message_proto::Message::new();
                        msg.set_file_response(fr);
                        if let Ok(bytes) = msg.write_to_bytes() {
                            if let Ok(state) = cs.lock() {
                                if let Some(ref stream) = state.input_stream {
                                    if let Ok(mut s) = stream.lock() {
                                        let _ = s.send_msg(&bytes);
                                    }
                                }
                            }
                        }
                        crate::config::write_log(&format!("[remote/ft] UPLOAD complete for id={} ({} files)", job_id, file_entries_for_thread.len()));
                    });

                    let mut fd = crate::protocol::message_proto::FileDirectory::new();
                    fd.id = id;
                    fd.path = path.to_string();
                    fd.entries = file_entries.into();
                    let val = crate::file_transfer::file_directory_to_value(&fd);
                    let _ = root.call_method("updateFolderFiles", &[val]);
                    }
                }
            } else {
                crate::config::write_log(&format!("[remote/ft] SendFiles parse error: expected 6 parts, got {}", parts.len()));
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ft-cancel-job") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            if let Ok(id) = txt.parse::<i32>() {
                let mut cancel = crate::protocol::message_proto::FileTransferCancel::new();
                cancel.id = id;
                let mut fa = crate::protocol::message_proto::FileAction::new();
                fa.set_cancel(cancel);
                client::send_file_action(client_state, fa);
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ft-create-dir") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = txt.splitn(3, '|').collect();
            if parts.len() >= 3 {
                let _id: i32 = parts[0].parse().unwrap_or(0);
                let path = parts[1];
                let is_remote = parts[2] == "1";

                if is_remote {
                    let mut cd = crate::protocol::message_proto::FileDirCreate::new();
                    cd.id = _id;
                    cd.path = path.to_string();
                    let mut fa = crate::protocol::message_proto::FileAction::new();
                    fa.set_create(cd);
                    client::send_file_action(client_state, fa);
                } else {
                    let _ = std::fs::create_dir_all(path);

                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ft-remove-file") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = txt.splitn(4, '|').collect();
            if parts.len() >= 4 {
                let id: i32 = parts[0].parse().unwrap_or(0);
                let path = parts[1];
                let file_num: i32 = parts[2].parse().unwrap_or(0);
                let is_remote = parts[3] == "1";

                if is_remote {
                    let mut rf = crate::protocol::message_proto::FileRemoveFile::new();
                    rf.id = id;
                    rf.path = path.to_string();
                    rf.file_num = file_num;
                    let mut fa = crate::protocol::message_proto::FileAction::new();
                    fa.set_remove_file(rf);
                    client::send_file_action(client_state, fa);
                } else {
                    let _ = std::fs::remove_file(path);
                    let mut done = crate::protocol::message_proto::FileTransferDone::new();
                    done.id = id;
                    done.file_num = file_num;
                    let _ = root.call_method("jobDone", &[
                        sciter::Value::from(id),
                        sciter::Value::from(file_num),
                    ]);
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ft-remove-dir") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = txt.splitn(3, '|').collect();
            if parts.len() >= 3 {
                let id: i32 = parts[0].parse().unwrap_or(0);
                let path = parts[1];
                let is_remote = parts[2] == "1";

                if is_remote {
                    let mut rd = crate::protocol::message_proto::FileRemoveDir::new();
                    rd.id = id;
                    rd.path = path.to_string();
                    let mut fa = crate::protocol::message_proto::FileAction::new();
                    fa.set_remove_dir(rd);
                    client::send_file_action(client_state, fa);
                } else {
                    let _ = std::fs::remove_dir(path);
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ft-remove-dir-all") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = txt.splitn(4, '|').collect();
            if parts.len() >= 4 {
                let id: i32 = parts[0].parse().unwrap_or(0);
                let path = parts[1];
                let is_remote = parts[2] == "1";
                let include_hidden = parts[3] == "1";

                if is_remote {
                    let mut rd = crate::protocol::message_proto::FileRemoveDir::new();
                    rd.id = id;
                    rd.path = path.to_string();
                    rd.recursive = true;
                    let mut fa = crate::protocol::message_proto::FileAction::new();
                    fa.set_remove_dir(rd);
                    client::send_file_action(client_state, fa);
                } else {
                    let _ = std::fs::remove_dir_all(path);
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ft-send-confirm") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = txt.splitn(4, '|').collect();
            if parts.len() >= 4 {
                let id: i32 = parts[0].parse().unwrap_or(0);
                let file_num: i32 = parts[1].parse().unwrap_or(0);
                let skip = parts[2] == "1";
                let offset: u64 = parts[3].parse().unwrap_or(0);

                let mut sc = crate::protocol::message_proto::FileTransferSendConfirmRequest::new();
                sc.id = id;
                sc.file_num = file_num;
                if skip {
                    sc.set_skip(true);
                } else {
                    sc.set_offset_blk(offset as u32);
                }
                let mut fa = crate::protocol::message_proto::FileAction::new();
                fa.set_send_confirm(sc);
                client::send_file_action(client_state, fa);
            }
        }
    }
}

unsafe fn render_frame_gdi(hwnd: sciter::types::HWINDOW, bgra: &[u8], width: i32, height: i32) {
    let video = VIDEO_HWND;
    if video.is_null() { return; }

    let mut parent_rect = [0i32; 4];
    GetClientRect(hwnd as *mut std::ffi::c_void, &mut parent_rect);
    let header_h = 33;
    let client_w = parent_rect[2];
    let client_h = parent_rect[3] - header_h;
    if client_w <= 0 || client_h <= 0 { return; }

    MoveWindow(video, 0, header_h, client_w, client_h, 0);

    let hdc = GetDC(video);
    if hdc.is_null() { return; }

    let scale_x = client_w as f64 / width as f64;
    let scale_y = client_h as f64 / height as f64;
    let scale = if scale_x < scale_y { scale_x } else { scale_y };
    let dest_w = (width as f64 * scale) as i32;
    let dest_h = (height as f64 * scale) as i32;
    let offset_x = (client_w - dest_w) / 2;
    let offset_y = (client_h - dest_h) / 2;

    VIDEO_OFFSET_X = offset_x;
    VIDEO_OFFSET_Y = offset_y;
    VIDEO_SCALE_W = dest_w;
    VIDEO_SCALE_H = dest_h;

    let brush = CreateSolidBrush(0x002A170F);
    let bg_rect = [0i32, 0, client_w, client_h];
    FillRect(hdc, &bg_rect, brush);
    DeleteObject(brush);

    SetStretchBltMode(hdc, 4);

    let mut bmi = [0u8; 40];
    bmi[0..4].copy_from_slice(&40u32.to_le_bytes());
    bmi[4..8].copy_from_slice(&width.to_le_bytes());
    bmi[8..12].copy_from_slice(&(-height).to_le_bytes());
    bmi[12..14].copy_from_slice(&1u16.to_le_bytes());
    bmi[14..16].copy_from_slice(&32u16.to_le_bytes());

    let _result = StretchDIBits(
        hdc,
        offset_x, offset_y, dest_w, dest_h,
        0, 0, width, height,
        bgra.as_ptr(),
        bmi.as_ptr(),
        0,
        0x00CC0020,
    );

    ReleaseDC(video, hdc);
}

fn handle_mouse_input(text: &str, client_state: &Arc<Mutex<client::ClientState>>) {
    let parts: Vec<&str> = text.split(',').collect();
    if parts.len() < 4 { return; }
    let event_type: i32 = parts[0].parse().unwrap_or(0);
    let x: i32 = parts[1].parse().unwrap_or(0);
    let y: i32 = parts[2].parse().unwrap_or(0);
    let buttons: i32 = parts[3].parse().unwrap_or(1);

    let alt = parts.get(4).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0) != 0;
    let ctrl = parts.get(5).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0) != 0;
    let shift = parts.get(6).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0) != 0;

    let mask = (buttons << 3) | event_type;

    client::send_mouse_event(client_state, mask, x, y, alt, ctrl, shift);
}

fn handle_key_input(text: &str, client_state: &Arc<Mutex<client::ClientState>>) {

    match text {
        "cad" => { crate::config::write_log("[remote] key-event: cad"); client::send_ctrl_alt_del(client_state); return; }
        "lock" => { crate::config::write_log("[remote] key-event: lock"); client::send_lock_screen(client_state); return; }
        "restart" => { crate::config::write_log("[remote] key-event: restart"); client::send_restart(client_state); return; }
        "keyboard_on" | "keyboard_off" => { return; }
        _ => {

            if text.starts_with("chat_toggle") || text.starts_with("debug") {
                crate::config::write_log(&format!("[remote] key-event: {}", text));
                return;
            }
        }
    }
    let parts: Vec<&str> = text.split(',').collect();
    if parts.len() < 2 { return; }
    let down: bool = parts[0].parse::<i32>().unwrap_or(0) == 0;
    let vk_code: u32 = parts[1].parse().unwrap_or(0);

    let alt = parts.get(2).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0) != 0;
    let ctrl = parts.get(3).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0) != 0;
    let shift = parts.get(4).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0) != 0;

    client::send_key_event(client_state, down, vk_code, alt, ctrl, shift);
}

fn save_frame_as_bmp(path: &str, width: i32, height: i32, bgra_data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let w = width as u32;
    let h = height as u32;
    let row_size = w * 3;
    let row_padded = (row_size + 3) & !3;
    let pixel_data_size = row_padded * h;
    let file_size = 54 + pixel_data_size;

    let mut f = std::fs::File::create(path)?;

    f.write_all(b"BM")?;
    f.write_all(&file_size.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&54u32.to_le_bytes())?;

    f.write_all(&40u32.to_le_bytes())?;
    f.write_all(&w.to_le_bytes())?;
    f.write_all(&h.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&24u16.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&pixel_data_size.to_le_bytes())?;
    f.write_all(&2835u32.to_le_bytes())?;
    f.write_all(&2835u32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;

    let stride = w as usize * 4;
    let pad = vec![0u8; (row_padded - row_size) as usize];
    for y in (0..h as usize).rev() {
        let row_start = y * stride;
        for x in 0..w as usize {
            let px = row_start + x * 4;
            if px + 2 < bgra_data.len() {
                f.write_all(&[bgra_data[px], bgra_data[px + 1], bgra_data[px + 2]])?;
            } else {
                f.write_all(&[0, 0, 0])?;
            }
        }
        if !pad.is_empty() {
            f.write_all(&pad)?;
        }
    }
    Ok(())
}
