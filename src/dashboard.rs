
use crate::config::{self, Config2};
use crate::crypto::Sha256;
use crate::websocket::WssClient;
use crate::wininet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DASHBOARD_API_URL: &str = "https://dashboard.hoptodesk.com/api";
const DASHBOARD_WS_HOST: &str = "dashboard.hoptodesk.com";
const HEARTBEAT_INTERVAL_SECS: u64 = 30;
const RECONNECT_DELAY_BASE_SECS: u64 = 1;
const RECONNECT_DELAY_MAX_SECS: u64 = 10;
const WS_READ_TIMEOUT_SECS: u64 = 90;

static DASHBOARD_RUNNING: AtomicBool = AtomicBool::new(false);
static IN_SESSION: AtomicBool = AtomicBool::new(false);
static SESSION_META: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));
static TICKET_REPLY_COUNTER: AtomicU64 = AtomicU64::new(0);
static LINKING_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

pub fn is_linked() -> bool {
    let cfg = Config2::load();
    !cfg.get_option("dashboard_user_id").is_empty()
}

pub fn get_invite_code() -> String {
    let cfg = Config2::load();
    let code = cfg.get_option("invite_code");
    if !code.is_empty() {
        return code;
    }
    let path = config::config_dir().join("InviteCode.toml");
    if let Ok(content) = std::fs::read_to_string(&path) {
        let trimmed = content.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    String::new()
}

pub fn get_dashboard_user_id() -> String {
    let cfg = Config2::load();
    cfg.get_option("dashboard_user_id")
}

pub fn validate_invite(invite_code: &str) -> Result<(String, String, String), String> {
    let url = format!(
        "{}?action=validateInvite&invite_code={}",
        DASHBOARD_API_URL, invite_code
    );
    let body = wininet::http_get(&url)?;
    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {}", e))?;
    if resp["success"].as_bool() != Some(true) {
        return Err(format!("validateInvite failed: {}", resp));
    }
    let invite = &resp["invite"];
    let enrollment_token = invite["enrollment_token"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let dashboard_user_id = invite["dashboard_user_id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let invite_type = invite["invite_type"]
        .as_str()
        .unwrap_or("standard")
        .to_string();
    if dashboard_user_id.is_empty() || dashboard_user_id.starts_with("DASH-") {
        return Err(format!("Invalid dashboard_user_id: {}", dashboard_user_id));
    }
    Ok((enrollment_token, dashboard_user_id, invite_type))
}

pub fn register_device(
    enrollment_token: &str,
    invite_code: &str,
    device_id: &str,
    device_name: &str,
    os_name: &str,
    mac: &str,
) -> Result<String, String> {
    let url = format!("{}?action=registerDevice", DASHBOARD_API_URL);
    let mut params: Vec<(&str, &str)> = vec![
        ("device_id", device_id),
        ("device_name", device_name),
        ("computer_name", device_name),
        ("os", os_name),
        ("mac_address", mac),
    ];
    if !enrollment_token.is_empty() {
        params.push(("enrollment_token", enrollment_token));
    }
    if !invite_code.is_empty() {
        params.push(("invite_code", invite_code));
    }
    let body = wininet::http_post_form(&url, &params)?;
    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {}", e))?;
    if resp["success"].as_bool() != Some(true) {
        return Err(format!("registerDevice failed: {}", resp));
    }
    let dashboard_user_id = resp["dashboard_user_id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    config::write_log(&format!(
        "[dashboard] Device registered (user_id={})",
        dashboard_user_id
    ));
    Ok(dashboard_user_id)
}

pub fn get_network_settings(invite_code: &str) -> Result<(), String> {
    let url = format!(
        "{}?action=getNetworkSettingsByInvite&invite_code={}",
        DASHBOARD_API_URL, invite_code
    );
    let body = wininet::http_get(&url)?;
    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {}", e))?;
    if resp["success"].as_bool() != Some(true) {
        config::write_log("[dashboard] getNetworkSettingsByInvite: not successful");
        return Ok(());
    }
    let network_type = resp["network_type"].as_str().unwrap_or("hoptodesk");
    if network_type == "custom" {
        let api_json = serde_json::json!({
            "turnservers": [{
                "protocol": "turn",
                "host": resp["turn_host"].as_str().unwrap_or(""),
                "port": resp["turn_port"].as_str().unwrap_or(""),
                "username": resp["turn_username"].as_str().unwrap_or(""),
                "password": resp["turn_password"].as_str().unwrap_or("")
            }],
            "rendezvous": {
                "host": resp["rendezvous_host"].as_str().unwrap_or(""),
                "port": resp["rendezvous_port"].as_str().unwrap_or("")
            },
            "none": "none"
        });
        let api_json_path = config::config_dir().join("api.json");
        std::fs::write(
            &api_json_path,
            serde_json::to_string_pretty(&api_json).unwrap_or_default(),
        )
        .map_err(|e| format!("Failed to write api.json: {}", e))?;
        config::write_log(&format!(
            "[dashboard] Wrote custom network config to {:?}",
            api_json_path
        ));
    }
    Ok(())
}

pub fn link_device() -> Result<(), String> {
    if LINKING_IN_PROGRESS.swap(true, Ordering::SeqCst) {
        return Err("Linking already in progress".to_string());
    }
    let result = link_device_inner();
    LINKING_IN_PROGRESS.store(false, Ordering::SeqCst);
    result
}

fn link_device_inner() -> Result<(), String> {
    let invite_code = get_invite_code();
    if invite_code.is_empty() {
        return Err("No invite code set".to_string());
    }

    config::write_log(&format!(
        "[dashboard] Linking device with invite code: {}...",
        &invite_code[..invite_code.len().min(8)]
    ));

    let (enrollment_token, dashboard_user_id, _invite_type) = validate_invite(&invite_code)?;

    let cfg = config::Config::load();
    let device_id = cfg.id.clone();
    if device_id.is_empty() {
        return Err("Device ID not available".to_string());
    }

    let device_name = hostname();
    let os_name = "windows";
    let mac = get_mac_address();

    let resolved_user_id = register_device(
        &enrollment_token,
        &invite_code,
        &device_id,
        &device_name,
        os_name,
        &mac,
    )?;

    let final_user_id = if !resolved_user_id.is_empty() {
        resolved_user_id
    } else {
        dashboard_user_id
    };

    if !final_user_id.is_empty() {
        let mut cfg2 = Config2::load();
        cfg2.set_option("dashboard_user_id", &final_user_id);
        cfg2.set_option("dashboard_device_id", &device_id);
        cfg2.save();
        config::write_log(&format!(
            "[dashboard] Dashboard user ID stored: {}",
            final_user_id
        ));
    }

    if let Err(e) = get_network_settings(&invite_code) {
        config::write_log(&format!(
            "[dashboard] Failed to get network settings: {}",
            e
        ));
    }

    // Clear invite code after successful linking
    let mut cfg2 = Config2::load();
    cfg2.set_option("invite_code", "");
    cfg2.save();
    let _ = std::fs::remove_file(config::config_dir().join("InviteCode.toml"));

    Ok(())
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "HopToDesk-XP".to_string())
}

fn get_mac_address() -> String {
    #[repr(C)]
    struct IpAdapterInfo {
        next: *mut IpAdapterInfo,
        combo_index: u32,
        adapter_name: [u8; 260],
        description: [u8; 132],
        address_length: u32,
        address: [u8; 8],
        index: u32,
        _type: u32,
        dhcp_enabled: u32,
        current_ip: [u8; 60],
        gateway: [u8; 60],
        dhcp_server: [u8; 60],
        have_wins: i32,
        primary_wins: [u8; 60],
        secondary_wins: [u8; 60],
        lease_obtained: u32,
        lease_expires: u32,
    }

    #[link(name = "iphlpapi")]
    extern "system" {
        fn GetAdaptersInfo(info: *mut IpAdapterInfo, size: *mut u32) -> u32;
    }

    unsafe {
        let mut size: u32 = 0;
        GetAdaptersInfo(ptr::null_mut(), &mut size);
        if size == 0 {
            return String::new();
        }
        let mut buffer = vec![0u8; size as usize];
        let info = buffer.as_mut_ptr() as *mut IpAdapterInfo;
        if GetAdaptersInfo(info, &mut size) != 0 {
            return String::new();
        }
        let adapter = &*info;
        let len = adapter.address_length as usize;
        if len == 0 || len > 8 {
            return String::new();
        }
        adapter.address[..len]
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(":")
    }
}

use std::ptr;

fn get_timezone() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        return tz;
    }
    // On XP, fall back to UTC offset via Win32 GetTimeZoneInformation
    #[repr(C)]
    struct TimeZoneInformation {
        bias: i32,
        _standard_name: [u16; 32],
        _standard_date: [u16; 8],
        _standard_bias: i32,
        _daylight_name: [u16; 32],
        _daylight_date: [u16; 8],
        _daylight_bias: i32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetTimeZoneInformation(tz: *mut TimeZoneInformation) -> u32;
    }
    unsafe {
        let mut tz: TimeZoneInformation = std::mem::zeroed();
        GetTimeZoneInformation(&mut tz);
        let offset_minutes = -tz.bias;
        let hours = offset_minutes / 60;
        let mins = (offset_minutes % 60).abs();
        if mins == 0 {
            format!("UTC{:+}", hours)
        } else {
            format!("UTC{:+}:{:02}", hours, mins)
        }
    }
}

