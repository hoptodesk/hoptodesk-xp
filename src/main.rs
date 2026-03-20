#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod capture;
mod client;
mod clipboard;
mod cm;
mod config;
mod crypto;
mod file_transfer;
mod input;
mod lang;
mod network;
mod platform;
mod protocol;
mod remote;
mod remote_handler;
mod server;
mod signal;
mod turn;
mod ui_handler;
mod vpx;
mod websocket;
mod wininet;

extern "C" {
    fn SciterAPI() -> *const std::ffi::c_void;
}
#[used]
static FORCE_SCITER_IMPORT: unsafe extern "C" fn() -> *const std::ffi::c_void = SciterAPI;

use std::sync::{Arc, Mutex};
use std::process::Stdio;
use std::thread;

#[link(name = "user32")]
extern "system" {
    fn SetTimer(
        hwnd: *mut std::ffi::c_void,
        id: usize,
        interval: u32,
        callback: Option<unsafe extern "system" fn(*mut std::ffi::c_void, u32, usize, u32)>,
    ) -> usize;
    fn SendMessageA(hwnd: *mut std::ffi::c_void, msg: u32, wparam: usize, lparam: usize) -> isize;
    fn LoadIconA(hinst: *mut std::ffi::c_void, name: *const u8) -> *mut std::ffi::c_void;
    fn GetModuleHandleA(name: *const u8) -> *mut std::ffi::c_void;
}

pub fn set_window_icon(hwnd: sciter::types::HWINDOW) {
    if hwnd.is_null() { return; }
    unsafe {
        let hinst = GetModuleHandleA(std::ptr::null());

        let icon = LoadIconA(hinst, 1 as *const u8);
        if !icon.is_null() {
            const WM_SETICON: u32 = 0x0080;
            SendMessageA(hwnd as *mut std::ffi::c_void, WM_SETICON, 0, icon as usize);
            SendMessageA(hwnd as *mut std::ffi::c_void, WM_SETICON, 1, icon as usize);
        }
    }
}

static mut TIMER_STATE: Option<Arc<Mutex<ui_handler::AppState>>> = None;
static mut TIMER_HWND: sciter::types::HWINDOW = std::ptr::null_mut();
static mut TIMER_TICK: u32 = 0;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn format_id(id: &str) -> String {
    if id.len() == 9 {
        format!("{} {} {}", &id[0..3], &id[3..6], &id[6..9])
    } else {
        id.to_string()
    }
}

fn main() {

    config::cleanup_old_logs();
    crate::config::write_log(&format!("[main] HopToDesk {} starting", VERSION));

    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 2 && args[1].starts_with("--") {
        let name = args[1].replace("--", "");
        if !name.is_empty() {
            config::set_log_subdir(&name);
        }
    }

    if args.len() >= 2 {
        match args[1].as_str() {
            "--connect" if args.len() >= 3 => {
                let target_id = args[2].replace(' ', "");
                let is_ft = args.iter().any(|a| a == "--file-transfer");

                let switch_uuid = args.iter().position(|a| a == "--switch_uuid" || a == "--switch-uuid")
                    .and_then(|i| args.get(i + 1).cloned());
                if let Some(ref uuid) = switch_uuid {
                    crate::config::write_log(&format!("[connect] Switch Sides connection to {} with UUID {}", target_id, uuid));
                }

                let peer_cfg = config::PeerConfig::load(&target_id);
                let saved_password = peer_cfg.get_option("password");
                crate::config::write_log(&format!("[connect] Starting connection to {} (file_transfer={}, has_saved_pw={}, switch={})",
                    target_id, is_ft, !saved_password.is_empty(), switch_uuid.is_some()));
                remote::run_connect_process_ex(&target_id, &saved_password, is_ft, switch_uuid.as_deref());
                std::process::exit(0);
            }
            "--cm" => {

                let session_id = args.get(2)
                    .cloned()
                    .or_else(|| std::env::var("HOPTODESK_CM_SESSION").ok())
                    .unwrap_or_default();
                if !session_id.is_empty() {
                    cm::run_cm_process(&session_id);
                } else {
                    crate::config::write_log("[cm] No session ID provided");
                }
                std::process::exit(0);
            }
            "--version" => {
                println!("{}", VERSION);
                std::process::exit(0);
            }
            "--get-id" => {
                let cfg = config::Config::load();
                println!("{}", cfg.id);
                std::process::exit(0);
            }
            "--password" if args.len() >= 3 => {
                let new_password = &args[2];
                let mut cfg = config::Config::load();
                cfg.password = new_password.to_string();
                cfg.save();
                crate::config::write_log(&format!("Password updated."));
                std::process::exit(0);
            }
            "--server" => {
                run_headless_server();
                std::process::exit(0);
            }
            "--changeid" => {
                let cfg_path = config::config_dir().join("HopToDesk.toml");
                if let Ok(content) = std::fs::read_to_string(&cfg_path) {
                    let filtered: String = content
                        .lines()
                        .filter(|line| !line.starts_with("id = ") && !line.starts_with("salt = "))
                        .map(|line| format!("{}\n", line))
                        .collect();
                    let _ = std::fs::write(&cfg_path, filtered);
                    crate::config::write_log(&format!("ID reset. Restart to generate new ID."));
                }
                std::process::exit(0);
            }
            "--import-config" if args.len() >= 3 => {
                import_config(&args[2]);
                std::process::exit(0);
            }
            _ => {}
        }
    }

    check_invite_code_from_filename();

    run_main_ui();
}

