
use crate::platform;
use crate::protocol::message_proto;
use std::sync::Mutex;

const FILEDESCRIPTOR_FORMAT_ID: i32 = 49334;
const FILEDESCRIPTORW_FORMAT_NAME: &str = "FileGroupDescriptorW";
const FILECONTENTS_FORMAT_ID: i32 = 49267;
const FILECONTENTS_FORMAT_NAME: &str = "FileContents";

const FILECONTENTS_SIZE: i32 = 1;
const FILECONTENTS_RANGE: i32 = 2;

const FD_ATTRIBUTES: u32 = 0x04;
const FD_FILESIZE: u32 = 0x40;
const FD_PROGRESSUI: u32 = 0x4000;

const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

const FD_STRUCT_SIZE: usize = 592;

struct ClipFileEntry {
    path: String,
    rel_name: String,
    size: u64,
    is_dir: bool,
}

struct RecvState {
    files: Vec<RecvFileEntry>,
    current_index: usize,
    current_offset: u64,
    temp_dir: String,
    waiting_size: bool,
}

struct RecvFileEntry {
    name: String,
    size: u64,
    is_dir: bool,
}

lazy_static::lazy_static! {
    static ref CLIP_FILES: Mutex<Vec<ClipFileEntry>> = Mutex::new(Vec::new());
    static ref LAST_FILE_PATHS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static ref RECV_STATE: Mutex<Option<RecvState>> = Mutex::new(None);
}

fn enumerate_files(paths: &[String]) -> Vec<ClipFileEntry> {
    let mut entries = Vec::new();
    for path in paths {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }

        if meta.is_dir() {
            entries.push(ClipFileEntry {
                path: path.clone(),
                rel_name: name.clone(),
                size: 0,
                is_dir: true,
            });
            enumerate_dir_recursive(path, &name, &mut entries);
        } else {
            entries.push(ClipFileEntry {
                path: path.clone(),
                rel_name: name,
                size: meta.len(),
                is_dir: false,
            });
        }
    }
    entries
}

fn enumerate_dir_recursive(dir_path: &str, rel_prefix: &str, entries: &mut Vec<ClipFileEntry>) {
    let read_dir = match std::fs::read_dir(dir_path) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy().to_string();
        let full_path = entry.path().to_string_lossy().to_string();
        let rel_name = format!("{}\\{}", rel_prefix, name_str);
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            entries.push(ClipFileEntry {
                path: full_path.clone(),
                rel_name: rel_name.clone(),
                size: 0,
                is_dir: true,
            });
            enumerate_dir_recursive(&full_path, &rel_name, entries);
        } else {
            entries.push(ClipFileEntry {
                path: full_path,
                rel_name,
                size: meta.len(),
                is_dir: false,
            });
        }
    }
}

fn build_file_group_descriptor(files: &[ClipFileEntry]) -> Vec<u8> {
    let total_size = 4 + files.len() * FD_STRUCT_SIZE;
    let mut data = vec![0u8; total_size];

    let count = files.len() as u32;
    data[0..4].copy_from_slice(&count.to_le_bytes());

    for (i, file) in files.iter().enumerate() {
        let offset = 4 + i * FD_STRUCT_SIZE;

        let flags = FD_ATTRIBUTES | FD_FILESIZE | FD_PROGRESSUI;
        data[offset..offset + 4].copy_from_slice(&flags.to_le_bytes());

        let attrs = if file.is_dir {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            FILE_ATTRIBUTE_NORMAL
        };
        data[offset + 36..offset + 40].copy_from_slice(&attrs.to_le_bytes());

        let size_high = (file.size >> 32) as u32;
        data[offset + 64..offset + 68].copy_from_slice(&size_high.to_le_bytes());

        let size_low = file.size as u32;
        data[offset + 68..offset + 72].copy_from_slice(&size_low.to_le_bytes());

        let wide: Vec<u16> = file.rel_name.encode_utf16().collect();
        let max_chars = std::cmp::min(wide.len(), 259);
        for j in 0..max_chars {
            let byte_offset = offset + 72 + j * 2;
            data[byte_offset..byte_offset + 2].copy_from_slice(&wide[j].to_le_bytes());
        }

    }

    data
}

