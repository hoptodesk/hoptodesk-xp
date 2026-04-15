
//! Minimal session recording — saves VP8 frames to a WebM file.
//! WebM is a subset of Matroska (EBML) containing VP8 video.

use std::io::{self, Write};
use std::fs::File;
use std::path::PathBuf;
use std::time::Instant;

pub struct Recorder {
    file: File,
    start_time: Instant,
    width: u32,
    height: u32,
    cluster_open: bool,
    frame_count: u64,
}

// EBML helper: write a variable-length integer (VINT)
fn write_vint(buf: &mut Vec<u8>, val: u64) {
    if val < 0x7F {
        buf.push(0x80 | val as u8);
    } else if val < 0x3FFF {
        buf.push(0x40 | (val >> 8) as u8);
        buf.push(val as u8);
    } else if val < 0x1FFFFF {
        buf.push(0x20 | (val >> 16) as u8);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    } else if val < 0x0FFFFFFF {
        buf.push(0x10 | (val >> 24) as u8);
        buf.push((val >> 16) as u8);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    } else {
        // 8-byte VINT for unknown size
        buf.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    }
}

fn write_element_id(buf: &mut Vec<u8>, id: u32) {
    if id <= 0xFF {
        buf.push(id as u8);
    } else if id <= 0xFFFF {
        buf.push((id >> 8) as u8);
        buf.push(id as u8);
    } else if id <= 0xFFFFFF {
        buf.push((id >> 16) as u8);
        buf.push((id >> 8) as u8);
        buf.push(id as u8);
    } else {
        buf.push((id >> 24) as u8);
        buf.push((id >> 16) as u8);
        buf.push((id >> 8) as u8);
        buf.push(id as u8);
    }
}

fn write_uint_element(buf: &mut Vec<u8>, id: u32, val: u64) {
    write_element_id(buf, id);
    if val <= 0xFF {
        write_vint(buf, 1);
        buf.push(val as u8);
    } else if val <= 0xFFFF {
        write_vint(buf, 2);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    } else if val <= 0xFFFFFF {
        write_vint(buf, 3);
        buf.push((val >> 16) as u8);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    } else if val <= 0xFFFFFFFF {
        write_vint(buf, 4);
        buf.push((val >> 24) as u8);
        buf.push((val >> 16) as u8);
        buf.push((val >> 8) as u8);
        buf.push(val as u8);
    } else {
        write_vint(buf, 8);
        for i in (0..8).rev() {
            buf.push((val >> (i * 8)) as u8);
        }
    }
}

fn write_float_element(buf: &mut Vec<u8>, id: u32, val: f64) {
    write_element_id(buf, id);
    write_vint(buf, 8);
    buf.extend_from_slice(&val.to_be_bytes());
}

fn write_string_element(buf: &mut Vec<u8>, id: u32, val: &str) {
    write_element_id(buf, id);
    write_vint(buf, val.len() as u64);
    buf.extend_from_slice(val.as_bytes());
}

