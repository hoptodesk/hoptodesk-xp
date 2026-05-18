
use crate::network::FramedStream;
use crate::protocol::message_proto;
use protobuf::Message;
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct ClientState {
    pub status: String,
    pub error: String,
    pub peer_name: String,
    pub peer_platform: String,
    pub displays: Vec<DisplayMeta>,

    pub frame_width: i32,
    pub frame_height: i32,
    pub frame_data: Vec<u8>,
    pub frame_seq: u64,

    pub input_stream: Option<Arc<Mutex<crate::network::FramedStream>>>,

    pub file_transfer_mode: bool,
    pub file_responses: Arc<Mutex<VecDeque<message_proto::FileResponse>>>,
    pub file_actions_out: Arc<Mutex<VecDeque<message_proto::FileAction>>>,

    pub chat_messages: Arc<Mutex<VecDeque<(String, String)>>>,

    pub recording: bool,
    pub recorder: Option<crate::recording::Recorder>,

    pub tcp_connected: bool,
}

pub struct DisplayMeta {
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
    pub name: String,
}

impl Default for ClientState {
    fn default() -> Self {
        Self {
            status: "connecting".into(),
            error: String::new(),
            peer_name: String::new(),
            peer_platform: String::new(),
            displays: Vec::new(),
            frame_width: 0,
            frame_height: 0,
            frame_data: Vec::new(),
            frame_seq: 0,
            input_stream: None,
            file_transfer_mode: false,
            file_responses: Arc::new(Mutex::new(VecDeque::new())),
            file_actions_out: Arc::new(Mutex::new(VecDeque::new())),
            chat_messages: Arc::new(Mutex::new(VecDeque::new())),
            recording: false,
            recorder: None,
            tcp_connected: false,
        }
    }
}

pub fn connect_to_peer(
    addr: &str,
    my_id: &str,
    target_id: &str,
    password: &str,
    client_state: Arc<Mutex<ClientState>>,
    stop: Arc<AtomicBool>,
) {
    crate::config::write_log(&format!("[client] Connecting to {}...", crate::config::mask_ip(&addr)));

    let result = run_client(addr, my_id, target_id, password, &client_state, &stop, false);

    if let Err(e) = result {
        crate::config::write_log(&format!("[client] Error: {}", e));
        if let Ok(mut state) = client_state.lock() {
            state.status = "error".into();
            state.error = e.to_string();
        }
    }

    if let Ok(mut state) = client_state.lock() {
        if state.status != "error" {
            state.status = "closed".into();
        }
    }
}

pub fn connect_to_peer_ft(
    addr: &str,
    my_id: &str,
    target_id: &str,
    password: &str,
    client_state: Arc<Mutex<ClientState>>,
    stop: Arc<AtomicBool>,
) {
    crate::config::write_log(&format!("[client] Connecting to {} (file transfer)...", crate::config::mask_ip(&addr)));
    let result = run_client(addr, my_id, target_id, password, &client_state, &stop, true);
    if let Err(e) = result {
        crate::config::write_log(&format!("[client] Error: {}", e));
        if let Ok(mut state) = client_state.lock() {
            state.status = "error".into();
            state.error = e.to_string();
        }
    }
    if let Ok(mut state) = client_state.lock() {
        if state.status != "error" {
            state.status = "closed".into();
        }
    }
}

pub fn run_client_on_stream(
    stream: FramedStream,
    my_id: &str,
    target_id: &str,
    password: &str,
    client_state: Arc<Mutex<ClientState>>,
    stop: Arc<AtomicBool>,
) {
    let result = run_client_inner(stream, my_id, target_id, password, &client_state, &stop, false);
    if let Err(e) = result {
        crate::config::write_log(&format!("[client] Error: {}", e));
        if let Ok(mut state) = client_state.lock() {
            state.status = "error".into();
            state.error = e.to_string();
        }
    }
    if let Ok(mut state) = client_state.lock() {
        if state.status != "error" {
            state.status = "closed".into();
        }
    }
}

pub fn run_client_on_stream_ft(
    stream: FramedStream,
    my_id: &str,
    target_id: &str,
    password: &str,
    client_state: Arc<Mutex<ClientState>>,
    stop: Arc<AtomicBool>,
) {
    let result = run_client_inner(stream, my_id, target_id, password, &client_state, &stop, true);
    if let Err(e) = result {
        crate::config::write_log(&format!("[client] Error: {}", e));
        if let Ok(mut state) = client_state.lock() {
            state.status = "error".into();
            state.error = e.to_string();
        }
    }
    if let Ok(mut state) = client_state.lock() {
        if state.status != "error" {
            state.status = "closed".into();
        }
    }
}

fn run_client(
    addr: &str,
    my_id: &str,
    target_id: &str,
    password: &str,
    client_state: &Arc<Mutex<ClientState>>,
    stop: &Arc<AtomicBool>,
    file_transfer: bool,
) -> io::Result<()> {

    let stream = FramedStream::connect(addr, CONNECT_TIMEOUT)?;
    crate::config::write_log(&format!("[client] Connected to {}", crate::config::mask_ip(&addr)));
    run_client_inner(stream, my_id, target_id, password, client_state, stop, file_transfer)
}

