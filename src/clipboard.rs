
use crate::platform;
use crate::protocol::message_proto;

fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    let out = crate::file_transfer::zstd_decompress(data);
    if !out.is_empty() {
        return Some(out);
    }
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let limit = crate::file_transfer::MAX_DECOMPRESSED_SIZE;
    let mut out = Vec::new();
    ZlibDecoder::new(data).take(limit + 1).read_to_end(&mut out).ok()?;
    if out.len() as u64 > limit {
        return None;
    }
    Some(out)
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

fn extract_html_from_multi(mc: &message_proto::MultiClipboards) -> Option<String> {
    for cb in &mc.clipboards {
        if cb.format.enum_value_or_default() == message_proto::ClipboardFormat::Html {
            let html = extract_text_from_clipboard(cb)?;
            if !html.is_empty() {
                return Some(html);
            }
        }
    }
    None
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
            let html = extract_html_from_multi(mc);
            platform::set_clipboard_text_and_html(&text, html.as_deref());
        }
    }
}

pub fn make_clipboard_message(text: &str) -> message_proto::Message {
    let mut cb = message_proto::Clipboard::new();
    cb.format = message_proto::ClipboardFormat::Text.into();
    cb.content = text.as_bytes().to_vec().into();

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
