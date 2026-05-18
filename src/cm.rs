
use std::path::PathBuf;

#[link(name = "user32")]
extern "system" {
    fn SetTimer(
        hwnd: *mut std::ffi::c_void,
        id: usize,
        interval: u32,
        callback: Option<unsafe extern "system" fn(*mut std::ffi::c_void, u32, usize, u32)>,
    ) -> usize;
    fn GetWindowRect(hwnd: *mut std::ffi::c_void, rect: *mut [i32; 4]) -> i32;
    fn MoveWindow(hwnd: *mut std::ffi::c_void, x: i32, y: i32, w: i32, h: i32, repaint: i32) -> i32;
    fn ShowWindow(hwnd: *mut std::ffi::c_void, cmd: i32) -> i32;
}

static mut CM_SESSION_ID: Option<String> = None;
static mut CM_HWND: sciter::types::HWINDOW = std::ptr::null_mut();
static mut CM_RESPONDED: bool = false;
static mut CM_CHAT_LINES_READ: usize = 0;
static mut CM_CHAT_PANEL_RESIZED: bool = false;
static mut CM_ACCEPTED_TICK: u32 = 0;
static mut CM_MINIMIZED: bool = false;

fn string_to_rgb(name: &str) -> String {
    let mut hash: u32 = 0;
    for b in name.bytes() {
        hash = b as u32 + ((hash << 5).wrapping_sub(hash));
    }
    let r = (hash & 0xFF) as u8;
    let g = ((hash >> 8) & 0xFF) as u8;
    let b = ((hash >> 16) & 0xFF) as u8;

    let r = r / 2 + 40;
    let g = g / 2 + 40;
    let b = b / 2 + 40;
    format!("rgb({},{},{})", r, g, b)
}

pub fn cm_temp_dir() -> PathBuf {
    if let Some(shared) = crate::config::shared_app_dir_pub() {
        let runtime = shared.join("runtime");
        let need_create = !runtime.exists();
        if std::fs::create_dir_all(&runtime).is_ok() {
            if need_create {
                make_users_writable(&runtime);
            }
            return runtime;
        }
    }
    let fallback = std::env::temp_dir();
    crate::config::write_log(&format!(
        "[cm] WARNING: shared runtime dir unavailable, falling back to {}",
        fallback.display()
    ));
    fallback
}

pub fn service_status_path() -> PathBuf {
    cm_temp_dir().join("service_status.txt")
}

pub fn write_service_status(status: &str) {
    let path = service_status_path();
    let _ = std::fs::write(&path, status);
    make_users_writable(&path);
}

pub fn read_service_status() -> Option<String> {
    std::fs::read_to_string(service_status_path()).ok().map(|s| s.trim().to_string())
}