fn run_client_inner(
    mut stream: FramedStream,
    my_id: &str,
    target_id: &str,
    password: &str,
    client_state: &Arc<Mutex<ClientState>>,
    stop: &Arc<AtomicBool>,
    file_transfer: bool,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();

    if let Ok(mut state) = client_state.lock() {
        state.status = "login".into();
        state.tcp_connected = true;
    }

    let data = stream.recv_msg()?;
    let msg = message_proto::Message::parse_from_bytes(&data).map_err(io_err)?;

    let hash = match msg.union {
        Some(message_proto::message::Union::Hash(h)) => h,
        Some(message_proto::message::Union::SignedId(signed_id)) => {
            crate::config::write_log("[client] Got SignedId, attempting key exchange");

            let mut server_pk = [0u8; 32];
            let mut has_server_pk = false;
            if let Ok(id_pk) = message_proto::IdPk::parse_from_bytes(&signed_id.id) {
                if id_pk.pk.len() == 32 {
                    server_pk.copy_from_slice(&id_pk.pk);
                    has_server_pk = true;
                }
            }
            if !has_server_pk && signed_id.id.len() > 64 {

                if let Ok(id_pk) = message_proto::IdPk::parse_from_bytes(&signed_id.id[64..]) {
                    if id_pk.pk.len() == 32 {
                        server_pk.copy_from_slice(&id_pk.pk);
                        has_server_pk = true;
                    }
                }
            }

            if has_server_pk {

                let (our_sk, our_pk) = crate::crypto::x25519_keypair();

                let sym_key_vec = crate::config::generate_random_bytes(32);
                let mut sym_key = [0u8; 32];
                sym_key.copy_from_slice(&sym_key_vec);

                let nonce = [0u8; 24];
                let sealed = crate::crypto::crypto_box_seal(&server_pk, &our_sk, &nonce, &sym_key);

                let mut pk_msg = message_proto::PublicKey::new();
                pk_msg.asymmetric_value = our_pk.to_vec().into();
                pk_msg.symmetric_value = sealed.into();
                let mut msg = message_proto::Message::new();
                msg.set_public_key(pk_msg);
                stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;

                stream.set_key(sym_key);
                crate::config::write_log("[client] Encryption established");
            } else {

                crate::config::write_log("[client] No server pk, sending empty PublicKey (no encryption)");
                let pk = message_proto::PublicKey::new();
                let mut msg = message_proto::Message::new();
                msg.set_public_key(pk);
                stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
            }

            let data = stream.recv_msg()?;
            let msg = message_proto::Message::parse_from_bytes(&data).map_err(io_err)?;
            match msg.union {
                Some(message_proto::message::Union::Hash(h)) => h,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Expected Hash after SignedId",
                    ))
                }
            }
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Expected Hash or SignedId",
            ))
        }
    };

    crate::config::write_log(&format!("[client] Got Hash challenge, authenticating..."));

    let switch_uuid = unsafe { crate::remote::SWITCH_UUID.take() };
    if let Some(ref uuid_str) = switch_uuid {
        crate::config::write_log(&format!("[client] Switch Sides mode — sending SwitchSidesResponse"));

        let uuid_clean = uuid_str.replace('-', "");
        let mut uuid_bytes = Vec::new();
        for i in (0..uuid_clean.len()).step_by(2) {
            if let Ok(b) = u8::from_str_radix(&uuid_clean[i..i+2], 16) {
                uuid_bytes.push(b);
            }
        }

        let mut lr = message_proto::LoginRequest::new();
        lr.my_id = my_id.to_string();
        lr.my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "XP-Client".into());
        lr.my_platform = "Windows".to_string();
        lr.version = env!("CARGO_PKG_VERSION").to_string();

        let mut option = message_proto::OptionMessage::new();
        let mut decoding = message_proto::SupportedDecoding::new();
        decoding.ability_vp8 = 1;
        decoding.prefer = message_proto::supported_decoding::PreferCodec::VP8.into();
        option.supported_decoding = protobuf::MessageField::some(decoding);
        lr.option = protobuf::MessageField::some(option);

        let mut resp = message_proto::SwitchSidesResponse::new();
        resp.uuid = uuid_bytes.into();
        resp.lr = protobuf::MessageField::some(lr);

        let mut msg = message_proto::Message::new();
        msg.set_switch_sides_response(resp);
        stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
        crate::config::write_log("[client] SwitchSidesResponse sent, waiting for PeerInfo...");

    } else {

        let pw_bytes: Vec<u8> = if password.is_empty() {
            Vec::new()
        } else {
            compute_password_hash(password, &hash.salt, &hash.challenge).to_vec()
        };

        let mut lr = message_proto::LoginRequest::new();
        lr.username = target_id.to_string();
        lr.my_id = my_id.to_string();
        lr.my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "XP-Client".into());
        lr.my_platform = "Windows".to_string();
        lr.password = pw_bytes.into();
        lr.version = env!("CARGO_PKG_VERSION").to_string();

        let mut option = message_proto::OptionMessage::new();
        let mut decoding = message_proto::SupportedDecoding::new();
        decoding.ability_vp8 = 1;
        decoding.prefer = message_proto::supported_decoding::PreferCodec::VP8.into();
        option.supported_decoding = protobuf::MessageField::some(decoding);
        lr.option = protobuf::MessageField::some(option);

        if file_transfer {
            let ft = message_proto::FileTransfer::new();
            lr.set_file_transfer(ft);
            if let Ok(mut state) = client_state.lock() {
                state.file_transfer_mode = true;
            }
        }

        let mut msg = message_proto::Message::new();
        msg.set_login_request(lr);
        stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
    }

    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let login_resp;
    let login_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() >= login_deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "Login response timeout"));
        }
        let data = stream.recv_msg()?;
        let msg = message_proto::Message::parse_from_bytes(&data).map_err(io_err)?;

        match msg.union {
            Some(message_proto::message::Union::LoginResponse(lr)) => {
                login_resp = lr;
                break;
            }
            Some(message_proto::message::Union::TestDelay(td)) => {

                let mut resp = message_proto::TestDelay::new();
                resp.time = td.time;
                resp.last_delay = td.last_delay;
                resp.from_client = true;
                let mut reply = message_proto::Message::new();
                reply.set_test_delay(resp);
                stream.send_msg(&reply.write_to_bytes().map_err(io_err)?)?;
                continue;
            }
            Some(message_proto::message::Union::Misc(misc)) => {

                if let Some(message_proto::misc::Union::CloseReason(reason)) = &misc.union {
                    return Err(io::Error::new(io::ErrorKind::ConnectionRefused, reason.clone()));
                }
                continue;
            }
            _ => {
                crate::config::write_log(&format!("[client] Ignoring non-login message during auth"));
                continue;
            }
        }
    }

    let final_peer_info: Option<message_proto::PeerInfo>;
    match login_resp.union {
        Some(message_proto::login_response::Union::Error(ref e))
            if e == "No Password Access" =>
        {

            crate::config::write_log("[client] No Password Access — waiting for remote acceptance...");
            if let Ok(mut state) = client_state.lock() {
                state.status = "waiting".into();
                state.error = "Waiting for remote acceptance...".into();
            }

            stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
            let accept_deadline = Instant::now() + Duration::from_secs(60);
            let mut accepted = false;
            loop {
                if Instant::now() >= accept_deadline {
                    crate::config::write_log("[client] Timed out waiting for remote acceptance");
                    if let Ok(mut state) = client_state.lock() {
                        state.status = "error".into();
                        state.error = "Remote side did not respond".into();
                    }
                    return Ok(());
                }
                let data = match stream.recv_msg() {
                    Ok(d) => d,
                    Err(e) => {
                        crate::config::write_log(&format!("[client] Error waiting for acceptance: {}", e));
                        if let Ok(mut state) = client_state.lock() {
                            state.status = "error".into();
                            state.error = format!("Connection error: {}", e);
                        }
                        return Ok(());
                    }
                };
                let msg = message_proto::Message::parse_from_bytes(&data).map_err(io_err)?;
                match msg.union {
                    Some(message_proto::message::Union::LoginResponse(lr)) => {
                        match lr.union {
                            Some(message_proto::login_response::Union::PeerInfo(pi)) => {
                                crate::config::write_log("[client] Remote accepted connection!");
                                final_peer_info = Some(pi);
                                accepted = true;
                                break;
                            }
                            Some(message_proto::login_response::Union::Error(e2)) => {
                                crate::config::write_log(&format!("[client] Remote rejected: {}", e2));
                                if let Ok(mut state) = client_state.lock() {
                                    state.status = "error".into();
                                    state.error = e2;
                                }
                                return Ok(());
                            }
                            None => continue,
                        }
                    }
                    Some(message_proto::message::Union::Misc(misc)) => {
                        if let Some(message_proto::misc::Union::CloseReason(reason)) = &misc.union {
                            crate::config::write_log(&format!("[client] Remote closed: {}", reason));
                            if let Ok(mut state) = client_state.lock() {
                                state.status = "error".into();
                                state.error = reason.clone();
                            }
                            return Ok(());
                        }
                        continue;
                    }
                    Some(message_proto::message::Union::TestDelay(td)) => {
                        let mut resp = message_proto::TestDelay::new();
                        resp.time = td.time;
                        resp.last_delay = td.last_delay;
                        resp.from_client = true;
                        let mut reply = message_proto::Message::new();
                        reply.set_test_delay(resp);
                        let _ = stream.send_msg(&reply.write_to_bytes().map_err(io_err)?);
                        continue;
                    }
                    _ => continue,
                }
            }
            if !accepted {
                return Ok(());
            }
        }
        Some(message_proto::login_response::Union::Error(e)) => {
            crate::config::write_log(&format!("[client] Login failed: {}", e));
            if let Ok(mut state) = client_state.lock() {
                state.status = "error".into();
                state.error = e;
            }
            return Ok(());
        }
        Some(message_proto::login_response::Union::PeerInfo(pi)) => {
            final_peer_info = Some(pi);
        }
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Empty LoginResponse",
            ));
        }
    }

    if let Some(pi) = final_peer_info {
            crate::config::write_log(&format!(
                "[client] Login OK! Peer: {} ({}) - {} displays",
                pi.hostname,
                pi.platform,
                pi.displays.len()
            ));

            if !password.is_empty() {
                let mut peer_cfg = crate::config::PeerConfig::load(target_id);
                peer_cfg.set_option("password", password);
                peer_cfg.save(target_id);
                crate::config::write_log("[client] Saved password to peer config");
            }

            {
                let mut peer_cfg = crate::config::PeerConfig::load(target_id);
                if !pi.username.is_empty() || !pi.hostname.is_empty() {

                    let mut local_cfg = crate::config::LocalConfig::load();
                    local_cfg.update_recent_peer(target_id, &pi.username, &pi.hostname, &pi.platform);
                }
            }

            if let Ok(mut state) = client_state.lock() {
                state.status = "connected".into();
                state.peer_name = pi.hostname.clone();
                state.peer_platform = pi.platform.clone();
                state.displays = pi
                    .displays
                    .iter()
                    .map(|d| DisplayMeta {
                        width: d.width,
                        height: d.height,
                        x: d.x,
                        y: d.y,
                        name: d.name.clone(),
                    })
                    .collect();
                if let Some(d) = pi.displays.first() {
                    state.frame_width = d.width;
                    state.frame_height = d.height;
                }
            }
    }

    let stream = Arc::new(Mutex::new(stream));
    if let Ok(mut state) = client_state.lock() {
        state.input_stream = Some(stream.clone());
    }

    if let Ok(mut s) = stream.lock() {
        s.set_read_timeout(Some(Duration::from_millis(500))).ok();
    }

    let mut decoder: Option<crate::vpx::Vp8Decoder> = None;
    let mut last_clipboard_text = String::new();
    let mut last_clipboard_check = Instant::now();

    if file_transfer {
        if let Ok(mut s) = stream.lock() {
            s.set_read_timeout(Some(Duration::from_millis(100))).ok();
        }
    }

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        {
            let actions: Vec<message_proto::FileAction> = {
                if let Ok(state) = client_state.lock() {
                    if let Ok(mut q) = state.file_actions_out.lock() {
                        q.drain(..).collect()
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            };
            if !actions.is_empty() {
                if let Ok(mut s) = stream.lock() {
                    for fa in actions {
                        let mut msg = message_proto::Message::new();
                        msg.set_file_action(fa);
                        if let Ok(bytes) = msg.write_to_bytes() {
                            let _ = s.send_msg(&bytes);
                        }
                    }
                }
            }
        }

        if last_clipboard_check.elapsed() >= Duration::from_millis(500) {
            last_clipboard_check = Instant::now();

            if let Some(msg) = crate::clipboard_file::check_clipboard_files_change() {
                if let Ok(mut s) = stream.lock() {
                    if let Ok(bytes) = msg.write_to_bytes() {
                        let _ = s.send_msg(&bytes);
                    }
                }
            } else if let Some(msg) = crate::clipboard::check_clipboard_change(&mut last_clipboard_text) {
                if let Ok(mut s) = stream.lock() {
                    if let Ok(bytes) = msg.write_to_bytes() {
                        let _ = s.send_msg(&bytes);
                    }
                }
            }
        }

        let data = {
            let mut s = match stream.lock() {
                Ok(s) => s,
                Err(_) => break,
            };
            match s.recv_msg() {
                Ok(d) => d,
                Err(ref e) if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => {
                    continue;
                }
                Err(e) => {
                    crate::config::write_log(&format!("[client] Recv error: {}", e));
                    break;
                }
            }
        };

        if let Ok(msg) = message_proto::Message::parse_from_bytes(&data) {
            match msg.union {
                Some(message_proto::message::Union::VideoFrame(vf)) => {

                    static mut VF_COUNT: u32 = 0;
                    unsafe {
                        VF_COUNT += 1;
                        if VF_COUNT <= 3 || VF_COUNT % 100 == 0 {
                            let ftype = match &vf.union {
                                Some(message_proto::video_frame::Union::Vp8s(_)) => "VP8",
                                Some(message_proto::video_frame::Union::Vp9s(_)) => "VP9",
                                Some(message_proto::video_frame::Union::H264s(_)) => "H264",
                                Some(message_proto::video_frame::Union::H265s(_)) => "H265",
                                Some(message_proto::video_frame::Union::Rgb(_)) => "RGB",
                                _ => "unknown",
                            };
                            crate::config::write_log(&format!("[client] VideoFrame #{}: type={}", VF_COUNT, ftype));
                        }
                    }
                    let mut s = stream.lock().unwrap();
                    handle_video_frame(&vf, &mut s, client_state, &mut decoder);
                }
                Some(message_proto::message::Union::CursorData(_cd)) => {

                }
                Some(message_proto::message::Union::CursorPosition(_cp)) => {

                }
                Some(message_proto::message::Union::Clipboard(cb)) => {
                    crate::clipboard::handle_clipboard_message(&cb);
                }
                Some(message_proto::message::Union::MultiClipboards(mc)) => {
                    crate::clipboard::handle_multi_clipboards_message(&mc);
                }
                Some(message_proto::message::Union::Cliprdr(ref cliprdr)) => {
                    let replies = crate::clipboard_file::handle_cliprdr_client(cliprdr);
                    if !replies.is_empty() {
                        if let Ok(mut s) = stream.lock() {
                            for reply in replies {
                                if let Ok(bytes) = reply.write_to_bytes() {
                                    let _ = s.send_msg(&bytes);
                                }
                            }
                        }
                    }
                }
                Some(message_proto::message::Union::Misc(misc)) => {
                    if handle_misc_from_server(&misc, &client_state) {
                        break;
                    }
                }
                Some(message_proto::message::Union::FileResponse(fr)) => {

                    if let Ok(state) = client_state.lock() {
                        if let Ok(mut q) = state.file_responses.lock() {
                            q.push_back(fr);
                        }
                    }
                }
                Some(message_proto::message::Union::FileAction(fa)) => {

                    handle_file_action_from_server(fa, &stream);
                }
                Some(message_proto::message::Union::TestDelay(td)) => {

                    let mut resp = message_proto::TestDelay::new();
                    resp.time = td.time;
                    resp.last_delay = td.last_delay;
                    resp.from_client = true;
                    let mut reply = message_proto::Message::new();
                    reply.set_test_delay(resp);
                    if let Ok(bytes) = reply.write_to_bytes() {
                        if let Ok(mut s) = stream.lock() {
                            let _ = s.send_msg(&bytes);
                        }
                    }
                }
                other => {
                    static mut UNK_COUNT: u32 = 0;
                    unsafe {
                        UNK_COUNT += 1;
                        if UNK_COUNT <= 10 {
                            let type_name = match &other {
                                Some(message_proto::message::Union::TestDelay(_)) => "TestDelay",
                                Some(message_proto::message::Union::CursorData(_)) => "CursorData",
                                Some(message_proto::message::Union::CursorPosition(_)) => "CursorPosition",
                                Some(message_proto::message::Union::CursorId(_)) => "CursorId",
                                Some(message_proto::message::Union::PeerInfo(_)) => "PeerInfo",
                                Some(message_proto::message::Union::MessageBox(_)) => "MessageBox",
                                _ => "Other",
                            };
                            crate::config::write_log(&format!("[client] Unhandled message: {}", type_name));
                        }
                    }
                }
            }
        }
    }

    if let Ok(mut s) = stream.lock() {
        let mut misc = message_proto::Misc::new();
        misc.set_close_reason("The remote partner has closed the session.".to_string());
        let mut close_msg = message_proto::Message::new();
        close_msg.set_misc(misc);
        let _ = s.send_msg(&close_msg.write_to_bytes().unwrap_or_default());
    }
    std::thread::sleep(Duration::from_millis(150));

    if let Ok(mut state) = client_state.lock() {
        state.input_stream = None;
    }

    Ok(())
}