fn parse_file_group_descriptor(data: &[u8]) -> Vec<RecvFileEntry> {
    if data.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut entries = Vec::new();

    for i in 0..count {
        let offset = 4 + i * FD_STRUCT_SIZE;
        if offset + FD_STRUCT_SIZE > data.len() {
            break;
        }

        let attrs = u32::from_le_bytes([
            data[offset + 36],
            data[offset + 37],
            data[offset + 38],
            data[offset + 39],
        ]);
        let is_dir = (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0;

        let size_high = u32::from_le_bytes([
            data[offset + 64],
            data[offset + 65],
            data[offset + 66],
            data[offset + 67],
        ]) as u64;
        let size_low = u32::from_le_bytes([
            data[offset + 68],
            data[offset + 69],
            data[offset + 70],
            data[offset + 71],
        ]) as u64;
        let size = (size_high << 32) | size_low;

        let name_offset = offset + 72;
        let mut name_chars = Vec::new();
        for j in 0..260 {
            let bo = name_offset + j * 2;
            if bo + 2 > data.len() {
                break;
            }
            let ch = u16::from_le_bytes([data[bo], data[bo + 1]]);
            if ch == 0 {
                break;
            }
            name_chars.push(ch);
        }
        let name = String::from_utf16_lossy(&name_chars);

        entries.push(RecvFileEntry { name, size, is_dir });
    }

    entries
}

pub fn check_clipboard_files_change() -> Option<message_proto::Message> {
    let paths = platform::get_clipboard_file_paths()?;
    if paths.is_empty() {
        return None;
    }

    {
        let mut last = LAST_FILE_PATHS.lock().unwrap();
        if *last == paths {
            return None;
        }
        *last = paths.clone();
    }

    let files = enumerate_files(&paths);
    if files.is_empty() {
        return None;
    }

    *CLIP_FILES.lock().unwrap() = files;

    let mut format_list = message_proto::CliprdrServerFormatList::new();
    let mut fmt1 = message_proto::CliprdrFormat::new();
    fmt1.id = FILEDESCRIPTOR_FORMAT_ID;
    fmt1.format = FILEDESCRIPTORW_FORMAT_NAME.to_string();
    let mut fmt2 = message_proto::CliprdrFormat::new();
    fmt2.id = FILECONTENTS_FORMAT_ID;
    fmt2.format = FILECONTENTS_FORMAT_NAME.to_string();
    format_list.formats = vec![fmt1, fmt2];

    let mut cliprdr = message_proto::Cliprdr::new();
    cliprdr.union = Some(message_proto::cliprdr::Union::FormatList(format_list));
    let mut msg = message_proto::Message::new();
    msg.union = Some(message_proto::message::Union::Cliprdr(cliprdr));
    Some(msg)
}

pub fn handle_cliprdr_host(cliprdr: &message_proto::Cliprdr) -> Vec<message_proto::Message> {
    match &cliprdr.union {
        Some(message_proto::cliprdr::Union::FormatListResponse(_)) => {

            vec![]
        }
        Some(message_proto::cliprdr::Union::FormatDataRequest(req)) => {
            handle_format_data_request_host(req.requested_format_id)
        }
        Some(message_proto::cliprdr::Union::FileContentsRequest(req)) => {
            handle_file_contents_request(req)
        }
        Some(message_proto::cliprdr::Union::FormatList(fl)) => {

            handle_incoming_format_list(fl)
        }
        Some(message_proto::cliprdr::Union::FormatDataResponse(resp)) => {
            handle_format_data_response(resp)
        }
        Some(message_proto::cliprdr::Union::FileContentsResponse(resp)) => {
            handle_file_contents_response(resp)
        }
        _ => vec![],
    }
}

pub fn handle_cliprdr_client(cliprdr: &message_proto::Cliprdr) -> Vec<message_proto::Message> {
    match &cliprdr.union {
        Some(message_proto::cliprdr::Union::FormatList(fl)) => {

            handle_incoming_format_list(fl)
        }
        Some(message_proto::cliprdr::Union::FormatListResponse(_)) => {
            vec![]
        }
        Some(message_proto::cliprdr::Union::FormatDataRequest(req)) => {
            handle_format_data_request_host(req.requested_format_id)
        }
        Some(message_proto::cliprdr::Union::FormatDataResponse(resp)) => {
            handle_format_data_response(resp)
        }
        Some(message_proto::cliprdr::Union::FileContentsRequest(req)) => {
            handle_file_contents_request(req)
        }
        Some(message_proto::cliprdr::Union::FileContentsResponse(resp)) => {
            handle_file_contents_response(resp)
        }
        _ => vec![],
    }
}

fn handle_format_data_request_host(requested_format_id: i32) -> Vec<message_proto::Message> {
    if requested_format_id == FILEDESCRIPTOR_FORMAT_ID {
        let files = CLIP_FILES.lock().unwrap();
        if files.is_empty() {
            return vec![make_format_data_response(0x2, vec![])];
        }
        let data = build_file_group_descriptor(&files);
        vec![make_format_data_response(0x1, data)]
    } else {
        vec![make_format_data_response(0x2, vec![])]
    }
}

fn handle_file_contents_request(
    req: &message_proto::CliprdrFileContentsRequest,
) -> Vec<message_proto::Message> {
    let files = CLIP_FILES.lock().unwrap();
    let index = req.list_index as usize;
    if index >= files.len() {
        return vec![make_file_contents_response(0x2, req.stream_id, vec![])];
    }
    let file = &files[index];

    if req.dw_flags == FILECONTENTS_SIZE {
        let mut data = vec![0u8; 8];
        let size_low = file.size as u32;
        let size_high = (file.size >> 32) as u32;
        data[0..4].copy_from_slice(&size_low.to_le_bytes());
        data[4..8].copy_from_slice(&size_high.to_le_bytes());
        vec![make_file_contents_response(0x1, req.stream_id, data)]
    } else if req.dw_flags == FILECONTENTS_RANGE {
        let position =
            (req.n_position_high as u64) << 32 | (req.n_position_low as u64 & 0xFFFFFFFF);
        let requested = req.cb_requested as usize;

        if file.is_dir {
            return vec![make_file_contents_response(0x1, req.stream_id, vec![])];
        }

        match read_file_chunk(&file.path, position, requested) {
            Ok(data) => vec![make_file_contents_response(0x1, req.stream_id, data)],
            Err(_) => vec![make_file_contents_response(0x2, req.stream_id, vec![])],
        }
    } else {
        vec![make_file_contents_response(0x2, req.stream_id, vec![])]
    }
}

fn read_file_chunk(path: &str, offset: u64, size: usize) -> Result<Vec<u8>, std::io::Error> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; size];
    let n = file.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