fn check_invite_code_from_filename() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let name = match exe.file_name() {
        Some(n) => n.to_string_lossy().to_string(),
        None => return,
    };

    if let Some(dash_pos) = name.find('-') {
        let id_part = &name[dash_pos + 1..];
        let mut id_end = 0;
        let mut has_uppercase = false;
        for (i, c) in id_part.chars().enumerate() {
            if c.is_ascii_uppercase() {
                has_uppercase = true;
            } else if !(c.is_ascii_lowercase() || c.is_ascii_digit()) {
                break;
            }
            id_end = i + 1;
        }

        if has_uppercase && id_end == 16 {
            let invite_code = &id_part[..id_end];
            let mut cfg2 = config::Config2::load();
            cfg2.set_option("invite_code", invite_code);
            cfg2.save();
            crate::config::write_log(&format!("[init] Invite code from filename: {}", invite_code));
        } else if !has_uppercase && (id_end == 16 || id_end == 32) {
            let team_id = &id_part[..id_end];
            let team_path = config::config_dir().join("TeamID.toml");
            let _ = std::fs::write(&team_path, team_id);
            crate::config::write_log(&format!("[init] TeamID from filename: {}", team_id));
        }
    }
}

fn import_config(path: &str) {
    let src = std::path::Path::new(path);
    if !src.exists() {
        crate::config::write_log(&format!("Config file not found: {}", path));
        return;
    }
    let dest = config::config_dir().join("HopToDesk.toml");
    match std::fs::copy(src, &dest) {
        Ok(_) => crate::config::write_log(&format!("Config imported from {} to {}", path, dest.display())),
        Err(e) => crate::config::write_log(&format!("Failed to import config: {}", e)),
    }
}

fn run_headless_server() {
    config::migrate_old_config();
    let cfg = config::Config::load();
    let my_id = cfg.id.clone();
    let password = cfg.password.clone();
    let pk = cfg.key_pair.1.clone();

    crate::config::write_log(&format!("[server] Starting headless server, ID={}", my_id));
    crate::config::write_log(&format!("[server] Press Ctrl+C to stop"));

    let signal_state = Arc::new(Mutex::new(signal::SignalState::default()));
    signal::run_signal_loop(my_id, password, pk, signal_state);
}

