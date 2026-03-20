
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

const INTERNET_OPTION_SECURITY_FLAGS: u32 = 31;
const SECURITY_FLAG_IGNORE_UNKNOWN_CA: u32 = 0x00000100;
const SECURITY_FLAG_IGNORE_REVOCATION: u32 = 0x00000080;
const SECURITY_FLAG_IGNORE_WRONG_USAGE: u32 = 0x00000200;
const SECURITY_FLAG_IGNORE_CERT_CN_INVALID: u32 = 0x00001000;
const SECURITY_FLAG_IGNORE_CERT_DATE_INVALID: u32 = 0x00002000;

pub fn http_get(url: &str) -> Result<String, String> {
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

        let mut flags = INTERNET_FLAG_RELOAD
            | INTERNET_FLAG_NO_CACHE_WRITE
            | INTERNET_FLAG_KEEP_CONNECTION
            | INTERNET_FLAG_IGNORE_REDIRECT_TO_HTTP
            | INTERNET_FLAG_IGNORE_REDIRECT_TO_HTTPS;

        if url.starts_with("https://") || url.starts_with("HTTPS://") {
            flags |= INTERNET_FLAG_SECURE
                | INTERNET_FLAG_IGNORE_CERT_CN_INVALID
                | INTERNET_FLAG_IGNORE_CERT_DATE_INVALID;
        }

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

        let sec_flags: u32 = SECURITY_FLAG_IGNORE_UNKNOWN_CA
            | SECURITY_FLAG_IGNORE_REVOCATION
            | SECURITY_FLAG_IGNORE_WRONG_USAGE
            | SECURITY_FLAG_IGNORE_CERT_CN_INVALID
            | SECURITY_FLAG_IGNORE_CERT_DATE_INVALID;
        InternetSetOptionA(
            h_url,
            INTERNET_OPTION_SECURITY_FLAGS,
            &sec_flags as *const u32 as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );

        let mut body = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let mut bytes_read: u32 = 0;
            let ok = InternetReadFile(
                h_url,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut bytes_read,
            );
            if ok == 0 || bytes_read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..bytes_read as usize]);
        }

        InternetCloseHandle(h_url);
        InternetCloseHandle(h_internet);

        String::from_utf8(body).map_err(|e| e.to_string())
    }
}