fn handle_video_frame(
    vf: &message_proto::VideoFrame,
    stream: &mut FramedStream,
    client_state: &Arc<Mutex<ClientState>>,
    decoder: &mut Option<crate::vpx::Vp8Decoder>,
) {
    match &vf.union {
        Some(message_proto::video_frame::Union::Rgb(_rgb)) => {

            if let Ok(pixel_data) = stream.recv_msg() {
                if let Ok(mut state) = client_state.lock() {
                    state.frame_data = pixel_data;
                    state.frame_seq += 1;
                }
            }
        }
        Some(message_proto::video_frame::Union::Vp8s(frames)) => {

            if decoder.is_none() {
                match crate::vpx::Vp8Decoder::new() {
                    Ok(d) => *decoder = Some(d),
                    Err(e) => {
                        crate::config::write_log(&format!("[client] Failed to create VP8 decoder: {}", e));
                        return;
                    }
                }
            }
            let dec = decoder.as_mut().unwrap();
            for frame in &frames.frames {

                if let Ok(mut state) = client_state.lock() {
                    if state.recording {
                        let keyframe = frame.key;
                        if let Some(ref mut rec) = state.recorder {
                            let _ = rec.write_vp8_frame(&frame.data, keyframe);
                        }
                    }
                }

                match dec.decode(&frame.data) {
                    Ok((bgra, w, h)) => {
                        if let Ok(mut state) = client_state.lock() {
                            state.frame_width = w;
                            state.frame_height = h;
                            state.frame_data = bgra;
                            state.frame_seq += 1;
                        }
                    }
                    Err(e) => {
                        crate::config::write_log(&format!("[client] VP8 decode error: {}", e));
                    }
                }
            }
        }
        _ => {}
    }
}

