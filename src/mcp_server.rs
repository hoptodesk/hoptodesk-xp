
use std::io::{self, BufRead, Write};
use serde_json::{json, Value};

const SERVER_NAME: &str = "hoptodesk-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "2024-11-05";

pub fn run() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if let Some(resp) = process_line(&line) {
            if writeln!(stdout, "{}", resp).is_err() {
                break;
            }
            let _ = stdout.flush();
        }
    }
}

pub fn handle_mcp_request(payload: &str) -> Option<String> {
    process_line(payload)
}

fn process_line(line: &str) -> Option<String> {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Some(error_response(Value::Null, -32700, "Parse error")),
    };

    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req["method"].as_str().unwrap_or("");

    match method {
        "initialize" => Some(success_response(
            &id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {},
                    "resources": {}
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION
                }
            }),
        )),
        "initialized" => None,
        "ping" => Some(success_response(&id, json!({}))),
        "tools/list" => Some(success_response(&id, json!({ "tools": tools_list() }))),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(json!({}));
            let tool_name = params["name"].as_str().unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let result = call_tool(tool_name, &args);
            Some(success_response(&id, result))
        }
        "resources/list" => Some(success_response(
            &id,
            json!({
                "resources": [
                    {
                        "uri": "hoptodesk://device/info",
                        "name": "Device Info",
                        "description": "Current device information",
                        "mimeType": "application/json"
                    },
                    {
                        "uri": "hoptodesk://device/config",
                        "name": "Device Config",
                        "description": "Device configuration",
                        "mimeType": "application/json"
                    },
                    {
                        "uri": "hoptodesk://device/peers",
                        "name": "Known Peers",
                        "description": "Recent and favorite peers",
                        "mimeType": "application/json"
                    }
                ]
            }),
        )),
        "resources/read" => {
            let uri = req["params"]["uri"].as_str().unwrap_or("");
            let result = read_resource(uri);
            Some(success_response(&id, result))
        }
        _ => Some(error_response(id, -32601, "Method not found")),
    }
}

fn success_response(id: &Value, result: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
    .to_string()
}

fn error_response(id: Value, code: i32, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
    .to_string()
}

fn tools_list() -> Value {
    json!([
        {
            "name": "get_device_info",
            "description": "Get device ID, version, OS, and hostname",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "screenshot",
            "description": "Capture a screenshot of the primary display. Returns base64-encoded BMP.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_clipboard",
            "description": "Get the current clipboard text content",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "set_clipboard",
            "description": "Set the clipboard text content",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Text to copy to clipboard" }
                },
                "required": ["text"]
            }
        },
        {
            "name": "list_files",
            "description": "List files in a directory",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory path to list" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "read_file",
            "description": "Read the contents of a text file",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to read" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "run_command",
            "description": "Run a shell command and return the output",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Command to execute" }
                },
                "required": ["command"]
            }
        },
        {
            "name": "type_text",
            "description": "Type text using simulated keyboard input",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Text to type" }
                },
                "required": ["text"]
            }
        }
    ])
}

fn call_tool(name: &str, args: &Value) -> Value {
    match name {
        "get_device_info" => tool_get_device_info(),
        "screenshot" => tool_screenshot(),
        "get_clipboard" => tool_get_clipboard(),
        "set_clipboard" => tool_set_clipboard(args),
        "list_files" => tool_list_files(args),
        "read_file" => tool_read_file(args),
        "run_command" => tool_run_command(args),
        "type_text" => tool_type_text(args),
        _ => tool_error(&format!("Unknown tool: {}", name)),
    }
}

fn tool_result(text: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }]
    })
}

fn tool_error(msg: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": msg }],
        "isError": true
    })
}

fn tool_get_device_info() -> Value {
    let cfg = crate::config::Config::load();
    let hostname = std::env::var("COMPUTERNAME").unwrap_or_default();
    let info = json!({
        "device_id": cfg.id,
        "version": SERVER_VERSION,
        "os": "windows-xp",
        "hostname": hostname,
        "dashboard_linked": crate::dashboard::is_linked()
    });
    tool_result(&info.to_string())
}

