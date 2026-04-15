
use std::ffi::CString;
use std::ptr;

#[link(name = "wininet")]
extern "system" {
    fn InternetOpenA(
        agent: *const u8,
        access_type: u32,
        proxy: *const u8,
        proxy_bypass: *const u8,
        flags: u32,
    ) -> *mut std::ffi::c_void;

    fn InternetOpenUrlA(
        internet: *mut std::ffi::c_void,
        url: *const u8,
        headers: *const u8,
        headers_length: u32,
        flags: u32,
        context: usize,
    ) -> *mut std::ffi::c_void;

    fn InternetConnectA(
        internet: *mut std::ffi::c_void,
        server_name: *const u8,
        server_port: u16,
        username: *const u8,
        password: *const u8,
        service: u32,
        flags: u32,
        context: usize,
    ) -> *mut std::ffi::c_void;

    fn HttpOpenRequestA(
        connect: *mut std::ffi::c_void,
        verb: *const u8,
        object_name: *const u8,
        version: *const u8,
        referrer: *const u8,
        accept_types: *const *const u8,
        flags: u32,
        context: usize,
    ) -> *mut std::ffi::c_void;

    fn HttpSendRequestA(
        request: *mut std::ffi::c_void,
        headers: *const u8,
        headers_length: u32,
        optional: *const u8,
        optional_length: u32,
    ) -> i32;

    fn HttpAddRequestHeadersA(
        request: *mut std::ffi::c_void,
        headers: *const u8,
        headers_length: u32,
        modifiers: u32,
    ) -> i32;

    fn InternetSetOptionA(
        internet: *mut std::ffi::c_void,
        option: u32,
        buffer: *const std::ffi::c_void,
        buffer_length: u32,
    ) -> i32;

    fn InternetReadFile(
        file: *mut std::ffi::c_void,
        buffer: *mut u8,
        bytes_to_read: u32,
        bytes_read: *mut u32,
    ) -> i32;

    fn InternetCloseHandle(internet: *mut std::ffi::c_void) -> i32;
}

extern "system" {
    fn GetLastError() -> u32;
}

const INTERNET_OPEN_TYPE_PRECONFIG: u32 = 0;
const INTERNET_FLAG_RELOAD: u32 = 0x80000000;
const INTERNET_FLAG_NO_CACHE_WRITE: u32 = 0x04000000;
const INTERNET_FLAG_SECURE: u32 = 0x00800000;
const INTERNET_FLAG_IGNORE_CERT_CN_INVALID: u32 = 0x00001000;
const INTERNET_FLAG_IGNORE_CERT_DATE_INVALID: u32 = 0x00002000;
const INTERNET_FLAG_IGNORE_REDIRECT_TO_HTTP: u32 = 0x00008000;
const INTERNET_FLAG_IGNORE_REDIRECT_TO_HTTPS: u32 = 0x00004000;
const INTERNET_FLAG_KEEP_CONNECTION: u32 = 0x00400000;

const INTERNET_SERVICE_HTTP: u32 = 3;
const INTERNET_DEFAULT_HTTP_PORT: u16 = 80;
const INTERNET_DEFAULT_HTTPS_PORT: u16 = 443;

const HTTP_ADDREQ_FLAG_ADD: u32 = 0x20000000;
const HTTP_ADDREQ_FLAG_REPLACE: u32 = 0x80000000;

const INTERNET_OPTION_SECURITY_FLAGS: u32 = 31;
const SECURITY_FLAG_IGNORE_UNKNOWN_CA: u32 = 0x00000100;
const SECURITY_FLAG_IGNORE_REVOCATION: u32 = 0x00000080;
const SECURITY_FLAG_IGNORE_WRONG_USAGE: u32 = 0x00000200;
const SECURITY_FLAG_IGNORE_CERT_CN_INVALID: u32 = 0x00001000;
const SECURITY_FLAG_IGNORE_CERT_DATE_INVALID: u32 = 0x00002000;

