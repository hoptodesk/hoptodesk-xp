
use std::collections::HashMap;
use std::path::PathBuf;
use std::io::Write;

const APP_NAME: &str = "HopToDesk";

const CHARS: &[u8] = b"23456789abcdefghijkmnpqrstuvwxyz";

pub struct Config {
    pub id: String,
    pub password: String,
    pub permanent_password: String,
    pub salt: String,
    pub key_confirmed: bool,
    pub encryption_key: String,

    pub key_pair: (Vec<u8>, Vec<u8>),
    path: PathBuf,
}

pub struct Config2 {
    pub rendezvous_server: String,
    pub nat_type: i32,
    pub serial: i32,
    pub options: HashMap<String, String>,
    path: PathBuf,
}

pub struct PeerConfig {
    pub alias: String,
    pub options: HashMap<String, String>,
}

#[derive(Clone)]
pub struct RecentPeer {
    pub id: String,
    pub username: String,
    pub hostname: String,
    pub platform: String,
}

pub struct LocalConfig {
    pub remote_id: String,
    pub size: (i32, i32, i32, i32),
    pub fav: Vec<String>,
    pub recent_peers: Vec<RecentPeer>,
    pub options: HashMap<String, String>,
    path: PathBuf,
}

fn shared_app_dir() -> Option<PathBuf> {
    if let Ok(pd) = std::env::var("ProgramData") {
        let dir = PathBuf::from(pd).join(APP_NAME);
        if ensure_dir(&dir) {
            return Some(dir);
        }
    }
    if let Ok(au) = std::env::var("ALLUSERSPROFILE") {
        let dir = PathBuf::from(au).join("Application Data").join(APP_NAME);
        if ensure_dir(&dir) {
            return Some(dir);
        }
    }
    let xp_default = PathBuf::from(
        "C:\\Documents and Settings\\All Users\\Application Data",
    )
    .join(APP_NAME);
    if ensure_dir(&xp_default) {
        return Some(xp_default);
    }
    None
}

fn installed_marker_exists() -> bool {
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    PathBuf::from(pf).join(APP_NAME).join("HopToDesk.exe").exists()
}

pub fn app_dir() -> PathBuf {
    if installed_marker_exists() {
        if let Some(shared) = shared_app_dir() {
            if shared.join("config").exists() {
                return shared;
            }
        }
    }

    if let Ok(appdata) = std::env::var("APPDATA") {
        let dir = PathBuf::from(appdata).join(APP_NAME);
        if ensure_dir(&dir) {
            return dir;
        }
    }

    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let dir = PathBuf::from(userprofile)
            .join("Application Data")
            .join(APP_NAME);
        if ensure_dir(&dir) {
            return dir;
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.to_path_buf();
        }
    }

    PathBuf::from(".")
}

pub fn shared_app_dir_pub() -> Option<PathBuf> {
    shared_app_dir()
}

pub fn config_dir() -> PathBuf {
    let dir = app_dir().join("config");
    ensure_dir(&dir);
    dir
}

static mut LOG_SUBDIR: Option<String> = None;

pub fn set_log_subdir(name: &str) {
    unsafe {
        LOG_SUBDIR = Some(name.to_string());
    }
}

pub fn log_dir() -> PathBuf {
    let mut dir = app_dir().join("log");
    unsafe {
        if let Some(ref sub) = LOG_SUBDIR {
            dir = dir.join(sub);
        }
    }
    ensure_dir(&dir);
    dir
}

fn ensure_dir(dir: &PathBuf) -> bool {
    if dir.exists() {
        return true;
    }
    std::fs::create_dir_all(dir).is_ok()
}

fn config_path(suffix: &str) -> PathBuf {
    config_dir().join(format!("{}{}.toml", APP_NAME, suffix))
}

