
use crate::network;
use crate::server;
use crate::turn;
use crate::websocket::WsClient;
use crate::wininet;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_API_HOST: &str = "api.hoptodesk.com";

pub fn get_api_url() -> String {
    let custom = crate::config::Config2::load().get_option("custom-rendezvous-server");
    if !custom.is_empty() {
        return custom;
    }
    format!("https://{}/", DEFAULT_API_HOST)
}

pub fn api_get() -> Result<String, String> {
    let custom = crate::config::Config2::load().get_option("custom-rendezvous-server");
    if !custom.is_empty() {
        crate::config::write_log(&format!("[api] Using custom URL: {}", custom));
        return crate::wininet::http_get(&custom);
    }

    let https_url = format!("https://{}/", DEFAULT_API_HOST);
    crate::config::write_log(&format!("[api] Fetching {}", https_url));
    match crate::wininet::http_get(&https_url) {
        Ok(body) if !body.is_empty() => {
            crate::config::write_log(&format!("[api] HTTPS OK ({} bytes)", body.len()));
            Ok(body)
        }
        Ok(_) => {
            let msg = "HTTPS returned empty response";
            crate::config::write_log(&format!("[api] {}", msg));
            Err(msg.to_string())
        }
        Err(e) => {
            crate::config::write_log(&format!("[api] HTTPS failed: {}", e));
            Err(e)
        }
    }
}
static FORCE_API_REFRESH: AtomicBool = AtomicBool::new(false);
const API_CACHE_MAX_AGE_SECS: u64 = 6 * 3600;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_api_cache() -> Option<(String, u64)> {
    let cfg = crate::config::Config2::load();
    let b64 = cfg.get_option("api-cache");
    if b64.is_empty() {
        return None;
    }
    let bytes = crate::config::base64_decode(&b64)?;
    let body = String::from_utf8(bytes).ok()?;
    let time: u64 = cfg.get_option("api-cache-time").parse().unwrap_or(0);
    Some((body, time))
}

fn store_api_cache(body: &str) {
    let mut cfg = crate::config::Config2::load();
    cfg.set_option("api-cache", &crate::config::base64_encode(body.as_bytes()));
    cfg.set_option("api-cache-time", &now_unix().to_string());
    cfg.save();
}

fn api_body_is_valid(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|j| {
            j.get("rendezvous")
                .and_then(|r| r.get("host"))
                .and_then(|h| h.as_str())
                .map(|h| !h.is_empty())
        })
        .unwrap_or(false)
}

pub fn api_get_cached(force_refresh: bool) -> Result<String, String> {
    let custom = crate::config::Config2::load().get_option("custom-rendezvous-server");
    if !custom.is_empty() {
        return api_get();
    }
    let cached = load_api_cache();
    if !force_refresh {
        if let Some((ref body, time)) = cached {
            let now = now_unix();
            if time > 0 && time <= now && now - time < API_CACHE_MAX_AGE_SECS {
                return Ok(body.clone());
            }
        }
    }
    let fetched = api_get();
    if let Ok(ref body) = fetched {
        if api_body_is_valid(body) {
            store_api_cache(body);
            return Ok(body.clone());
        }
    }
    let reason = match &fetched {
        Ok(_) => "invalid API response".to_string(),
        Err(e) => e.clone(),
    };
    if let Some((body, _)) = cached {
        crate::config::write_log(&format!(
            "[api] Fetch unusable ({}), using cached response",
            reason
        ));
        return Ok(body);
    }
    fetched
}

const HEALTHCHECK: &str = r#"{"protocol":"one-to-self","data":"healthcheck"}"#;
const HEALTHCHECK_TIMEOUT: u64 = 90;
const SERVER_TIMEOUT: u64 = 30;
const RENDEZVOUS_TIMEOUT: u64 = 12;
const CONNECT_TIMEOUT_MS: u64 = 18_000;

pub struct SignalState {
    pub status: String,
    pub error: String,
    pub ws_host: String,
    pub ws_port: u16,
    pub incoming_session: Option<IncomingSession>,
}

pub struct IncomingSession {
    pub peer_id: String,
    pub accepted: bool,
}