fn tool_screenshot() -> Value {
    #[link(name = "user32")]
    extern "system" {
        fn GetDC(hwnd: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn ReleaseDC(hwnd: *mut std::ffi::c_void, hdc: *mut std::ffi::c_void) -> i32;
        fn GetSystemMetrics(index: i32) -> i32;
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn CreateCompatibleDC(hdc: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn CreateCompatibleBitmap(
            hdc: *mut std::ffi::c_void,
            w: i32,
            h: i32,
        ) -> *mut std::ffi::c_void;
        fn SelectObject(
            hdc: *mut std::ffi::c_void,
            obj: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
        fn BitBlt(
            dst: *mut std::ffi::c_void,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            src: *mut std::ffi::c_void,
            sx: i32,
            sy: i32,
            rop: u32,
        ) -> i32;
        fn GetDIBits(
            hdc: *mut std::ffi::c_void,
            bitmap: *mut std::ffi::c_void,
            start: u32,
            lines: u32,
            bits: *mut u8,
            info: *mut u8,
            usage: u32,
        ) -> i32;
        fn DeleteDC(hdc: *mut std::ffi::c_void) -> i32;
        fn DeleteObject(obj: *mut std::ffi::c_void) -> i32;
    }

    const SM_CXSCREEN: i32 = 0;
    const SM_CYSCREEN: i32 = 1;
    const SRCCOPY: u32 = 0x00CC0020;

    unsafe {
        let w = GetSystemMetrics(SM_CXSCREEN);
        let h = GetSystemMetrics(SM_CYSCREEN);
        if w <= 0 || h <= 0 {
            return tool_error("Failed to get screen dimensions");
        }

        let hdc_screen = GetDC(std::ptr::null_mut());
        if hdc_screen.is_null() {
            return tool_error("Failed to get screen DC");
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        let hbmp = CreateCompatibleBitmap(hdc_screen, w, h);
        let old = SelectObject(hdc_mem, hbmp);

        BitBlt(hdc_mem, 0, 0, w, h, hdc_screen, 0, 0, SRCCOPY);

        let mut bmi = vec![0u8; 44];

        bmi[0..4].copy_from_slice(&40u32.to_le_bytes());

        bmi[4..8].copy_from_slice(&(w as u32).to_le_bytes());

        bmi[8..12].copy_from_slice(&(-(h as i32)).to_le_bytes());

        bmi[12..14].copy_from_slice(&1u16.to_le_bytes());

        bmi[14..16].copy_from_slice(&24u16.to_le_bytes());

        let row_size = ((w * 3 + 3) & !3) as usize;
        let img_size = row_size * h as usize;
        let mut pixels = vec![0u8; img_size];

        GetDIBits(
            hdc_mem,
            hbmp,
            0,
            h as u32,
            pixels.as_mut_ptr(),
            bmi.as_mut_ptr(),
            0,
        );

        SelectObject(hdc_mem, old);
        DeleteObject(hbmp);
        DeleteDC(hdc_mem);
        ReleaseDC(std::ptr::null_mut(), hdc_screen);

        let file_size = 14 + 40 + img_size;
        let mut bmp = Vec::with_capacity(file_size);

        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(file_size as u32).to_le_bytes());
        bmp.extend_from_slice(&0u16.to_le_bytes());
        bmp.extend_from_slice(&0u16.to_le_bytes());
        bmp.extend_from_slice(&54u32.to_le_bytes());

        bmp.extend_from_slice(&bmi[..40]);

        bmp.extend_from_slice(&pixels);

        let b64 = base64_encode(&bmp);
        json!({
            "content": [{
                "type": "image",
                "data": b64,
                "mimeType": "image/bmp"
            }]
        })
    }
}

fn tool_get_clipboard() -> Value {
    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hwnd: *mut std::ffi::c_void) -> i32;
        fn GetClipboardData(format: u32) -> *mut std::ffi::c_void;
        fn CloseClipboard() -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalLock(mem: *mut std::ffi::c_void) -> *const u8;
        fn GlobalUnlock(mem: *mut std::ffi::c_void) -> i32;
    }

    const CF_TEXT: u32 = 1;

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return tool_error("Failed to open clipboard");
        }
        let handle = GetClipboardData(CF_TEXT);
        if handle.is_null() {
            CloseClipboard();
            return tool_result("");
        }
        let ptr = GlobalLock(handle);
        let text = if !ptr.is_null() {
            let mut len = 0;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).to_string()
        } else {
            String::new()
        };
        GlobalUnlock(handle);
        CloseClipboard();
        tool_result(&text)
    }
}

fn tool_set_clipboard(args: &Value) -> Value {
    let text = args["text"].as_str().unwrap_or("");

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hwnd: *mut std::ffi::c_void) -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(format: u32, mem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn CloseClipboard() -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut std::ffi::c_void;
        fn GlobalLock(mem: *mut std::ffi::c_void) -> *mut u8;
        fn GlobalUnlock(mem: *mut std::ffi::c_void) -> i32;
    }

    const CF_TEXT: u32 = 1;
    const GMEM_MOVEABLE: u32 = 0x0002;

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return tool_error("Failed to open clipboard");
        }
        EmptyClipboard();
        let bytes = text.as_bytes();
        let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes.len() + 1);
        if !hmem.is_null() {
            let ptr = GlobalLock(hmem);
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
                *ptr.add(bytes.len()) = 0;
                GlobalUnlock(hmem);
            }
            SetClipboardData(CF_TEXT, hmem);
        }
        CloseClipboard();
        tool_result("Clipboard set")
    }
}

fn tool_list_files(args: &Value) -> Value {
    let path = args["path"].as_str().unwrap_or(".");
    match std::fs::read_dir(path) {
        Ok(entries) => {
            let mut files = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                files.push(json!({
                    "name": name,
                    "is_dir": is_dir,
                    "size": size
                }));
            }
            tool_result(&serde_json::to_string_pretty(&files).unwrap_or_default())
        }
        Err(e) => tool_error(&format!("Failed to list directory: {}", e)),
    }
}