fn build_ui_translations() -> String {
    let keys = [
        "This Device", "Your ID", "Password", "Set", "Unattended Access",
        "Remote Control", "Partner ID", "Enter Remote ID", "Connect",
        "Transfer File", "Recent Sessions", "Favorites", "Settings",
        "Remote Access", "Keyboard/Mouse", "Clipboard", "File Transfer",
        "Security", "Permanent Password", "About HopToDesk",
        "Password Settings", "Cancel", "Save", "Language",
        "Rename", "Add to Favorites", "Remove from Favorites",
        "Forget Password", "Remove", "Rename Peer", "Enter alias",
        "Connecting...", "Ready", "Not connected",
        "Website", "Privacy Statement", "OK", "Version",
        "Password must be at least 6 characters", "Passwords do not match",
    ];

    let mut parts = Vec::new();
    for key in &keys {
        let translated = lang::translate(key.to_string());
        let escaped = translated
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        parts.push(format!("\"{}\":\"{}\"", key.replace('"', "\\\""), escaped));
    }
    format!("{{{}}}", parts.join(","))
}

fn get_lang_name(code: &str) -> String {
    if let Some(arr) = lang::LANGS.as_array() {
        for item in arr {
            if let Some(pair) = item.as_array() {
                if pair.len() == 2 {
                    if let Some(c) = pair[0].as_str() {
                        if c == code {
                            if let Some(n) = pair[1].as_str() {
                                return n.to_string();
                            }
                        }
                    }
                }
            }
        }
    }
    "English".to_string()
}

fn send_wol_packet(mac_str: &str) {
    let mac_bytes: Vec<u8> = mac_str.split(':')
        .filter_map(|s| u8::from_str_radix(s, 16).ok())
        .collect();
    if mac_bytes.len() != 6 {
        config::write_log(&format!("[WOL] Invalid MAC: {}", mac_str));
        return;
    }
    let mut packet = vec![0xFFu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac_bytes);
    }
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        let _ = socket.set_broadcast(true);
        match socket.send_to(&packet, "255.255.255.255:9") {
            Ok(_) => config::write_log(&format!("[WOL] Magic packet sent to {}", mac_str)),
            Err(e) => config::write_log(&format!("[WOL] Send failed: {}", e)),
        }
    } else {
        config::write_log("[WOL] Failed to bind UDP socket");
    }
}

fn run_main_ui() {
    let state = Arc::new(Mutex::new(ui_handler::AppState::new()));

    let (my_id, my_password) = {
        let s = state.lock().unwrap();
        (s.config.id.clone(), s.config.password.clone())
    };

    {
        let state_clone = state.clone();
        thread::spawn(move || {
            let (my_id, password, pk, signal_state) = {
                let s = state_clone.lock().unwrap();
                (
                    s.config.id.clone(),
                    s.config.password.clone(),
                    s.config.key_pair.1.clone(),
                    s.signal_state.clone(),
                )
            };
            signal::run_signal_loop(my_id, password, pk, signal_state);
        });
    }

    sciter::set_options(sciter::RuntimeOptions::GfxLayer(sciter::GFX_LAYER::CPU)).ok();

    let mut frame = sciter::Window::new();

    let html_template = include_str!("ui/index.html");
    let id_formatted = format_id(&my_id);

    let saved_lang = {
        if let Ok(s) = state.lock() {
            s.local_config.get_option("lang")
        } else {
            String::new()
        }
    };
    if !saved_lang.is_empty() {
        lang::set_lang(&saved_lang);
    }

    let langs_json = lang::LANGS.to_string();
    let current_lang_code = if saved_lang.is_empty() { "en".to_string() } else { saved_lang.clone() };
    let current_lang_name = get_lang_name(&current_lang_code);
    let tr_json = build_ui_translations();

    let html = html_template
        .replace("Loading...", &id_formatted)
        .replace("------", &my_password)
        .replace(">Version<", &format!(">Version {}<", VERSION))
        .replace(">English<", &format!(">{}<", current_lang_name))
        .replace("id=\"langs-data\" style=\"visibility:hidden;height:0;overflow:hidden;\"></div>",
                 &format!("id=\"langs-data\" style=\"visibility:hidden;height:0;overflow:hidden;\">{}</div>", langs_json))
        .replace("id=\"tr-data\" style=\"visibility:hidden;height:0;overflow:hidden;\"></div>",
                 &format!("id=\"tr-data\" style=\"visibility:hidden;height:0;overflow:hidden;\">{}</div>", tr_json))
        .replace("id=\"current-lang-code\" style=\"visibility:hidden;height:0;overflow:hidden;\"></div>",
                 &format!("id=\"current-lang-code\" style=\"visibility:hidden;height:0;overflow:hidden;\">{}</div>", current_lang_code));

    frame.load_html(html.as_bytes(), Some("this://app/index.html"));
    frame.set_title("HopToDesk");

    let hwnd = frame.get_hwnd();
    set_window_icon(hwnd);

    unsafe {
        TIMER_STATE = Some(state.clone());
        TIMER_HWND = hwnd;
        SetTimer(hwnd as *mut std::ffi::c_void, 1, 1000, Some(main_timer_callback));
    }

    crate::config::write_log(&format!("[UI] Window created, entering event loop"));
    frame.run_app();
    std::process::exit(0);
}