impl Default for SignalState {
    fn default() -> Self {
        Self {
            status: "offline".to_string(),
            error: String::new(),
            ws_host: String::new(),
            ws_port: 0,
            incoming_session: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SignalMessage {
    pub protocol: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub sender_id: String,
    #[serde(default)]
    pub data: String,
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub public_addr: String,
    #[serde(default)]
    pub pk: String,
    #[serde(default)]
    pub nat_type: i32,
    #[serde(default)]
    pub lan_ipv4: String,
}

fn discover_signal_server() -> Result<(String, u16), String> {
    crate::config::write_log("[signal] Discovering signal server...");
    match api_get_cached(FORCE_API_REFRESH.swap(false, Ordering::SeqCst)) {
        Ok(body) => {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(obj) = json.get("rendezvous").and_then(|v| v.as_object()) {
                    let host = obj.get("host").and_then(|v| v.as_str()).unwrap_or("");
                    let port = obj
                        .get("port")
                        .and_then(|v| v.as_str())
                        .unwrap_or("21118");
                    if !host.is_empty() {
                        let port_num: u16 = port.parse().unwrap_or(21118);
                        crate::config::write_log(&format!("[signal] Discovered server: {}:{}", host, port_num));
                        return Ok((host.to_string(), port_num));
                    }
                }
            }
            Err("API returned no valid rendezvous server".into())
        }
        Err(e) => {
            Err(format!("API call failed: {}", e))
        }
    }
}

fn log_truncate(msg: &str) -> &str {
    match msg.char_indices().nth(200) {
        Some((i, _)) => &msg[..i],
        None => msg,
    }
}

pub fn run_signal_loop(
    my_id: String,
    password: String,
    pk: Vec<u8>,
    signal_state: Arc<Mutex<SignalState>>,
) {
    run_signal_loop_ex(my_id, password, pk, signal_state, false)
}

pub fn run_signal_loop_ex(
    my_id: String,
    password: String,
    pk: Vec<u8>,
    signal_state: Arc<Mutex<SignalState>>,
    yield_to_service: bool,
) {
    loop {
        if yield_to_service && crate::install::is_installed() {
            let st = crate::cm::read_service_status().unwrap_or_else(|| "offline".into());
            if let Ok(mut state) = signal_state.lock() {
                if state.status != st {
                    state.status = st;
                    state.error.clear();
                }
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }

        let cycle_start = Instant::now();

        if let Ok(mut state) = signal_state.lock() {
            state.status = "connecting".to_string();
            state.error.clear();
        }
        if !yield_to_service {
            crate::cm::write_service_status("connecting");
        }

        match run_signal_once(&my_id, &password, &pk, &signal_state, yield_to_service) {
            Ok(()) => {

            }
            Err(e) => {
                if let Ok(mut state) = signal_state.lock() {
                    state.status = "offline".to_string();
                    state.error = e.clone();
                }
                if !yield_to_service {
                    crate::cm::write_service_status("offline");
                }
                crate::config::write_log(&format!("[signal] Error: {}", e));
            }
        }

        let elapsed_ms = cycle_start.elapsed().as_millis() as u64;
        if elapsed_ms < CONNECT_TIMEOUT_MS {
            std::thread::sleep(Duration::from_millis(CONNECT_TIMEOUT_MS - elapsed_ms));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn run_signal_once(
    my_id: &str,
    password: &str,
    pk: &[u8],
    signal_state: &Arc<Mutex<SignalState>>,
    yield_to_service: bool,
) -> Result<(), String> {

    crate::config::write_log(&format!("[signal] Fetching API..."));
    let (host, port) = discover_signal_server()?;
    crate::config::write_log(&format!("[signal] Signal server: {}:{}", host, port));

    let path = format!("/?user={}", my_id);
    crate::config::write_log(&format!(
        "[signal] Connecting WebSocket to {}:{}{}...",
        host, port, path
    ));
    let mut ws = match WsClient::connect(&host, port, &path) {
        Ok(ws) => ws,
        Err(e) => {
            FORCE_API_REFRESH.store(true, Ordering::SeqCst);
            return Err(format!("WebSocket connect failed: {}", e));
        }
    };

    if let Ok(mut state) = signal_state.lock() {
        state.status = "online".to_string();
        state.ws_host = host.clone();
        state.ws_port = port;
        state.error.clear();
    }
    if !yield_to_service {
        crate::cm::write_service_status("online");
    }
    crate::config::write_log(&format!("[signal] Connected and online"));

    ws.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("set_read_timeout: {}", e))?;

    let mut last_data_received = Instant::now();
    let mut healthcheck_sent: Option<Instant> = None;

    loop {

        match ws.recv_text() {
            Ok(msg) => {
                last_data_received = Instant::now();
                healthcheck_sent = None;

                if msg != HEALTHCHECK {
                    crate::config::write_log(&format!("[signal] Recv: {}", log_truncate(&msg)));
                }

                if msg == HEALTHCHECK {

                } else if msg.contains("username is taken") {
                    crate::config::write_log(&format!("[signal] ID already connected, waiting for timeout..."));
                    return Err("username_taken".into());
                } else if msg.contains("You are removed") {
                    crate::config::write_log(&format!("[signal] Removed from server"));
                    return Err("removed".into());
                } else if let Ok(sm) = serde_json::from_str::<SignalMessage>(&msg) {
                    handle_signal_message(
                        &sm,
                        &msg,
                        my_id,
                        password,
                        pk,
                        &mut ws,
                        &host,
                        port,
                        signal_state,
                    );
                } else {
                    crate::config::write_log(&format!("[signal] Unknown message: {}", msg));
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                {
                    if yield_to_service && crate::install::is_installed() {
                        return Err("service installed, handing the connection to it".into());
                    }
                } else {

                    return Err(format!("WebSocket recv error: {}", e));
                }
            }
        }

        let now = Instant::now();

        if let Some(sent_at) = healthcheck_sent {
            if now.duration_since(sent_at).as_secs() > SERVER_TIMEOUT {
                return Err("Server unresponsive (healthcheck timeout)".into());
            }
        }

        if now.duration_since(last_data_received).as_secs() > HEALTHCHECK_TIMEOUT
            && healthcheck_sent.is_none()
        {
            crate::config::write_log(&format!("[signal] Sending healthcheck"));
            ws.send_text(HEALTHCHECK)
                .map_err(|e| format!("Healthcheck send failed: {}", e))?;
            healthcheck_sent = Some(Instant::now());
        }
    }
}

fn handle_signal_message(
    sm: &SignalMessage,
    raw_msg: &str,
    my_id: &str,
    password: &str,
    pk: &[u8],
    ws: &mut WsClient,
    ws_host: &str,
    ws_port: u16,
    signal_state: &Arc<Mutex<SignalState>>,
) {
    if sm.protocol == "one-to-one" {
        if !sm.sender_id.is_empty() && sm.addr.is_empty() && sm.pk.is_empty() {

            crate::config::write_log(&format!(
                "[signal] ConnectRequest from {}",
                sm.sender_id
            ));
            handle_connect_request(sm, my_id, password, pk, ws, ws_host, ws_port, signal_state);
        } else if !sm.addr.is_empty() && !sm.sender_id.is_empty() && sm.data == "closesessions" {

            crate::config::write_log(&format!("[signal] CloseSessions from {}", sm.sender_id));

            if let Ok(mut state) = signal_state.lock() {
                if let Some(ref session) = state.incoming_session {
                    if session.peer_id == sm.sender_id {
                        state.incoming_session = None;
                        crate::config::write_log(&format!("[signal] Cleared incoming session for {}", sm.sender_id));
                    }
                }
            }
        } else if !sm.addr.is_empty() && sm.sender_id.is_empty() && sm.pk.is_empty() {

            crate::config::write_log(&format!("[signal] RelayConnection: addr={} from {}", crate::config::mask_ip(&sm.addr), sm.endpoint));
            handle_relay_connection(&sm.addr, my_id, password, pk, &sm.endpoint, ws, ws_host, ws_port);
        } else if sm.addr.is_empty() && sm.pk.is_empty() && sm.sender_id.is_empty() {
            crate::config::write_log(&format!("[signal] Other message from {}: protocol={}", sm.endpoint, sm.protocol));
        }
    }
}

fn handle_connect_request(
    cr: &SignalMessage,
    my_id: &str,
    password: &str,
    pk: &[u8],
    ws: &mut WsClient,
    _ws_host: &str,
    _ws_port: u16,
    signal_state: &Arc<Mutex<SignalState>>,
) {
    let sender_id = cr.sender_id.clone();

    let cfg = crate::config::Config2::load();
    if cfg.get_option("stop-service") == "Y" {
        crate::config::write_log(&format!("[signal] Rejecting connection from {} — incoming connections disabled", sender_id));
        return;
    }

    if let Ok(mut state) = signal_state.lock() {
        state.incoming_session = Some(IncomingSession {
            peer_id: sender_id.clone(),
            accepted: false,
        });
    }

    let t0 = Instant::now();
    let local_ip = network::get_local_ip();
    let listener = match network::new_listener(&local_ip, 0) {
        Ok(l) => l,
        Err(e) => {
            crate::config::write_log(&format!("[signal] Bind listener failed: {}", e));
            return;
        }
    };
    let listen_addr = match listener.local_addr() {
        Ok(a) => a,
        Err(e) => {
            crate::config::write_log(&format!("[signal] Get local addr: {}", e));
            return;
        }
    };

    crate::config::write_log(&format!(
        "[signal] Bound listener at {} for peer {} ({}ms)",
        crate::config::mask_ip(&listen_addr), sender_id, t0.elapsed().as_millis()
    ));

    let t1 = Instant::now();
    let public_addr = turn::get_public_ip()
        .map(|mut a| { a.set_port(listen_addr.port()); a.to_string() })
        .unwrap_or_else(|| listen_addr.to_string());
    let lan_addr = format!("{}:{}", local_ip, listen_addr.port());

    crate::config::write_log(&format!("[signal] Public addr: {}, LAN addr: {} (get_public_ip took {}ms)",
        crate::config::mask_ip(&public_addr), crate::config::mask_ip(&lan_addr), t1.elapsed().as_millis()));

    let pk_b64 = crate::config::base64_encode(pk);
    let listening = serde_json::json!({
        "protocol": "one-to-one",
        "endpoint": sender_id,
        "addr": listen_addr.to_string(),
        "public_addr": public_addr,
        "pk": pk_b64,
        "nat_type": 0,
        "lan_ipv4": lan_addr
    });

    let listening_json = listening.to_string();
    crate::config::write_log(&format!("[signal] Sending Listening ({}ms since ConnectRequest): {}",
        t0.elapsed().as_millis(), &listening_json[..listening_json.len().min(300)]));
    if let Err(e) = ws.send_text(&listening_json) {
        crate::config::write_log(&format!("[signal] Send Listening failed: {}", e));
        return;
    }
    crate::config::write_log(&format!("[signal] Sent Listening to {} (total {}ms)", sender_id, t0.elapsed().as_millis()));

    let my_id = my_id.to_string();
    let password = password.to_string();
    let pk = pk.to_vec();
    let signal_state = signal_state.clone();

    std::thread::spawn(move || {
        let stop = Arc::new(AtomicBool::new(false));
        server::accept_connection(listener, &my_id, &password, &pk, stop);

        if let Ok(mut state) = signal_state.lock() {
            state.incoming_session = None;
        }
    });
}

fn handle_relay_connection(
    relay_addr: &str,
    my_id: &str,
    password: &str,
    pk: &[u8],
    peer_id: &str,
    ws: &mut WsClient,
    _ws_host: &str,
    _ws_port: u16,
) {
    crate::config::write_log(&format!("[relay] Connecting to relay at {}...", crate::config::mask_ip(&relay_addr)));

    let (relay_host, relay_port) = match relay_addr.rfind(':') {
        Some(pos) => (&relay_addr[..pos], relay_addr[pos + 1..].parse::<u16>().unwrap_or(21118)),
        None => {
            crate::config::write_log(&format!("[relay] Invalid relay address {}", crate::config::mask_ip(&relay_addr)));
            return;
        }
    };

    let tcp_stream = match crate::tls_client::connect_tcp_timeout(relay_host, relay_port, Duration::from_secs(10)) {
        Ok(s) => s,
        Err(e) => {
            crate::config::write_log(&format!("[relay] Failed to connect to relay {}: {}", crate::config::mask_ip(&relay_addr), e));
            return;
        }
    };
    crate::config::write_log(&format!("[relay] Connected to relay {}", crate::config::mask_ip(&relay_addr)));
    tcp_stream.set_nodelay(true).ok();

    ws.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let got_ready = loop {
        match ws.recv_text() {
            Ok(msg) => {
                crate::config::write_log(&format!("[relay] WS recv while waiting for RelayReady: {}", log_truncate(&msg)));

                if let Ok(sm) = serde_json::from_str::<SignalMessage>(&msg) {
                    if sm.protocol == "one-to-one" && sm.addr.is_empty() && sm.pk.is_empty() && sm.sender_id.is_empty() {
                        crate::config::write_log(&format!("[relay] Got RelayReady from {}", sm.endpoint));
                        break true;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                crate::config::write_log(&format!("[relay] Timeout waiting for RelayReady"));
                break false;
            }
            Err(e) => {
                crate::config::write_log(&format!("[relay] WS error waiting for RelayReady: {}", e));
                break false;
            }
        }
    };

    ws.set_read_timeout(Some(Duration::from_secs(5))).ok();

    if !got_ready {
        crate::config::write_log(&format!("[relay] Did not receive RelayReady, aborting relay"));
        return;
    }

    let my_id = my_id.to_string();
    let password = password.to_string();
    let pk = pk.to_vec();

    std::thread::spawn(move || {
        let mut stream = network::FramedStream::from_tcp(tcp_stream);
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(10))).ok();

        let stop = Arc::new(AtomicBool::new(false));
        if let Err(e) = server::run_session_public(&mut stream, &my_id, &password, &pk, &stop) {
            crate::config::write_log(&format!("[relay] Session error: {}", e));
        }
        crate::config::write_log(&format!("[relay] Session ended"));
    });
}

pub fn send_connect_request(
    my_id: &str,
    target_id: &str,
    signal_state: &Arc<Mutex<SignalState>>,
) -> Result<PeerListening, String> {

    let (ws_host, ws_port) = {
        let state = signal_state.lock().map_err(|e| e.to_string())?;
        if state.status == "online" && !state.ws_host.is_empty() {
            (state.ws_host.clone(), state.ws_port)
        } else {

            crate::config::write_log(&format!("[signal] Signal state not online, discovering server..."));
            discover_signal_server()?
        }
    };

    if let Ok(mut state) = signal_state.lock() {
        state.ws_host = ws_host.clone();
        state.ws_port = ws_port;
    }

    let temp_id = format!("{}_{}", my_id, std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis());
    let path = format!("/?user={}", temp_id);

    let mut ws = WsClient::connect(&ws_host, ws_port, &path)
        .map_err(|e| format!("WS connect failed: {}", e))?;

    let req = serde_json::json!({
        "protocol": "one-to-one",
        "endpoint": target_id,
        "sender_id": temp_id,
    });

    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }

        ws.send_text(&req.to_string())
            .map_err(|e| format!("Send ConnectRequest failed: {}", e))?;
        crate::config::write_log(&format!("[signal] Sent ConnectRequest to {} (attempt {})", target_id, attempt + 1));

        ws.set_read_timeout(Some(Duration::from_secs(RENDEZVOUS_TIMEOUT)))
            .map_err(|e| format!("set timeout: {}", e))?;

        let deadline = Instant::now() + Duration::from_secs(RENDEZVOUS_TIMEOUT);
        while Instant::now() < deadline {
            match ws.recv_text() {
                Ok(msg) => {
                    if let Ok(sm) = serde_json::from_str::<SignalMessage>(&msg) {

                        if !sm.addr.is_empty() {
                            crate::config::write_log(&format!(
                                "[signal] Got Listening: addr={} public_addr={} pk={}",
                                crate::config::mask_ip(&sm.addr), crate::config::mask_ip(&sm.public_addr), sm.pk
                            ));
                            return Ok(PeerListening {
                                addr: sm.addr,
                                public_addr: sm.public_addr,
                                pk: sm.pk,
                                nat_type: sm.nat_type,
                                lan_ipv4: sm.lan_ipv4,
                            });
                        }
                    }

                    if msg.contains("Could not find") || msg.contains("Not found") {
                        crate::config::write_log(&format!("[signal] Peer {} not found", target_id));
                        if attempt < 2 {
                            break;
                        }
                        return Err(format!("Peer {} is offline", target_id));
                    }
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(e) => {
                    return Err(format!("WS recv error: {}", e));
                }
            }
        }
    }

    Err(format!("Timeout waiting for {} to respond", target_id))
}

pub struct PeerListening {
    pub addr: String,
    pub public_addr: String,
    pub pk: String,
    pub nat_type: i32,
    pub lan_ipv4: String,
}
