
use crate::protocol::message_proto;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

const BLOCK_SIZE: usize = 128 * 1024;

pub fn read_dir_to_proto(path: &str, include_hidden: bool) -> io::Result<message_proto::FileDirectory> {
    let dir = Path::new(path);
    if !dir.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("Path not found: {}", path)));
    }

    let mut fd = message_proto::FileDirectory::new();
    fd.path = path.to_string();

    if dir.is_dir() {
        let entries = std::fs::read_dir(dir)?;
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().to_string();

            let is_hidden = is_file_hidden(&entry.path());
            if is_hidden && !include_hidden {
                continue;
            }

            let mut fe = message_proto::FileEntry::new();
            fe.name = name;
            fe.is_hidden = is_hidden;

            if meta.is_dir() {
                fe.entry_type = message_proto::FileType::Dir.into();
                fe.size = 0;
            } else {
                fe.entry_type = message_proto::FileType::File.into();
                fe.size = meta.len();
            }

            if let Ok(modified) = meta.modified() {
                if let Ok(dur) = modified.duration_since(UNIX_EPOCH) {
                    fe.modified_time = dur.as_secs();
                }
            }

            fd.entries.push(fe);
        }
    }

    fd.entries.sort_by(|a, b| {
        let a_dir = a.entry_type.enum_value_or_default() == message_proto::FileType::Dir;
        let b_dir = b.entry_type.enum_value_or_default() == message_proto::FileType::Dir;
        if a_dir != b_dir {
            return if a_dir { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
        }
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });

    Ok(fd)
}

fn is_file_hidden(path: &Path) -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileAttributesW(lpFileName: *const u16) -> u32;
    }
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const INVALID_FILE_ATTRIBUTES: u32 = 0xFFFFFFFF;

    let wide: Vec<u16> = path.to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let attrs = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if attrs == INVALID_FILE_ATTRIBUTES {
        return false;
    }
    (attrs & FILE_ATTRIBUTE_HIDDEN) != 0
}

pub fn get_home_dir() -> String {
    std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".to_string())
}

pub fn get_path_sep(is_remote: bool, remote_platform: &str) -> String {
    if is_remote {
        if remote_platform.to_lowercase().contains("linux")
            || remote_platform.to_lowercase().contains("mac")
        {
            return "/".to_string();
        }
    }
    "\\".to_string()
}

pub fn read_file_block(path: &str, offset: u64) -> io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; BLOCK_SIZE];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

pub fn read_file_blocks(path: &str) -> io::Result<Vec<Vec<u8>>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut blocks = Vec::new();
    loop {
        let mut buf = vec![0u8; BLOCK_SIZE];
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        buf.truncate(n);
        blocks.push(buf);
    }
    Ok(blocks)
}

pub fn zstd_wrap_raw(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    out.extend_from_slice(&[0x28, 0xB5, 0x2F, 0xFD]);
    out.push(0xA0);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    if data.is_empty() {
        let header: u32 = 1;
        out.extend_from_slice(&header.to_le_bytes()[..3]);
        return out;
    }
    let max_block = 128 * 1024;
    let mut offset = 0;
    while offset < data.len() {
        let chunk_len = std::cmp::min(max_block, data.len() - offset);
        let last: u32 = if offset + chunk_len >= data.len() { 1 } else { 0 };
        let header: u32 = ((chunk_len as u32) << 3) | last;
        out.extend_from_slice(&header.to_le_bytes()[..3]);
        out.extend_from_slice(&data[offset..offset + chunk_len]);
        offset += chunk_len;
    }
    out
}

pub fn zstd_decompress(data: &[u8]) -> Vec<u8> {
    use std::io::Read;
    match ruzstd::StreamingDecoder::new(data) {
        Ok(mut decoder) => {
            let mut out = Vec::new();
            if decoder.read_to_end(&mut out).is_ok() {
                out
            } else {
                Vec::new()
            }
        }
        Err(_) => Vec::new(),
    }
}

pub fn write_file_block(path: &str, data: &[u8], offset: u64) -> io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};

    if data.is_empty() {
        return Ok(());
    }

    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(offset == 0)
        .open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    f.write_all(data)?;
    Ok(())
}

pub fn get_drives() -> message_proto::FileDirectory {
    let mut fd = message_proto::FileDirectory::new();
    fd.path = "/".to_string();

    for letter in b'C'..=b'Z' {
        let check_path = format!("{}:\\", letter as char);
        if Path::new(&check_path).exists() {
            let mut fe = message_proto::FileEntry::new();
            fe.name = format!("{}:", letter as char);
            fe.entry_type = message_proto::FileType::DirDrive.into();
            fd.entries.push(fe);
        }
    }
    fd
}

#[derive(Clone, Debug)]
pub struct TransferJob {
    pub id: i32,
    pub path: String,
    pub to: String,
    pub file_num: i32,
    pub is_remote: bool,
    pub include_hidden: bool,
    pub files: Vec<message_proto::FileEntry>,
    pub finished_size: u64,
    pub total_size: u64,
    pub cancelled: bool,
    pub no_confirm: bool,
}

