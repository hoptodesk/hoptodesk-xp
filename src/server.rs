
use crate::capture::{self, CapturerGDI};
use crate::cm;
use crate::file_transfer;
use crate::input;
use crate::network::FramedStream;
use crate::platform;
use crate::protocol::message_proto;
use crate::vpx;
use protobuf::Message;
use std::collections::HashMap;
use std::io;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CM_POLL_INTERVAL: Duration = Duration::from_millis(200);
const CM_TIMEOUT: Duration = Duration::from_secs(60);

lazy_static::lazy_static! {
    static ref CURRENT_CM_SESSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
}

/// Persistent direct IP access listener. Accepts connections in a loop
/// on a fixed port when "direct-server" is enabled.
pub fn run_direct_server(
    my_id: String,
    password: String,
    pk: Vec<u8>,
) {
    loop {
        let cfg = crate::config::Config2::load();
        let enabled = cfg.get_option("direct-server");
        let service_stopped = cfg.get_option("stop-service") == "Y";
        if enabled.is_empty() || enabled == "N" || service_stopped {
            // Not enabled or service stopped — check again in a few seconds
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }

        let port: u16 = cfg.get_option("direct-access-port")
            .parse()
            .unwrap_or(21118);
        let addr = format!("0.0.0.0:{}", port);

        let listener = match TcpListener::bind(&addr) {
            Ok(l) => {
                crate::config::write_log(&format!("[direct] Listening on port {}", port));
                l
            }
            Err(e) => {
                crate::config::write_log(&format!("[direct] Failed to bind {}: {}", addr, e));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };

        listener.set_nonblocking(true).ok();

        loop {
            // Check if still enabled or port changed
            let cfg2 = crate::config::Config2::load();
            let still_enabled = cfg2.get_option("direct-server");
            let service_stopped = cfg2.get_option("stop-service") == "Y";
            if still_enabled.is_empty() || still_enabled == "N" || service_stopped {
                crate::config::write_log("[direct] Direct server disabled or service stopped, stopping listener");
                break;
            }
            let new_port: u16 = cfg2.get_option("direct-access-port")
                .parse()
                .unwrap_or(21118);
            if new_port != port {
                crate::config::write_log(&format!("[direct] Port changed to {}, rebinding", new_port));
                break;
            }

            match listener.accept() {
                Ok((tcp_stream, peer_addr)) => {
                    crate::config::write_log(&format!(
                        "[direct] Incoming connection from {}",
                        crate::config::mask_ip(&peer_addr)
                    ));
                    tcp_stream.set_nodelay(true).ok();

                    let my_id = my_id.clone();
                    let password = password.clone();
                    let pk = pk.clone();
                    std::thread::spawn(move || {
                        let mut stream = FramedStream::from_tcp(tcp_stream);
                        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(10))).ok();
                        let stop = Arc::new(AtomicBool::new(false));
                        if let Err(e) = run_session_public(&mut stream, &my_id, &password, &pk, &stop) {
                            crate::config::write_log(&format!("[direct] Session error: {}", e));
                        }
                        crate::config::write_log("[direct] Session ended");
                    });
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => {
                    crate::config::write_log(&format!("[direct] Accept error: {}", e));
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }
}

pub fn accept_connection(
    listener: TcpListener,
    my_id: &str,
    password: &str,
    pk: &[u8],
    stop: Arc<AtomicBool>,
) {

    listener.set_nonblocking(true).ok();
    let deadline = Instant::now() + CONNECT_TIMEOUT;

    crate::config::write_log(&format!("[server] Waiting for incoming connection ({}s timeout)...", CONNECT_TIMEOUT.as_secs()));

    loop {
        match listener.accept() {
            Ok((tcp_stream, peer_addr)) => {
                crate::config::write_log(&format!("[server] Accepted connection from {}", crate::config::mask_ip(&peer_addr)));
                tcp_stream.set_nodelay(true).ok();
                let mut stream = FramedStream::from_tcp(tcp_stream);
                stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
                stream.set_write_timeout(Some(Duration::from_secs(10))).ok();

                if let Err(e) = run_session_public(&mut stream, my_id, password, pk, &stop) {
                    crate::config::write_log(&format!("[server] Session error: {}", e));
                }
                crate::config::write_log(&format!("[server] Session ended"));
                return;
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline || stop.load(Ordering::Relaxed) {
                    crate::config::write_log(&format!("[server] Accept timed out"));
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                crate::config::write_log(&format!("[server] Accept failed: {}", e));
                return;
            }
        }
    }
}

pub fn run_session_public(
    stream: &mut FramedStream,
    my_id: &str,
    password: &str,
    pk: &[u8],
    stop: &Arc<AtomicBool>,
) -> io::Result<()> {

    let config = crate::config::Config::load();
    let config_salt = config.salt.clone();
    let (signing_sk, signing_pk) = (config.key_pair.0.clone(), config.key_pair.1.clone());

    // Generate ephemeral Curve25519 keypair for key exchange
    let (eph_sk, eph_pk) = crate::crypto::x25519_keypair();

    {
        let mut id_pk = message_proto::IdPk::new();
        id_pk.id = my_id.to_string();
        id_pk.pk = eph_pk.to_vec().into();
        let id_pk_bytes = id_pk.write_to_bytes().map_err(io_err)?;

        let mut signed_id = message_proto::SignedId::new();
        // Ed25519 sign: signed_id.id = signature(64) || id_pk_bytes (matches normal server)
        if signing_sk.len() == 64 {
            let mut sk_arr = [0u8; 64];
            sk_arr.copy_from_slice(&signing_sk);
            signed_id.id = crate::crypto::ed25519_sign(&id_pk_bytes, &sk_arr).into();
            crate::config::write_log("[server] Sent Ed25519-signed IdPk");
        } else {
            signed_id.id = id_pk_bytes.into();
            crate::config::write_log("[server] Sent unsigned IdPk (no signing key)");
        }

        let mut msg = message_proto::Message::new();
        msg.set_signed_id(signed_id);
        stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
    }

    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();

    // Helper closure to process a PublicKey message and set up encryption
    let try_setup_encryption = |pk_msg: &message_proto::PublicKey, stream: &mut FramedStream, eph_sk: &[u8; 32]| {
        let their_pk = &pk_msg.asymmetric_value;
        let sealed_key = &pk_msg.symmetric_value;
        if their_pk.len() == 32 && !sealed_key.is_empty() {
            let mut their_pk_arr = [0u8; 32];
            their_pk_arr.copy_from_slice(their_pk);
            let nonce = [0u8; 24];
            match crate::crypto::crypto_box_open(&their_pk_arr, eph_sk, &nonce, sealed_key) {
                Ok(sym_key) if sym_key.len() == 32 => {
                    let mut key = [0u8; 32];
                    key.copy_from_slice(&sym_key);
                    stream.set_key(key);
                    crate::config::write_log(&format!("[server] Encryption established"));
                    true
                }
                _ => {
                    crate::config::write_log(&format!("[server] Failed to decrypt symmetric key"));
                    false
                }
            }
        } else {
            crate::config::write_log(&format!("[server] Client sent no encryption keys, proceeding unencrypted"));
            false
        }
    };

    let data = stream.recv_msg()?;
    let msg = message_proto::Message::parse_from_bytes(&data).map_err(io_err)?;

    match &msg.union {
        Some(message_proto::message::Union::PublicKey(pk_msg)) => {
            try_setup_encryption(pk_msg, stream, &eph_sk);
        }
        Some(message_proto::message::Union::Misc(_misc)) => {
            crate::config::write_log(&format!("[server] Got Misc from client during handshake"));

            let mut resp = message_proto::UnauthenticatedInitialPublicKeyResponse::new();
            // Send Ed25519 signing pk (not ephemeral Curve25519 pk) — client uses this to verify SignedId
            resp.unauthenticated_initial_public_key = signing_pk.clone().into();
            let mut misc_resp = message_proto::Misc::new();
            misc_resp.set_unauthenticated_initial_public_key_response(resp);
            let mut msg_out = message_proto::Message::new();
            msg_out.set_misc(misc_resp);
            stream.send_msg(&msg_out.write_to_bytes().map_err(io_err)?)?;
            crate::config::write_log(&format!("[server] Sent UnauthenticatedInitialPublicKeyResponse"));

            let data2 = stream.recv_msg()?;
            let msg2 = message_proto::Message::parse_from_bytes(&data2).map_err(io_err)?;
            match &msg2.union {
                Some(message_proto::message::Union::PublicKey(pk_msg)) => {
                    try_setup_encryption(pk_msg, stream, &eph_sk);
                }
                _ => {
                    crate::config::write_log(&format!("[server] Unexpected message after pk exchange"));
                }
            }
        }
        _ => {
            crate::config::write_log(&format!("[server] Got empty/unknown message during handshake, proceeding"));
        }
    }

    let salt = config_salt;
    let challenge = generate_random_string(6);

    let mut hash = message_proto::Hash::new();
    hash.salt = salt.clone();
    hash.challenge = challenge.clone();

    let mut msg = message_proto::Message::new();
    msg.set_hash(hash);
    stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
    crate::config::write_log(&format!("[server] Sent Hash challenge"));

    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();

    let mut lr: message_proto::LoginRequest;
    let login_deadline = Instant::now() + Duration::from_secs(120);
    let mut login_attempt = 0;
    let mut approval_cm_session: Option<String> = None;
    loop {
        if Instant::now() >= login_deadline {
            crate::config::write_log(&format!("[server] Login timeout (120s)"));
            return Ok(());
        }

        let data = match stream.recv_msg() {
            Ok(d) => d,
            Err(e) => {

                let msg = format!("{}", e);
                if msg.contains("timed out") || msg.contains("WouldBlock") {
                    continue;
                }

                crate::config::write_log(&format!("[server] Login recv error: {}", e));
                return Err(e);
            }
        };
        let msg = match message_proto::Message::parse_from_bytes(&data) {
            Ok(m) => m,
            Err(e) => {
                crate::config::write_log(&format!("[server] Login parse error: {}", e));
                continue;
            }
        };

        match &msg.union {
            Some(message_proto::message::Union::TestDelay(td)) => {

                let mut resp = message_proto::TestDelay::new();
                resp.time = td.time;
                resp.last_delay = td.last_delay;
                resp.from_client = false;
                let mut reply = message_proto::Message::new();
                reply.set_test_delay(resp);
                stream.send_msg(&reply.write_to_bytes().map_err(io_err)?)?;
                continue;
            }
            Some(message_proto::message::Union::Misc(_)) => {

                continue;
            }
            Some(message_proto::message::Union::LoginRequest(_)) => {

            }
            Some(message_proto::message::Union::SwitchSidesResponse(ref ssr)) => {

                if let Some(switch_lr) = ssr.lr.as_ref() {
                    let uuid_hex: String = ssr.uuid.iter().map(|b| format!("{:02x}", b)).collect();
                    crate::config::write_log(&format!("[server] SwitchSidesResponse received, UUID={}", uuid_hex));

                    let owned_lr: message_proto::LoginRequest = (*switch_lr).clone();
                    lr = owned_lr;
                    login_attempt = 0;

                    break;
                } else {
                    crate::config::write_log("[server] SwitchSidesResponse missing LoginRequest");
                    continue;
                }
            }
            _ => {
                crate::config::write_log(&format!("[server] Ignoring non-login message during auth"));
                continue;
            }
        }

        lr = match &msg.union {
            Some(message_proto::message::Union::LoginRequest(login_req)) => login_req.clone(),
            _ => continue,
        };

        login_attempt += 1;
        crate::config::write_log(&format!(
            "[server] LoginRequest from {} ({}), platform={} (attempt {})",
            lr.my_id, lr.my_name, lr.my_platform, login_attempt
        ));

        let current_cfg = crate::config::Config::load();
        let current_password = &current_cfg.password;
        let perm_pw = &current_cfg.permanent_password;
        let mut password_ok = false;
        if lr.password.len() == 32 {

            if !current_password.is_empty() {
                let expected = compute_password_hash(current_password, &salt, &challenge);
                let expected_prehashed = compute_prehashed_check(current_password, &challenge);
                if lr.password[..] == expected[..] || lr.password[..] == expected_prehashed[..] {
                    password_ok = true;
                }
            }

            if !password_ok && !perm_pw.is_empty() {
                let expected = compute_password_hash(perm_pw, &salt, &challenge);
                let expected_prehashed = compute_prehashed_check(perm_pw, &challenge);
                if lr.password[..] == expected[..] || lr.password[..] == expected_prehashed[..] {
                    password_ok = true;
                }
            }
        }

        if password_ok {
            crate::config::write_log(&format!("[server] Password OK"));
            break;
        }

        if lr.password.is_empty() {

            crate::config::write_log(&format!("[server] Empty password, launching CM for approval..."));
            let session_id = generate_random_string(8);
            approval_cm_session = Some(session_id.clone());
            cm::write_cm_info(&session_id, &lr.my_id, &lr.my_name, &lr.my_platform);
            cm::spawn_cm_process(&session_id);

            stream.set_read_timeout(Some(Duration::from_millis(200))).ok();
            let cm_deadline = Instant::now() + CM_TIMEOUT;
            let mut cm_accepted = false;
            loop {
                if Instant::now() >= cm_deadline {
                    crate::config::write_log(&format!("[server] CM timeout — rejecting"));
                    cm::signal_cm_ended(&session_id);
                    cm::cleanup_cm_files(&session_id);
                    let mut resp = message_proto::LoginResponse::new();
                    resp.union = Some(message_proto::login_response::Union::Error(
                        "Connection rejected (timeout)".to_string(),
                    ));
                    let mut msg = message_proto::Message::new();
                    msg.set_login_response(resp);
                    stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
                    return Ok(());
                }

                if let Ok(data) = stream.recv_msg() {
                    if let Ok(msg) = message_proto::Message::parse_from_bytes(&data) {
                        match &msg.union {
                            Some(message_proto::message::Union::TestDelay(td)) => {
                                let mut resp = message_proto::TestDelay::new();
                                resp.time = td.time;
                                resp.last_delay = td.last_delay;
                                resp.from_client = false;
                                let mut reply = message_proto::Message::new();
                                reply.set_test_delay(resp);
                                let _ = stream.send_msg(&reply.write_to_bytes().unwrap_or_default());
                            }
                            Some(message_proto::message::Union::LoginRequest(new_lr)) => {

                                if new_lr.password.len() == 32 {
                                    let exp1 = compute_password_hash(password, &salt, &challenge);
                                    let exp1p = compute_prehashed_check(password, &challenge);
                                    let mut pw_ok = new_lr.password[..] == exp1[..] || new_lr.password[..] == exp1p[..];
                                    if !pw_ok && !perm_pw.is_empty() {
                                        let exp2 = compute_password_hash(&perm_pw, &salt, &challenge);
                                        let exp2p = compute_prehashed_check(&perm_pw, &challenge);
                                        pw_ok = new_lr.password[..] == exp2[..] || new_lr.password[..] == exp2p[..];
                                    }
                                    if pw_ok {
                                        crate::config::write_log(&format!("[server] Password received while CM open — accepting"));

                                        lr = new_lr.clone();

                                        cm::signal_cm_connected(&session_id);
                                        cm_accepted = true;
                                        break;
                                    } else {

                                        crate::config::write_log(&format!("[server] Wrong password while CM open, sending error"));
                                        let mut resp = message_proto::LoginResponse::new();
                                        resp.union = Some(message_proto::login_response::Union::Error(
                                            "Wrong Password".to_string(),
                                        ));
                                        let mut err_msg = message_proto::Message::new();
                                        err_msg.set_login_response(resp);
                                        let _ = stream.send_msg(&err_msg.write_to_bytes().unwrap_or_default());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }

                match cm::check_cm_response(&session_id) {
                    Some(true) => {
                        crate::config::write_log(&format!("[server] CM accepted"));

                        cm::signal_cm_connected(&session_id);
                        cm_accepted = true;
                        break;
                    }
                    Some(false) => {
                        crate::config::write_log(&format!("[server] CM rejected"));
                        cm::signal_cm_ended(&session_id);
                        cm::cleanup_cm_files(&session_id);
                        let mut resp = message_proto::LoginResponse::new();
                        resp.union = Some(message_proto::login_response::Union::Error(
                            "Connection rejected".to_string(),
                        ));
                        let mut msg = message_proto::Message::new();
                        msg.set_login_response(resp);
                        stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
                        return Ok(());
                    }
                    None => {}
                }

                std::thread::sleep(CM_POLL_INTERVAL);
            }

            if cm_accepted {
                break;
            }

        } else {

            crate::config::write_log(&format!("[server] Wrong password (attempt {}), sending error and waiting for retry", login_attempt));
            let mut resp = message_proto::LoginResponse::new();
            resp.union = Some(message_proto::login_response::Union::Error(
                "Wrong Password".to_string(),
            ));
            let mut msg = message_proto::Message::new();
            msg.set_login_response(resp);
            stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
        }

        if login_attempt >= 5 {
            crate::config::write_log(&format!("[server] Max login attempts reached"));
            return Ok(());
        }
    }

    // 2FA check
    if let Some(totp_secret) = crate::auth_2fa::get_2fa_secret() {
        crate::config::write_log("[server] 2FA enabled, sending 2FA Required");
        let mut resp_2fa = message_proto::LoginResponse::new();
        resp_2fa.union = Some(message_proto::login_response::Union::Error(
            "2FA Required".to_string(),
        ));
        let mut msg_2fa = message_proto::Message::new();
        msg_2fa.set_login_response(resp_2fa);
        stream.send_msg(&msg_2fa.write_to_bytes().map_err(io_err)?)?;

        // Wait for Auth2FA message
        stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
        let mut tfa_verified = false;
        let mut tfa_attempts = 0;
        while tfa_attempts < 5 {
            let data = match stream.recv_msg() {
                Ok(d) => d,
                Err(_) => {
                    crate::config::write_log("[server] 2FA: connection closed or timeout");
                    return Ok(());
                }
            };
            let msg = match message_proto::Message::parse_from_bytes(&data) {
                Ok(m) => m,
                Err(_) => continue,
            };
            match &msg.union {
                Some(message_proto::message::Union::Auth2fa(tfa)) => {
                    tfa_attempts += 1;
                    if crate::auth_2fa::verify_code(&totp_secret, &tfa.code) {
                        crate::config::write_log("[server] 2FA code verified");
                        tfa_verified = true;
                        break;
                    } else {
                        crate::config::write_log(&format!(
                            "[server] 2FA wrong code (attempt {})",
                            tfa_attempts
                        ));
                        let mut resp_err = message_proto::LoginResponse::new();
                        resp_err.union = Some(message_proto::login_response::Union::Error(
                            "2FA Required".to_string(),
                        ));
                        let mut msg_err = message_proto::Message::new();
                        msg_err.set_login_response(resp_err);
                        stream.send_msg(&msg_err.write_to_bytes().map_err(io_err)?)?;
                    }
                }
                Some(message_proto::message::Union::TestDelay(td)) => {
                    let mut resp_td = message_proto::TestDelay::new();
                    resp_td.time = td.time;
                    resp_td.last_delay = td.last_delay;
                    resp_td.from_client = false;
                    let mut reply = message_proto::Message::new();
                    reply.set_test_delay(resp_td);
                    let _ = stream.send_msg(&reply.write_to_bytes().unwrap_or_default());
                }
                _ => {}
            }
        }
        if !tfa_verified {
            crate::config::write_log("[server] 2FA failed, disconnecting");
            return Ok(());
        }
    }

    crate::config::write_log(&format!("[server] Password OK, sending PeerInfo"));

    let is_file_transfer = lr.union.as_ref().map_or(false, |u| {
        matches!(u, message_proto::login_request::Union::FileTransfer(_))
    });
    let is_port_forward = lr.union.as_ref().map_or(false, |u| {
        matches!(u, message_proto::login_request::Union::PortForward(_))
    });
    let is_terminal = lr.union.as_ref().map_or(false, |u| {
        matches!(u, message_proto::login_request::Union::Terminal(_))
    });
    crate::config::write_log(&format!("[server] Session type: file_transfer={}, port_forward={}, terminal={}", is_file_transfer, is_port_forward, is_terminal));

    // Check feature permissions
    let cfg2_perms = crate::config::Config2::load();
    if is_file_transfer && cfg2_perms.get_option("enable-file-transfer") == "N" {
        crate::config::write_log("[server] File transfer denied — disabled in settings");
        let mut resp = message_proto::LoginResponse::new();
        resp.union = Some(message_proto::login_response::Union::Error(
            "File transfer is not enabled on the remote machine".to_string(),
        ));
        let mut msg = message_proto::Message::new();
        msg.set_login_response(resp);
        stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
        return Ok(());
    }
    if is_terminal && cfg2_perms.get_option("enable-terminal") == "N" {
        crate::config::write_log("[server] Terminal denied — disabled in settings");
        let mut resp = message_proto::LoginResponse::new();
        resp.union = Some(message_proto::login_response::Union::Error(
            "Remote terminal is not enabled on the remote machine".to_string(),
        ));
        let mut msg = message_proto::Message::new();
        msg.set_login_response(resp);
        stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
        return Ok(());
    }
    if is_port_forward && cfg2_perms.get_option("enable-tunnel") == "N" {
        crate::config::write_log("[server] TCP tunneling denied — disabled in settings");
        let mut resp = message_proto::LoginResponse::new();
        resp.union = Some(message_proto::login_response::Union::Error(
            "TCP tunneling is not enabled on the remote machine".to_string(),
        ));
        let mut msg = message_proto::Message::new();
        msg.set_login_response(resp);
        stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
        return Ok(());
    }

    let mut peer_info = message_proto::PeerInfo::new();
    peer_info.username = std::env::var("USERNAME").unwrap_or_default();
    peer_info.hostname = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "XP-PC".into());
    peer_info.platform = "Windows".to_string();
    peer_info.version = env!("CARGO_PKG_VERSION").to_string();

    let displays = capture::enumerate_displays();

    if !is_file_transfer && !is_terminal {
        let mut display_infos = Vec::new();
        for d in &displays {
            let mut di = message_proto::DisplayInfo::new();
            di.x = d.x;
            di.y = d.y;
            di.width = d.width;
            di.height = d.height;
            let name_str: String = d.name.iter().filter(|&&c| c != 0).map(|&c| c as u8 as char).collect();
            di.name = name_str;
            di.online = true;
            display_infos.push(di);
        }
        peer_info.displays = display_infos;
        peer_info.current_display = 0;

        let mut encoding = message_proto::SupportedEncoding::new();
        encoding.vp8 = true;
        peer_info.encoding = protobuf::MessageField::some(encoding);
    }

    let mut features = message_proto::Features::new();
    features.privacy_mode = false;
    features.terminal = cfg2_perms.get_option("enable-terminal") != "N";
    peer_info.features = protobuf::MessageField::some(features);

    let mut resp = message_proto::LoginResponse::new();
    resp.union = Some(message_proto::login_response::Union::PeerInfo(peer_info));

    let mut msg = message_proto::Message::new();
    msg.set_login_response(resp);
    stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;

    crate::config::write_log(&format!("[server] Login successful, starting session"));

    // Send initial permission states so controlling client knows what's allowed
    {
        let cfg2 = crate::config::Config2::load();
        let perms = [
            (message_proto::permission_info::Permission::Keyboard, cfg2.get_option("enable-keyboard") != "N"),
            (message_proto::permission_info::Permission::Clipboard, cfg2.get_option("enable-clipboard") != "N"),
            (message_proto::permission_info::Permission::Audio, true),
            (message_proto::permission_info::Permission::File, cfg2.get_option("enable-file-transfer") != "N"),
            (message_proto::permission_info::Permission::Restart, cfg2.get_option("enable-remote-restart") != "N"),
        ];
        for (perm, enabled) in perms {
            let mut perm_info = message_proto::PermissionInfo::new();
            perm_info.permission = protobuf::EnumOrUnknown::new(perm);
            perm_info.enabled = enabled;
            let mut misc = message_proto::Misc::new();
            misc.set_permission_info(perm_info);
            let mut pmsg = message_proto::Message::new();
            pmsg.set_misc(misc);
            let _ = stream.send_msg(&pmsg.write_to_bytes().unwrap_or_default());
        }
    }

    let cm_session_id = if let Some(ref existing) = approval_cm_session {
        crate::config::write_log(&format!("[server] Reusing approval CM {} for session", existing));

        existing.clone()
    } else {
        let new_id = generate_random_string(8);
        cm::write_cm_info(&new_id, &lr.my_id, &lr.my_name, &lr.my_platform);
        cm::signal_cm_connected(&new_id);
        cm::spawn_cm_process(&new_id);
        new_id
    };

    if let Ok(mut s) = CURRENT_CM_SESSION.lock() {
        *s = Some(cm_session_id.clone());
    }

    if is_file_transfer {
        crate::config::write_log(&format!("[server] File transfer session"));

        let result = run_file_transfer_loop(stream, stop);
        if let Ok(mut s) = CURRENT_CM_SESSION.lock() { *s = None; }
        cm::signal_cm_ended(&cm_session_id);
        cm::cleanup_cm_files(&cm_session_id);
        rotate_password_if_needed();
        return result;
    }

    if is_terminal {
        crate::config::write_log(&format!("[server] Terminal session"));

        let result = crate::terminal_service::run_terminal_loop(stream, stop);
        if let Ok(mut s) = CURRENT_CM_SESSION.lock() { *s = None; }
        cm::signal_cm_ended(&cm_session_id);
        cm::cleanup_cm_files(&cm_session_id);
        rotate_password_if_needed();
        return result;
    }

    platform::try_change_desktop();

    platform::set_prevent_sleep(true);

    let result = run_video_input_loop(stream, &displays, stop, &lr.my_id);

    platform::set_prevent_sleep(false);

    if let Ok(mut s) = CURRENT_CM_SESSION.lock() { *s = None; }
    cm::signal_cm_ended(&cm_session_id);
    cm::cleanup_cm_files(&cm_session_id);
    rotate_password_if_needed();

    result
}

fn rotate_password_if_needed() {
    let cfg2 = crate::config::Config2::load();
    let ua = cfg2.get_option("unattended-access");
    if ua != "Y" {
        let new_pw = crate::config::generate_random_string(6);
        let mut cfg = crate::config::Config::load();
        cfg.password = new_pw;
        cfg.save();
        crate::config::write_log(&format!("[server] Password rotated (unattended access disabled)"));
    }
}

fn run_video_input_loop(
    stream: &mut FramedStream,
    displays: &[capture::DisplayInfo],
    stop: &Arc<AtomicBool>,
    peer_id: &str,
) -> io::Result<()> {
    if displays.is_empty() {
        return Err(io::Error::new(io::ErrorKind::Other, "No displays found"));
    }

    let d = &displays[0];
    let mut capturer = CapturerGDI::new(&d.name, d.width, d.height)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    let screen_width = d.width;
    let screen_height = d.height;

    let mut encoder = vpx::Vp8Encoder::new(screen_width as u32, screen_height as u32)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    crate::config::write_log(&format!("[server] VP8 encoder initialized ({}x{})", screen_width, screen_height));

    let mut frame_buf = Vec::new();
    let mut yuv_buf = Vec::new();
    let target_fps = 10;
    let frame_interval = Duration::from_millis(1000 / target_fps as u64);

    stream.set_read_timeout(Some(Duration::from_millis(1))).ok();

    let mut last_frame = Instant::now();
    let mut last_clipboard_check = Instant::now();
    let mut last_clipboard_text = String::new();
    let clipboard_check_interval = Duration::from_secs(1);
    let mut force_keyframe = true;

    let mut last_cm_check = Instant::now();

    let cfg2 = crate::config::Config2::load();
    let mut keyboard_enabled = cfg2.get_option("enable-keyboard") != "N";
    let mut clipboard_enabled = cfg2.get_option("enable-clipboard") != "N";

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        if last_cm_check.elapsed() >= Duration::from_millis(500) {
            last_cm_check = Instant::now();
            if let Ok(s) = CURRENT_CM_SESSION.lock() {
                if let Some(ref sid) = *s {
                    if cm::cm_rejected_path(sid).exists() {
                        crate::config::write_log(&format!("[server] CM user disconnected session"));

                        let mut misc = message_proto::Misc::new();
                        misc.set_close_reason("The remote partner has closed the session.".to_string());
                        let mut close_msg = message_proto::Message::new();
                        close_msg.set_misc(misc);
                        let _ = stream.send_msg(&close_msg.write_to_bytes().unwrap_or_default());

                        std::thread::sleep(Duration::from_millis(150));
                        break;
                    }
                }
            }
        }

        if platform::desktop_changed() {
            platform::try_change_desktop();
        }

        match stream.recv_msg() {
            Ok(data) => {
                if let Ok(msg) = message_proto::Message::parse_from_bytes(&data) {

                    if let Some(message_proto::message::Union::Misc(ref misc)) = msg.union {
                        if let Some(message_proto::misc::Union::SwitchSidesRequest(ref req)) = misc.union {
                            crate::config::write_log(&format!("[server] Switch Sides request received from {}", peer_id));

                            if let Ok(uuid_bytes) = std::convert::TryInto::<[u8; 16]>::try_into(req.uuid.to_vec()) {
                                let uuid_str = format!(
                                    "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                                    u32::from_be_bytes([uuid_bytes[0], uuid_bytes[1], uuid_bytes[2], uuid_bytes[3]]),
                                    u16::from_be_bytes([uuid_bytes[4], uuid_bytes[5]]),
                                    u16::from_be_bytes([uuid_bytes[6], uuid_bytes[7]]),
                                    uuid_bytes[8], uuid_bytes[9],
                                    uuid_bytes[10], uuid_bytes[11], uuid_bytes[12], uuid_bytes[13], uuid_bytes[14], uuid_bytes[15]
                                );
                                crate::config::write_log(&format!("[server] Spawning reverse connection with UUID {}", uuid_str));

                                let exe = std::env::current_exe().unwrap_or_default();
                                let _ = std::process::Command::new(&exe)
                                    .args(["--connect", peer_id, "--switch_uuid", &uuid_str])
                                    .stdin(std::process::Stdio::null())
                                    .stdout(std::process::Stdio::null())
                                    .stderr(std::process::Stdio::null())
                                    .spawn();
                            }

                            break;
                        }
                    }
                    // Handle Cliprdr (file clipboard) messages separately — may produce multiple replies
                    if let Some(message_proto::message::Union::Cliprdr(ref cliprdr)) = msg.union {
                        if clipboard_enabled {
                            let replies = crate::clipboard_file::handle_cliprdr_host(cliprdr);
                            for reply in replies {
                                let _ = stream.send_msg(&reply.write_to_bytes().unwrap_or_default());
                            }
                        }
                    } else if let Some(reply) = handle_peer_message(&msg, screen_width, screen_height, keyboard_enabled, clipboard_enabled) {
                        let _ = stream.send_msg(&reply.write_to_bytes().unwrap_or_default());
                    }
                }
            }
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) => {
                crate::config::write_log(&format!("[server] Peer disconnected: {}", e));
                break;
            }
        }

        // Process permission toggles from CM BEFORE clipboard check
        if let Ok(s) = CURRENT_CM_SESSION.lock() {
            if let Some(ref sid) = *s {
                let path = cm::cm_perm_path(sid);
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let _ = std::fs::remove_file(&path);
                    if !text.is_empty() {

                        let parts: Vec<&str> = text.splitn(2, '|').collect();
                        if parts.len() == 2 {
                            let perm_name = parts[0];
                            let enabled = parts[1] == "1";
                            crate::config::write_log(&format!("[server] Permission change: {}={}", perm_name, enabled));

                            // Send PermissionInfo to controlling client (this is what it listens for)
                            let perm_enum = match perm_name {
                                "keyboard" => Some(message_proto::permission_info::Permission::Keyboard),
                                "clipboard" => Some(message_proto::permission_info::Permission::Clipboard),
                                "audio" => Some(message_proto::permission_info::Permission::Audio),
                                _ => None,
                            };
                            if let Some(perm) = perm_enum {
                                let mut perm_info = message_proto::PermissionInfo::new();
                                perm_info.permission = protobuf::EnumOrUnknown::new(perm);
                                perm_info.enabled = enabled;
                                let mut misc = message_proto::Misc::new();
                                misc.set_permission_info(perm_info);
                                let mut msg = message_proto::Message::new();
                                msg.set_misc(misc);
                                let _ = stream.send_msg(&msg.write_to_bytes().unwrap_or_default());
                            }

                            match perm_name {
                                "keyboard" => { keyboard_enabled = enabled; }
                                "clipboard" => {
                                    clipboard_enabled = enabled;
                                    if !enabled {
                                        // Clear tracked clipboard so re-enabling doesn't immediately re-send
                                        last_clipboard_text.clear();
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        if clipboard_enabled && last_clipboard_check.elapsed() >= clipboard_check_interval {
            last_clipboard_check = Instant::now();
            // Check for file clipboard first (CF_HDROP takes priority)
            if let Some(msg) = crate::clipboard_file::check_clipboard_files_change() {
                let _ = stream.send_msg(&msg.write_to_bytes().unwrap_or_default());
            } else if let Some(msg) = crate::clipboard::check_clipboard_change(&mut last_clipboard_text) {
                let _ = stream.send_msg(&msg.write_to_bytes().unwrap_or_default());
            }
        }

        if let Ok(s) = CURRENT_CM_SESSION.lock() {
            if let Some(ref sid) = *s {
                let path = cm::cm_chat_send_path(sid);
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let _ = std::fs::remove_file(&path);
                    if !text.is_empty() {
                        let mut chat = message_proto::ChatMessage::new();
                        chat.text = text;
                        let mut misc = message_proto::Misc::new();
                        misc.union = Some(message_proto::misc::Union::ChatMessage(chat));
                        let mut msg = message_proto::Message::new();
                        msg.set_misc(misc);
                        let _ = stream.send_msg(&msg.write_to_bytes().unwrap_or_default());
                    }
                }
            }
        }

        let now = Instant::now();
        if now.duration_since(last_frame) >= frame_interval {
            if let Err(e) = capturer.frame(&mut frame_buf) {
                crate::config::write_log(&format!("[server] Capture error: {}", e));
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }

            vpx::bgra_to_i420(&frame_buf, screen_width as usize, screen_height as usize, &mut yuv_buf);

            match encoder.encode(&mut yuv_buf, force_keyframe) {
                Ok(encoded_frames) => {
                    force_keyframe = false;
                    if !encoded_frames.is_empty() {

                        let mut evfs = message_proto::EncodedVideoFrames::new();
                        for ef in &encoded_frames {
                            let mut evf = message_proto::EncodedVideoFrame::new();
                            evf.data = ef.data.clone().into();
                            evf.key = ef.key;
                            evf.pts = ef.pts;
                            evfs.frames.push(evf);
                        }

                        let mut vf = message_proto::VideoFrame::new();
                        vf.union = Some(message_proto::video_frame::Union::Vp8s(evfs));
                        vf.display = 0;

                        let mut msg = message_proto::Message::new();
                        msg.set_video_frame(vf);

                        if let Err(e) = stream.send_msg(&msg.write_to_bytes().map_err(io_err)?) {
                            crate::config::write_log(&format!("[server] Send VP8 frame error: {}", e));
                            break;
                        }
                    }
                }
                Err(e) => {
                    crate::config::write_log(&format!("[server] VP8 encode error: {}", e));
                }
            }

            last_frame = now;
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    crate::clipboard_file::reset();
    Ok(())
}

fn handle_peer_message(msg: &message_proto::Message, screen_w: i32, screen_h: i32, keyboard_enabled: bool, clipboard_enabled: bool) -> Option<message_proto::Message> {
    match &msg.union {
        Some(message_proto::message::Union::MouseEvent(me)) => {
            if keyboard_enabled {
                handle_mouse_event(me, screen_w, screen_h);
            }
        }
        Some(message_proto::message::Union::KeyEvent(ke)) => {
            if keyboard_enabled {
                handle_key_event(ke);
            }
        }
        Some(message_proto::message::Union::Clipboard(cb)) => {
            if clipboard_enabled {
                crate::clipboard::handle_clipboard_message(cb);
            }
        }
        Some(message_proto::message::Union::MultiClipboards(mc)) => {
            if clipboard_enabled {
                crate::clipboard::handle_multi_clipboards_message(mc);
            }
        }
        Some(message_proto::message::Union::Misc(misc)) => {
            return handle_misc(misc);
        }
        Some(message_proto::message::Union::TestDelay(td)) => {

            let mut resp = message_proto::TestDelay::new();
            resp.time = td.time;
            resp.last_delay = td.last_delay;
            resp.from_client = false;
            let mut reply = message_proto::Message::new();
            reply.set_test_delay(resp);
            return Some(reply);
        }
        _ => {}
    }
    None
}

const MOUSE_TYPE_MOVE: i32 = 0;
const MOUSE_TYPE_DOWN: i32 = 1;
const MOUSE_TYPE_UP: i32 = 2;
const MOUSE_TYPE_WHEEL: i32 = 3;
const MOUSE_TYPE_TRACKPAD: i32 = 4;

const MOUSE_BUTTON_LEFT: i32 = 0x01;
const MOUSE_BUTTON_RIGHT: i32 = 0x02;
const MOUSE_BUTTON_WHEEL: i32 = 0x04;

fn handle_mouse_event(me: &message_proto::MouseEvent, _screen_w: i32, _screen_h: i32) {
    let mask = me.mask;
    let x = me.x;
    let y = me.y;

    let evt_type = mask & 0x7;
    let buttons = mask >> 3;

    match evt_type {
        MOUSE_TYPE_MOVE => {
            input::mouse_move_to(x, y);
        }
        MOUSE_TYPE_DOWN => {

            if x != 0 || y != 0 {
                input::mouse_move_to(x, y);
            }
            match buttons {
                MOUSE_BUTTON_LEFT => input::mouse_down(input::MouseButton::Left),
                MOUSE_BUTTON_RIGHT => input::mouse_down(input::MouseButton::Right),
                MOUSE_BUTTON_WHEEL => input::mouse_down(input::MouseButton::Middle),
                _ => {}
            }
        }
        MOUSE_TYPE_UP => {
            if x != 0 || y != 0 {
                input::mouse_move_to(x, y);
            }
            match buttons {
                MOUSE_BUTTON_LEFT => input::mouse_up(input::MouseButton::Left),
                MOUSE_BUTTON_RIGHT => input::mouse_up(input::MouseButton::Right),
                MOUSE_BUTTON_WHEEL => input::mouse_up(input::MouseButton::Middle),
                _ => {}
            }
        }
        MOUSE_TYPE_WHEEL | MOUSE_TYPE_TRACKPAD => {

            let _hscroll = -x;
            let vscroll = y;
            if vscroll != 0 {
                input::mouse_scroll(vscroll);
            }
        }
        _ => {}
    }
}

fn handle_key_event(ke: &message_proto::KeyEvent) {
    let down = ke.down;
    let mode = ke.mode.enum_value_or(message_proto::KeyboardMode::Legacy);

    match mode {
        message_proto::KeyboardMode::Legacy => legacy_keyboard_mode(ke),
        message_proto::KeyboardMode::Translate => translate_keyboard_mode(ke),
        message_proto::KeyboardMode::Map => map_keyboard_mode(ke),
        _ => legacy_keyboard_mode(ke),
    }

    if ke.press {
        if let Some(message_proto::key_event::Union::ControlKey(ck)) = &ke.union {
            if let Some(vk) = control_key_to_vk(*ck) {
                input::vk_event(vk, true);
                input::vk_event(vk, false);
            }
        }
    }
}

fn legacy_keyboard_mode(ke: &message_proto::KeyEvent) {
    let down = ke.down;

    let has_modifiers = ke.modifiers.iter().any(|m| {
        let k = m.enum_value_or_default();
        matches!(k,
            message_proto::ControlKey::Control |
            message_proto::ControlKey::RControl |
            message_proto::ControlKey::Alt |
            message_proto::ControlKey::RAlt |
            message_proto::ControlKey::Meta |
            message_proto::ControlKey::RWin
        )
    });

    match &ke.union {
        Some(message_proto::key_event::Union::ControlKey(ck)) => {
            if let Some(vk) = control_key_to_vk(*ck) {
                input::vk_event(vk, down);
            }
        }
        Some(message_proto::key_event::Union::Chr(chr)) => {

            input::legacy_char_event(*chr, down, has_modifiers);
        }
        Some(message_proto::key_event::Union::Unicode(ch)) => {

            if let Ok(c) = char::try_from(*ch) {
                if down {
                    input::unicode_char_down(c as u16);
                } else {
                    input::unicode_char_up(c as u16);
                }
            }
        }
        Some(message_proto::key_event::Union::Seq(seq)) => {

            input::unicode_sequence(seq);
        }
        Some(message_proto::key_event::Union::Win2winHotkey(code)) => {
            input::win2win_hotkey_event(*code, down);
        }
        _ => {}
    }
}

fn translate_keyboard_mode(ke: &message_proto::KeyEvent) {
    let down = ke.down;

    match &ke.union {
        Some(message_proto::key_event::Union::Chr(code)) => {
            input::translate_chr_event(*code, down);
        }
        Some(message_proto::key_event::Union::Seq(seq)) => {

            input::unicode_sequence(seq);
        }
        Some(message_proto::key_event::Union::Win2winHotkey(code)) => {
            input::win2win_hotkey_event(*code, down);
        }
        Some(message_proto::key_event::Union::ControlKey(ck)) => {
            if let Some(vk) = control_key_to_vk(*ck) {
                input::vk_event(vk, down);
            }
        }
        _ => {}
    }
}

fn map_keyboard_mode(ke: &message_proto::KeyEvent) {
    let down = ke.down;

    match &ke.union {
        Some(message_proto::key_event::Union::Chr(code)) => {

            input::key_event(*code as u16, down);
        }
        Some(message_proto::key_event::Union::ControlKey(ck)) => {
            if let Some(vk) = control_key_to_vk(*ck) {
                input::vk_event(vk, down);
            }
        }
        Some(message_proto::key_event::Union::Seq(seq)) => {
            input::unicode_sequence(seq);
        }
        _ => {}
    }
}

fn handle_misc(misc: &message_proto::Misc) -> Option<message_proto::Message> {
    match &misc.union {
        Some(message_proto::misc::Union::ChatMessage(chat)) => {
            crate::config::write_log(&format!("[server] Chat: {}", chat.text));

            if let Ok(s) = CURRENT_CM_SESSION.lock() {
                if let Some(ref sid) = *s {
                    cm::append_chat_message(sid, "Peer", &chat.text);
                }
            }
        }
        Some(message_proto::misc::Union::CloseReason(reason)) => {
            crate::config::write_log(&format!("[server] Peer closed: {}", reason));
        }
        Some(message_proto::misc::Union::TogglePrivacyMode(_)) => {

            platform::blank_screen(true);
        }
        Some(message_proto::misc::Union::RestartRemoteDevice(_)) => {
            crate::config::write_log(&format!("[server] Restart requested by remote peer"));
            // Check if remote restart is allowed (default-on: enabled unless "N")
            let restart_opt = crate::config::Config2::load().get_option("enable-remote-restart");
            if restart_opt == "N" {
                crate::config::write_log("[server] Remote restart denied — disabled in settings");
                return None;
            }

            unsafe {
                #[repr(C)]
                struct Luid { low: u32, high: i32 }
                #[repr(C)]
                struct LuidAndAttributes { luid: Luid, attributes: u32 }
                #[repr(C)]
                struct TokenPrivileges { count: u32, privileges: [LuidAndAttributes; 1] }

                extern "system" {
                    fn GetCurrentProcess() -> *mut std::ffi::c_void;
                    fn OpenProcessToken(proc_h: *mut std::ffi::c_void, access: u32, token: *mut *mut std::ffi::c_void) -> i32;
                    fn LookupPrivilegeValueA(system: *const u8, name: *const u8, luid: *mut Luid) -> i32;
                    fn AdjustTokenPrivileges(token: *mut std::ffi::c_void, disable: i32, new_state: *mut TokenPrivileges, buf: u32, prev: *mut std::ffi::c_void, ret: *mut u32) -> i32;
                    fn CloseHandle(h: *mut std::ffi::c_void) -> i32;
                    fn ExitWindowsEx(flags: u32, reason: u32) -> i32;
                }

                let mut token: *mut std::ffi::c_void = std::ptr::null_mut();
                if OpenProcessToken(GetCurrentProcess(), 0x0020 | 0x0008, &mut token) != 0 {
                    let mut luid = Luid { low: 0, high: 0 };
                    if LookupPrivilegeValueA(std::ptr::null(), b"SeShutdownPrivilege\0".as_ptr(), &mut luid) != 0 {
                        let mut tp = TokenPrivileges {
                            count: 1,
                            privileges: [LuidAndAttributes { luid, attributes: 0x00000002 }],
                        };
                        AdjustTokenPrivileges(token, 0, &mut tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
                    }
                    CloseHandle(token);
                }
                let ret = ExitWindowsEx(0x02 | 0x04, 0);
                crate::config::write_log(&format!("[server] ExitWindowsEx returned {}", ret));
            }
        }
        _ => {}
    }
    None
}

fn control_key_to_vk(ck: protobuf::EnumOrUnknown<message_proto::ControlKey>) -> Option<u16> {
    use message_proto::ControlKey;
    let ck = ck.enum_value_or_default();
    let vk = match ck {
        ControlKey::Alt => 0x12,
        ControlKey::Backspace => 0x08,
        ControlKey::CapsLock => 0x14,
        ControlKey::Control => 0x11,
        ControlKey::Delete => 0x2E,
        ControlKey::DownArrow => 0x28,
        ControlKey::End => 0x23,
        ControlKey::Escape => 0x1B,
        ControlKey::F1 => 0x70,
        ControlKey::F2 => 0x71,
        ControlKey::F3 => 0x72,
        ControlKey::F4 => 0x73,
        ControlKey::F5 => 0x74,
        ControlKey::F6 => 0x75,
        ControlKey::F7 => 0x76,
        ControlKey::F8 => 0x77,
        ControlKey::F9 => 0x78,
        ControlKey::F10 => 0x79,
        ControlKey::F11 => 0x7A,
        ControlKey::F12 => 0x7B,
        ControlKey::Home => 0x24,
        ControlKey::LeftArrow => 0x25,
        ControlKey::Meta => 0x5B,
        ControlKey::PageDown => 0x22,
        ControlKey::PageUp => 0x21,
        ControlKey::Return => 0x0D,
        ControlKey::RightArrow => 0x27,
        ControlKey::Shift => 0x10,
        ControlKey::Space => 0x20,
        ControlKey::Tab => 0x09,
        ControlKey::UpArrow => 0x26,
        ControlKey::Numpad0 => 0x60,
        ControlKey::Numpad1 => 0x61,
        ControlKey::Numpad2 => 0x62,
        ControlKey::Numpad3 => 0x63,
        ControlKey::Numpad4 => 0x64,
        ControlKey::Numpad5 => 0x65,
        ControlKey::Numpad6 => 0x66,
        ControlKey::Numpad7 => 0x67,
        ControlKey::Numpad8 => 0x68,
        ControlKey::Numpad9 => 0x69,
        ControlKey::Insert => 0x2D,
        ControlKey::Scroll => 0x91,
        ControlKey::NumLock => 0x90,
        ControlKey::Pause => 0x13,
        ControlKey::Multiply => 0x6A,
        ControlKey::Add => 0x6B,
        ControlKey::Subtract => 0x6D,
        ControlKey::Decimal => 0x6E,
        ControlKey::Divide => 0x6F,
        ControlKey::RShift => 0xA1,
        ControlKey::RControl => 0xA3,
        ControlKey::RAlt => 0xA5,
        ControlKey::Apps => 0x5D,
        ControlKey::RWin => 0x5C,
        ControlKey::LockScreen => {
            platform::lock_screen();
            return None;
        }
        _ => return None,
    };
    Some(vk)
}

pub fn compute_password_hash(password: &str, salt: &str, challenge: &str) -> [u8; 32] {
    crate::crypto::compute_password_hash(password, salt, challenge)
}

fn compute_prehashed_check(password: &str, challenge: &str) -> [u8; 32] {
    use crate::crypto::Sha256;
    let mut h1 = Sha256::new();
    h1.update(password.as_bytes());
    let d1 = h1.finalize();
    let mut h2 = Sha256::new();
    h2.update(&d1);
    h2.update(challenge.as_bytes());
    h2.finalize()
}

fn generate_random_string(len: usize) -> String {
    crate::config::generate_random_string(len)
}

fn run_port_forward_loop(
    stream: &mut FramedStream,
    target_host: &str,
    target_port: u16,
    stop: &Arc<AtomicBool>,
) -> io::Result<()> {
    use std::net::TcpStream as StdTcpStream;

    crate::config::write_log(&format!("[server/pf] Connecting to target {}:{}", target_host, target_port));

    let addrs: Vec<std::net::SocketAddr> =
        std::net::ToSocketAddrs::to_socket_addrs(&(target_host, target_port))?
            .collect();
    if addrs.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "DNS resolve failed for tunnel target"));
    }
    let mut target = StdTcpStream::connect_timeout(&addrs[0], Duration::from_secs(5))?;
    target.set_nodelay(true).ok();
    target.set_nonblocking(true).ok();

    crate::config::write_log("[server/pf] Connected to target, starting bidirectional relay");
    stream.set_read_timeout(Some(Duration::from_millis(20))).ok();

    let mut buf = [0u8; 65536];
    let mut idle_count = 0u32;
    loop {
        if stop.load(Ordering::Relaxed) { break; }

        let mut had_data = false;

        // Remote client → target service (raw bytes)
        match stream.recv_msg() {
            Ok(data) => {
                if data.is_empty() { break; }
                had_data = true;
                use std::io::Write;
                if target.write_all(&data).is_err() { break; }
                target.flush().ok();
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }

        // Target service → remote client (raw bytes)
        match std::io::Read::read(&mut target, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                had_data = true;
                if stream.send_msg(&buf[..n]).is_err() { break; }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }

        if !had_data {
            idle_count += 1;
            if idle_count > 10 {
                std::thread::sleep(Duration::from_millis(5));
            }
        } else {
            idle_count = 0;
        }
    }

    crate::config::write_log("[server/pf] Port forward session ended");
    Ok(())
}

fn run_file_transfer_loop(
    stream: &mut FramedStream,
    stop: &Arc<AtomicBool>,
) -> io::Result<()> {
    crate::config::write_log("[server/ft] Entering file transfer loop");

    stream.set_read_timeout(Some(Duration::from_millis(100))).ok();

    let mut send_jobs: HashMap<i32, SendJob> = HashMap::new();

    let mut write_jobs: HashMap<i32, WriteJob> = HashMap::new();
    let mut msg_count: u64 = 0;
    let mut timeout_count: u64 = 0;

    let mut delayed_dir_sent = false;

    loop {
        if stop.load(Ordering::Relaxed) {
            crate::config::write_log("[server/ft] Stop flag set, exiting loop");
            break;
        }

        let data = match stream.recv_msg() {
            Ok(d) => d,
            Err(ref e) if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => {
                timeout_count += 1;

                if !delayed_dir_sent && timeout_count >= 5 {
                    delayed_dir_sent = true;

                    let home = crate::file_transfer::get_home_dir();
                    let fd = if !home.is_empty() && std::path::Path::new(&home).is_dir() {
                        file_transfer::read_dir_to_proto(&home, false)
                            .unwrap_or_else(|_| file_transfer::get_drives())
                    } else {
                        file_transfer::get_drives()
                    };
                    crate::config::write_log(&format!("[server/ft] Sending delayed initial dir: path='{}' {} entries", fd.path, fd.entries.len()));
                    let mut fr = message_proto::FileResponse::new();
                    fr.set_dir(fd);
                    let mut msg = message_proto::Message::new();
                    msg.set_file_response(fr);
                    let _ = stream.send_msg(&msg.write_to_bytes().map_err(io_err)?);
                }
                if timeout_count % 50 == 1 {
                    crate::config::write_log(&format!("[server/ft] Waiting for messages... (timeouts={})", timeout_count));
                }

                send_pending_blocks(stream, &mut send_jobs)?;
                continue;
            }
            Err(e) => {
                crate::config::write_log(&format!("[server/ft] Peer disconnected: {}", e));
                break;
            }
        };

        msg_count += 1;
        crate::config::write_log(&format!("[server/ft] Received message #{} ({} bytes)", msg_count, data.len()));

        if let Ok(msg) = message_proto::Message::parse_from_bytes(&data) {
            let msg_type = match &msg.union {
                Some(message_proto::message::Union::FileAction(_)) => "FileAction",
                Some(message_proto::message::Union::FileResponse(_)) => "FileResponse",
                Some(message_proto::message::Union::TestDelay(_)) => "TestDelay",
                Some(message_proto::message::Union::Misc(_)) => "Misc",
                Some(_) => "Other",
                None => "None",
            };
            crate::config::write_log(&format!("[server/ft] Message type: {}", msg_type));

            match msg.union {
                Some(message_proto::message::Union::FileAction(fa)) => {
                    handle_file_action(stream, fa, &mut send_jobs, &mut write_jobs)?;
                }
                Some(message_proto::message::Union::FileResponse(fr)) => {
                    handle_file_response_server(stream, fr, &mut write_jobs)?;
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
                    match &misc.union {
                        Some(message_proto::misc::Union::CloseReason(reason)) => {
                            crate::config::write_log(&format!("[server/ft] Peer closed: {}", reason));
                            break;
                        }
                        Some(message_proto::misc::Union::ChatMessage(chat)) => {
                            crate::config::write_log(&format!("[server/ft] Chat: {}", chat.text));
                            if let Ok(s) = CURRENT_CM_SESSION.lock() {
                                if let Some(ref sid) = *s {
                                    cm::append_chat_message(sid, "Peer", &chat.text);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        send_pending_blocks(stream, &mut send_jobs)?;
    }

    Ok(())
}

struct SendJob {
    path: String,
    file_size: u64,
    offset: u64,
    done: bool,
    confirmed: bool,
    id: i32,
    file_num: i32,
    blk_id: u32,
}

struct WriteJob {
    path: String,
    offset: u64,
    id: i32,
    file_num: i32,
    done: bool,
}

fn handle_file_action(
    stream: &mut FramedStream,
    fa: message_proto::FileAction,
    send_jobs: &mut HashMap<i32, SendJob>,
    write_jobs: &mut HashMap<i32, WriteJob>,
) -> io::Result<()> {
    match fa.union {
        Some(message_proto::file_action::Union::ReadDir(rd)) => {
            crate::config::write_log(&format!("[server/ft] ReadDir request: path='{}', hidden={}", rd.path, rd.include_hidden));
            let path = if rd.path.is_empty() { "" } else { &rd.path };
            let fd = if path.is_empty() {
                file_transfer::get_drives()
            } else {
                file_transfer::read_dir_to_proto(path, rd.include_hidden)
                    .unwrap_or_else(|e| {
                        crate::config::write_log(&format!("[server/ft] ReadDir error: {}", e));
                        let mut fd = message_proto::FileDirectory::new();
                        fd.path = path.to_string();
                        fd
                    })
            };
            crate::config::write_log(&format!("[server/ft] ReadDir response: path='{}', entries={}", fd.path, fd.entries.len()));
            let mut fr = message_proto::FileResponse::new();
            fr.set_dir(fd);
            let mut msg = message_proto::Message::new();
            msg.set_file_response(fr);
            stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
        }
        Some(message_proto::file_action::Union::Receive(recv_req)) => {

            let id = recv_req.id;
            let dest_path = recv_req.path.clone();
            crate::config::write_log(&format!("[server/ft] Receive (upload): id={} dest='{}' files_count={}", id, dest_path, recv_req.files.len()));

            if let Some(parent) = std::path::Path::new(&dest_path).parent() {
                if !parent.exists() {
                    let _ = std::fs::create_dir_all(parent);
                    crate::config::write_log(&format!("[server/ft] Created parent dir: '{}'", parent.display()));
                }
            }

            let is_single_file = recv_req.files.len() <= 1
                && recv_req.files.first().map(|f| f.name.is_empty()).unwrap_or(true);
            if is_single_file {

                let p = std::path::Path::new(&dest_path);
                let existing_size = if p.exists() && p.is_file() {
                    p.metadata().map(|m| m.len()).unwrap_or(0)
                } else {
                    0
                };

                let client_file_size = recv_req.files.first().map(|f| f.size).unwrap_or(0);

                crate::config::write_log(&format!("[server/ft] Write job (single): id={} file_num=0 dest='{}' client_size={} existing={}", id, dest_path, client_file_size, existing_size));
                write_jobs.insert(id * 1000, WriteJob {
                    path: dest_path.clone(),
                    offset: existing_size,
                    id,
                    file_num: 0,
                    done: false,
                });

                let mut digest = message_proto::FileTransferDigest::new();
                digest.id = id;
                digest.file_num = 0;
                digest.file_size = client_file_size;
                digest.transferred_size = existing_size;
                let mut fr = message_proto::FileResponse::new();
                fr.set_digest(digest);
                let mut msg = message_proto::Message::new();
                msg.set_file_response(fr);
                stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
                crate::config::write_log(&format!("[server/ft] Sent Digest for upload: id={} file_size={} transferred={}", id, client_file_size, existing_size));
            } else {

                for (i, fe) in recv_req.files.iter().enumerate() {

                    if fe.name.ends_with('/') {
                        let dir_path = std::path::Path::new(&dest_path).join(&fe.name);
                        let _ = std::fs::create_dir_all(&dir_path);
                        crate::config::write_log(&format!("[server/ft] Created empty dir: '{}'", dir_path.display()));
                        continue;
                    }
                    let file_path = std::path::Path::new(&dest_path).join(&fe.name)
                        .to_string_lossy().to_string();
                    let p = std::path::Path::new(&file_path);
                    let existing_size = if p.exists() && p.is_file() {
                        p.metadata().map(|m| m.len()).unwrap_or(0)
                    } else {
                        0
                    };
                    crate::config::write_log(&format!("[server/ft] Write job (multi): id={} file_num={} dest='{}' existing={}", id, i, file_path, existing_size));
                    write_jobs.insert(id * 1000 + i as i32, WriteJob {
                        path: file_path,
                        offset: existing_size,
                        id,
                        file_num: i as i32,
                        done: false,
                    });

                    let mut digest = message_proto::FileTransferDigest::new();
                    digest.id = id;
                    digest.file_num = i as i32;
                    digest.file_size = fe.size;
                    digest.transferred_size = existing_size;
                    let mut fr = message_proto::FileResponse::new();
                    fr.set_digest(digest);
                    let mut msg = message_proto::Message::new();
                    msg.set_file_response(fr);
                    stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
                }
                crate::config::write_log(&format!("[server/ft] Sent {} Digests for multi-file upload", recv_req.files.len()));
            }
        }
        Some(message_proto::file_action::Union::Send(send_req)) => {

            let id = send_req.id;
            let path = send_req.path.clone();
            let file_num = send_req.file_num;
            let include_hidden = send_req.include_hidden;
            crate::config::write_log(&format!("[server/ft] Send request (download): id={} path='{}' file_num={}", id, path, file_num));

            let p = std::path::Path::new(&path);
            if p.is_file() {

                let file_size = p.metadata().map(|m| m.len()).unwrap_or(0);

                let mut entry = message_proto::FileEntry::new();
                entry.name = String::new();
                entry.entry_type = protobuf::EnumOrUnknown::new(message_proto::FileType::File);
                entry.size = file_size;
                if let Ok(meta) = p.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                            entry.modified_time = dur.as_secs();
                        }
                    }
                }
                let mut fd = message_proto::FileDirectory::new();
                fd.id = id;
                fd.path = path.clone();
                fd.entries.push(entry);
                let mut fr_dir = message_proto::FileResponse::new();
                fr_dir.set_dir(fd);
                let mut msg_dir = message_proto::Message::new();
                msg_dir.set_file_response(fr_dir);
                stream.send_msg(&msg_dir.write_to_bytes().map_err(io_err)?)?;
                crate::config::write_log(&format!("[server/ft] Sent Dir for single file: id={} name='{}' size={}", id, path, file_size));

                let mut digest = message_proto::FileTransferDigest::new();
                digest.id = id;
                digest.file_num = file_num;
                digest.file_size = file_size;
                if let Ok(meta) = p.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                            digest.last_modified = dur.as_secs();
                        }
                    }
                }
                let mut fr = message_proto::FileResponse::new();
                fr.set_digest(digest);
                let mut msg = message_proto::Message::new();
                msg.set_file_response(fr);
                stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
                crate::config::write_log(&format!("[server/ft] Sent Digest: id={} file_num={} size={}", id, file_num, file_size));

                send_jobs.insert(id * 1000 + file_num, SendJob {
                    path: path.clone(),
                    file_size,
                    offset: 0,
                    done: false,
                    confirmed: false,
                    id,
                    file_num,
                    blk_id: 0,
                });
                crate::config::write_log(&format!("[server/ft] Queued send job: id={} file_num={} size={}", id, file_num, file_size));
            } else if p.is_dir() {

                let recursive_files = file_transfer::get_recursive_files(&path, include_hidden)
                    .unwrap_or_default();
                crate::config::write_log(&format!("[server/ft] Recursive enumeration of '{}': {} files", path, recursive_files.len()));

                let mut fd = message_proto::FileDirectory::new();
                fd.id = id;
                fd.path = path.clone();
                fd.entries = recursive_files.clone().into();
                let mut fr = message_proto::FileResponse::new();
                fr.set_dir(fd);
                let mut msg = message_proto::Message::new();
                msg.set_file_response(fr);
                stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;

                let mut queued = 0;
                for (i, entry) in recursive_files.iter().enumerate() {

                    let file_path = std::path::Path::new(&path).join(&entry.name);
                    let file_path_str = file_path.to_string_lossy().to_string();
                    let fp = std::path::Path::new(&file_path_str);
                    if !fp.exists() || !fp.is_file() { continue; }
                    let file_size = fp.metadata().map(|m| m.len()).unwrap_or(0);
                    let fnum = i as i32;

                    let mut digest = message_proto::FileTransferDigest::new();
                    digest.id = id;
                    digest.file_num = fnum;
                    digest.file_size = file_size;
                    if let Ok(meta) = fp.metadata() {
                        if let Ok(modified) = meta.modified() {
                            if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                                digest.last_modified = dur.as_secs();
                            }
                        }
                    }
                    let mut fr2 = message_proto::FileResponse::new();
                    fr2.set_digest(digest);
                    let mut msg2 = message_proto::Message::new();
                    msg2.set_file_response(fr2);
                    stream.send_msg(&msg2.write_to_bytes().map_err(io_err)?)?;

                    send_jobs.insert(id * 1000 + fnum, SendJob {
                        path: file_path_str,
                        file_size,
                        offset: 0,
                        done: false,
                        confirmed: false,
                        id,
                        file_num: fnum,
                        blk_id: 0,
                    });
                    queued += 1;
                }
                crate::config::write_log(&format!("[server/ft] Queued {} send jobs for dir '{}'", queued, path));
            } else {

                let mut err = message_proto::FileTransferError::new();
                err.id = id;
                err.file_num = file_num;
                err.error = "Not exists".to_string();
                let mut fr = message_proto::FileResponse::new();
                fr.set_error(err);
                let mut msg = message_proto::Message::new();
                msg.set_file_response(fr);
                stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
                crate::config::write_log(&format!("[server/ft] Send error: path '{}' not found", path));
            }
        }
        Some(message_proto::file_action::Union::SendConfirm(confirm)) => {
            let key = confirm.id * 1000 + confirm.file_num;
            crate::config::write_log(&format!("[server/ft] SendConfirm: id={} file_num={} key={}", confirm.id, confirm.file_num, key));
            if let Some(job) = send_jobs.get_mut(&key) {
                job.confirmed = true;
                match confirm.union {
                    Some(message_proto::file_transfer_send_confirm_request::Union::Skip(true)) => {
                        job.done = true;

                        let mut done = message_proto::FileTransferDone::new();
                        done.id = job.id;
                        done.file_num = job.file_num;
                        let mut fr = message_proto::FileResponse::new();
                        fr.set_done(done);
                        let mut msg = message_proto::Message::new();
                        msg.set_file_response(fr);
                        stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
                    }
                    Some(message_proto::file_transfer_send_confirm_request::Union::OffsetBlk(offset)) => {
                        job.offset = offset as u64 * 128 * 1024;
                        job.blk_id = offset;
                        crate::config::write_log(&format!("[server/ft] SendConfirm: starting from offset blk={} byte_offset={}", offset, job.offset));
                    }
                    _ => {

                        crate::config::write_log("[server/ft] SendConfirm: starting from beginning (no offset)");
                    }
                }
            } else {
                crate::config::write_log(&format!("[server/ft] SendConfirm: no job found for key={}", key));
            }
        }
        Some(message_proto::file_action::Union::Create(create)) => {
            if let Err(e) = std::fs::create_dir_all(&create.path) {
                crate::config::write_log(&format!("[server/ft] Create dir error: {}", e));
            }
        }
        Some(message_proto::file_action::Union::RemoveDir(rd)) => {
            crate::config::write_log(&format!("[server/ft] RemoveDir: path='{}' id={} recursive={}", rd.path, rd.id, rd.recursive));
            let result = if rd.recursive {
                std::fs::remove_dir_all(&rd.path)
            } else {
                std::fs::remove_dir(&rd.path)
            };
            match result {
                Ok(_) => crate::config::write_log(&format!("[server/ft] RemoveDir OK: '{}'", rd.path)),
                Err(e) => crate::config::write_log(&format!("[server/ft] RemoveDir error: {}", e)),
            }
        }
        Some(message_proto::file_action::Union::RemoveFile(rf)) => {
            crate::config::write_log(&format!("[server/ft] RemoveFile: path='{}' id={} file_num={}", rf.path, rf.id, rf.file_num));
            match std::fs::remove_file(&rf.path) {
                Ok(_) => crate::config::write_log(&format!("[server/ft] RemoveFile OK: '{}'", rf.path)),
                Err(e) => crate::config::write_log(&format!("[server/ft] RemoveFile error: {}", e)),
            }
        }
        Some(message_proto::file_action::Union::Cancel(cancel)) => {

            send_jobs.retain(|_, job| {
                if job.id == cancel.id {
                    false
                } else {
                    true
                }
            });
        }
        Some(message_proto::file_action::Union::Rename(rename)) => {
            let src = std::path::Path::new(&rename.path);
            if let Some(parent) = src.parent() {
                let dest = parent.join(&rename.new_name);
                if let Err(e) = std::fs::rename(src, &dest) {
                    crate::config::write_log(&format!("[server/ft] Rename error: {}", e));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_file_response_server(
    stream: &mut FramedStream,
    fr: message_proto::FileResponse,
    write_jobs: &mut HashMap<i32, WriteJob>,
) -> io::Result<()> {

    let fr_type = match &fr.union {
        Some(message_proto::file_response::Union::Block(b)) => format!("Block(id={} fn={} len={})", b.id, b.file_num, b.data.len()),
        Some(message_proto::file_response::Union::Done(d)) => format!("Done(id={} fn={})", d.id, d.file_num),
        Some(message_proto::file_response::Union::Error(e)) => format!("Error(id={} err={})", e.id, e.error),
        Some(message_proto::file_response::Union::Digest(d)) => format!("Digest(id={} fn={} size={})", d.id, d.file_num, d.file_size),
        Some(message_proto::file_response::Union::Dir(d)) => format!("Dir(path={} entries={})", d.path, d.entries.len()),
        _ => "Unknown".to_string(),
    };
    crate::config::write_log(&format!("[server/ft] FileResponse: {} (write_jobs={})", fr_type, write_jobs.len()));

    match fr.union {
        Some(message_proto::file_response::Union::Block(block)) => {
            let key = block.id * 1000 + block.file_num;
            if let Some(job) = write_jobs.get_mut(&key) {
                if job.done { return Ok(()); }

                match file_transfer::write_file_block(&job.path, &block.data, job.offset) {
                    Ok(_) => {
                        job.offset += block.data.len() as u64;
                        if block.blk_id % 100 == 0 {
                            crate::config::write_log(&format!("[server/ft] Writing block: id={} file_num={} blk={} offset={}",
                                block.id, block.file_num, block.blk_id, job.offset));
                        }
                    }
                    Err(e) => {
                        crate::config::write_log(&format!("[server/ft] Write error: id={} file_num={} err={}", block.id, block.file_num, e));
                        job.done = true;

                        let mut err = message_proto::FileTransferError::new();
                        err.id = block.id;
                        err.file_num = block.file_num;
                        err.error = e.to_string();
                        let mut fr_err = message_proto::FileResponse::new();
                        fr_err.set_error(err);
                        let mut msg = message_proto::Message::new();
                        msg.set_file_response(fr_err);
                        stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
                    }
                }
            } else {
                crate::config::write_log(&format!("[server/ft] Received block for unknown job: id={} file_num={} key={}", block.id, block.file_num, key));
            }
        }
        Some(message_proto::file_response::Union::Done(done)) => {
            let key = done.id * 1000 + done.file_num;
            if let Some(job) = write_jobs.get_mut(&key) {
                job.done = true;
                crate::config::write_log(&format!("[server/ft] Write complete: id={} file_num={} path='{}' size={}", done.id, done.file_num, job.path, job.offset));
            }
        }
        _ => {}
    }
    Ok(())
}

fn send_pending_blocks(
    stream: &mut FramedStream,
    send_jobs: &mut HashMap<i32, SendJob>,
) -> io::Result<()> {
    let keys: Vec<i32> = send_jobs.keys().cloned().collect();
    for key in keys {
        let job = match send_jobs.get_mut(&key) {
            Some(j) if !j.done && j.confirmed => j,
            _ => continue,
        };

        if job.offset >= job.file_size {

            let mut done = message_proto::FileTransferDone::new();
            done.id = job.id;
            done.file_num = job.file_num;
            let mut fr = message_proto::FileResponse::new();
            fr.set_done(done);
            let mut msg = message_proto::Message::new();
            msg.set_file_response(fr);
            stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
            job.done = true;
            continue;
        }

        if job.blk_id == 0 {
            crate::config::write_log(&format!("[server/ft] Starting block send: id={} file='{}' size={}", job.id, job.path, job.file_size));
        }
        match file_transfer::read_file_block(&job.path, job.offset) {
            Ok(data) => {
                let data_len = data.len() as u64;
                let mut block = message_proto::FileTransferBlock::new();
                block.id = job.id;
                block.file_num = job.file_num;
                block.data = data.into();
                block.compressed = false;
                block.blk_id = job.blk_id;
                let mut fr = message_proto::FileResponse::new();
                fr.set_block(block);
                let mut msg = message_proto::Message::new();
                msg.set_file_response(fr);
                stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;

                job.offset += data_len;
                job.blk_id += 1;
                if job.blk_id % 100 == 0 {
                    crate::config::write_log(&format!("[server/ft] Sending blocks: id={} blk={} offset={}/{}", job.id, job.blk_id, job.offset, job.file_size));
                }
            }
            Err(e) => {
                let mut err = message_proto::FileTransferError::new();
                err.id = job.id;
                err.file_num = job.file_num;
                err.error = e.to_string();
                let mut fr = message_proto::FileResponse::new();
                fr.set_error(err);
                let mut msg = message_proto::Message::new();
                msg.set_file_response(fr);
                stream.send_msg(&msg.write_to_bytes().map_err(io_err)?)?;
                job.done = true;
            }
        }
    }

    send_jobs.retain(|_, job| !job.done);
    Ok(())
}

fn io_err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}