fn handle_incoming_format_list(
    fl: &message_proto::CliprdrServerFormatList,
) -> Vec<message_proto::Message> {

    let has_fd = fl.formats.iter().any(|f| {
        f.format == FILEDESCRIPTORW_FORMAT_NAME || f.id == FILEDESCRIPTOR_FORMAT_ID
    });

    if !has_fd {
        return vec![make_format_list_response(0x1)];
    }

    let fd_id = fl
        .formats
        .iter()
        .find(|f| f.format == FILEDESCRIPTORW_FORMAT_NAME || f.id == FILEDESCRIPTOR_FORMAT_ID)
        .map(|f| f.id)
        .unwrap_or(FILEDESCRIPTOR_FORMAT_ID);

    vec![
        make_format_list_response(0x1),
        make_format_data_request(fd_id),
    ]
}

fn handle_format_data_response(
    resp: &message_proto::CliprdrServerFormatDataResponse,
) -> Vec<message_proto::Message> {
    if resp.msg_flags != 0x1 || resp.format_data.is_empty() {
        return vec![];
    }

    let entries = parse_file_group_descriptor(&resp.format_data);
    if entries.is_empty() {
        return vec![];
    }

    let temp_dir = format!(
        "{}\\hoptodesk_clip_{}",
        std::env::temp_dir().to_string_lossy().trim_end_matches('\\'),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let _ = std::fs::create_dir_all(&temp_dir);

    for entry in &entries {
        if entry.is_dir {
            let dir_path = format!("{}\\{}", temp_dir, entry.name.replace('/', "\\"));
            let _ = std::fs::create_dir_all(&dir_path);
        }
    }

    let first_file_index = entries.iter().position(|e| !e.is_dir);

    let mut state = RecvState {
        files: entries,
        current_index: first_file_index.unwrap_or(0),
        current_offset: 0,
        temp_dir,
        waiting_size: true,
    };

    if let Some(idx) = first_file_index {
        let msg = make_file_contents_request(idx as i32, FILECONTENTS_SIZE, 0, 8);
        *RECV_STATE.lock().unwrap() = Some(state);
        vec![msg]
    } else {

        finalize_received_files(&state);
        *RECV_STATE.lock().unwrap() = None;
        vec![]
    }
}

fn handle_file_contents_response(
    resp: &message_proto::CliprdrFileContentsResponse,
) -> Vec<message_proto::Message> {
    let mut state_lock = RECV_STATE.lock().unwrap();
    let state = match state_lock.as_mut() {
        Some(s) => s,
        None => return vec![],
    };

    if resp.msg_flags != 0x1 {

        return advance_to_next_file(state);
    }

    let idx = state.current_index;
    if idx >= state.files.len() {
        let s = state_lock.take().unwrap();
        finalize_received_files(&s);
        return vec![];
    }

    if state.waiting_size {

        if resp.requested_data.len() >= 8 {
            let size_low = u32::from_le_bytes([
                resp.requested_data[0],
                resp.requested_data[1],
                resp.requested_data[2],
                resp.requested_data[3],
            ]) as u64;
            let size_high = u32::from_le_bytes([
                resp.requested_data[4],
                resp.requested_data[5],
                resp.requested_data[6],
                resp.requested_data[7],
            ]) as u64;
            state.files[idx].size = (size_high << 32) | size_low;
        }
        state.waiting_size = false;
        state.current_offset = 0;

        if state.files[idx].size == 0 || state.files[idx].is_dir {

            if !state.files[idx].is_dir {
                let path = format!(
                    "{}\\{}",
                    state.temp_dir,
                    state.files[idx].name.replace('/', "\\")
                );
                let _ = std::fs::write(&path, b"");
            }
            return advance_to_next_file(state);
        }

        let chunk_size = std::cmp::min(state.files[idx].size, 4 * 1024 * 1024) as i32;
        vec![make_file_contents_request(
            idx as i32,
            FILECONTENTS_RANGE,
            0,
            chunk_size,
        )]
    } else {

        let file_path = format!(
            "{}\\{}",
            state.temp_dir,
            state.files[idx].name.replace('/', "\\")
        );

        if let Some(parent) = std::path::Path::new(&file_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let write_result = if state.current_offset == 0 {
            std::fs::write(&file_path, &resp.requested_data)
        } else {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .open(&file_path)
                .and_then(|mut f| f.write_all(&resp.requested_data))
        };

        if write_result.is_err() {
            return advance_to_next_file(state);
        }

        state.current_offset += resp.requested_data.len() as u64;

        if state.current_offset >= state.files[idx].size {

            return advance_to_next_file(state);
        }

        let remaining = state.files[idx].size - state.current_offset;
        let chunk_size = std::cmp::min(remaining, 4 * 1024 * 1024) as i32;
        let pos_low = state.current_offset as i32;
        let pos_high = (state.current_offset >> 32) as i32;
        vec![make_file_contents_request_full(
            idx as i32,
            FILECONTENTS_RANGE,
            pos_low,
            pos_high,
            chunk_size,
        )]
    }
}

fn advance_to_next_file(state: &mut RecvState) -> Vec<message_proto::Message> {

    let next = state
        .files
        .iter()
        .enumerate()
        .skip(state.current_index + 1)
        .find(|(_, e)| !e.is_dir)
        .map(|(i, _)| i);

    match next {
        Some(idx) => {
            state.current_index = idx;
            state.current_offset = 0;
            state.waiting_size = true;
            vec![make_file_contents_request(idx as i32, FILECONTENTS_SIZE, 0, 8)]
        }
        None => {

            finalize_received_files(state);
            vec![]
        }
    }
}

fn finalize_received_files(state: &RecvState) {

    let mut top_level_paths = Vec::new();
    for entry in &state.files {

        if !entry.name.contains('\\') {
            let path = format!("{}\\{}", state.temp_dir, entry.name);
            top_level_paths.push(path);
        }
    }

    if !top_level_paths.is_empty() {
        platform::set_clipboard_files(&top_level_paths);
    }
}

pub fn reset() {
    *CLIP_FILES.lock().unwrap() = Vec::new();
    *LAST_FILE_PATHS.lock().unwrap() = Vec::new();
    *RECV_STATE.lock().unwrap() = None;
}

fn make_format_list_response(flags: i32) -> message_proto::Message {
    let mut resp = message_proto::CliprdrServerFormatListResponse::new();
    resp.msg_flags = flags;
    let mut cliprdr = message_proto::Cliprdr::new();
    cliprdr.union = Some(message_proto::cliprdr::Union::FormatListResponse(resp));
    let mut msg = message_proto::Message::new();
    msg.union = Some(message_proto::message::Union::Cliprdr(cliprdr));
    msg
}

fn make_format_data_request(format_id: i32) -> message_proto::Message {
    let mut req = message_proto::CliprdrServerFormatDataRequest::new();
    req.requested_format_id = format_id;
    let mut cliprdr = message_proto::Cliprdr::new();
    cliprdr.union = Some(message_proto::cliprdr::Union::FormatDataRequest(req));
    let mut msg = message_proto::Message::new();
    msg.union = Some(message_proto::message::Union::Cliprdr(cliprdr));
    msg
}

fn make_format_data_response(flags: i32, data: Vec<u8>) -> message_proto::Message {
    let mut resp = message_proto::CliprdrServerFormatDataResponse::new();
    resp.msg_flags = flags;
    resp.format_data = data;
    let mut cliprdr = message_proto::Cliprdr::new();
    cliprdr.union = Some(message_proto::cliprdr::Union::FormatDataResponse(resp));
    let mut msg = message_proto::Message::new();
    msg.union = Some(message_proto::message::Union::Cliprdr(cliprdr));
    msg
}

fn make_file_contents_response(flags: i32, stream_id: i32, data: Vec<u8>) -> message_proto::Message {
    let mut resp = message_proto::CliprdrFileContentsResponse::new();
    resp.msg_flags = flags;
    resp.stream_id = stream_id;
    resp.requested_data = data;
    let mut cliprdr = message_proto::Cliprdr::new();
    cliprdr.union = Some(message_proto::cliprdr::Union::FileContentsResponse(resp));
    let mut msg = message_proto::Message::new();
    msg.union = Some(message_proto::message::Union::Cliprdr(cliprdr));
    msg
}

fn make_file_contents_request(
    list_index: i32,
    dw_flags: i32,
    position: u64,
    cb_requested: i32,
) -> message_proto::Message {
    let pos_low = position as i32;
    let pos_high = (position >> 32) as i32;
    make_file_contents_request_full(list_index, dw_flags, pos_low, pos_high, cb_requested)
}

fn make_file_contents_request_full(
    list_index: i32,
    dw_flags: i32,
    n_position_low: i32,
    n_position_high: i32,
    cb_requested: i32,
) -> message_proto::Message {
    let mut req = message_proto::CliprdrFileContentsRequest::new();
    req.stream_id = list_index;
    req.list_index = list_index;
    req.dw_flags = dw_flags;
    req.n_position_low = n_position_low;
    req.n_position_high = n_position_high;
    req.cb_requested = cb_requested;
    let mut cliprdr = message_proto::Cliprdr::new();
    cliprdr.union = Some(message_proto::cliprdr::Union::FileContentsRequest(req));
    let mut msg = message_proto::Message::new();
    msg.union = Some(message_proto::message::Union::Cliprdr(cliprdr));
    msg
}