impl TransferJob {
    pub fn new(id: i32, path: String, to: String, file_num: i32, include_hidden: bool, is_remote: bool) -> Self {
        Self {
            id,
            path,
            to,
            file_num,
            is_remote,
            include_hidden,
            files: Vec::new(),
            finished_size: 0,
            total_size: 0,
            cancelled: false,
            no_confirm: false,
        }
    }
}

pub fn file_directory_to_value(fd: &message_proto::FileDirectory) -> sciter::Value {
    let mut v = sciter::Value::map();
    v.set_item(sciter::Value::from("id"), sciter::Value::from(fd.id));
    v.set_item(sciter::Value::from("path"), sciter::Value::from(fd.path.as_str()));

    let mut entries = sciter::Value::array(0);
    for fe in &fd.entries {
        let mut e = sciter::Value::map();
        let entry_type = fe.entry_type.enum_value_or_default();
        let type_num = match entry_type {
            message_proto::FileType::Dir => 1,
            message_proto::FileType::DirLink => 2,
            message_proto::FileType::DirDrive => 3,
            _ => 0,
        };
        e.set_item(sciter::Value::from("type"), sciter::Value::from(type_num));
        e.set_item(sciter::Value::from("name"), sciter::Value::from(fe.name.as_str()));
        e.set_item(sciter::Value::from("size"), sciter::Value::from(fe.size as f64));
        e.set_item(sciter::Value::from("time"), sciter::Value::from(fe.modified_time as f64));
        e.set_item(sciter::Value::from("is_hidden"), sciter::Value::from(fe.is_hidden));
        entries.push(e);
    }
    v.set_item(sciter::Value::from("entries"), entries);
    v
}

pub fn get_files_for_send(path: &str, include_hidden: bool) -> Vec<message_proto::FileEntry> {
    let mut result = Vec::new();
    let p = Path::new(path);
    if p.is_file() {
        if let Ok(meta) = p.metadata() {
            let mut fe = message_proto::FileEntry::new();
            fe.name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            fe.entry_type = message_proto::FileType::File.into();
            fe.size = meta.len();
            if let Ok(modified) = meta.modified() {
                if let Ok(dur) = modified.duration_since(UNIX_EPOCH) {
                    fe.modified_time = dur.as_secs();
                }
            }
            result.push(fe);
        }
    } else if p.is_dir() {
        if let Ok(fd) = read_dir_to_proto(path, include_hidden) {
            result = fd.entries.into();
        }
    }
    result
}

pub fn get_recursive_files(path: &str, include_hidden: bool) -> io::Result<Vec<message_proto::FileEntry>> {
    let p = Path::new(path);
    crate::config::write_log(&format!("[ft] get_recursive_files: path='{}' is_file={} is_dir={}", path, p.is_file(), p.is_dir()));
    if p.is_file() {
        let meta = p.metadata()?;
        let mut fe = message_proto::FileEntry::new();
        fe.name = String::new();
        fe.entry_type = message_proto::FileType::File.into();
        fe.size = meta.len();
        if let Ok(modified) = meta.modified() {
            if let Ok(dur) = modified.duration_since(UNIX_EPOCH) {
                fe.modified_time = dur.as_secs();
            }
        }
        Ok(vec![fe])
    } else if p.is_dir() {
        let mut files = Vec::new();
        read_dir_recursive_inner(p, Path::new(""), include_hidden, &mut files)?;
        Ok(files)
    } else {
        Err(io::Error::new(io::ErrorKind::NotFound, format!("Not exists: {}", path)))
    }
}

fn read_dir_recursive_inner(
    dir: &Path,
    prefix: &Path,
    include_hidden: bool,
    files: &mut Vec<message_proto::FileEntry>,
) -> io::Result<()> {
    let fd = read_dir_to_proto(&dir.to_string_lossy(), include_hidden)?;
    for entry in fd.entries.iter() {
        let entry_type = entry.entry_type.enum_value_or_default();
        match entry_type {
            message_proto::FileType::File => {
                let mut fe = entry.clone();
                let rel_path = prefix.join(&entry.name);
                fe.name = rel_path.to_string_lossy().to_string();
                files.push(fe);
            }
            message_proto::FileType::Dir => {
                let child_dir = dir.join(&entry.name);
                let child_prefix = prefix.join(&entry.name);
                let before = files.len();
                read_dir_recursive_inner(&child_dir, &child_prefix, include_hidden, files)?;

                if files.len() == before {
                    let mut fe = message_proto::FileEntry::new();
                    fe.name = format!("{}/", child_prefix.to_string_lossy().replace('\\', "/"));
                    fe.entry_type = message_proto::FileType::Dir.into();
                    fe.size = 0;
                    files.push(fe);
                }
            }
            _ => {}
        }
    }
    Ok(())
}