fn handle_misc_from_server(misc: &message_proto::Misc, client_state: &Arc<Mutex<ClientState>>) -> bool {
    match &misc.union {
        Some(message_proto::misc::Union::SwitchDisplay(sd)) => {
            crate::config::write_log(&format!(
                "[client] Switch display: {}x{} at ({},{})",
                sd.width, sd.height, sd.x, sd.y
            ));
        }
        Some(message_proto::misc::Union::CloseReason(reason)) => {
            crate::config::write_log(&format!("[client] Close reason: {}", reason));
            return true;
        }
        Some(message_proto::misc::Union::ChatMessage(cm)) => {
            crate::config::write_log(&format!("[client] Chat received: {}", cm.text));

            if let Ok(state) = client_state.lock() {
                if let Ok(mut q) = state.chat_messages.lock() {
                    q.push_back(("Peer".to_string(), cm.text.clone()));
                }
            }
        }
        _ => {}
    }
    false
}

pub fn send_chat_message(client_state: &Arc<Mutex<ClientState>>, text: &str) {
    let stream = {
        let s = match client_state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        match &s.input_stream {
            Some(st) => st.clone(),
            None => return,
        }
    };

    let mut chat = message_proto::ChatMessage::new();
    chat.text = text.to_string();
    let mut misc = message_proto::Misc::new();
    misc.union = Some(message_proto::misc::Union::ChatMessage(chat));
    let mut msg = message_proto::Message::new();
    msg.set_misc(misc);
    if let Ok(bytes) = msg.write_to_bytes() {
        if let Ok(mut s) = stream.lock() {
            let _ = s.send_msg(&bytes);
        }
    }
    crate::config::write_log(&format!("[client] Chat sent: {}", text));
}