fn make_users_writable(path: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let p = path.to_string_lossy();
    let _ = std::process::Command::new("cacls")
        .arg(p.as_ref())
        .args(["/e", "/g", "Users:M"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

pub fn cm_info_path(session_id: &str) -> PathBuf {
    cm_temp_dir().join(format!("hoptodesk_cm_{}.json", session_id))
}

pub fn cm_accepted_path(session_id: &str) -> PathBuf {
    cm_temp_dir().join(format!("hoptodesk_cm_{}.accepted", session_id))
}

pub fn cm_rejected_path(session_id: &str) -> PathBuf {
    cm_temp_dir().join(format!("hoptodesk_cm_{}.rejected", session_id))
}

pub fn cm_connected_path(session_id: &str) -> PathBuf {
    cm_temp_dir().join(format!("hoptodesk_cm_{}.connected", session_id))
}

pub fn signal_cm_connected(session_id: &str) {
    let path = cm_connected_path(session_id);
    let _ = std::fs::write(&path, "connected");
    make_users_writable(&path);
}

pub fn write_cm_info(session_id: &str, peer_id: &str, peer_name: &str, peer_platform: &str) {
    let info = serde_json::json!({
        "peer_id": peer_id,
        "peer_name": peer_name,
        "peer_platform": peer_platform,
    })
    .to_string();
    let path = cm_info_path(session_id);
    let _ = std::fs::write(&path, &info);
    make_users_writable(&path);
    crate::config::write_log(&format!("[cm] Wrote peer info to {}", path.display()));
}

pub fn check_cm_response(session_id: &str) -> Option<bool> {
    if cm_accepted_path(session_id).exists() {
        Some(true)
    } else if cm_rejected_path(session_id).exists() {
        Some(false)
    } else {
        None
    }
}

pub fn cm_chat_path(session_id: &str) -> PathBuf {
    cm_temp_dir().join(format!("hoptodesk_cm_{}.chat", session_id))
}

pub fn cm_chat_send_path(session_id: &str) -> PathBuf {
    cm_temp_dir().join(format!("hoptodesk_cm_{}.chatsend", session_id))
}

pub fn append_chat_message(session_id: &str, from: &str, text: &str) {
    use std::io::Write;
    let path = cm_chat_path(session_id);
    let new_file = !path.exists();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}:{}", from, text);
    }
    if new_file {
        make_users_writable(&path);
    }
}

pub fn cm_perm_path(session_id: &str) -> PathBuf {
    cm_temp_dir().join(format!("hoptodesk_cm_{}.perm", session_id))
}

pub fn cm_ended_path(session_id: &str) -> PathBuf {
    cm_temp_dir().join(format!("hoptodesk_cm_{}.ended", session_id))
}

pub fn signal_cm_ended(session_id: &str) {
    let path = cm_ended_path(session_id);
    let _ = std::fs::write(&path, "ended");
    make_users_writable(&path);
    crate::config::write_log(&format!("[cm] Wrote session-ended signal for {}", session_id));
}

pub fn cleanup_runtime_dir() {
    let dir = cm_temp_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if name.starts_with("hoptodesk_cm_") {
            if std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        crate::config::write_log(&format!(
            "[cm] cleaned up {} stale CM file(s) in {}",
            removed,
            dir.display()
        ));
    }
}

pub fn cleanup_cm_files(session_id: &str) {
    let _ = std::fs::remove_file(cm_info_path(session_id));
    let _ = std::fs::remove_file(cm_accepted_path(session_id));
    let _ = std::fs::remove_file(cm_rejected_path(session_id));
    let _ = std::fs::remove_file(cm_connected_path(session_id));
    let _ = std::fs::remove_file(cm_chat_path(session_id));
    let _ = std::fs::remove_file(cm_chat_send_path(session_id));
    let _ = std::fs::remove_file(cm_perm_path(session_id));
    let _ = std::fs::remove_file(cm_ended_path(session_id));
}

pub fn spawn_cm_process(session_id: &str) {
    use std::sync::atomic::Ordering;
    if crate::install::RUNNING_AS_SERVICE.load(Ordering::SeqCst) {
        crate::config::write_log(&format!(
            "[cm] CM info written for session {}; user-session tray will spawn the dialog",
            session_id
        ));
        return;
    }
    use std::process::Stdio;
    let exe = std::env::current_exe().unwrap_or_default();
    crate::config::write_log(&format!("[cm] Spawning: {} --cm {}", exe.display(), session_id));
    match std::process::Command::new(&exe)
        .args(["--cm", session_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => {}
        Err(e) => crate::config::write_log(&format!("[cm] Spawn failed: {}", e)),
    }
}

fn read_peer_info(session_id: &str) -> (String, String, String) {
    let path = cm_info_path(session_id);
    let data = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return ("Unknown".into(), "Unknown".into(), "Unknown".into()),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(e) => {
            crate::config::write_log(&format!("[cm] read_peer_info: bad JSON: {}", e));
            return ("Unknown".into(), "Unknown".into(), "Unknown".into());
        }
    };
    let take = |k: &str| {
        parsed
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    (take("peer_id"), take("peer_name"), take("peer_platform"))
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn run_cm_process(session_id: &str) {
    crate::config::write_log(&format!("[cm] CM process started for session {}", session_id));

    let (peer_id, peer_name, peer_platform) = read_peer_info(session_id);
    crate::config::write_log(&format!("[cm] Peer: {} ({}) on {}", peer_name, peer_id, peer_platform));

    sciter::set_options(sciter::RuntimeOptions::GfxLayer(sciter::GFX_LAYER::CPU)).ok();

    let mut frame = sciter::Window::new();

    let html_template = include_str!("ui/cm.html");
    let display_id = crate::format_id(&peer_id);
    let display_name = if peer_name.is_empty() { &display_id } else { &peer_name };

    let avatar_color = string_to_rgb(display_name);
    let avatar_letter = display_name.chars().next().unwrap_or('?').to_uppercase().to_string();

    let safe_id = html_escape(&display_id);
    let safe_name = html_escape(display_name);
    let safe_platform = html_escape(&peer_platform);
    let safe_letter = html_escape(&avatar_letter);

    let html = html_template
        .replace("__PEER_ID__", &safe_id)
        .replace("__PEER_NAME__", &safe_name)
        .replace("__PEER_PLATFORM__", &safe_platform)
        .replace("__AVATAR_COLOR__", &avatar_color)
        .replace("__AVATAR_LETTER__", &safe_letter);

    frame.load_html(html.as_bytes(), Some("this://app/cm.html"));
    frame.set_title("HopToDesk - Incoming Connection");

    let hwnd = frame.get_hwnd();
    crate::set_window_icon(hwnd);
    unsafe {
        CM_SESSION_ID = Some(session_id.to_string());
        CM_HWND = hwnd;
        CM_RESPONDED = false;
        CM_CHAT_LINES_READ = 0;
        CM_CHAT_PANEL_RESIZED = false;
        SetTimer(hwnd as *mut std::ffi::c_void, 1, 200, Some(cm_timer_callback));
    }

    frame.run_app();

    unsafe {
        if !CM_RESPONDED {
            if let Some(ref sid) = CM_SESSION_ID {
                let _ = std::fs::write(cm_rejected_path(sid), "rejected");
            }
        }
    }
}

fn show_connected_state(root: &sciter::Element) {
    crate::config::write_log("[cm] show_connected_state called");
    if let Ok(Some(mut status)) = root.find_first("#cm-status") {
        let _ = status.set_text("");
        let _ = status.set_style_attribute("display", "none");
    }

    if let Ok(Some(mut btn_row)) = root.find_first("#button-row") {
        let _ = btn_row.set_style_attribute("display", "none");
    }

    if let Ok(Some(mut disc_row)) = root.find_first("#disconnect-row") {
        let _ = disc_row.set_style_attribute("display", "block");
        let _ = disc_row.set_style_attribute("visibility", "visible");
        let _ = disc_row.set_style_attribute("height", "auto");
    }
}

unsafe extern "system" fn cm_timer_callback(
    _hwnd: *mut std::ffi::c_void,
    _msg: u32,
    _id: usize,
    _time: u32,
) {
    let hwnd = CM_HWND;
    if hwnd.is_null() {
        return;
    }

    let session_id = match CM_SESSION_ID.as_ref() {
        Some(s) => s.clone(),
        None => return,
    };

    if cm_ended_path(&session_id).exists() {
        crate::config::write_log("[cm] Session ended signal received, exiting");
        let _ = std::fs::remove_file(cm_ended_path(&session_id));
        std::process::exit(0);
    }

    if CM_RESPONDED && !cm_info_path(&session_id).exists() {
        crate::config::write_log("[cm] Session ended (info file removed), exiting");
        std::process::exit(0);
    }

    let root = match sciter::Element::from_window(hwnd) {
        Ok(r) => r,
        Err(_) => return,
    };

    if let Ok(Some(mut el)) = root.find_first("#accept-flag") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            crate::config::write_log(&format!("[cm] User accepted connection"));
            let _ = std::fs::write(cm_accepted_path(&session_id), "accepted");
            CM_RESPONDED = true;
            CM_ACCEPTED_TICK = 1;

            show_connected_state(&root);
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#reject-flag") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            crate::config::write_log(&format!("[cm] User rejected connection"));
            let _ = std::fs::write(cm_rejected_path(&session_id), "rejected");
            CM_RESPONDED = true;
            std::process::exit(0);
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#disconnect-flag") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");
            crate::config::write_log(&format!("[cm] User disconnected"));
            let _ = std::fs::remove_file(cm_accepted_path(&session_id));
            let _ = std::fs::write(cm_rejected_path(&session_id), "disconnected");
            std::process::exit(0);
        }
    }

    if !CM_RESPONDED {
        let connected_exists = cm_connected_path(&session_id).exists();
        if connected_exists {
            CM_RESPONDED = true;
            CM_ACCEPTED_TICK = 1;
            crate::config::write_log("[cm] Server signaled connection established");
            show_connected_state(&root);
        }
    }

    if CM_ACCEPTED_TICK > 0 && !CM_MINIMIZED {
        CM_ACCEPTED_TICK += 1;
        if CM_ACCEPTED_TICK >= 25 {
            CM_MINIMIZED = true;
            crate::config::write_log("[cm] Auto-minimizing after 5 seconds");
            ShowWindow(hwnd as *mut std::ffi::c_void, 6);
        }
    }

    if let Ok(content) = std::fs::read_to_string(cm_chat_path(&session_id)) {
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() > CM_CHAT_LINES_READ {

            let mut html = String::new();
            for line in &lines {
                if let Some(colon) = line.find(':') {
                    let from = &line[..colon];
                    let text = &line[colon + 1..];
                    let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
                    html.push_str(&format!(
                        "<div class=\"chat-msg\"><b>{}</b>: {}</div>",
                        from, escaped
                    ));
                }
            }
            if let Ok(Some(mut msgs)) = root.find_first("#chat-msgs") {
                let _ = msgs.set_html(html.as_bytes(), None);
            }
            CM_CHAT_LINES_READ = lines.len();

            if let Ok(Some(mut chat)) = root.find_first("#right-panel") {
                let _ = chat.set_style_attribute("display", "block");
            }

            if !CM_CHAT_PANEL_RESIZED {
                CM_CHAT_PANEL_RESIZED = true;
                let hwnd_ptr = CM_HWND as *mut std::ffi::c_void;
                let mut rect = [0i32; 4];
                if GetWindowRect(hwnd_ptr, &mut rect) != 0 {
                    let x = rect[0];
                    let y = rect[1];
                    let w = rect[2] - rect[0];
                    let h = rect[3] - rect[1];
                    if w < 600 {

                        let new_x = x - (600 - w);
                        MoveWindow(hwnd_ptr, new_x, y, 600, h, 1);
                    }
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#perm-flag") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");

            let path = cm_perm_path(&session_id);
            let _ = std::fs::write(&path, &txt);
            crate::config::write_log(&format!("[cm] Permission toggle: {}", txt));
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#chat-send-flag") {
        let txt = el.get_text();
        if !txt.is_empty() {
            let _ = el.set_text("");

            let path = cm_chat_send_path(&session_id);
            let _ = std::fs::write(&path, &txt);

            append_chat_message(&session_id, "Me", &txt);
        }
    }
}