fn send_socketio_event(ws: &mut WssClient, event: &str, data: &serde_json::Value) -> Result<(), String> {
    let payload = format!("42/device,[{},{}]", serde_json::json!(event), data);
    ws.send_text(&payload).map_err(|e| format!("WS send failed: {}", e))
}

fn handle_incoming_message(text: &str) -> Result<Option<(String, serde_json::Value)>, String> {
    if let Some(json_str) = text.strip_prefix("42/device,") {
        if let Ok(arr) = serde_json::from_str::<serde_json::Value>(json_str) {
            if let Some(event) = arr.get(0).and_then(|v| v.as_str()) {
                match event {
                    "registered" => {
                        if let Some(data) = arr.get(1) {
                            if let Some(uid) = data["dashboard_user_id"].as_str() {
                                if !uid.is_empty() && get_dashboard_user_id().is_empty() {
                                    let mut cfg2 = Config2::load();
                                    cfg2.set_option("dashboard_user_id", uid);
                                    cfg2.save();
                                    config::write_log("[dashboard] Stored dashboard_user_id from WS register ACK");
                                }
                            }
                        }
                    }
                    "heartbeat_ack" => {}
                    "unlinked" => {
                        config::write_log("[dashboard] Device has been permanently deleted, unlinking");
                        let mut cfg2 = Config2::load();
                        cfg2.set_option("dashboard_user_id", "");
                        cfg2.set_option("invite_code", "");
                        cfg2.save();
                        return Err("Device unlinked from dashboard".to_string());
                    }
                    "ticket:reply" => {
                        if arr.get(1).is_some() {
                            config::write_log("[dashboard] Ticket reply notification received");
                            TICKET_REPLY_COUNTER.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    "wol:send" => {
                        if let Some(data) = arr.get(1) {
                            if let Some(target_mac) = data["target_mac"].as_str() {
                                config::write_log(&format!("[dashboard] WoL request for MAC {}", target_mac));
                                send_wol_packet(target_mac);
                            }
                        }
                    }
                    "mcp:request" => {
                        if let Some(data) = arr.get(1) {
                            let request_id = data["request_id"].as_str().unwrap_or("").to_string();
                            let payload = &data["payload"];
                            let payload_str = payload.to_string();
                            config::write_log(&format!("[dashboard] MCP request (id={})", request_id));
                            let mcp_resp = crate::mcp_server::handle_mcp_request(&payload_str)
                                .unwrap_or_else(|| r#"{"error":"no response"}"#.to_string());
                            let resp_val: serde_json::Value = serde_json::from_str(&mcp_resp).unwrap_or_default();
                            return Ok(Some(("mcp:response".to_string(), serde_json::json!({
                                "request_id": request_id,
                                "response": resp_val
                            }))));
                        }
                    }
                    _ => {
                        config::write_log(&format!("[dashboard] Unknown event '{}': {}", event, json_str));
                    }
                }
            }
        }
    }
    Ok(None)
}

fn dashboard_ws_loop(dashboard_user_id: &str) -> Result<(), String> {
    let path = format!(
        "/socket.io/?dashboard_user_id={}&EIO=4&transport=websocket",
        dashboard_user_id
    );
    config::write_log("[dashboard] Connecting to dashboard WebSocket");

    let mut ws = WssClient::connect(DASHBOARD_WS_HOST, 443, &path)
        .map_err(|e| format!("WSS connect failed: {}", e))?;

    // Read Engine.IO open packet
    let open_msg = ws.recv_text().map_err(|e| format!("WS read open: {}", e))?;
    if !open_msg.starts_with('0') {
        return Err(format!("Expected Engine.IO open packet, got: {}", open_msg));
    }

    // Join /device namespace
    ws.send_text("40/device,").map_err(|e| format!("WS send ns join: {}", e))?;

    let ack_msg = ws.recv_text().map_err(|e| format!("WS read ns ack: {}", e))?;
    if !ack_msg.starts_with("40/device") {
        return Err(format!("Expected namespace ACK, got: {}", ack_msg));
    }

    // Register device
    let cfg = config::Config::load();
    let device_id = cfg.id.clone();
    let computer_name = hostname();
    let timezone = get_timezone();
    let mac = get_mac_address();

    let wol_enabled = is_wol_enabled();
    let register_data = serde_json::json!({
        "device_id": device_id,
        "dashboard_user_id": dashboard_user_id,
        "timezone": timezone,
        "computer_name": computer_name,
        "os": "windows",
        "mac_address": mac,
        "wol_enabled": wol_enabled
    });

    std::thread::sleep(Duration::from_millis(500));
    send_socketio_event(&mut ws, "register", &register_data)?;

    config::write_log("[dashboard] WebSocket connected and registered");

    // Set read timeout for the main loop
    ws.set_read_timeout(Some(Duration::from_secs(HEARTBEAT_INTERVAL_SECS + 5)))
        .map_err(|e| format!("set_read_timeout: {}", e))?;

    let mut last_heartbeat = Instant::now();
    let mut last_ws_data = Instant::now();
    let mut was_in_session = false;

    loop {
        // Try to read a message (will timeout after HEARTBEAT_INTERVAL + 5s)
        match ws.recv_text() {
            Ok(text) => {
                last_ws_data = Instant::now();

                // Engine.IO ping
                if text == "2" {
                    ws.send_text("3").map_err(|e| format!("WS pong failed: {}", e))?;
                    continue;
                }
                // Engine.IO pong
                if text == "3" {
                    continue;
                }

                if let Some((resp_event, resp_data)) = handle_incoming_message(&text)? {
                    send_socketio_event(&mut ws, &resp_event, &resp_data)?;
                }
            }
            Err(e) => {
                let kind = e.kind();
                if kind == std::io::ErrorKind::WouldBlock || kind == std::io::ErrorKind::TimedOut {
                    // Timeout — check if we've gone too long without any data
                    if last_ws_data.elapsed() > Duration::from_secs(WS_READ_TIMEOUT_SECS) {
                        config::write_log(&format!(
                            "[dashboard] No data for {}s, reconnecting",
                            WS_READ_TIMEOUT_SECS
                        ));
                        break;
                    }
                } else {
                    return Err(format!("WS read error: {}", e));
                }
            }
        }

        // Send heartbeat if interval has elapsed
        if last_heartbeat.elapsed() >= Duration::from_secs(HEARTBEAT_INTERVAL_SECS) {
            let current_in_session = IN_SESSION.load(Ordering::Relaxed);

            // Session start/end events
            if current_in_session && !was_in_session {
                let (stype, rip) = SESSION_META.lock().unwrap().clone();
                let session_start = serde_json::json!({
                    "device_id": device_id,
                    "session_type": if stype.is_empty() { "screen".to_string() } else { stype },
                    "remote_ip": rip,
                    "timestamp": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                });
                send_socketio_event(&mut ws, "remote_session_start", &session_start)?;
            } else if !current_in_session && was_in_session {
                let (stype, rip) = SESSION_META.lock().unwrap().clone();
                let session_end = serde_json::json!({
                    "device_id": device_id,
                    "session_type": if stype.is_empty() { "screen".to_string() } else { stype },
                    "remote_ip": rip,
                    "timestamp": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                });
                send_socketio_event(&mut ws, "remote_session_end", &session_end)?;
            }
            was_in_session = current_in_session;

            let wol_now = is_wol_enabled();
            let heartbeat = serde_json::json!({
                "device_id": device_id,
                "timezone": timezone,
                "in_session": current_in_session,
                "wol_enabled": wol_now
            });
            send_socketio_event(&mut ws, "heartbeat", &heartbeat)?;
            last_heartbeat = Instant::now();
        }
    }

    Ok(())
}

/// Start the dashboard background thread.
/// Called from main.rs after UI is up.
pub fn start() {
    if DASHBOARD_RUNNING.swap(true, Ordering::SeqCst) {
        config::write_log("[dashboard] Already running");
        return;
    }

    // If there's an invite code, link now
    if !get_invite_code().is_empty() {
        match link_device() {
            Ok(()) => config::write_log("[dashboard] Device linked successfully"),
            Err(e) => {
                config::write_log(&format!("[dashboard] Failed to link device: {}", e));
                DASHBOARD_RUNNING.store(false, Ordering::SeqCst);
                return;
            }
        }
    }

    let dashboard_user_id = get_dashboard_user_id();
    if dashboard_user_id.is_empty() {
        config::write_log("[dashboard] No dashboard_user_id, not starting WebSocket");
        // Still check for new invite codes periodically
        loop {
            std::thread::sleep(Duration::from_secs(10));
            let code = get_invite_code();
            if !code.is_empty() && !is_linked() {
                match link_device() {
                    Ok(()) => {
                        config::write_log("[dashboard] Device linked via UI code");
                        break; // Fall through to WebSocket loop
                    }
                    Err(e) => config::write_log(&format!(
                        "[dashboard] Failed to link device via UI code: {}",
                        e
                    )),
                }
            }
        }
    }

    let dashboard_user_id = get_dashboard_user_id();
    if dashboard_user_id.is_empty() {
        DASHBOARD_RUNNING.store(false, Ordering::SeqCst);
        return;
    }

    config::write_log("[dashboard] Starting dashboard WebSocket connection");
    let mut reconnect_delay = RECONNECT_DELAY_BASE_SECS;

    loop {
        match dashboard_ws_loop(&dashboard_user_id) {
            Ok(()) => {
                config::write_log("[dashboard] WebSocket loop ended normally");
                reconnect_delay = RECONNECT_DELAY_BASE_SECS;
            }
            Err(e) => {
                config::write_log(&format!("[dashboard] WebSocket error: {}", e));
            }
        }

        if get_dashboard_user_id().is_empty() {
            config::write_log("[dashboard] Device unlinked, stopping reconnection");
            break;
        }

        config::write_log(&format!(
            "[dashboard] Reconnecting in {}s...",
            reconnect_delay
        ));
        std::thread::sleep(Duration::from_secs(reconnect_delay));
        reconnect_delay = (reconnect_delay * 2).min(RECONNECT_DELAY_MAX_SECS);

        // Check for new invite code during reconnect
        let code = get_invite_code();
        if !code.is_empty() && !is_linked() {
            if let Ok(()) = link_device() {
                config::write_log("[dashboard] Device re-linked during reconnect");
            }
        }
    }
}

const ATTACHMENT_MAGIC: &[u8; 4] = b"HTDE";

pub fn encrypt_attachment(data: &[u8], dashboard_user_id: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(dashboard_user_id.as_bytes());
    let key = hasher.finalize();
    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(ATTACHMENT_MAGIC);
    for (i, &b) in data.iter().enumerate() {
        out.push(b ^ key[i % 32]);
    }
    out
}

pub fn decrypt_attachment(data: &[u8], dashboard_user_id: &str) -> Vec<u8> {
    if data.len() < 4 || &data[..4] != ATTACHMENT_MAGIC {
        return data.to_vec();
    }
    let mut hasher = Sha256::new();
    hasher.update(dashboard_user_id.as_bytes());
    let key = hasher.finalize();
    let encoded = &data[4..];
    encoded
        .iter()
        .enumerate()
        .map(|(i, &b)| b ^ key[i % 32])
        .collect()
}

pub fn percent_decode_path(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                result.push(byte);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).to_string()
}

// --- Ticket functions ---

pub fn submit_ticket(
    email: &str,
    subject: &str,
    description: &str,
    priority: &str,
) -> Result<i64, String> {
    let cfg = config::Config::load();
    let device_id = cfg.id.clone();
    let dashboard_user_id = get_dashboard_user_id();
    let device_name = hostname();
    let user_name = std::env::var("USERNAME").unwrap_or_default();

    let url = format!("{}?action=submitTicket", DASHBOARD_API_URL);
    let body = wininet::http_post_form(
        &url,
        &[
            ("device_id", &device_id),
            ("dashboard_user_id", &dashboard_user_id),
            ("device_name", &device_name),
            ("user_name", &user_name),
            ("user_email", email),
            ("subject", subject),
            ("description", description),
            ("priority", priority),
        ],
    )?;
    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {}", e))?;
    if resp["success"].as_bool() != Some(true) {
        return Err(format!(
            "submitTicket failed: {}",
            resp["error"].as_str().unwrap_or("unknown error")
        ));
    }
    Ok(resp["ticket_id"].as_i64().unwrap_or(0))
}

pub fn get_my_tickets() -> Result<serde_json::Value, String> {
    let cfg = config::Config::load();
    let device_id = cfg.id.clone();
    let dashboard_user_id = get_dashboard_user_id();
    let url = format!(
        "{}?action=getMyTickets&device_id={}&dashboard_user_id={}",
        DASHBOARD_API_URL, device_id, dashboard_user_id
    );
    let body = wininet::http_get(&url)?;
    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {}", e))?;
    if resp["success"].as_bool() != Some(true) {
        return Err(format!(
            "getMyTickets failed: {}",
            resp["error"].as_str().unwrap_or("unknown error")
        ));
    }
    Ok(resp["tickets"].clone())
}

pub fn get_conversation(ticket_id: i64) -> Result<serde_json::Value, String> {
    let cfg = config::Config::load();
    let device_id = cfg.id.clone();
    let url = format!("{}?action=getCustomerConversation", DASHBOARD_API_URL);
    let body = wininet::http_post_form(
        &url,
        &[
            ("ticket_id", &ticket_id.to_string()),
            ("device_id", &device_id),
        ],
    )?;
    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {}", e))?;
    if resp["success"].as_bool() != Some(true) {
        return Err(format!(
            "getCustomerConversation failed: {}",
            resp["error"].as_str().unwrap_or("unknown error")
        ));
    }
    Ok(resp["messages"].clone())
}

pub fn add_reply(ticket_id: i64, message: &str) -> Result<(), String> {
    let cfg = config::Config::load();
    let device_id = cfg.id.clone();
    let url = format!("{}?action=addCustomerReply", DASHBOARD_API_URL);
    let body = wininet::http_post_form(
        &url,
        &[
            ("ticket_id", &ticket_id.to_string()),
            ("device_id", &device_id),
            ("message", message),
        ],
    )?;
    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {}", e))?;
    if resp["success"].as_bool() != Some(true) {
        return Err(format!(
            "addCustomerReply failed: {}",
            resp["error"].as_str().unwrap_or("unknown error")
        ));
    }
    Ok(())
}

pub fn get_attachments(ticket_id: i64) -> Result<serde_json::Value, String> {
    let cfg = config::Config::load();
    let device_id = cfg.id.clone();
    let url = format!(
        "{}?action=getAttachments&ticket_id={}&device_id={}",
        DASHBOARD_API_URL, ticket_id, device_id
    );
    let body = wininet::http_get(&url)?;
    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {}", e))?;
    if resp["success"].as_bool() != Some(true) {
        return Err(format!(
            "getAttachments failed: {}",
            resp["error"].as_str().unwrap_or("unknown error")
        ));
    }
    Ok(resp["attachments"].clone())
}

pub fn upload_attachment(ticket_id: i64, file_path: &str) -> Result<(), String> {
    let cfg = config::Config::load();
    let device_id = cfg.id.clone();
    let dashboard_user_id = get_dashboard_user_id();
    let file_path = percent_decode_path(file_path);
    let path = std::path::Path::new(&file_path);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let file_content =
        std::fs::read(&file_path).map_err(|e| format!("Cannot read file '{}': {}", file_path, e))?;

    let encrypted = if !dashboard_user_id.is_empty() {
        encrypt_attachment(&file_content, &dashboard_user_id)
    } else {
        file_content
    };

    let url = format!("{}?action=customerUploadAttachment", DASHBOARD_API_URL);
    let body = wininet::http_post_multipart(
        &url,
        &[
            ("action", "customerUploadAttachment"),
            ("ticket_id", &ticket_id.to_string()),
            ("device_id", &device_id),
            ("customer_name", "Customer"),
        ],
        "file",
        &file_name,
        &encrypted,
    )?;
    let resp: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    if resp["success"].as_bool() == Some(false) {
        return Err(
            resp["message"]
                .as_str()
                .unwrap_or("upload failed")
                .to_string(),
        );
    }
    config::write_log(&format!(
        "[dashboard] Attachment uploaded: {}",
        file_name
    ));
    Ok(())
}

pub fn get_ticket_reply_counter() -> u64 {
    TICKET_REPLY_COUNTER.load(Ordering::Relaxed)
}

pub fn set_in_session(active: bool, session_type: &str, remote_ip: &str) {
    if active {
        if let Ok(mut meta) = SESSION_META.lock() {
            *meta = (session_type.to_string(), remote_ip.to_string());
        }
    }
    IN_SESSION.store(active, Ordering::Relaxed);
}

pub fn get_attachment_download_url(download_url: &str) -> String {
    if download_url.starts_with("http") {
        download_url.to_string()
    } else {
        format!("{}/{}", DASHBOARD_API_URL, download_url)
    }
}

pub fn send_wol_packet(mac_str: &str) {
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

fn is_wol_enabled() -> bool {
    let cfg = Config2::load();
    cfg.get_option("enable-wol") == "Y"
}
