
use crate::platform;
use crate::protocol::message_proto;

fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    Some(out)
}

fn compress(data: &[u8]) -> Option<Vec<u8>> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).ok()?;
    let compressed = encoder.finish().ok()?;
    if compressed.len() < data.len() {
        Some(compressed)
    } else {
        None
    }
}

fn extract_text_from_clipboard(cb: &message_proto::Clipboard) -> Option<String> {
    if cb.content.is_empty() {
        return None;
    }
    let bytes = if cb.compress {
        decompress(&cb.content)?
    } else {
        cb.content.to_vec()
    };
    String::from_utf8(bytes).ok()
}

fn extract_text_from_multi(mc: &message_proto::MultiClipboards) -> Option<String> {
    for cb in &mc.clipboards {
        let fmt = cb.format.enum_value_or_default();
        if fmt == message_proto::ClipboardFormat::Text {
            return extract_text_from_clipboard(cb);
        }
    }

    mc.clipboards.first().and_then(extract_text_from_clipboard)
}

pub fn handle_clipboard_message(cb: &message_proto::Clipboard) {
    if let Some(text) = extract_text_from_clipboard(cb) {
        if !text.is_empty() {
            platform::set_clipboard_text(&text);
        }
    }
}

pub fn handle_multi_clipboards_message(mc: &message_proto::MultiClipboards) {
    if let Some(text) = extract_text_from_multi(mc) {
        if !text.is_empty() {
            platform::set_clipboard_text(&text);
        }
    }
}

pub fn make_clipboard_message(text: &str) -> message_proto::Message {
    let raw = text.as_bytes();
    let mut cb = message_proto::Clipboard::new();
    cb.format = message_proto::ClipboardFormat::Text.into();

    if let Some(compressed) = compress(raw) {
        cb.compress = true;
        cb.content = compressed;
    } else {
        cb.compress = false;
        cb.content = raw.to_vec();
    }

    let mut msg = message_proto::Message::new();
    msg.set_clipboard(cb);
    msg
}

pub fn check_clipboard_change(last_text: &mut String) -> Option<message_proto::Message> {
    let text = platform::get_clipboard_text()?;
    if text.is_empty() || text == *last_text {
        return None;
    }
    *last_text = text.clone();
    Some(make_clipboard_message(&text))
}
