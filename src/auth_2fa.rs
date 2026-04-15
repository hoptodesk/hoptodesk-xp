
use crate::config::{self, Config2};
use ring::hmac;
use std::sync::Mutex;

const ISSUER: &str = "HopToDesk";
const TAG_LOGIN: &str = "Connection";
const DIGITS: u32 = 6;
const PERIOD: u64 = 30;

lazy_static::lazy_static! {
    static ref PENDING_SECRET: Mutex<Option<Vec<u8>>> = Mutex::new(None);
}

fn base32_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut result = String::new();
    let mut buffer: u64 = 0;
    let mut bits = 0;
    for &byte in data {
        buffer = (buffer << 8) | byte as u64;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            result.push(ALPHABET[((buffer >> bits) & 0x1F) as usize] as char);
        }
    }
    if bits > 0 {
        buffer <<= 5 - bits;
        result.push(ALPHABET[(buffer & 0x1F) as usize] as char);
    }
    while result.len() % 8 != 0 {
        result.push('=');
    }
    result
}

fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=').to_uppercase();
    let mut buffer: u64 = 0;
    let mut bits = 0;
    let mut result = Vec::new();
    for c in s.chars() {
        let val = match c {
            'A'..='Z' => c as u64 - 'A' as u64,
            '2'..='7' => c as u64 - '2' as u64 + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | val;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            result.push((buffer >> bits) as u8);
        }
    }
    Some(result)
}

fn generate_totp(secret: &[u8], time: u64) -> String {
    let counter = time / PERIOD;
    let counter_bytes = counter.to_be_bytes();

    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let signature = hmac::sign(&key, &counter_bytes);
    let hash = signature.as_ref();

    let offset = (hash[hash.len() - 1] & 0x0F) as usize;
    let code = ((hash[offset] as u32 & 0x7F) << 24)
        | ((hash[offset + 1] as u32) << 16)
        | ((hash[offset + 2] as u32) << 8)
        | (hash[offset + 3] as u32);

    let modulus = 10u32.pow(DIGITS);
    format!("{:0>width$}", code % modulus, width = DIGITS as usize)
}

fn current_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn verify_code(secret: &[u8], code: &str) -> bool {
    let time = current_time();
    // Check current period and ±1 window for clock skew
    for offset in [0i64, -1, 1] {
        let t = (time as i64 + offset * PERIOD as i64) as u64;
        if generate_totp(secret, t) == code {
            return true;
        }
    }
    false
}

fn xor_encrypt(data: &[u8], key_material: &str) -> Vec<u8> {
    let mut hasher = crate::crypto::Sha256::new();
    hasher.update(key_material.as_bytes());
    let key = hasher.finalize();
    data.iter().enumerate().map(|(i, &b)| b ^ key[i % 32]).collect()
}

fn save_secret(secret: &[u8]) {
    let encrypted = xor_encrypt(secret, "hoptodesk-2fa-key");
    let encoded = base32_encode(&encrypted);
    let mut cfg2 = Config2::load();
    cfg2.set_option("2fa", &encoded);
    cfg2.save();
}

fn load_secret() -> Option<Vec<u8>> {
    let cfg2 = Config2::load();
    let encoded = cfg2.get_option("2fa");
    if encoded.is_empty() {
        return None;
    }
    let encrypted = base32_decode(&encoded)?;
    let secret = xor_encrypt(&encrypted, "hoptodesk-2fa-key");
    // Verify it's a valid TOTP secret (should be 20 bytes for SHA1)
    if secret.len() < 10 || secret.len() > 64 {
        return None;
    }
    Some(secret)
}

pub fn has_valid_2fa() -> bool {
    load_secret().is_some()
}

pub fn generate2fa() -> String {
    let cfg = config::Config::load();
    let id = cfg.id.clone();

    let mut secret = vec![0u8; 20];
    let rng = ring::rand::SystemRandom::new();
    if let Err(e) = ring::rand::SecureRandom::fill(&rng, &mut secret) {
        config::write_log(&format!("[2FA] ring random failed: {}, using fallback", e));
        // Fallback: use system time + pointer as entropy
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let seed = t.as_nanos();
        for i in 0..20 {
            secret[i] = ((seed >> (i * 3)) & 0xFF) as u8;
        }
    }

    let b32_secret = base32_encode(&secret);
    let url = format!(
        "otpauth://totp/{issuer}%20{tag}:{name}?secret={secret}&issuer={issuer}%20{tag}&digits={digits}&period={period}&algorithm=SHA1",
        issuer = ISSUER,
        tag = TAG_LOGIN,
        name = id,
        secret = b32_secret,
        digits = DIGITS,
        period = PERIOD
    );

    *PENDING_SECRET.lock().unwrap() = Some(secret);
    url
}