pub fn send_mouse_event(
    client_state: &Arc<Mutex<ClientState>>,
    mask: i32,
    x: i32,
    y: i32,
    alt: bool,
    ctrl: bool,
    shift: bool,
) {
    let stream = {
        let s = match client_state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        match &s.input_stream {
            Some(st) => st.clone(),
            None => return,
        }
    };

    let mut me = message_proto::MouseEvent::new();
    me.mask = mask;
    me.x = x;
    me.y = y;
    if alt {
        me.modifiers.push(message_proto::ControlKey::Alt.into());
    }
    if ctrl {
        me.modifiers.push(message_proto::ControlKey::Control.into());
    }
    if shift {
        me.modifiers.push(message_proto::ControlKey::Shift.into());
    }

    let mut msg = message_proto::Message::new();
    msg.set_mouse_event(me);
    if let Ok(bytes) = msg.write_to_bytes() {
        if let Ok(mut s) = stream.lock() {
            let _ = s.send_msg(&bytes);
        }
    }
}

pub fn send_key_event(
    client_state: &Arc<Mutex<ClientState>>,
    down: bool,
    vk_code: u32,
    alt: bool,
    ctrl: bool,
    shift: bool,
) {
    let stream = {
        let s = match client_state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        match &s.input_stream {
            Some(st) => st.clone(),
            None => return,
        }
    };

    let mut ke = message_proto::KeyEvent::new();
    ke.down = down;

    if let Some(ck) = vk_to_control_key(vk_code) {
        ke.union = Some(message_proto::key_event::Union::ControlKey(ck.into()));
    } else if let Some(chr) = vk_to_chr(vk_code) {
        ke.union = Some(message_proto::key_event::Union::Chr(chr));
    } else {

        return;
    }

    if alt && vk_code != 0x12 && vk_code != 0xA4 && vk_code != 0xA5 {
        ke.modifiers.push(message_proto::ControlKey::Alt.into());
    }
    if ctrl && vk_code != 0x11 && vk_code != 0xA2 && vk_code != 0xA3 {
        ke.modifiers.push(message_proto::ControlKey::Control.into());
    }
    if shift && vk_code != 0x10 && vk_code != 0xA0 && vk_code != 0xA1 {
        ke.modifiers.push(message_proto::ControlKey::Shift.into());
    }

    let mut msg = message_proto::Message::new();
    msg.set_key_event(ke);
    if let Ok(bytes) = msg.write_to_bytes() {
        if let Ok(mut s) = stream.lock() {
            let _ = s.send_msg(&bytes);
        }
    }
}