impl Config {
    pub fn load() -> Self {
        let path = config_path("");
        let mut cfg = Config {
            id: String::new(),
            password: String::new(),
            permanent_password: String::new(),
            salt: String::new(),
            key_confirmed: false,
            encryption_key: String::new(),
            key_pair: (Vec::new(), Vec::new()),
            path,
        };
        cfg.read();

        let mut generated = false;
        if cfg.id.is_empty() {
            cfg.id = generate_id();
            generated = true;
        }
        if cfg.password.is_empty() {
            cfg.password = generate_password();
            generated = true;
        }
        if cfg.salt.is_empty() {
            cfg.salt = generate_random_string(6);
            generated = true;
        }
        if cfg.encryption_key.is_empty() {
            cfg.encryption_key = generate_random_string(32);
            generated = true;
        }
        if cfg.key_pair.0.len() != 64 || cfg.key_pair.1.len() != 32
            || cfg.key_pair.0[32..] != cfg.key_pair.1[..] {
            cfg.key_pair = generate_key_pair();
            generated = true;
        }

        if generated {
            cfg.save();
        }
        cfg
    }

    fn read(&mut self) {
        if let Ok(content) = std::fs::read_to_string(&self.path) {
            for line in content.lines() {
                let line = line.trim();
                if let Some((key, val)) = parse_toml_line(line) {
                    match key {
                        "id" => self.id = val,
                        "password" => self.password = val,
                        "permanent_password" => self.permanent_password = val,
                        "salt" => self.salt = val,
                        "key_confirmed" => self.key_confirmed = val == "true",
                        "encryption_key" => self.encryption_key = val,
                        "key_pair_sk" => self.key_pair.0 = hex_decode(&val),
                        "key_pair_pk" => self.key_pair.1 = hex_decode(&val),
                        _ => {}
                    }
                }
            }
        }
    }

    pub fn save(&self) {
        let mut content = format!(
            "id = {}\npassword = {}\nsalt = {}\nkey_confirmed = {}\nkey_pair_sk = {}\nkey_pair_pk = {}\n",
            toml_encode_str(&self.id),
            toml_encode_str(&self.password),
            toml_encode_str(&self.salt),
            self.key_confirmed,
            toml_encode_str(&hex_encode(&self.key_pair.0)),
            toml_encode_str(&hex_encode(&self.key_pair.1))
        );
        if !self.permanent_password.is_empty() {
            content.push_str(&format!("permanent_password = {}\n", toml_encode_str(&self.permanent_password)));
        }
        if !self.encryption_key.is_empty() {
            content.push_str(&format!("encryption_key = {}\n", toml_encode_str(&self.encryption_key)));
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&self.path, &content) {
            write_log(&format!("[config] Failed to save {}: {}", self.path.display(), e));
        }
    }

    pub fn get_salt(&self) -> &str {
        &self.salt
    }
}

impl Config2 {
    pub fn load() -> Self {
        let path = config_path("2");
        let mut cfg = Config2 {
            rendezvous_server: String::new(),
            nat_type: 0,
            serial: 0,
            options: HashMap::new(),
            path,
        };
        cfg.read();
        cfg
    }