fn tool_read_file(args: &Value) -> Value {
    let path = args["path"].as_str().unwrap_or("");
    if path.is_empty() {
        return tool_error("Path is required");
    }
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let truncated = if content.len() > 100_000 {
                format!("{}...\n[truncated at 100KB]", &content[..100_000])
            } else {
                content
            };
            tool_result(&truncated)
        }
        Err(e) => tool_error(&format!("Failed to read file: {}", e)),
    }
}

fn tool_run_command(args: &Value) -> Value {
    let command = args["command"].as_str().unwrap_or("");
    if command.is_empty() {
        return tool_error("Command is required");
    }
    match std::process::Command::new("cmd")
        .args(["/C", command])
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let result = json!({
                "exit_code": output.status.code().unwrap_or(-1),
                "stdout": stdout,
                "stderr": stderr
            });
            tool_result(&result.to_string())
        }
        Err(e) => tool_error(&format!("Failed to run command: {}", e)),
    }
}

fn tool_type_text(args: &Value) -> Value {
    let text = args["text"].as_str().unwrap_or("");
    if text.is_empty() {
        return tool_error("Text is required");
    }

    #[repr(C)]
    struct KeybdInput {
        input_type: u32,
        vk: u16,
        scan: u16,
        flags: u32,
        time: u32,
        extra: usize,
    }

    #[link(name = "user32")]
    extern "system" {
        fn SendInput(count: u32, inputs: *const KeybdInput, size: i32) -> u32;
        fn VkKeyScanA(ch: u8) -> i16;
    }

    const INPUT_KEYBOARD: u32 = 1;
    const KEYEVENTF_KEYUP: u32 = 0x0002;

    for ch in text.bytes() {
        unsafe {
            let vk_result = VkKeyScanA(ch);
            let vk = (vk_result & 0xFF) as u16;
            let shift = (vk_result >> 8) & 1;

            let mut inputs = Vec::new();

            if shift != 0 {
                inputs.push(KeybdInput {
                    input_type: INPUT_KEYBOARD,
                    vk: 0x10,
                    scan: 0,
                    flags: 0,
                    time: 0,
                    extra: 0,
                });
            }

            inputs.push(KeybdInput {
                input_type: INPUT_KEYBOARD,
                vk,
                scan: 0,
                flags: 0,
                time: 0,
                extra: 0,
            });
            inputs.push(KeybdInput {
                input_type: INPUT_KEYBOARD,
                vk,
                scan: 0,
                flags: KEYEVENTF_KEYUP,
                time: 0,
                extra: 0,
            });

            if shift != 0 {
                inputs.push(KeybdInput {
                    input_type: INPUT_KEYBOARD,
                    vk: 0x10,
                    scan: 0,
                    flags: KEYEVENTF_KEYUP,
                    time: 0,
                    extra: 0,
                });
            }

            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<KeybdInput>() as i32,
            );
        }
    }

    tool_result(&format!("Typed {} characters", text.len()))
}

fn read_resource(uri: &str) -> Value {
    match uri {
        "hoptodesk://device/info" => {
            let cfg = crate::config::Config::load();
            let hostname = std::env::var("COMPUTERNAME").unwrap_or_default();
            let content = json!({
                "device_id": cfg.id,
                "version": SERVER_VERSION,
                "os": "windows-xp",
                "hostname": hostname,
                "dashboard_linked": crate::dashboard::is_linked(),
                "dashboard_user_id": crate::dashboard::get_dashboard_user_id()
            });
            json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": content.to_string()
                }]
            })
        }
        "hoptodesk://device/config" => {
            let cfg2 = crate::config::Config2::load();
            let content = json!({
                "config_directory": crate::config::config_dir().to_string_lossy(),
                "dashboard_user_id": cfg2.get_option("dashboard_user_id"),
                "rendezvous_server": cfg2.rendezvous_server
            });
            json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": content.to_string()
                }]
            })
        }
        "hoptodesk://device/peers" => {
            let local = crate::config::LocalConfig::load();
            let peers: Vec<Value> = local
                .recent_peers
                .iter()
                .map(|p| {
                    json!({
                        "id": p.id,
                        "username": p.username,
                        "hostname": p.hostname,
                        "platform": p.platform
                    })
                })
                .collect();
            json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": serde_json::to_string(&peers).unwrap_or_default()
                }]
            })
        }
        _ => json!({
            "contents": [],
            "error": format!("Unknown resource: {}", uri)
        }),
    }
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i] as u32;
        let b1 = if i + 1 < data.len() { data[i + 1] as u32 } else { 0 };
        let b2 = if i + 2 < data.len() { data[i + 2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if i + 1 < data.len() {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if i + 2 < data.len() {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        i += 3;
    }
    result
}