pub fn send_ctrl_alt_del(client_state: &Arc<Mutex<ClientState>>) {
    crate::config::write_log("[client] send_ctrl_alt_del called");
    let stream = {
        let s = match client_state.lock() {
            Ok(s) => s,
            Err(_) => {
                crate::config::write_log("[client] send_ctrl_alt_del: failed to lock state");
                return;
            }
        };
        match &s.input_stream {
            Some(st) => st.clone(),
            None => {
                crate::config::write_log("[client] send_ctrl_alt_del: no input_stream");
                return;
            }
        }
    };

    let mut ke = message_proto::KeyEvent::new();
    ke.down = true;
    ke.union = Some(message_proto::key_event::Union::ControlKey(
        message_proto::ControlKey::CtrlAltDel.into(),
    ));

    let mut msg = message_proto::Message::new();
    msg.set_key_event(ke);
    match msg.write_to_bytes() {
        Ok(bytes) => {
            crate::config::write_log(&format!("[client] send_ctrl_alt_del: sending {} bytes", bytes.len()));
            match stream.lock() {
                Ok(mut s) => {
                    match s.send_msg(&bytes) {
                        Ok(_) => crate::config::write_log("[client] send_ctrl_alt_del: sent OK"),
                        Err(e) => crate::config::write_log(&format!("[client] send_ctrl_alt_del: send error: {}", e)),
                    }
                }
                Err(e) => crate::config::write_log(&format!("[client] send_ctrl_alt_del: stream lock error: {}", e)),
            }
        }
        Err(e) => crate::config::write_log(&format!("[client] send_ctrl_alt_del: serialize error: {}", e)),
    }
}

pub fn send_lock_screen(client_state: &Arc<Mutex<ClientState>>) {
    let stream = {
        let s = match client_state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        match &s.input_stream {
            Some(st) => st.clone(),
            None => return,
        }
    };

    let mut ke = message_proto::KeyEvent::new();
    ke.down = true;
    ke.union = Some(message_proto::key_event::Union::ControlKey(
        message_proto::ControlKey::LockScreen.into(),
    ));

    let mut msg = message_proto::Message::new();
    msg.set_key_event(ke);
    if let Ok(bytes) = msg.write_to_bytes() {
        if let Ok(mut s) = stream.lock() {
            let _ = s.send_msg(&bytes);
        }
    }
}