    fn read(&mut self) {
        if let Ok(content) = std::fs::read_to_string(&self.path) {
            let mut in_options = false;
            for line in content.lines() {
                let line = line.trim();
                if line == "[options]" {
                    in_options = true;
                    continue;
                }
                if line.starts_with('[') {
                    in_options = false;
                    continue;
                }
                if let Some((key, val)) = parse_toml_line(line) {
                    if in_options {
                        self.options.insert(key.to_string(), val);
                    } else {
                        match key {
                            "rendezvous_server" => self.rendezvous_server = val,
                            "nat_type" => self.nat_type = val.parse().unwrap_or(0),
                            "serial" => self.serial = val.parse().unwrap_or(0),
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    pub fn save(&self) {
        let mut content = String::new();
        if !self.rendezvous_server.is_empty() {
            content.push_str(&format!("rendezvous_server = {}\n", toml_encode_str(&self.rendezvous_server)));
        }
        content.push_str(&format!("nat_type = {}\n", self.nat_type));
        content.push_str(&format!("serial = {}\n", self.serial));

        if !self.options.is_empty() {
            content.push_str("\n[options]\n");
            let mut keys: Vec<&String> = self.options.keys().collect();
            keys.sort();
            for key in keys {
                if let Some(val) = self.options.get(key) {
                    content.push_str(&format!("{} = {}\n", key, toml_encode_str(val)));
                }
            }
        }

        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&self.path, &content) {
            write_log(&format!("[config2] Failed to save {}: {}", self.path.display(), e));
        }
    }

    pub fn get_option(&self, key: &str) -> String {
        self.options.get(key).cloned().unwrap_or_default()
    }

    pub fn set_option(&mut self, key: &str, value: &str) {
        if value.is_empty() {
            self.options.remove(key);
        } else {
            self.options.insert(key.to_string(), value.to_string());
        }
        self.save();
    }

    pub fn get_rendezvous_servers(&self) -> Vec<String> {
        let custom = self.get_option("custom-rendezvous-server");
        if !custom.is_empty() {
            return vec![custom];
        }
        if !self.rendezvous_server.is_empty() {
            return vec![self.rendezvous_server.clone()];
        }
        Vec::new()
    }
}

impl LocalConfig {
    pub fn load() -> Self {
        let path = config_path("_local");
        let mut cfg = LocalConfig {
            remote_id: String::new(),
            size: (0, 0, 0, 0),
            fav: Vec::new(),
            recent_peers: Vec::new(),
            options: HashMap::new(),
            path,
        };
        cfg.read();
        cfg
    }

    fn read(&mut self) {
        if let Ok(content) = std::fs::read_to_string(&self.path) {
            let mut in_options = false;
            let mut in_recent = false;
            for line in content.lines() {
                let line = line.trim();
                if line == "[options]" {
                    in_options = true;
                    in_recent = false;
                    continue;
                }
                if line == "[recent]" {
                    in_recent = true;
                    in_options = false;
                    continue;
                }
                if line.starts_with('[') {
                    in_options = false;
                    in_recent = false;
                    continue;
                }
                if in_recent {

                    if let Some((_, val)) = parse_toml_line(line) {
                        let parts: Vec<&str> = val.splitn(4, '|').collect();
                        if !parts.is_empty() && !parts[0].is_empty() {
                            self.recent_peers.push(RecentPeer {
                                id: parts[0].to_string(),
                                username: parts.get(1).unwrap_or(&"").to_string(),
                                hostname: parts.get(2).unwrap_or(&"").to_string(),
                                platform: parts.get(3).unwrap_or(&"").to_string(),
                            });
                        }
                    }
                    continue;
                }
                if let Some((key, val)) = parse_toml_line(line) {
                    if in_options {
                        self.options.insert(key.to_string(), val);
                    } else {
                        match key {
                            "remote_id" => self.remote_id = val,
                            "fav" => {

                                let trimmed = val.trim_start_matches('[').trim_end_matches(']');
                                for item in trimmed.split(',') {
                                    let item_dec = toml_decode_str(item.trim());
                                    let item = item_dec.as_str();
                                    if !item.is_empty() {
                                        self.fav.push(item.to_string());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    pub fn save(&self) {
        let mut content = String::new();
        if !self.remote_id.is_empty() {
            content.push_str(&format!("remote_id = {}\n", toml_encode_str(&self.remote_id)));
        }
        if self.size != (0, 0, 0, 0) {
            content.push_str(&format!(
                "size = [{}, {}, {}, {}]\n",
                self.size.0, self.size.1, self.size.2, self.size.3
            ));
        }
        if !self.fav.is_empty() {
            content.push_str(&format!(
                "fav = [{}]\n",
                self.fav
                    .iter()
                    .map(|f| toml_encode_str(f))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        if !self.recent_peers.is_empty() {
            content.push_str("\n[recent]\n");
            for (i, p) in self.recent_peers.iter().enumerate() {
                let joined = format!("{}|{}|{}|{}", p.id, p.username, p.hostname, p.platform);
                content.push_str(&format!("peer{} = {}\n", i, toml_encode_str(&joined)));
            }
        }

        if !self.options.is_empty() {
            content.push_str("\n[options]\n");
            let mut keys: Vec<&String> = self.options.keys().collect();
            keys.sort();
            for key in keys {
                if let Some(val) = self.options.get(key) {
                    content.push_str(&format!("{} = {}\n", key, toml_encode_str(val)));
                }
            }
        }

        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&self.path, &content) {
            write_log(&format!("[local] Failed to save {}: {}", self.path.display(), e));
        }
    }

    pub fn get_remote_id(&self) -> &str {
        &self.remote_id
    }

    pub fn set_remote_id(&mut self, id: &str) {
        self.remote_id = id.to_string();
        self.save();
    }

    pub fn add_recent_peer(&mut self, id: &str) {

        self.recent_peers.retain(|p| p.id != id);

        self.recent_peers.insert(0, RecentPeer {
            id: id.to_string(),
            username: String::new(),
            hostname: String::new(),
            platform: String::new(),
        });

        if self.recent_peers.len() > 20 {
            self.recent_peers.truncate(20);
        }
        self.save();
    }

    pub fn config_file(&self) -> &std::path::Path {
        &self.path
    }

    pub fn update_recent_peer(&mut self, id: &str, username: &str, hostname: &str, platform: &str) {
        if self.recent_peers.iter().all(|p| p.id != id) {
            self.recent_peers.insert(
                0,
                RecentPeer {
                    id: id.to_string(),
                    username: String::new(),
                    hostname: String::new(),
                    platform: String::new(),
                },
            );
            if self.recent_peers.len() > 20 {
                self.recent_peers.truncate(20);
            }
        }
        if let Some(peer) = self.recent_peers.iter_mut().find(|p| p.id == id) {
            if !username.is_empty() { peer.username = username.to_string(); }
            if !hostname.is_empty() { peer.hostname = hostname.to_string(); }
            if !platform.is_empty() { peer.platform = platform.to_string(); }
            self.save();
        }
    }

    pub fn remove_recent_peer(&mut self, id: &str) {
        self.recent_peers.retain(|p| p.id != id);
        self.save();
    }

    pub fn toggle_fav(&mut self, id: &str) -> bool {
        if let Some(pos) = self.fav.iter().position(|f| f == id) {
            self.fav.remove(pos);
            self.save();
            false
        } else {
            self.fav.insert(0, id.to_string());
            self.save();
            true
        }
    }

    pub fn is_fav(&self, id: &str) -> bool {
        self.fav.iter().any(|f| f == id)
    }

    pub fn get_option(&self, key: &str) -> String {
        self.options.get(key).cloned().unwrap_or_default()
    }

    pub fn set_option(&mut self, key: &str, value: &str) {
        if value.is_empty() {
            self.options.remove(key);
        } else {
            self.options.insert(key.to_string(), value.to_string());
        }
        self.save();
    }
}

impl PeerConfig {
    pub fn load(id: &str) -> Self {
        let path = peers_dir().join(format!("{}.toml", id));
        let mut cfg = PeerConfig {
            alias: String::new(),
            options: HashMap::new(),
        };
        if let Ok(content) = std::fs::read_to_string(&path) {
            for line in content.lines() {
                if let Some((key, val)) = parse_toml_line(line.trim()) {
                    match key {
                        "alias" => cfg.alias = val,
                        "password" => {

                            cfg.options.insert("password".to_string(), decrypt_password(&val));
                        }
                        _ => { cfg.options.insert(key.to_string(), val); }
                    }
                }
            }
        }
        cfg
    }

    pub fn save(&self, id: &str) {
        let dir = peers_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.toml", id));
        let mut content = String::new();
        if !self.alias.is_empty() {
            content.push_str(&format!("alias = {}\n", toml_encode_str(&self.alias)));
        }
        for (k, v) in &self.options {
            if k == "password" {

                let encrypted = encrypt_password(v);
                content.push_str(&format!("{} = {}\n", k, toml_encode_str(&encrypted)));
            } else {
                content.push_str(&format!("{} = {}\n", k, toml_encode_str(v)));
            }
        }
        let _ = std::fs::write(&path, &content);
    }

    pub fn get_option(&self, key: &str) -> String {
        self.options.get(key).cloned().unwrap_or_default()
    }

    pub fn set_option(&mut self, key: &str, value: &str) {
        if value.is_empty() {
            self.options.remove(key);
        } else {
            self.options.insert(key.to_string(), value.to_string());
        }
    }
}

fn peers_dir() -> PathBuf {
    config_dir().join("peers")
}

fn parse_toml_line(line: &str) -> Option<(&str, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
        return None;
    }
    let eq_pos = line.find('=')?;
    let key = line[..eq_pos].trim();
    let val = line[eq_pos + 1..].trim();
    Some((key, toml_decode_str(val)))
}

pub fn toml_encode_str(val: &str) -> String {
    let needs_basic = val
        .chars()
        .any(|c| c == '\'' || c == '\n' || c == '\r' || c.is_control());
    if !needs_basic {
        format!("'{}'", val)
    } else {
        let mut out = String::with_capacity(val.len() + 2);
        out.push('"');
        for c in val.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}

fn toml_decode_str(val: &str) -> String {
    let val = val.trim();
    let bytes = val.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        val[1..val.len() - 1].to_string()
    } else if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        unescape_basic(&val[1..val.len() - 1])
    } else {
        val.to_string()
    }
}

fn unescape_basic(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let machine_hash = {
        let name = std::env::var("COMPUTERNAME").unwrap_or_default();
        let mut h: u64 = 5381;
        for b in name.bytes() {
            h = h.wrapping_mul(33).wrapping_add(b as u64);
        }
        h
    };
    let combined = seed.wrapping_add(machine_hash as u128);
    format!("{}", 100_000_000 + (combined % 900_000_000) as u64)
}

fn generate_password() -> String {
    generate_random_string(8)
}

pub fn generate_random_bytes(len: usize) -> Vec<u8> {
    if let Some(bytes) = generate_secure_random_bytes(len) {
        return bytes;
    }
    write_log("[WARN] CryptGenRandom failed, falling back to weak RNG - keys may be predictable");
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(std::process::id() as u64);
    let mut result = Vec::with_capacity(len);
    for _ in 0..len {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        result.push((seed >> 33) as u8);
    }
    result
}

pub fn generate_secure_random_bytes(len: usize) -> Option<Vec<u8>> {
    use winapi::um::wincrypt::{
        CryptAcquireContextA, CryptGenRandom, CryptReleaseContext,
        HCRYPTPROV, PROV_RSA_FULL, CRYPT_VERIFYCONTEXT,
    };
    unsafe {
        let mut prov: HCRYPTPROV = 0;
        if CryptAcquireContextA(
            &mut prov,
            std::ptr::null(),
            std::ptr::null(),
            PROV_RSA_FULL,
            CRYPT_VERIFYCONTEXT,
        ) == 0
        {
            return None;
        }
        let mut buf = vec![0u8; len];
        let ok = CryptGenRandom(prov, len as u32, buf.as_mut_ptr());
        CryptReleaseContext(prov, 0);
        if ok == 0 {
            return None;
        }
        Some(buf)
    }
}

pub fn generate_random_string(len: usize) -> String {
    let bytes = generate_random_bytes(len);
    let mut result = String::with_capacity(len);
    for b in bytes {
        let idx = (b as usize) & (CHARS.len() - 1);
        result.push(CHARS[idx] as char);
    }
    result
}

pub fn migrate_old_config() {
    let new_dir = config_dir();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let old = exe_dir.join("HopToDesk.toml");
            let new_path = new_dir.join("HopToDesk.toml");
            if old.exists() && !new_path.exists() {
                write_log(&format!("[config] Migrating config from exe dir: {}", old.display()));
                if let Ok(content) = std::fs::read_to_string(&old) {
                    let _ = std::fs::write(&new_path, &content);
                }
            }
        }
    }

    let app = app_dir();
    let config_files = &[
        "HopToDesk.toml",
        "HopToDesk2.toml",
        "HopToDesk_local.toml",
    ];
    for filename in config_files {
        let old = app.join(filename);
        let new_path = new_dir.join(filename);
        if old.exists() && !new_path.exists() {
            write_log(&format!("[config] Migrating {} to config/", filename));
            if let Ok(content) = std::fs::read_to_string(&old) {
                let _ = std::fs::write(&new_path, &content);

            }
        }
    }

    let old_peers = app.join("peers");
    let new_peers = new_dir.join("peers");
    if old_peers.is_dir() && !new_peers.exists() {
        write_log(&format!("[config] Migrating peers/ to config/peers/"));
        let _ = std::fs::create_dir_all(&new_peers);
        if let Ok(entries) = std::fs::read_dir(&old_peers) {
            for entry in entries.flatten() {
                let src = entry.path();
                if src.is_file() {
                    let dest = new_peers.join(entry.file_name());
                    if let Ok(content) = std::fs::read_to_string(&src) {
                        let _ = std::fs::write(&dest, &content);
                    }
                }
            }
        }
    }
}

fn generate_key_pair() -> (Vec<u8>, Vec<u8>) {
    let seed_vec = generate_random_bytes(32);
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&seed_vec);
    let (sk, pk) = crate::crypto::ed25519_keypair(&seed);
    (sk.to_vec(), pk.to_vec())
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&s[i..i.min(s.len()) + 2.min(s.len() - i)], 16).ok())
        .collect()
}

pub fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i] as u32;
        let b1 = if i + 1 < data.len() { data[i + 1] as u32 } else { 0 };
        let b2 = if i + 2 < data.len() { data[i + 2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        result.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if i + 1 < data.len() {
            result.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if i + 2 < data.len() {
            result.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        i += 3;
    }
    result
}

pub fn log_path() -> PathBuf {
    let date = format_date_now();
    log_dir().join(format!("hoptodesk_r{}.log", date))
}

fn format_date_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let days = (secs / 86400) as i64;
    let (y, m, d) = days_to_ymd(days + 719468);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn days_to_ymd(g: i64) -> (i64, i64, i64) {
    let era = if g >= 0 { g } else { g - 146096 } / 146097;
    let doe = g - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

pub fn mask_ip(addr: &impl std::fmt::Display) -> String {
    let s = addr.to_string();
    if let Some(colon_pos) = s.rfind(':') {
        let ip_part = &s[..colon_pos];
        let port_part = &s[colon_pos..];
        let truncated = ip_part.split('.').take(3).collect::<Vec<&str>>().join(".");
        if truncated != ip_part {
            return format!("{}{}", truncated, port_part);
        }
    }
    let truncated = s.split('.').take(3).collect::<Vec<&str>>().join(".");
    if truncated != s { truncated } else { s }
}

lazy_static::lazy_static! {
    static ref LOG_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}

pub fn write_log(msg: &str) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let line = format!("[{:02}:{:02}:{:02}] {}\n", h, m, s, msg);

    let path = log_path();
    let _guard = LOG_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn cleanup_old_logs() {
    let dir = log_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let cutoff = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(31 * 86400)
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("hoptodesk_r") && name.ends_with(".log") {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        use std::time::UNIX_EPOCH;
                        if let Ok(dur) = modified.duration_since(UNIX_EPOCH) {
                            if dur.as_secs() < cutoff {
                                let _ = std::fs::remove_file(&path);
                            }
                        }
                    }
                }
            }
        }
    }
}

fn base64_decode_byte(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if bytes.is_empty() { return Some(Vec::new()); }
    let mut result = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        let a = base64_decode_byte(bytes[i])?;
        let b = base64_decode_byte(bytes[i + 1])?;
        result.push((a << 2) | (b >> 4));
        if bytes[i + 2] != b'=' {
            let c = base64_decode_byte(bytes[i + 2])?;
            result.push((b << 4) | (c >> 2));
            if bytes[i + 3] != b'=' {
                let d = base64_decode_byte(bytes[i + 3])?;
                result.push((c << 6) | d);
            }
        }
        i += 4;
    }
    Some(result)
}

fn symmetric_crypt(data: &[u8], key: &[u8]) -> Vec<u8> {
    if key.is_empty() { return data.to_vec(); }
    data.iter().enumerate()
        .map(|(i, b)| b ^ key[i % key.len()])
        .collect()
}

fn get_encryption_key() -> Vec<u8> {
    let cfg = Config::load();
    cfg.encryption_key.as_bytes().to_vec()
}

pub fn encrypt_password(plain: &str) -> String {
    if plain.is_empty() { return String::new(); }
    let key = get_encryption_key();
    let encrypted = symmetric_crypt(plain.as_bytes(), &key);
    format!("00{}", base64_encode(&encrypted))
}

pub fn decrypt_password(stored: &str) -> String {
    if stored.is_empty() { return String::new(); }
    if stored.starts_with("00") {
        let data_str = &stored[2..];
        if let Some(bytes) = base64_decode(data_str) {
            let key = get_encryption_key();
            let decrypted = symmetric_crypt(&bytes, &key);
            if let Ok(s) = String::from_utf8(decrypted) {
                return s;
            }
        }
    }

    stored.to_string()
}