unsafe extern "system" fn main_timer_callback(
    _hwnd: *mut std::ffi::c_void,
    _msg: u32,
    _id: usize,
    _time: u32,
) {
    let state = match TIMER_STATE.as_ref() {
        Some(s) => s,
        None => return,
    };
    let hwnd = TIMER_HWND;
    if hwnd.is_null() {
        return;
    }

    let root = match sciter::Element::from_window(hwnd) {
        Ok(r) => r,
        Err(_) => return,
    };

    let signal_status = {
        let s = match state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let sig = s.signal_state.lock().unwrap_or_else(|e| e.into_inner());
        sig.status.clone()
    };

    if let Ok(Some(mut icon)) = root.find_first("#status-icon") {
        let (css_class, status_key) = match signal_status.as_str() {
            "online" => ("connect-status-icon status-online", "Ready"),
            "connecting" => ("connect-status-icon status-connecting", "Connecting..."),
            _ => ("connect-status-icon status-offline", "Not connected"),
        };
        let _ = icon.set_attribute("class", css_class);
        if let Ok(Some(mut txt)) = root.find_first("#status-text") {
            let _ = txt.set_text(&lang::translate(status_key.to_string()));
        }
    }

    TIMER_TICK += 1;
    if TIMER_TICK % 3 == 0 {
        let disk_cfg = config::Config::load();
        let mut pw_changed = false;
        if let Ok(mut s) = state.lock() {
            if s.config.password != disk_cfg.password {
                s.config.password = disk_cfg.password.clone();
                pw_changed = true;
            }
        }
        if pw_changed {
            if let Ok(Some(mut pwbox)) = root.find_first("#pwbox") {
                let _ = pwbox.set_text(&disk_cfg.password);
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#connect-target") {
        let target = el.get_text();
        if !target.is_empty() {
            let _ = el.set_text("");
            let target_id = target.replace(' ', "");

            if let Ok(mut s) = state.lock() {
                s.local_config.set_remote_id(&target_id);
                s.local_config.add_recent_peer(&target_id);
                s.sessions_dirty = true;
            }

            let exe = std::env::current_exe().unwrap_or_default();
            crate::config::write_log(&format!("[UI] Spawning: {} --connect {}", exe.display(), target_id));
            match std::process::Command::new(&exe)
                .args(["--connect", &target_id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(_) => {},
                Err(e) => crate::config::write_log(&format!("[UI] Spawn failed: {}", e)),
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ft-target") {
        let target = el.get_text();
        if !target.is_empty() {
            let _ = el.set_text("");
            let target_id = target.replace(' ', "");
            crate::config::write_log(&format!("[UI] File transfer target: {}", target_id));

            if let Ok(mut s) = state.lock() {
                s.local_config.set_remote_id(&target_id);
                s.local_config.add_recent_peer(&target_id);
                s.sessions_dirty = true;
            }

            let exe = std::env::current_exe().unwrap_or_default();
            crate::config::write_log(&format!("[UI] Spawning: {} --connect {} --file-transfer", exe.display(), target_id));
            match std::process::Command::new(&exe)
                .args(["--connect", &target_id, "--file-transfer"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(_) => {},
                Err(e) => crate::config::write_log(&format!("[UI] Spawn failed: {}", e)),
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#tab-switch-flag") {
        let tab = el.get_text();
        if !tab.is_empty() {
            let _ = el.set_text("");
            if let Ok(mut s) = state.lock() {
                s.active_tab = tab;
                s.sessions_dirty = true;
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#fav-toggle-flag") {
        let id = el.get_text();
        if !id.is_empty() {
            let _ = el.set_text("");
            if let Ok(mut s) = state.lock() {
                s.local_config.toggle_fav(&id);
                s.sessions_dirty = true;
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#remove-peer-flag") {
        let id = el.get_text();
        if !id.is_empty() {
            let _ = el.set_text("");
            if let Ok(mut s) = state.lock() {
                s.local_config.remove_recent_peer(&id);
                s.sessions_dirty = true;
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#forget-pw-flag") {
        let id = el.get_text();
        if !id.is_empty() {
            let _ = el.set_text("");
            let mut peer_cfg = config::PeerConfig::load(&id);
            peer_cfg.options.remove("password");
            peer_cfg.save(&id);
            config::write_log(&format!("[UI] Forgot password for peer {}", id));
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-perm-pw-flag") {
        let pw = el.get_text();
        if !pw.is_empty() {
            let _ = el.set_text("");
            if let Ok(mut s) = state.lock() {
                if pw == "__CLEAR__" {
                    s.config.permanent_password.clear();
                } else {
                    s.config.permanent_password = pw;
                }
                s.config.save();
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-option-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = text.splitn(2, '|').collect();
            if parts.len() == 2 {
                if let Ok(mut s) = state.lock() {
                    s.config2.set_option(parts[0], parts[1]);
                    s.config2.save();
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#rename-peer-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = text.splitn(2, '|').collect();
            if parts.len() == 2 {
                let peer_id = parts[0];
                let alias = parts[1];
                let mut peer_cfg = config::PeerConfig::load(peer_id);
                peer_cfg.alias = alias.to_string();
                peer_cfg.save(peer_id);
                if let Ok(mut s) = state.lock() {
                    s.sessions_dirty = true;
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#wol-flag") {
        let peer_id = el.get_text();
        if !peer_id.is_empty() {
            let _ = el.set_text("");
            let peer_cfg = config::PeerConfig::load(&peer_id);
            let mac = peer_cfg.get_option("mac_address");
            if mac.is_empty() {
                config::write_log(&format!("[WOL] No MAC address set for peer {}", peer_id));
            } else {
                send_wol_packet(&mac);
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-mac-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = text.splitn(2, '|').collect();
            if parts.len() == 2 {
                let peer_id = parts[0];
                let mac = parts[1];
                let mut peer_cfg = config::PeerConfig::load(peer_id);
                peer_cfg.set_option("mac_address", mac);
                peer_cfg.save(peer_id);
                config::write_log(&format!("[WOL] MAC address set for peer {}: {}", peer_id, mac));
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-lang-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            crate::config::write_log(&format!("[UI] Language changed to: {}", text));
            lang::set_lang(&text);
            if let Ok(mut s) = state.lock() {
                s.local_config.set_option("lang", &text);
                s.local_config.save();
            }

            // Update translations JSON so TIS can re-translate
            let tr_json = build_ui_translations();
            if let Ok(Some(mut tr_el)) = root.find_first("#tr-data") {
                let _ = tr_el.set_text(&tr_json);
            }

            // Update current lang code
            if let Ok(Some(mut code_el)) = root.find_first("#current-lang-code") {
                let _ = code_el.set_text(&text);
            }

            // Trigger TIS re-translation
            if let Ok(Some(body)) = root.find_first("body") {
                let _ = body.eval_script("try { initTranslations(); applyTranslations(); } catch(e) {}");
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#open-url-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            let _ = std::process::Command::new("cmd")
                .args(&["/C", "start", &text])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }

    let (ua_enabled, has_perm) = if let Ok(s) = state.lock() {
        let ua = s.config2.get_option("unattended-access") == "Y";
        let pw = !s.config.permanent_password.is_empty();
        (ua, pw)
    } else {
        (false, false)
    };
    if let Ok(Some(mut el)) = root.find_first("#ua-status") {
        let _ = el.set_text("");
    }
    if let Ok(Some(mut sw)) = root.find_first("#sw-ua") {
        let has_class = sw.get_attribute("class").map_or(false, |c| c.contains("on"));
        if ua_enabled && !has_class {
            let _ = sw.set_attribute("class", "toggle-switch on");
        } else if !ua_enabled && has_class {
            let _ = sw.set_attribute("class", "toggle-switch");
        }
    }

    {
        let mut should_update = false;
        let mut items_html = String::new();

        if let Ok(mut s) = state.lock() {
            if s.sessions_dirty {
                s.sessions_dirty = false;
                should_update = true;
                let tab = s.active_tab.clone();

                let peers: Vec<config::RecentPeer> = if tab == "fav" {
                    s.local_config.fav.iter().filter_map(|fav_id| {
                        s.local_config.recent_peers.iter().find(|p| p.id == *fav_id).cloned()
                            .or_else(|| Some(config::RecentPeer {
                                id: fav_id.clone(),
                                username: String::new(),
                                hostname: String::new(),
                                platform: String::new(),
                            }))
                    }).collect()
                } else {
                    s.local_config.recent_peers.clone()
                };

                if peers.is_empty() {
                    let msg = if tab == "fav" {
                        lang::translate("empty_favorite_tip".to_string())
                    } else {
                        lang::translate("empty_recent_tip".to_string())
                    };
                    items_html = format!("<div class=\"empty-state\">{}</div>", msg);
                } else {
                    for p in &peers {
                        let display_id = crate::format_id(&p.id);
                        let peer_cfg = config::PeerConfig::load(&p.id);
                        let is_fav = s.local_config.is_fav(&p.id);
                        let fav_class = if is_fav { "session-fav is-fav" } else { "session-fav" };
                        let fav_char = if is_fav { "&#9733;" } else { "&#9734;" };
                        let display_name = if !peer_cfg.alias.is_empty() {
                            peer_cfg.alias.clone()
                        } else {
                            display_id.clone()
                        };
                        let sub = if !peer_cfg.alias.is_empty() {
                            display_id.clone()
                        } else if !p.hostname.is_empty() {
                            if !p.username.is_empty() {
                                format!("{}@{}", p.username, p.hostname)
                            } else {
                                p.hostname.clone()
                            }
                        } else {
                            String::new()
                        };

                        let plat_label = match p.platform.to_lowercase().as_str() {
                            "windows" => "Win",
                            "linux" => "Lin",
                            "mac os" | "macos" => "Mac",
                            "android" => "And",
                            _ if !p.platform.is_empty() => &p.platform[..3.min(p.platform.len())],
                            _ => &p.id[p.id.len().saturating_sub(2)..],
                        };

                        items_html.push_str(&format!(
                            "<div class=\"session-item\" data-id=\"{}\">\
                                <div class=\"session-platform\">{}</div>\
                                <div class=\"session-info\">\
                                    <div class=\"session-name\">{}</div>\
                                    <div class=\"session-sub\">{}</div>\
                                </div>\
                                <div class=\"{}\">{}</div>\
                                <div class=\"session-menu\">...</div>\
                            </div>",
                            p.id, plat_label, display_name, sub, fav_class, fav_char
                        ));
                    }
                }
            }
        }

        if should_update {
            if let Ok(Some(mut list)) = root.find_first("#sessions-list") {
                let _ = list.set_html(items_html.as_bytes(), None);
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ctx-menu-flag") {
        let peer_id = el.get_text();
        if !peer_id.is_empty() {
            let _ = el.set_text("");

            if let Ok(Some(body)) = root.find_first("body") {
                let _ = body.eval_script(&format!("try {{ showCtxMenuById('{}'); }} catch(e) {{}}", peer_id));
            }
        }
    }
}