pub fn send_restart(client_state: &Arc<Mutex<ClientState>>) {
    crate::config::write_log("[client] send_restart called");
    let stream = {
        let s = match client_state.lock() {
            Ok(s) => s,
            Err(_) => {
                crate::config::write_log("[client] send_restart: failed to lock state");
                return;
            }
        };
        match &s.input_stream {
            Some(st) => st.clone(),
            None => {
                crate::config::write_log("[client] send_restart: no input_stream");
                return;
            }
        }
    };

    let mut misc = message_proto::Misc::new();
    misc.set_restart_remote_device(true);
    let mut msg = message_proto::Message::new();
    msg.set_misc(misc);
    match msg.write_to_bytes() {
        Ok(bytes) => {
            crate::config::write_log(&format!("[client] send_restart: sending {} bytes", bytes.len()));
            match stream.lock() {
                Ok(mut s) => {
                    match s.send_msg(&bytes) {
                        Ok(_) => crate::config::write_log("[client] send_restart: sent OK"),
                        Err(e) => crate::config::write_log(&format!("[client] send_restart: send error: {}", e)),
                    }
                }
                Err(e) => crate::config::write_log(&format!("[client] send_restart: stream lock error: {}", e)),
            }
        }
        Err(e) => crate::config::write_log(&format!("[client] send_restart: serialize error: {}", e)),
    }
}

pub fn send_switch_sides_request_bytes(client_state: &Arc<Mutex<ClientState>>, uuid_bytes: &[u8]) {
    crate::config::write_log(&format!("[client] send_switch_sides_request: {} bytes", uuid_bytes.len()));
    let stream = {
        let s = match client_state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        match &s.input_stream {
            Some(st) => st.clone(),
            None => return,
        }
    };

    let mut req = message_proto::SwitchSidesRequest::new();
    req.uuid = uuid_bytes.to_vec();
    let mut misc = message_proto::Misc::new();
    misc.set_switch_sides_request(req);
    let mut msg = message_proto::Message::new();
    msg.set_misc(misc);
    if let Ok(bytes) = msg.write_to_bytes() {
        if let Ok(mut s) = stream.lock() {
            match s.send_msg(&bytes) {
                Ok(_) => crate::config::write_log("[client] send_switch_sides_request: sent OK"),
                Err(e) => crate::config::write_log(&format!("[client] send_switch_sides_request: send error: {}", e)),
            }
        }
    }
}

fn vk_to_control_key(vk: u32) -> Option<message_proto::ControlKey> {
    use message_proto::ControlKey;
    match vk {
        0x08 => Some(ControlKey::Backspace),
        0x09 => Some(ControlKey::Tab),
        0x0D => Some(ControlKey::Return),
        0x10 | 0xA0 => Some(ControlKey::Shift),
        0xA1 => Some(ControlKey::RShift),
        0x11 | 0xA2 => Some(ControlKey::Control),
        0xA3 => Some(ControlKey::RControl),
        0x12 | 0xA4 => Some(ControlKey::Alt),
        0xA5 => Some(ControlKey::RAlt),
        0x13 => Some(ControlKey::Pause),
        0x14 => Some(ControlKey::CapsLock),
        0x1B => Some(ControlKey::Escape),
        0x20 => Some(ControlKey::Space),
        0x21 => Some(ControlKey::PageUp),
        0x22 => Some(ControlKey::PageDown),
        0x23 => Some(ControlKey::End),
        0x24 => Some(ControlKey::Home),
        0x25 => Some(ControlKey::LeftArrow),
        0x26 => Some(ControlKey::UpArrow),
        0x27 => Some(ControlKey::RightArrow),
        0x28 => Some(ControlKey::DownArrow),
        0x2C => Some(ControlKey::Snapshot),
        0x2D => Some(ControlKey::Insert),
        0x2E => Some(ControlKey::Delete),
        0x5B => Some(ControlKey::Meta),
        0x5C => Some(ControlKey::RWin),
        0x5D => Some(ControlKey::Apps),
        0x5F => Some(ControlKey::Sleep),
        0x60 => Some(ControlKey::Numpad0),
        0x61 => Some(ControlKey::Numpad1),
        0x62 => Some(ControlKey::Numpad2),
        0x63 => Some(ControlKey::Numpad3),
        0x64 => Some(ControlKey::Numpad4),
        0x65 => Some(ControlKey::Numpad5),
        0x66 => Some(ControlKey::Numpad6),
        0x67 => Some(ControlKey::Numpad7),
        0x68 => Some(ControlKey::Numpad8),
        0x69 => Some(ControlKey::Numpad9),
        0x6A => Some(ControlKey::Multiply),
        0x6B => Some(ControlKey::Add),
        0x6C => Some(ControlKey::Separator),
        0x6D => Some(ControlKey::Subtract),
        0x6E => Some(ControlKey::Decimal),
        0x6F => Some(ControlKey::Divide),
        0x70 => Some(ControlKey::F1),
        0x71 => Some(ControlKey::F2),
        0x72 => Some(ControlKey::F3),
        0x73 => Some(ControlKey::F4),
        0x74 => Some(ControlKey::F5),
        0x75 => Some(ControlKey::F6),
        0x76 => Some(ControlKey::F7),
        0x77 => Some(ControlKey::F8),
        0x78 => Some(ControlKey::F9),
        0x79 => Some(ControlKey::F10),
        0x7A => Some(ControlKey::F11),
        0x7B => Some(ControlKey::F12),
        0x90 => Some(ControlKey::NumLock),
        0x91 => Some(ControlKey::Scroll),
        _ => None,
    }
}