pub struct ParsedUrl {
    pub host: String,
    pub port: u16,
    pub path: String,
    pub is_https: bool,
}

pub fn parse_url(url: &str) -> Result<ParsedUrl, String> {
    let (scheme, rest) = if url.starts_with("https://") {
        (true, &url[8..])
    } else if url.starts_with("http://") {
        (false, &url[7..])
    } else {
        return Err("URL must start with http:// or https://".to_string());
    };

    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };

    let (host, port) = match host_port.find(':') {
        Some(i) => {
            let p = host_port[i + 1..].parse::<u16>().map_err(|e| e.to_string())?;
            (&host_port[..i], p)
        }
        None => {
            let p = if scheme { INTERNET_DEFAULT_HTTPS_PORT } else { INTERNET_DEFAULT_HTTP_PORT };
            (host_port, p)
        }
    };

    Ok(ParsedUrl {
        host: host.to_string(),
        port,
        path: path.to_string(),
        is_https: scheme,
    })
}

fn set_security_flags(handle: *mut std::ffi::c_void) {
    unsafe {
        let sec_flags: u32 = SECURITY_FLAG_IGNORE_UNKNOWN_CA
            | SECURITY_FLAG_IGNORE_REVOCATION
            | SECURITY_FLAG_IGNORE_WRONG_USAGE
            | SECURITY_FLAG_IGNORE_CERT_CN_INVALID
            | SECURITY_FLAG_IGNORE_CERT_DATE_INVALID;
        InternetSetOptionA(
            handle,
            INTERNET_OPTION_SECURITY_FLAGS,
            &sec_flags as *const u32 as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

fn read_response(handle: *mut std::ffi::c_void) -> Result<String, String> {
    let mut body = Vec::new();
    let mut buf = [0u8; 4096];
    unsafe {
        loop {
            let mut bytes_read: u32 = 0;
            let ok = InternetReadFile(
                handle,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut bytes_read,
            );
            if ok == 0 || bytes_read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..bytes_read as usize]);
        }
    }
    String::from_utf8(body).map_err(|e| e.to_string())
}

fn request_flags(is_https: bool) -> u32 {
    let mut flags = INTERNET_FLAG_RELOAD
        | INTERNET_FLAG_NO_CACHE_WRITE
        | INTERNET_FLAG_KEEP_CONNECTION
        | INTERNET_FLAG_IGNORE_REDIRECT_TO_HTTP
        | INTERNET_FLAG_IGNORE_REDIRECT_TO_HTTPS;
    if is_https {
        flags |= INTERNET_FLAG_SECURE
            | INTERNET_FLAG_IGNORE_CERT_CN_INVALID
            | INTERNET_FLAG_IGNORE_CERT_DATE_INVALID;
    }
    flags
}

pub fn http_get(url: &str) -> Result<String, String> {
    if url.starts_with("https://") || url.starts_with("HTTPS://") {
        return crate::tls_client::http_get(url);
    }
    let agent = CString::new("").map_err(|e| e.to_string())?;
    let url_c = CString::new(url).map_err(|e| e.to_string())?;

    unsafe {
        let h_internet = InternetOpenA(
            agent.as_ptr() as *const u8,
            INTERNET_OPEN_TYPE_PRECONFIG,
            ptr::null(),
            ptr::null(),
            0,
        );
        if h_internet.is_null() {
            let err = GetLastError();
            return Err(format!("InternetOpenA failed (error {})", err));
        }

        let is_https = url.starts_with("https://") || url.starts_with("HTTPS://");
        let flags = request_flags(is_https);

        let h_url = InternetOpenUrlA(
            h_internet,
            url_c.as_ptr() as *const u8,
            ptr::null(),
            0,
            flags,
            0,
        );
        if h_url.is_null() {
            let err = GetLastError();
            InternetCloseHandle(h_internet);
            return Err(format!("InternetOpenUrlA failed (error {})", err));
        }

        set_security_flags(h_url);
        let result = read_response(h_url);

        InternetCloseHandle(h_url);
        InternetCloseHandle(h_internet);

        result
    }
}

fn open_request(
    url: &str,
    verb: &str,
    content_type: &str,
    body: &[u8],
) -> Result<String, String> {
    let parsed = parse_url(url)?;
    let agent = CString::new("HopToDesk").map_err(|e| e.to_string())?;
    let host_c = CString::new(parsed.host.as_str()).map_err(|e| e.to_string())?;
    let path_c = CString::new(parsed.path.as_str()).map_err(|e| e.to_string())?;
    let verb_c = CString::new(verb).map_err(|e| e.to_string())?;
    let header = format!("Content-Type: {}\r\n", content_type);
    let header_c = CString::new(header.as_str()).map_err(|e| e.to_string())?;

    unsafe {
        let h_internet = InternetOpenA(
            agent.as_ptr() as *const u8,
            INTERNET_OPEN_TYPE_PRECONFIG,
            ptr::null(),
            ptr::null(),
            0,
        );
        if h_internet.is_null() {
            return Err(format!("InternetOpenA failed (error {})", GetLastError()));
        }

        let h_connect = InternetConnectA(
            h_internet,
            host_c.as_ptr() as *const u8,
            parsed.port,
            ptr::null(),
            ptr::null(),
            INTERNET_SERVICE_HTTP,
            0,
            0,
        );
        if h_connect.is_null() {
            let err = GetLastError();
            InternetCloseHandle(h_internet);
            return Err(format!("InternetConnectA failed (error {})", err));
        }

        let flags = request_flags(parsed.is_https);
        let h_request = HttpOpenRequestA(
            h_connect,
            verb_c.as_ptr() as *const u8,
            path_c.as_ptr() as *const u8,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            flags,
            0,
        );
        if h_request.is_null() {
            let err = GetLastError();
            InternetCloseHandle(h_connect);
            InternetCloseHandle(h_internet);
            return Err(format!("HttpOpenRequestA failed (error {})", err));
        }

        set_security_flags(h_request);

        HttpAddRequestHeadersA(
            h_request,
            header_c.as_ptr() as *const u8,
            header.len() as u32,
            HTTP_ADDREQ_FLAG_ADD | HTTP_ADDREQ_FLAG_REPLACE,
        );

        let ok = HttpSendRequestA(
            h_request,
            ptr::null(),
            0,
            if body.is_empty() { ptr::null() } else { body.as_ptr() },
            body.len() as u32,
        );
        if ok == 0 {
            let err = GetLastError();
            InternetCloseHandle(h_request);
            InternetCloseHandle(h_connect);
            InternetCloseHandle(h_internet);
            return Err(format!("HttpSendRequestA failed (error {})", err));
        }

        let result = read_response(h_request);

        InternetCloseHandle(h_request);
        InternetCloseHandle(h_connect);
        InternetCloseHandle(h_internet);

        result
    }
}

pub fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}

pub fn http_post_form(url: &str, params: &[(&str, &str)]) -> Result<String, String> {
    if url.starts_with("https://") || url.starts_with("HTTPS://") {
        return crate::tls_client::http_post_form(url, params);
    }
    let body: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    open_request(url, "POST", "application/x-www-form-urlencoded", body.as_bytes())
}

pub fn http_post_multipart(
    url: &str,
    fields: &[(&str, &str)],
    file_field: &str,
    file_name: &str,
    file_data: &[u8],
) -> Result<String, String> {
    if url.starts_with("https://") || url.starts_with("HTTPS://") {
        return crate::tls_client::http_post_multipart(url, fields, file_field, file_name, file_data);
    }
    let boundary = "----HopToDesk7b3a4c";
    let mut body = Vec::new();

    for (k, v) in fields {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n", k, v).as_bytes(),
        );
    }

    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n",
            file_field, file_name
        )
        .as_bytes(),
    );
    body.extend_from_slice(file_data);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

    let content_type = format!("multipart/form-data; boundary={}", boundary);
    open_request(url, "POST", &content_type, &body)
}