fn write_binary_element(buf: &mut Vec<u8>, id: u32, data: &[u8]) {
    write_element_id(buf, id);
    write_vint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

fn write_master_element(buf: &mut Vec<u8>, id: u32, content: &[u8]) {
    write_element_id(buf, id);
    write_vint(buf, content.len() as u64);
    buf.extend_from_slice(content);
}

// EBML element IDs
const EBML: u32 = 0x1A45DFA3;
const EBML_VERSION: u32 = 0x4286;
const EBML_READ_VERSION: u32 = 0x42F7;
const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
const DOC_TYPE: u32 = 0x4282;
const DOC_TYPE_VERSION: u32 = 0x4287;
const DOC_TYPE_READ_VERSION: u32 = 0x4285;
const SEGMENT: u32 = 0x18538067;
const INFO: u32 = 0x1549A966;
const TIMECODE_SCALE: u32 = 0x2AD7B1;
const MUXING_APP: u32 = 0x4D80;
const WRITING_APP: u32 = 0x5741;
const TRACKS: u32 = 0x1654AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_UID: u32 = 0x73C5;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;
const CLUSTER: u32 = 0x1F43B675;
const TIMECODE: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;

impl Recorder {
    pub fn new(width: u32, height: u32, target_id: &str) -> io::Result<Self> {
        let dir = recording_dir();
        std::fs::create_dir_all(&dir)?;

        let now = chrono_timestamp();
        let filename = format!("outgoing_{}_{}.webm", target_id, now);
        let path = dir.join(&filename);

        crate::config::write_log(&format!("[recording] Starting: {}", path.display()));

        let mut file = File::create(&path)?;

        // Write EBML header
        let mut ebml_content = Vec::new();
        write_uint_element(&mut ebml_content, EBML_VERSION, 1);
        write_uint_element(&mut ebml_content, EBML_READ_VERSION, 1);
        write_uint_element(&mut ebml_content, EBML_MAX_ID_LENGTH, 4);
        write_uint_element(&mut ebml_content, EBML_MAX_SIZE_LENGTH, 8);
        write_string_element(&mut ebml_content, DOC_TYPE, "webm");
        write_uint_element(&mut ebml_content, DOC_TYPE_VERSION, 2);
        write_uint_element(&mut ebml_content, DOC_TYPE_READ_VERSION, 2);

        let mut header = Vec::new();
        write_master_element(&mut header, EBML, &ebml_content);
        file.write_all(&header)?;

        // Segment (unknown size — streaming)
        let mut seg_header = Vec::new();
        write_element_id(&mut seg_header, SEGMENT);
        // Unknown size VINT: 0x01FFFFFFFFFFFFFF
        seg_header.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        file.write_all(&seg_header)?;

        // Info element
        let mut info_content = Vec::new();
        write_uint_element(&mut info_content, TIMECODE_SCALE, 1_000_000); // 1ms units
        write_string_element(&mut info_content, MUXING_APP, "HopToDesk");
        write_string_element(&mut info_content, WRITING_APP, "HopToDesk XP");

        let mut info = Vec::new();
        write_master_element(&mut info, INFO, &info_content);
        file.write_all(&info)?;

        // Tracks element
        let mut video_content = Vec::new();
        write_uint_element(&mut video_content, PIXEL_WIDTH, width as u64);
        write_uint_element(&mut video_content, PIXEL_HEIGHT, height as u64);

        let mut track_content = Vec::new();
        write_uint_element(&mut track_content, TRACK_NUMBER, 1);
        write_uint_element(&mut track_content, TRACK_UID, 1);
        write_uint_element(&mut track_content, TRACK_TYPE, 1); // video
        write_string_element(&mut track_content, CODEC_ID, "V_VP8");
        write_master_element(&mut track_content, VIDEO, &video_content);

        let mut tracks_content = Vec::new();
        write_master_element(&mut tracks_content, TRACK_ENTRY, &track_content);

        let mut tracks = Vec::new();
        write_master_element(&mut tracks, TRACKS, &tracks_content);
        file.write_all(&tracks)?;

        file.flush()?;

        Ok(Recorder {
            file,
            start_time: Instant::now(),
            width,
            height,
            cluster_open: false,
            frame_count: 0,
        })
    }

    /// Write a VP8 frame. `keyframe` should be true for keyframes.
    pub fn write_vp8_frame(&mut self, data: &[u8], keyframe: bool) -> io::Result<()> {
        let timestamp_ms = self.start_time.elapsed().as_millis() as u64;

        // Start a new cluster on each keyframe (or first frame)
        if keyframe || !self.cluster_open {
            // Cluster header (unknown size for streaming)
            let mut cluster_header = Vec::new();
            write_element_id(&mut cluster_header, CLUSTER);
            cluster_header.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
            self.file.write_all(&cluster_header)?;

            // Cluster timecode
            let mut tc = Vec::new();
            write_uint_element(&mut tc, TIMECODE, timestamp_ms);
            self.file.write_all(&tc)?;

            self.cluster_open = true;
        }

        // SimpleBlock: track=1, timecode relative to cluster=0, flags
        let flags: u8 = if keyframe { 0x80 } else { 0x00 }; // bit 0 = keyframe
        let block_data_len = 1 + 2 + 1 + data.len(); // track_vint + timecode(2) + flags(1) + payload

        let mut block = Vec::with_capacity(block_data_len + 8);
        write_element_id(&mut block, SIMPLE_BLOCK);
        write_vint(&mut block, block_data_len as u64);
        // Track number as VINT (track 1)
        block.push(0x81); // VINT for 1
        // Relative timecode (int16, 0 since we put absolute in cluster)
        block.push(0x00);
        block.push(0x00);
        // Flags
        block.push(flags);
        // Frame data
        block.extend_from_slice(data);

        self.file.write_all(&block)?;
        self.frame_count += 1;

        // Flush periodically
        if self.frame_count % 30 == 0 {
            self.file.flush()?;
        }

        Ok(())
    }

    pub fn finish(&mut self) -> io::Result<()> {
        self.file.flush()?;
        crate::config::write_log(&format!("[recording] Finished, {} frames written", self.frame_count));
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn recording_dir() -> PathBuf {
    let base = crate::config::config_dir();
    base.join("recordings")
}

fn chrono_timestamp() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Simple UTC timestamp: YYYYMMDD_HHMMSS
    let s = secs;
    let sec = s % 60;
    let min = (s / 60) % 60;
    let hour = (s / 3600) % 24;
    let days = s / 86400;
    // Approximate date (good enough for filenames)
    let (y, m, d) = days_to_ymd(days);
    format!("{:04}{:02}{:02}_{:02}{:02}{:02}", y, m, d, hour, min, sec)
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Simple Gregorian calendar conversion from Unix epoch days
    let mut y = 1970;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year { break; }
        remaining -= days_in_year;
        y += 1;
    }
    let months = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 1;
    for &ml in &months {
        if remaining < ml { break; }
        remaining -= ml;
        m += 1;
    }
    (y, m, remaining + 1)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