pub fn verify2fa(code: &str) -> bool {
    let pending = PENDING_SECRET.lock().unwrap().clone();
    if let Some(secret) = pending {
        if verify_code(&secret, code) {
            save_secret(&secret);
            *PENDING_SECRET.lock().unwrap() = None;
            config::write_log("[2FA] 2FA setup verified and saved");
            return true;
        }
    }
    false
}

pub fn disable_2fa() {
    let mut cfg2 = Config2::load();
    cfg2.set_option("2fa", "");
    cfg2.save();
    config::write_log("[2FA] 2FA disabled");
}

pub fn get_2fa_secret() -> Option<Vec<u8>> {
    load_secret()
}

/// Generate a QR code BMP image as a base64 data URI string.
pub fn generate_qr_data_uri(data: &str) -> String {
    use qrcode::QrCode;
    let code = match QrCode::new(data.as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            config::write_log(&format!("[2FA] QR generation failed: {}", e));
            return String::new();
        }
    };
    let matrix = code.to_colors();
    let width = code.width();
    let scale = 4; // pixels per module
    let border = 4; // quiet zone modules
    let img_w = (width + border * 2) * scale;
    let img_h = img_w;

    // Generate 1-bit BMP
    let row_bytes = ((img_w + 31) / 32) * 4; // rows padded to 4-byte boundary
    let pixel_data_size = row_bytes * img_h;
    let file_size = 14 + 40 + 8 + pixel_data_size; // header + DIB + color table + pixels

    let mut bmp = Vec::with_capacity(file_size);

    // BMP file header (14 bytes)
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(file_size as u32).to_le_bytes());
    bmp.extend_from_slice(&[0u8; 4]); // reserved
    bmp.extend_from_slice(&62u32.to_le_bytes()); // pixel data offset = 14+40+8

    // DIB header (BITMAPINFOHEADER, 40 bytes)
    bmp.extend_from_slice(&40u32.to_le_bytes()); // header size
    bmp.extend_from_slice(&(img_w as i32).to_le_bytes()); // width
    bmp.extend_from_slice(&(img_h as i32).to_le_bytes()); // height (positive = bottom-up)
    bmp.extend_from_slice(&1u16.to_le_bytes()); // color planes
    bmp.extend_from_slice(&1u16.to_le_bytes()); // bits per pixel
    bmp.extend_from_slice(&0u32.to_le_bytes()); // compression (none)
    bmp.extend_from_slice(&(pixel_data_size as u32).to_le_bytes());
    bmp.extend_from_slice(&2835u32.to_le_bytes()); // h resolution (72 DPI)
    bmp.extend_from_slice(&2835u32.to_le_bytes()); // v resolution
    bmp.extend_from_slice(&2u32.to_le_bytes()); // colors in palette
    bmp.extend_from_slice(&0u32.to_le_bytes()); // important colors

    // Color table: index 0 = black (dark module), index 1 = white (light)
    bmp.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // black
    bmp.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0x00]); // white

    // Pixel data (bottom-up rows)
    for y in (0..img_h).rev() {
        let mut row = vec![0u8; row_bytes];
        for x in 0..img_w {
            // Avoid negative integer division (Rust truncates toward zero, not floor)
            let px = x as isize - (border * scale) as isize;
            let py = y as isize - (border * scale) as isize;
            let is_dark = if px >= 0 && py >= 0 {
                let mx = px as usize / scale;
                let my = py as usize / scale;
                mx < width && my < width && matrix[my * width + mx] == qrcode::Color::Dark
            } else {
                false // quiet zone is white
            };
            if !is_dark {
                // Set bit to 1 (white) — in 1-bit BMP, bit=1 maps to color index 1
                let byte_idx = x / 8;
                let bit_idx = 7 - (x % 8);
                row[byte_idx] |= 1 << bit_idx;
            }
        }
        bmp.extend_from_slice(&row);
    }

    let b64 = config::base64_encode(&bmp);
    format!("data:image/bmp;base64,{}", b64)
}