fn vk_to_chr(vk: u32) -> Option<u32> {
    match vk {

        0x41..=0x5A => Some(vk - 0x41 + 'a' as u32),

        0x30..=0x39 => Some(vk - 0x30 + '0' as u32),

        0xBA => Some(';' as u32),
        0xBB => Some('=' as u32),
        0xBC => Some(',' as u32),
        0xBD => Some('-' as u32),
        0xBE => Some('.' as u32),
        0xBF => Some('/' as u32),
        0xC0 => Some('`' as u32),
        0xDB => Some('[' as u32),
        0xDC => Some('\\' as u32),
        0xDD => Some(']' as u32),
        0xDE => Some('\'' as u32),
        _ => None,
    }
}

fn handle_file_action_from_server(fa: message_proto::FileAction, stream: &Arc<Mutex<FramedStream>>) {
    match fa.union {
        Some(message_proto::file_action::Union::ReadDir(rd)) => {
            let fd = if rd.path.is_empty() {
                crate::file_transfer::get_drives()
            } else {
                crate::file_transfer::read_dir_to_proto(&rd.path, rd.include_hidden)
                    .unwrap_or_else(|_| {
                        let mut fd = message_proto::FileDirectory::new();
                        fd.path = rd.path;
                        fd
                    })
            };
            let mut fr = message_proto::FileResponse::new();
            fr.set_dir(fd);
            let mut msg = message_proto::Message::new();
            msg.set_file_response(fr);
            if let Ok(bytes) = msg.write_to_bytes() {
                if let Ok(mut s) = stream.lock() {
                    let _ = s.send_msg(&bytes);
                }
            }
        }
        Some(message_proto::file_action::Union::Send(ref send)) => {
            crate::config::write_log(&format!("[client] Server requesting send: id={} path={}", send.id, send.path));

            let path = &send.path;
            let id = send.id;
            let file_num = send.file_num;

            if std::path::Path::new(path).is_file() {
                let mut offset: u64 = 0;
                let mut blk_id: u32 = 0;
                let mut had_error = false;
                loop {
                    match crate::file_transfer::read_file_block(path, offset) {
                        Ok(data) => {
                            if data.is_empty() { break; }
                            let data_len = data.len() as u64;
                            let mut block = message_proto::FileTransferBlock::new();
                            block.id = id;
                            block.file_num = file_num;
                            block.data = data.into();
                            block.blk_id = blk_id;
                            let mut fr = message_proto::FileResponse::new();
                            fr.set_block(block);
                            let mut msg = message_proto::Message::new();
                            msg.set_file_response(fr);
                            if let Ok(bytes) = msg.write_to_bytes() {
                                if let Ok(mut s) = stream.lock() {
                                    let _ = s.send_msg(&bytes);
                                }
                            }
                            offset += data_len;
                            blk_id += 1;
                        }
                        Err(e) => {
                            crate::config::write_log(&format!("[client] Error reading file {}: {}", path, e));
                            let mut err = message_proto::FileTransferError::new();
                            err.id = id;
                            err.file_num = file_num;
                            err.error = format!("{}", e);
                            let mut fr = message_proto::FileResponse::new();
                            fr.set_error(err);
                            let mut msg = message_proto::Message::new();
                            msg.set_file_response(fr);
                            if let Ok(bytes) = msg.write_to_bytes() {
                                if let Ok(mut s) = stream.lock() {
                                    let _ = s.send_msg(&bytes);
                                }
                            }
                            had_error = true;
                            break;
                        }
                    }
                }
                if !had_error {
                    let mut done = message_proto::FileTransferDone::new();
                    done.id = id;
                    done.file_num = file_num;
                    let mut fr = message_proto::FileResponse::new();
                    fr.set_done(done);
                    let mut msg = message_proto::Message::new();
                    msg.set_file_response(fr);
                    if let Ok(bytes) = msg.write_to_bytes() {
                        if let Ok(mut s) = stream.lock() {
                            let _ = s.send_msg(&bytes);
                        }
                    }
                    crate::config::write_log(&format!("[client] File send complete for id={}", id));
                }
            } else {
                crate::config::write_log(&format!("[client] Send path is not a file: {}", path));
                let mut err = message_proto::FileTransferError::new();
                err.id = id;
                err.file_num = file_num;
                err.error = "Not a file".to_string();
                let mut fr = message_proto::FileResponse::new();
                fr.set_error(err);
                let mut msg = message_proto::Message::new();
                msg.set_file_response(fr);
                if let Ok(bytes) = msg.write_to_bytes() {
                    if let Ok(mut s) = stream.lock() {
                        let _ = s.send_msg(&bytes);
                    }
                }
            }
        }
        other => {
            let name = match &other {
                Some(message_proto::file_action::Union::Receive(_)) => "Receive",
                Some(message_proto::file_action::Union::Cancel(_)) => "Cancel",
                _ => "Unknown",
            };
            crate::config::write_log(&format!("[client] Unhandled FileAction from server: {}", name));
        }
    }
}

pub fn send_file_action(client_state: &Arc<Mutex<ClientState>>, fa: message_proto::FileAction) {
    if let Ok(state) = client_state.lock() {
        if let Ok(mut q) = state.file_actions_out.lock() {
            q.push_back(fa);
        }
    }
}

fn compute_password_hash(password: &str, salt: &str, challenge: &str) -> [u8; 32] {
    crate::crypto::compute_password_hash(password, salt, challenge)
}

fn io_err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}
