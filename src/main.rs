#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod auth_2fa;
mod capture;
mod client;
mod clipboard;
mod clipboard_file;
mod cm;
mod config;
mod crypto;
mod dashboard;
mod file_transfer;
mod input;
mod install;
mod lang;
mod mcp_server;
mod network;
mod platform;
mod protocol;
mod recording;
mod remote;
mod remote_handler;
mod server;
mod signal;
mod terminal_service;
mod tray;
mod turn;
mod ui_handler;
mod vpx;
mod websocket;
mod tls_client;
mod wininet;

use std::sync::{Arc, Mutex};
use std::process::Stdio;
use std::thread;

#[link(name = "user32")]
extern "system" {
    fn SetTimer(
        hwnd: *mut std::ffi::c_void,
        id: usize,
        interval: u32,
        callback: Option<unsafe extern "system" fn(*mut std::ffi::c_void, u32, usize, u32)>,
    ) -> usize;
    fn SendMessageA(hwnd: *mut std::ffi::c_void, msg: u32, wparam: usize, lparam: usize) -> isize;
    fn LoadIconA(hinst: *mut std::ffi::c_void, name: *const u8) -> *mut std::ffi::c_void;
    fn GetModuleHandleA(name: *const u8) -> *mut std::ffi::c_void;
}

pub fn set_window_icon(hwnd: sciter::types::HWINDOW) {
    if hwnd.is_null() { return; }
    unsafe {
        let hinst = GetModuleHandleA(std::ptr::null());

        let icon = LoadIconA(hinst, 1 as *const u8);
        if !icon.is_null() {
            const WM_SETICON: u32 = 0x0080;
            SendMessageA(hwnd as *mut std::ffi::c_void, WM_SETICON, 0, icon as usize);
            SendMessageA(hwnd as *mut std::ffi::c_void, WM_SETICON, 1, icon as usize);
        }
    }
}

static mut TIMER_STATE: Option<Arc<Mutex<ui_handler::AppState>>> = None;
static mut TIMER_HWND: sciter::types::HWINDOW = std::ptr::null_mut();
static mut TIMER_TICK: u32 = 0;
static mut INSTALL_PARENT_PID: u32 = 0;

#[cfg(target_os = "windows")]
fn terminate_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn TerminateProcess(handle: *mut std::ffi::c_void, exit_code: u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    const PROCESS_TERMINATE: u32 = 0x0001;
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !h.is_null() {
            TerminateProcess(h, 0);
            CloseHandle(h);
        }
    }
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

struct MainHandler;

impl MainHandler {
    fn is_installed(&self) -> bool {
        let result = crate::install::is_installed();
        crate::config::write_log(&format!("[handler] is_installed -> {}", result));
        result
    }

    fn get_app_name(&self) -> String {
        crate::install::APP_NAME.to_string()
    }

    fn install_path(&self) -> String {
        crate::install::default_install_path()
            .to_string_lossy()
            .to_string()
    }

    fn get_option(&self, name: String) -> String {
        let cfg2 = crate::config::Config2::load();
        cfg2.get_option(&name)
    }

    fn show_run_without_install(&self) -> bool {
        false
    }

    fn run_without_install(&self) {

    }

    fn open_url(&self, url: String) {
        open_external_url(&url);
    }

    fn is_installed_daemon(&self, _prompt: bool) -> bool {
        false
    }

    fn install_me(&self, args: String, install_path: String) {
        crate::config::write_log(&format!(
            "[handler] install_me invoked args='{}' path='{}'",
            args, install_path
        ));
        std::thread::spawn(move || {
            match crate::install::install_me(&args, &install_path) {
                Ok(()) => {
                    crate::config::write_log("[handler] install completed; exiting UI process");
                    unsafe { terminate_pid(INSTALL_PARENT_PID); }
                    win_info(
                        "HopToDesk is now installed and the service is running.\n\nUse the Start Menu or desktop icon to open it.",
                        "Install complete",
                    );
                    std::process::exit(0);
                }
                Err(e) => {
                    crate::config::write_log(&format!("[handler] install failed: {}", e));
                    win_info(
                        &format!("Install failed:\n\n{}\n\nMake sure HopToDesk is running as Administrator.", e),
                        "Install failed",
                    );
                    std::process::exit(1);
                }
            }
        });
    }

    fn goto_install(&self) {
        crate::config::write_log("[handler] goto_install invoked");

        std::thread::spawn(|| {
            let install_path = crate::install::default_install_path();
            let confirm = format!(
                "Install HopToDesk to:\n\n{}\n\nThis will also register HopToDesk as a Windows Service so it can accept connections without anyone being logged in.\n\nAdministrator privileges are required.",
                install_path.display()
            );
            if !win_confirm(&confirm, "Install HopToDesk") {
                crate::config::write_log("[handler] user declined install");
                return;
            }

            match crate::install::install_me("startmenu desktopicon", "") {
                Ok(()) => {
                    crate::config::write_log("[handler] install completed; exiting UI process");
                    win_info(
                        "HopToDesk is now installed and the service is running.\n\nThe app will close. Use the Start Menu or desktop icon to open it again.",
                        "Install complete",
                    );
                    std::process::exit(0);
                }
                Err(e) => {
                    crate::config::write_log(&format!("[handler] install failed: {}", e));
                    win_info(
                        &format!("Install failed:\n\n{}\n\nMake sure HopToDesk is running as Administrator.", e),
                        "Install failed",
                    );
                }
            }
        });
    }
}

#[cfg(target_os = "windows")]
pub fn win_confirm(text: &str, title: &str) -> bool {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            hwnd: *mut std::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            utype: u32,
        ) -> i32;
    }
    const MB_OKCANCEL: u32 = 0x00000001;
    const MB_ICONQUESTION: u32 = 0x00000020;
    const IDOK: i32 = 1;
    let text_w: Vec<u16> = OsStr::new(text).encode_wide().chain(once(0)).collect();
    let title_w: Vec<u16> = OsStr::new(title).encode_wide().chain(once(0)).collect();
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text_w.as_ptr(),
            title_w.as_ptr(),
            MB_OKCANCEL | MB_ICONQUESTION,
        )
    };
    result == IDOK
}

#[cfg(target_os = "windows")]
pub fn win_confirm_warn(text: &str, title: &str) -> bool {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            hwnd: *mut std::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            utype: u32,
        ) -> i32;
    }
    const MB_YESNO: u32 = 0x00000004;
    const MB_ICONWARNING: u32 = 0x00000030;
    const MB_SETFOREGROUND: u32 = 0x00010000;
    const MB_TOPMOST: u32 = 0x00040000;
    const IDYES: i32 = 6;
    let text_w: Vec<u16> = OsStr::new(text).encode_wide().chain(once(0)).collect();
    let title_w: Vec<u16> = OsStr::new(title).encode_wide().chain(once(0)).collect();
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text_w.as_ptr(),
            title_w.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST,
        )
    };
    result == IDYES
}

#[cfg(target_os = "windows")]
pub fn win_info(text: &str, title: &str) {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            hwnd: *mut std::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            utype: u32,
        ) -> i32;
    }
    const MB_OK: u32 = 0x00000000;
    const MB_ICONINFORMATION: u32 = 0x00000040;
    let text_w: Vec<u16> = OsStr::new(text).encode_wide().chain(once(0)).collect();
    let title_w: Vec<u16> = OsStr::new(title).encode_wide().chain(once(0)).collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text_w.as_ptr(),
            title_w.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

impl sciter::EventHandler for MainHandler {
    sciter::dispatch_script_call! {
        fn is_installed();
        fn get_app_name();
        fn install_path();
        fn get_option(String);
        fn show_run_without_install();
        fn run_without_install();
        fn open_url(String);
        fn is_installed_daemon(bool);
        fn install_me(String, String);
        fn goto_install();
    }
}

#[cfg(target_os = "windows")]
fn open_external_url(url: &str) {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut std::ffi::c_void,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }
    let op: Vec<u16> = OsStr::new("open").encode_wide().chain(once(0)).collect();
    let file: Vec<u16> = OsStr::new(url).encode_wide().chain(once(0)).collect();
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        );
    }
}

pub fn format_id(id: &str) -> String {
    if id.len() == 9 {
        format!("{} {} {}", &id[0..3], &id[3..6], &id[6..9])
    } else {
        id.to_string()
    }
}

#[cfg(target_os = "windows")]
fn relocate_from_temp_if_needed() {
    use std::path::PathBuf;

    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let current_path_str = current_exe.to_string_lossy().to_lowercase();

    let temp_dir = std::env::temp_dir().to_string_lossy().to_lowercase();
    let in_temp = !temp_dir.is_empty() && current_path_str.starts_with(&temp_dir);
    if !in_temp {
        return;
    }

    let appdata = match std::env::var("APPDATA") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => return,
    };
    let target_dir = appdata.join("HopToDesk");
    let _ = std::fs::create_dir_all(&target_dir);
    let target_exe = target_dir.join("HopToDesk.exe");
    let target_dll = target_dir.join("sciter.dll");

    let cur_len = std::fs::metadata(&current_exe).ok().map(|m| m.len()).unwrap_or(0);
    let tgt_len = std::fs::metadata(&target_exe).ok().map(|m| m.len()).unwrap_or(0);
    if cur_len == 0 || cur_len != tgt_len {
        crate::config::write_log(&format!(
            "[main] Relocating exe: {} -> {}",
            current_exe.display(),
            target_exe.display()
        ));
        if let Err(e) = std::fs::copy(&current_exe, &target_exe) {
            crate::config::write_log(&format!("[main] Exe copy failed: {}", e));
            return;
        }
    }

    if let Some(src_dir) = current_exe.parent() {
        let src_dll = src_dir.join("sciter.dll");
        let src_dll_len = std::fs::metadata(&src_dll).ok().map(|m| m.len()).unwrap_or(0);
        let tgt_dll_len = std::fs::metadata(&target_dll).ok().map(|m| m.len()).unwrap_or(0);
        if src_dll_len > 0 && src_dll_len != tgt_dll_len {
            crate::config::write_log(&format!(
                "[main] Relocating sciter.dll: {} -> {}",
                src_dll.display(),
                target_dll.display()
            ));
            if let Err(e) = std::fs::copy(&src_dll, &target_dll) {
                crate::config::write_log(&format!("[main] sciter.dll copy failed: {}", e));
                return;
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(&std::env::temp_dir()) {
        for entry in entries.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("7zS") || name.starts_with("7z") {

                if current_exe.parent().map(|cp| cp == p).unwrap_or(false) {
                    continue;
                }
                let _ = std::fs::remove_dir_all(&p);
            }
        }
    }

    crate::config::write_log(&format!("[main] Relaunching from {}", target_exe.display()));
    let args_forward: Vec<String> = std::env::args().skip(1).collect();
    match std::process::Command::new(&target_exe).args(&args_forward).spawn() {
        Ok(_) => {
            crate::config::write_log("[main] Relaunch succeeded; exiting temp process");
            std::process::exit(0);
        }
        Err(e) => {
            crate::config::write_log(&format!("[main] Relaunch failed: {} — continuing in temp", e));
        }
    }
}

#[cfg(target_os = "windows")]
fn run_uninstall_with_ui() {
    let confirm = "Uninstall HopToDesk?\n\nThis will stop and remove the HopToDesk service, delete the installed files, and remove Start Menu / desktop shortcuts.\n\nAdministrator privileges are required.";
    if !win_confirm(confirm, "Uninstall HopToDesk") {
        crate::config::write_log("[uninstaller] User declined uninstall");
        return;
    }
    match crate::install::uninstall_me() {
        Ok(()) => {
            win_info(
                "HopToDesk has been uninstalled.",
                "Uninstall complete",
            );
        }
        Err(e) => {
            win_info(
                &format!("Uninstall failed:\n\n{}\n\nMake sure you are running as Administrator.", e),
                "Uninstall failed",
            );
        }
    }
}

const CARD_SVG_WINDOWS: &str = "<svg class=\"session-plat\" viewBox=\"0 0 448 512\"><path d=\"M0 93.7l183.6-25.3v177.4H0V93.7zm0 324.6l183.6 25.3V268.4H0v149.9zm203.8 28L448 480V268.4H203.8v177.9zm0-380.6v180.1H448V32L203.8 65.7z\" fill=\"none\" stroke=\"#FFFFFF\" stroke-width=\"20\"/></svg>";
const CARD_SVG_MAC: &str = "<svg class=\"session-plat\" viewBox=\"0 0 384 512\"><path d=\"M318.7 268.7c-.2-36.7 16.4-64.4 50-84.8-18.8-26.9-47.2-41.7-84.7-44.6-35.5-2.8-74.3 20.7-88.5 20.7-15 0-49.4-19.7-76.4-19.7C63.3 141.2 4 184.8 4 273.5q0 39.3 14.4 81.2c12.8 36.7 59 126.7 107.2 125.2 25.2-.6 43-17.9 75.8-17.9 31.8 0 48.3 17.9 76.4 17.9 48.6-.7 90.4-82.5 102.6-119.3-65.2-30.7-61.7-90-61.7-91.9zm-56.6-164.2c27.3-32.4 24.8-61.9 24-72.5-24.1 1.4-52 16.4-67.9 34.9-17.5 19.8-27.8 44.3-25.6 71.9 26.1 2 49.9-11.4 69.5-34.3z\" fill=\"none\" stroke=\"#FFFFFF\" stroke-width=\"20\"/></svg>";
const CARD_SVG_LINUX: &str = "<svg class=\"session-plat\" viewBox=\"0 0 256 256\"><g transform=\"translate(0 256) scale(.1 -.1)\" fill=\"#FFFFFF\"><path d=\"m1215 2537c-140-37-242-135-286-278-23-75-23-131 1-383l18-200-54-60c-203-224-383-615-384-831v-51l-66-43c-113-75-194-199-194-300 0-110 99-234 244-305 103-50 185-69 296-69 100 0 156 14 211 54 26 18 35 19 78 10 86-18 233-24 335-12 85 10 222 38 269 56 9 4 19-7 29-35 20-50 52-64 136-57 98 8 180 52 282 156 124 125 180 244 180 380 0 80-28 142-79 179l-36 26 4 119c5 175-22 292-105 460-74 149-142 246-286 409-43 49-78 92-78 97 0 4-7 52-15 107-8 54-19 140-24 189-13 121-41 192-103 260-95 104-248 154-373 122zm172-112c62-19 134-80 163-140 15-31 28-92 41-193 27-214 38-276 57-304 9-14 59-74 111-134 92-106 191-246 236-334 69-137 115-339 101-451l-7-55-71 10c-100 13-234-5-265-36-54-55-85-207-82-412l1-141-51-17c-104-34-245-51-380-45-69 3-142 10-162 16-32 10-37 17-53 68-23 72-87 201-136 273-80 117-158 188-237 215-37 13-37 13-34 61 13 211 182 555 373 759 57 62 58 63 58 121 0 33-9 149-19 259-21 224-18 266 26 347 67 122 193 174 330 133zm687-1720c32-9 71-25 87-36 60-42 59-151-4-274-59-119-221-250-317-257-34-3-35-2-48 47-18 65-20 329-3 413 16 83 29 110 55 115 51 10 177 6 230-8zm-1418-80c79-46 187-195 247-340 41-99 43-121 12-141-39-25-148-30-238-10-142 32-264 112-307 202-20 41-21 50-10 87 24 83 102 166 192 207 54 25 53 25 104-5z\"/><path d=\"m1395 1945c-92-16-220-52-256-70-28-15-29-18-29-89 0-247 165-397 345-312 60 28 77 46 106 111 54 123 0 378-80 374-9 0-47-7-86-14zm74-156c15-69 14-112-5-159s-55-70-111-70c-48 0-78 20-102 68-15 29-41 131-41 159 0 9 230 63 242 57 3-2 11-27 17-55z\"/></g></svg>";
const CARD_SVG_ANDROID: &str = "<svg class=\"session-plat\" viewBox=\"0 0 553 553\"><path d=\"M77 179a33 33 0 0 0-25 10 33 33 0 0 0-9 24v143a33 33 0 0 0 10 24 33 33 0 0 0 24 10c9 0 17-3 24-10a33 33 0 0 0 10-24V213c0-9-4-17-10-24a33 33 0 0 0-24-10zM352 51l24-44c1-3 1-5-2-6-3-2-5-1-7 2l-24 43a163 163 0 0 0-133 0L186 3c-2-3-4-4-7-2-2 1-3 3-1 6l23 44c-24 12-43 29-57 51a129 129 0 0 0-21 72h307c0-26-7-50-21-72a146 146 0 0 0-57-51zm-136 63a13 13 0 0 1-10 4 13 13 0 0 1-12-13c0-4 1-7 3-9 3-3 6-4 9-4s7 1 10 4c2 2 3 5 3 9s-1 7-3 9zm140 0a12 12 0 0 1-9 4c-4 0-7-1-9-4a12 12 0 0 1-4-9c0-4 1-7 4-9 2-3 5-4 9-4a12 12 0 0 1 9 4c2 2 3 5 3 9s-1 7-3 9zM124 407c0 10 4 19 11 26s15 10 26 10h24v76c0 9 4 17 10 24s15 10 24 10c10 0 18-3 25-10s10-15 10-24v-76h45v76c0 9 4 17 10 24s15 10 25 10c9 0 17-3 24-10s10-15 10-24v-76h25a35 35 0 0 0 25-10c7-7 11-16 11-26V185H124v222zm352-228a33 33 0 0 0-24 10 33 33 0 0 0-10 24v143a34 34 0 0 0 34 34c10 0 18-3 25-10s10-15 10-24V213c0-9-4-17-10-24a33 33 0 0 0-25-10z\" fill=\"none\" stroke=\"#FFFFFF\" stroke-width=\"20\"/></svg>";
const CARD_HEART_FILLED: &str = "<svg viewBox=\"0 0 24 24\"><path d=\"M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z\" fill=\"#FFFFFF\"/></svg>";
const CARD_HEART_OUTLINE: &str = "<svg viewBox=\"0 0 24 24\"><path d=\"M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z\" fill=\"none\" stroke=\"#FFFFFF\" stroke-width=\"2\"/></svg>";
const CARD_MENU_DOTS: &str = "<svg viewBox=\"0 0 512 512\" fill=\"currentColor\"><circle cx=\"256\" cy=\"64\" r=\"64\"/><circle cx=\"256\" cy=\"256\" r=\"64\"/><circle cx=\"256\" cy=\"448\" r=\"64\"/></svg>";

fn platform_card_svg(platform: &str) -> &'static str {
    match platform.to_lowercase().as_str() {
        "linux" | "freebsd" => CARD_SVG_LINUX,
        "mac os" | "macos" | "mac" => CARD_SVG_MAC,
        "android" | "ios" => CARD_SVG_ANDROID,
        _ => CARD_SVG_WINDOWS,
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_string()
        };
        crate::config::write_log(&format!("[panic] {} at {}", msg, location));
    }));
    config::cleanup_old_logs();
    crate::config::write_log(&format!("[main] HopToDesk {} starting", VERSION));

    let args: Vec<String> = std::env::args().collect();
    crate::config::write_log(&format!("[main] CLI args: {:?}", args));

    #[cfg(target_os = "windows")]
    if args.len() < 2 || !args[1].starts_with("--") {
        relocate_from_temp_if_needed();
    }

    if args.len() >= 2 && args[1].starts_with("--") {
        let name = args[1].replace("--", "");
        if !name.is_empty() {
            config::set_log_subdir(&name);
        }
    }

    if args.len() >= 2 {
        match args[1].as_str() {
            "--connect" if args.len() >= 3 => {
                let target_id = args[2].replace(' ', "");
                let is_ft = args.iter().any(|a| a == "--file-transfer");

                let switch_uuid = args.iter().position(|a| a == "--switch_uuid" || a == "--switch-uuid")
                    .and_then(|i| args.get(i + 1).cloned());
                if let Some(ref uuid) = switch_uuid {
                    crate::config::write_log(&format!("[connect] Switch Sides connection to {} with UUID {}", target_id, uuid));
                }

                let peer_cfg = config::PeerConfig::load(&target_id);
                let saved_password = peer_cfg.get_option("password");
                crate::config::write_log(&format!("[connect] Starting connection to {} (file_transfer={}, has_saved_pw={}, switch={})",
                    target_id, is_ft, !saved_password.is_empty(), switch_uuid.is_some()));
                remote::run_connect_process_ex(&target_id, &saved_password, is_ft, switch_uuid.as_deref());
                std::process::exit(0);
            }
            "--cm" => {

                let session_id = args.get(2)
                    .cloned()
                    .or_else(|| std::env::var("HOPTODESK_CM_SESSION").ok())
                    .unwrap_or_default();
                if !session_id.is_empty() {
                    cm::run_cm_process(&session_id);
                } else {
                    crate::config::write_log("[cm] No session ID provided");
                }
                std::process::exit(0);
            }
            "--version" => {
                println!("{}", VERSION);
                std::process::exit(0);
            }
            "--get-id" => {
                let cfg = config::Config::load();
                println!("{}", cfg.id);
                std::process::exit(0);
            }
            "--password" if args.len() >= 3 => {
                let new_password = &args[2];
                let mut cfg = config::Config::load();
                cfg.password = new_password.to_string();
                cfg.save();
                crate::config::write_log(&format!("Password updated."));
                std::process::exit(0);
            }
            "--server" => {
                run_headless_server();
                std::process::exit(0);
            }
            "--changeid" => {
                let cfg_path = config::config_dir().join("HopToDesk.toml");
                if let Ok(content) = std::fs::read_to_string(&cfg_path) {
                    let filtered: String = content
                        .lines()
                        .filter(|line| !line.starts_with("id = ") && !line.starts_with("salt = "))
                        .map(|line| format!("{}\n", line))
                        .collect();
                    let _ = std::fs::write(&cfg_path, filtered);
                    crate::config::write_log(&format!("ID reset. Restart to generate new ID."));
                }
                std::process::exit(0);
            }
            "--import-config" if args.len() >= 3 => {
                import_config(&args[2]);
                std::process::exit(0);
            }
            "--mcp" => {
                mcp_server::run();
                std::process::exit(0);
            }
            "--ticket" => {
                run_ticket_window();
                std::process::exit(0);
            }
            "--install-ui" => {
                if let Some(ppid) = args.get(2).and_then(|s| s.parse::<u32>().ok()) {
                    unsafe { INSTALL_PARENT_PID = ppid; }
                }
                run_install_window();
                std::process::exit(0);
            }
            "--service" => {
                install::run_as_service();
                std::process::exit(0);
            }
            "--tray" => {
                tray::start();
                std::process::exit(0);
            }
            "--install" => {
                let install_args = args.get(2).cloned().unwrap_or_default();
                let install_path = args.get(3).cloned().unwrap_or_default();
                match install::install_me(&install_args, &install_path) {
                    Ok(()) => {
                        println!("Installed");
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("Install failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            "--uninstall" => {
                run_uninstall_with_ui();
                std::process::exit(0);
            }
            other => {
                crate::config::write_log(&format!("[main] Unrecognised CLI flag '{}', falling through to UI", other));
            }
        }
    }

    check_invite_code_from_filename();

    run_main_ui();
}

fn check_invite_code_from_filename() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let name = match exe.file_name() {
        Some(n) => n.to_string_lossy().to_string(),
        None => return,
    };

    if let Some(dash_pos) = name.find('-') {
        let id_part = &name[dash_pos + 1..];
        let mut id_end = 0;
        let mut has_uppercase = false;
        for (i, c) in id_part.chars().enumerate() {
            if c.is_ascii_uppercase() {
                has_uppercase = true;
            } else if !(c.is_ascii_lowercase() || c.is_ascii_digit()) {
                break;
            }
            id_end = i + 1;
        }

        if has_uppercase && id_end == 16 {
            let invite_code = &id_part[..id_end];
            let mut cfg2 = config::Config2::load();
            cfg2.set_option("invite_code", invite_code);
            cfg2.save();
            crate::config::write_log(&format!("[init] Invite code from filename: {}", invite_code));
        }
    }
}

fn import_config(path: &str) {
    let src = std::path::Path::new(path);
    if !src.exists() {
        crate::config::write_log(&format!("Config file not found: {}", path));
        return;
    }
    let dest = config::config_dir().join("HopToDesk.toml");
    match std::fs::copy(src, &dest) {
        Ok(_) => crate::config::write_log(&format!("Config imported from {} to {}", path, dest.display())),
        Err(e) => crate::config::write_log(&format!("Failed to import config: {}", e)),
    }
}

fn run_headless_server() {
    config::migrate_old_config();
    let cfg = config::Config::load();
    let my_id = cfg.id.clone();
    let password = cfg.password.clone();
    let pk = cfg.key_pair.1.clone();

    crate::config::write_log(&format!("[server] Starting headless server, ID={}", my_id));
    crate::config::write_log(&format!("[server] Press Ctrl+C to stop"));

    {
        let my_id = my_id.clone();
        let password = password.clone();
        let pk = pk.clone();
        std::thread::spawn(move || {
            server::run_direct_server(my_id, password, pk);
        });
    }

    let signal_state = Arc::new(Mutex::new(signal::SignalState::default()));
    signal::run_signal_loop(my_id, password, pk, signal_state);
}

fn ui_shared_resources() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("common.css", include_bytes!("ui/common.css").as_slice()),
        ("index.css", include_bytes!("ui/index.css").as_slice()),
        ("font.css", include_bytes!("ui/font.css").as_slice()),
        ("common.tis", include_bytes!("ui/common.tis").as_slice()),
        ("msgbox.tis", include_bytes!("ui/msgbox.tis").as_slice()),
        ("install.tis", include_bytes!("ui/install.tis").as_slice()),
        ("ticket.tis", include_bytes!("ui/ticket.tis").as_slice()),
    ]
}

fn run_ticket_window() {
    sciter::set_options(sciter::RuntimeOptions::GfxLayer(sciter::GFX_LAYER::CPU)).ok();
    let mut frame = sciter::Window::new();
    let html = include_str!("ui/ticket.html");
    frame.register_resources(&ui_shared_resources());
    frame.load_html(html.as_bytes(), Some("this://app/ticket.html"));
    frame.set_title("HopToDesk - Tickets");
    let hwnd = frame.get_hwnd();
    set_window_icon(hwnd);
    frame.run_app();
}

fn run_install_window() {
    sciter::set_options(sciter::RuntimeOptions::GfxLayer(sciter::GFX_LAYER::CPU)).ok();
    let mut frame = sciter::Window::new();
    let html = include_str!("ui/install.html");
    frame.event_handler(MainHandler);
    frame.register_resources(&ui_shared_resources());
    frame.load_html(html.as_bytes(), Some("this://app/install.html"));
    frame.set_title("HopToDesk");
    let hwnd = frame.get_hwnd();
    set_window_icon(hwnd);
    frame.run_app();
}

fn build_ui_translations() -> String {
    let keys = [
        "This Device", "Your ID", "Password", "Set", "Unattended Access",
        "Remote Control", "Partner ID", "Enter Remote ID", "Connect",
        "Transfer File", "Recent Sessions", "Favorites", "Settings",
        "Remote Access", "Keyboard/Mouse", "Clipboard", "File Transfer",
        "Remote Restart", "TCP Tunneling", "Remote Printing", "Wake On LAN",
        "Network", "Choose Network", "Proxy Settings", "Direct IP Access", "LAN Discovery",
        "Security", "Permanent Password", "Allow Incoming Connections",
        "Two-Factor Authentication", "Appearance", "Dark Theme",
        "Dashboard", "Linked", "Enter Invite Code",
        "About HopToDesk",
        "Password Settings", "Cancel", "Save", "Language",
        "Rename", "Add to Favorites", "Remove from Favorites",
        "Forget Password", "Remove", "Rename Peer", "Enter alias",
        "Always connect via relay",
        "Your IP", "Switch to ID", "Switch to local IP",
        "No local IP available - showing ID",
        "Your local IP address - share this for direct LAN connections",
        "Connecting...", "Ready", "Not connected",
        "Website", "Privacy Statement", "OK", "Version",
        "Password must be at least 6 characters", "Passwords do not match",
        "Enable", "Disable", "Verify", "On", "Off",
        "Enter your 6-digit code", "2FA enabled successfully",
        "Invalid code, please try again", "2FA has been disabled",
        "Scan this QR code or enter the secret manually in your authenticator app:",
        "This device is linked to a dashboard.",
        "Enter your invite code to link this device to a dashboard.",
        "Invalid invite code. Must be 16 characters (letters and numbers).",
        "Linking to dashboard... This device will appear in the dashboard shortly.",
        "Hostname", "Username", "Type",
        "HopToDesk Network (Default)", "Custom",
        "Incoming Connections Off.",
    ];

    let mut parts = Vec::new();
    for key in &keys {
        let translated = lang::translate(key.to_string());
        let escaped = translated
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        parts.push(format!("\"{}\":\"{}\"", key.replace('"', "\\\""), escaped));
    }
    format!("{{{}}}", parts.join(","))
}

fn get_lang_name(code: &str) -> String {
    if let Some(arr) = lang::LANGS.as_array() {
        for item in arr {
            if let Some(pair) = item.as_array() {
                if pair.len() == 2 {
                    if let Some(c) = pair[0].as_str() {
                        if c == code {
                            if let Some(n) = pair[1].as_str() {
                                return n.to_string();
                            }
                        }
                    }
                }
            }
        }
    }
    "English".to_string()
}

fn send_wol_packet(mac_str: &str) {
    dashboard::send_wol_packet(mac_str);
}

fn run_main_ui() {
    let state = Arc::new(Mutex::new(ui_handler::AppState::new()));

    let (my_id, my_password) = {
        let s = state.lock().unwrap();
        (s.config.id.clone(), s.config.password.clone())
    };

    let service_running = install::is_installed();
    if service_running {
        crate::config::write_log("[main] service is installed; UI mirrors its status and connects itself only if it goes away");
    }
    {
        let state_clone = state.clone();
        thread::spawn(move || {
            let (my_id, password, pk, signal_state) = {
                let s = state_clone.lock().unwrap();
                (
                    s.config.id.clone(),
                    s.config.password.clone(),
                    s.config.key_pair.1.clone(),
                    s.signal_state.clone(),
                )
            };
            signal::run_signal_loop_ex(my_id, password, pk, signal_state, true);
        });
    }
    if !service_running {

        {
            let state_clone = state.clone();
            thread::spawn(move || {
                let (my_id, password, pk) = {
                    let s = state_clone.lock().unwrap();
                    (
                        s.config.id.clone(),
                        s.config.password.clone(),
                        s.config.key_pair.1.clone(),
                    )
                };
                server::run_direct_server(my_id, password, pk);
            });
        }

        thread::spawn(|| {
            dashboard::start();
        });
    }

    sciter::set_options(sciter::RuntimeOptions::GfxLayer(sciter::GFX_LAYER::CPU)).ok();

    let mut frame = sciter::Window::new();

    let html_template = include_str!("ui/index.html");
    let id_formatted = format_id(&my_id);

    let saved_lang = {
        if let Ok(s) = state.lock() {
            s.local_config.get_option("lang")
        } else {
            String::new()
        }
    };
    if !saved_lang.is_empty() {
        lang::set_lang(&saved_lang);
    }

    let langs_json = lang::LANGS.to_string();
    let current_lang_code = if saved_lang.is_empty() { "en".to_string() } else { saved_lang.clone() };
    let current_lang_name = get_lang_name(&current_lang_code);
    let tr_json = build_ui_translations();

    let (proxy_json, options_json) = {
        let cfg2 = config::Config2::load();
        let proxy = cfg2.get_option("socks-proxy");
        let username = cfg2.get_option("socks-username");
        let password = cfg2.get_option("socks-password");
        let proxy_type = cfg2.get_option("socks-proxy-type");
        let pj = serde_json::json!({
            "proxy": proxy,
            "username": username,
            "password": password,
            "proxy_type": if proxy_type.is_empty() { "auto".to_string() } else { proxy_type }
        }).to_string();
        let oj = serde_json::json!({
            "enable-keyboard": cfg2.get_option("enable-keyboard"),
            "enable-clipboard": cfg2.get_option("enable-clipboard"),
            "enable-file-transfer": cfg2.get_option("enable-file-transfer"),
            "enable-remote-restart": cfg2.get_option("enable-remote-restart"),
            "enable-tunnel": cfg2.get_option("enable-tunnel"),
            "enable-remote-printing": cfg2.get_option("enable-remote-printing"),
            "enable-wol": cfg2.get_option("enable-wol"),
            "direct-server": cfg2.get_option("direct-server"),
            "enable-lan-discovery": cfg2.get_option("enable-lan-discovery"),
            "stop-service": cfg2.get_option("stop-service"),
            "allow-darktheme": cfg2.get_option("allow-darktheme"),
            "dashboard_user_id": cfg2.get_option("dashboard_user_id"),
            "custom-rendezvous-server": cfg2.get_option("custom-rendezvous-server"),
            "show-local-ip": cfg2.get_option("show-local-ip"),
            "local_ip": crate::network::get_local_ip(),
            "id_formatted": id_formatted,
        }).to_string();
        (pj, oj)
    };

    let is_installed_now = crate::install::is_installed();
    let install_style = if is_installed_now {
        "display:none; margin-left:16px;"
    } else {
        "display:inline-block; margin-left:16px;"
    };

    let html = html_template
        .replace(
            "id=\"install-btn\" style=\"display:none; margin-left:16px;\"",
            &format!("id=\"install-btn\" style=\"{}\"", install_style),
        )
        .replace("Loading...", &id_formatted)
        .replace("------", &my_password)
        .replace(">Version<", &format!(">Version {}<", VERSION))
        .replace(">English<", &format!(">{}<", current_lang_name))
        .replace("id=\"langs-data\" style=\"visibility:hidden;height:0;overflow:hidden;\"></div>",
                 &format!("id=\"langs-data\" style=\"visibility:hidden;height:0;overflow:hidden;\">{}</div>", langs_json))
        .replace("id=\"tr-data\" style=\"visibility:hidden;height:0;overflow:hidden;\"></div>",
                 &format!("id=\"tr-data\" style=\"visibility:hidden;height:0;overflow:hidden;\">{}</div>", tr_json))
        .replace("id=\"current-lang-code\" style=\"visibility:hidden;height:0;overflow:hidden;\"></div>",
                 &format!("id=\"current-lang-code\" style=\"visibility:hidden;height:0;overflow:hidden;\">{}</div>", current_lang_code))
        .replace("id=\"proxy-data\" style=\"visibility:hidden;height:0;overflow:hidden;\"></div>",
                 &format!("id=\"proxy-data\" style=\"visibility:hidden;height:0;overflow:hidden;\">{}</div>", proxy_json))
        .replace("id=\"2fa-status\" style=\"visibility:hidden;height:0;overflow:hidden;\"></div>",
                 &format!("id=\"2fa-status\" style=\"visibility:hidden;height:0;overflow:hidden;\">{}</div>",
                     if auth_2fa::has_valid_2fa() { "on" } else { "" }))
        .replace("id=\"options-data\" style=\"visibility:hidden;height:0;overflow:hidden;\"></div>",
                 &format!("id=\"options-data\" style=\"visibility:hidden;height:0;overflow:hidden;\">{}</div>", options_json));

    frame.event_handler(MainHandler);
    frame.load_html(html.as_bytes(), Some("this://app/index.html"));
    frame.set_title("HopToDesk");

    let hwnd = frame.get_hwnd();
    set_window_icon(hwnd);

    unsafe {
        TIMER_STATE = Some(state.clone());
        TIMER_HWND = hwnd;
        SetTimer(hwnd as *mut std::ffi::c_void, 1, 1000, Some(main_timer_callback));
    }

    crate::config::write_log(&format!("[UI] Window created, entering event loop"));
    frame.run_app();
    std::process::exit(0);
}

unsafe extern "system" fn main_timer_callback(
    _hwnd: *mut std::ffi::c_void,
    _msg: u32,
    _id: usize,
    _time: u32,
) {
    if sciter::engine::host::script_busy() {
        return;
    }
    let state = match TIMER_STATE.as_ref() {
        Some(s) => s,
        None => return,
    };
    let hwnd = TIMER_HWND;
    if hwnd.is_null() {
        return;
    }

    let root = match sciter::Element::from_window(hwnd) {
        Ok(r) => r,
        Err(_) => return,
    };

    let signal_status = {
        let s = match state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let sig = s.signal_state.lock().unwrap_or_else(|e| e.into_inner());
        sig.status.clone()
    };

    if let Ok(Some(mut icon)) = root.find_first("#status-icon") {
        let (css_class, status_key) = match signal_status.as_str() {
            "online" => ("connect-status-icon status-online", "Ready"),
            "connecting" => ("connect-status-icon status-connecting", "Connecting..."),
            _ => ("connect-status-icon status-offline", "Not connected"),
        };
        let _ = icon.set_attribute("class", css_class);
        if let Ok(Some(mut txt)) = root.find_first("#status-text") {
            let _ = txt.set_text(&lang::translate(status_key.to_string()));
        }
    }

    TIMER_TICK += 1;
    if TIMER_TICK % 10 == 0 {
        let disk_cfg = config::Config::load();
        let mut pw_changed = false;
        if let Ok(mut s) = state.lock() {
            if s.config.password != disk_cfg.password {
                s.config.password = disk_cfg.password.clone();
                pw_changed = true;
            }
        }
        if pw_changed {
            if let Ok(Some(mut pwbox)) = root.find_first("#pwbox") {
                let _ = pwbox.set_text(&disk_cfg.password);
            }
        }
    }

    {
        let installed = crate::install::is_installed();
        if let Ok(Some(mut btn)) = root.find_first("#install-btn") {
            let _ = btn.set_style_attribute(
                "display",
                if installed { "none" } else { "inline-block" },
            );
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#install-trigger") {
        let val = el.get_text();
        if !val.is_empty() {
            let _ = el.set_text("");
            crate::config::write_log("[install-ui] Install button clicked, opening install window");
            let exe = std::env::current_exe().unwrap_or_default();
            let _ = std::process::Command::new(&exe)
                .arg("--install-ui")
                .arg(std::process::id().to_string())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#connect-target") {
        let target = el.get_text();
        if !target.is_empty() {
            let _ = el.set_text("");
            let target_id = target.replace(' ', "");

            if let Ok(mut s) = state.lock() {
                s.local_config.set_remote_id(&target_id);
            }

            let exe = std::env::current_exe().unwrap_or_default();
            crate::config::write_log(&format!("[UI] Spawning: {} --connect {}", exe.display(), target_id));
            match std::process::Command::new(&exe)
                .args(["--connect", &target_id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(_) => {},
                Err(e) => crate::config::write_log(&format!("[UI] Spawn failed: {}", e)),
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ft-target") {
        let target = el.get_text();
        if !target.is_empty() {
            let _ = el.set_text("");
            let target_id = target.replace(' ', "");
            crate::config::write_log(&format!("[UI] File transfer target: {}", target_id));

            if let Ok(mut s) = state.lock() {
                s.local_config.set_remote_id(&target_id);
            }

            let exe = std::env::current_exe().unwrap_or_default();
            crate::config::write_log(&format!("[UI] Spawning: {} --connect {} --file-transfer", exe.display(), target_id));
            match std::process::Command::new(&exe)
                .args(["--connect", &target_id, "--file-transfer"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(_) => {},
                Err(e) => crate::config::write_log(&format!("[UI] Spawn failed: {}", e)),
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#tab-switch-flag") {
        let tab = el.get_text();
        if !tab.is_empty() {
            let _ = el.set_text("");
            if let Ok(mut s) = state.lock() {
                s.active_tab = tab;
                s.sessions_dirty = true;
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#fav-toggle-flag") {
        let id = el.get_text();
        if !id.is_empty() {
            let _ = el.set_text("");
            if let Ok(mut s) = state.lock() {
                s.local_config.toggle_fav(&id);
                s.sessions_dirty = true;
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#remove-peer-flag") {
        let id = el.get_text();
        if !id.is_empty() {
            let _ = el.set_text("");
            if let Ok(mut s) = state.lock() {
                s.local_config.remove_recent_peer(&id);
                s.sessions_dirty = true;
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#forget-pw-flag") {
        let id = el.get_text();
        if !id.is_empty() {
            let _ = el.set_text("");
            let mut peer_cfg = config::PeerConfig::load(&id);
            peer_cfg.options.remove("password");
            peer_cfg.save(&id);
            config::write_log(&format!("[UI] Forgot password for peer {}", id));
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-perm-pw-flag") {
        let pw = el.get_text();
        if !pw.is_empty() {
            let _ = el.set_text("");
            if let Ok(mut s) = state.lock() {
                if pw == "__CLEAR__" {
                    s.config.permanent_password.clear();
                } else {
                    s.config.permanent_password = pw;
                }
                s.config.save();
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-option-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = text.splitn(2, '|').collect();
            if parts.len() == 2 {
                if let Ok(mut s) = state.lock() {
                    s.config2.set_option(parts[0], parts[1]);
                    s.config2.save();
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#rename-peer-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = text.splitn(2, '|').collect();
            if parts.len() == 2 {
                let peer_id = parts[0];
                let alias = parts[1];
                let mut peer_cfg = config::PeerConfig::load(peer_id);
                peer_cfg.alias = alias.to_string();
                peer_cfg.save(peer_id);
                if let Ok(mut s) = state.lock() {
                    s.sessions_dirty = true;
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#wol-flag") {
        let peer_id = el.get_text();
        if !peer_id.is_empty() {
            let _ = el.set_text("");
            let peer_cfg = config::PeerConfig::load(&peer_id);
            let mac = peer_cfg.get_option("mac_address");
            if mac.is_empty() {
                config::write_log(&format!("[WOL] No MAC address set for peer {}", peer_id));
            } else {
                send_wol_packet(&mac);
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-mac-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = text.splitn(2, '|').collect();
            if parts.len() == 2 {
                let peer_id = parts[0];
                let mac = parts[1];
                let mut peer_cfg = config::PeerConfig::load(peer_id);
                peer_cfg.set_option("mac_address", mac);
                peer_cfg.save(peer_id);
                config::write_log(&format!("[WOL] MAC address set for peer {}: {}", peer_id, mac));
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-relay-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            let parts: Vec<&str> = text.splitn(2, '|').collect();
            if parts.len() == 2 {
                let peer_id = parts[0];
                let mut peer_cfg = config::PeerConfig::load(peer_id);
                peer_cfg.set_option("force-always-relay", parts[1]);
                peer_cfg.save(peer_id);
                if let Ok(mut s) = state.lock() {
                    s.sessions_dirty = true;
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-proxy-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Ok(mut s) = state.lock() {
                    s.config2.set_option("socks-proxy", v["proxy"].as_str().unwrap_or(""));
                    s.config2.set_option("socks-username", v["username"].as_str().unwrap_or(""));
                    s.config2.set_option("socks-password", v["password"].as_str().unwrap_or(""));
                    s.config2.set_option("socks-proxy-type", v["proxy_type"].as_str().unwrap_or("auto"));
                    s.config2.save();
                    config::write_log(&format!("[proxy] Proxy settings saved: {}", v["proxy"].as_str().unwrap_or("")));
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-lang-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            crate::config::write_log(&format!("[UI] Language changed to: {}", text));
            lang::set_lang(&text);
            if let Ok(mut s) = state.lock() {
                s.local_config.set_option("lang", &text);
                s.local_config.save();
            }

            let tr_json = build_ui_translations();
            if let Ok(Some(mut tr_el)) = root.find_first("#tr-data") {
                let _ = tr_el.set_text(&tr_json);
            }

            if let Ok(Some(mut code_el)) = root.find_first("#current-lang-code") {
                let _ = code_el.set_text(&text);
            }

            if let Ok(Some(body)) = root.find_first("body") {
                let _ = body.eval_script("try { initTranslations(); applyTranslations(); } catch(e) {}");
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#open-url-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            let _ = std::process::Command::new("cmd")
                .args(&["/C", "start", &text])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#2fa-generate-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            config::write_log("[2FA] Generate requested");
            let url = auth_2fa::generate2fa();
            config::write_log(&format!("[2FA] Generated URL ({} chars)", url.len()));
            let qr_uri = auth_2fa::generate_qr_data_uri(&url);
            config::write_log(&format!("[2FA] QR data URI ({} chars)", qr_uri.len()));
            if let Ok(Some(mut qr_el)) = root.find_first("#2fa-qr-data") {
                let _ = qr_el.set_text(&qr_uri);
            }
            if let Ok(Some(mut url_el)) = root.find_first("#2fa-url-data") {
                let _ = url_el.set_text(&url);
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#2fa-verify-flag") {
        let code = el.get_text();
        if !code.is_empty() {
            let _ = el.set_text("");
            let result = if auth_2fa::verify2fa(&code) { "ok" } else { "fail" };
            if let Ok(Some(mut res_el)) = root.find_first("#2fa-verify-result") {
                let _ = res_el.set_text(result);
            }
            if result == "ok" {
                if let Ok(Some(mut status_el)) = root.find_first("#2fa-status") {
                    let _ = status_el.set_text("on");
                }
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#2fa-disable-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            auth_2fa::disable_2fa();
            if let Ok(Some(mut status_el)) = root.find_first("#2fa-status") {
                let _ = status_el.set_text("");
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-custom-api-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            if let Ok(mut s) = state.lock() {
                s.config2.set_option("custom-rendezvous-server", &text);
                s.config2.save();
                config::write_log(&format!("[UI] Custom network set to: {}", text));
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#set-invite-code-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            if let Ok(mut s) = state.lock() {
                s.config2.set_option("invite_code", &text);
                s.config2.save();
                config::write_log(&format!("[UI] Invite code set: {}", text));
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#open-ticket-flag") {
        let text = el.get_text();
        if !text.is_empty() {
            let _ = el.set_text("");
            let exe = std::env::current_exe().unwrap_or_default();
            let _ = std::process::Command::new(&exe)
                .args(["--ticket"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }

    let (ua_enabled, has_perm) = if let Ok(s) = state.lock() {
        let ua = s.config2.get_option("unattended-access") == "Y";
        let pw = !s.config.permanent_password.is_empty();
        (ua, pw)
    } else {
        (false, false)
    };
    if let Ok(Some(mut el)) = root.find_first("#ua-status") {
        let _ = el.set_text("");
    }
    if let Ok(Some(mut sw)) = root.find_first("#sw-ua") {
        let has_class = sw.get_attribute("class").map_or(false, |c| c.contains("on"));
        if ua_enabled && !has_class {
            let _ = sw.set_attribute("class", "toggle-switch on");
        } else if !ua_enabled && has_class {
            let _ = sw.set_attribute("class", "toggle-switch");
        }
    }

    {
        let mut should_update = false;
        let mut items_html = String::new();

        if let Ok(mut s) = state.lock() {
            let disk_mtime = std::fs::metadata(s.local_config.config_file())
                .and_then(|m| m.modified())
                .ok();
            if disk_mtime != s.local_config_mtime {
                if s.local_config_mtime.is_some() {
                    s.local_config = config::LocalConfig::load();
                    s.sessions_dirty = true;
                }
                s.local_config_mtime = disk_mtime;
            }
            if s.sessions_dirty {
                s.sessions_dirty = false;
                should_update = true;
                let tab = s.active_tab.clone();

                let peers: Vec<config::RecentPeer> = if tab == "fav" {
                    s.local_config.fav.iter().filter_map(|fav_id| {
                        s.local_config.recent_peers.iter().find(|p| p.id == *fav_id).cloned()
                            .or_else(|| Some(config::RecentPeer {
                                id: fav_id.clone(),
                                username: String::new(),
                                hostname: String::new(),
                                platform: String::new(),
                            }))
                    }).collect()
                } else {
                    s.local_config.recent_peers.clone()
                };

                if peers.is_empty() {
                    let msg = if tab == "fav" {
                        lang::translate("empty_favorite_tip".to_string())
                    } else {
                        lang::translate("empty_recent_tip".to_string())
                    };
                    items_html = format!("<div class=\"empty-state\">{}</div>", msg);
                } else {
                    for p in &peers {
                        let display_id = crate::format_id(&p.id);
                        let peer_cfg = config::PeerConfig::load(&p.id);
                        let is_fav = s.local_config.is_fav(&p.id);
                        let fav_class = if is_fav { "session-fav is-fav" } else { "session-fav" };
                        let heart = if is_fav { CARD_HEART_FILLED } else { CARD_HEART_OUTLINE };
                        let display_name = if !peer_cfg.alias.is_empty() {
                            peer_cfg.alias.clone()
                        } else {
                            display_id.clone()
                        };
                        let caption = if !p.hostname.is_empty() {
                            if !p.username.is_empty() && p.username != "android" && p.username != "ios" {
                                format!("{}@{}", p.username, p.hostname)
                            } else {
                                p.hostname.clone()
                            }
                        } else {
                            display_id.clone()
                        };

                        items_html.push_str(&format!(
                            "<div class=\"session-item\" data-id=\"{}\" data-relay=\"{}\">\
                                <div class=\"session-tile\">{}<div class=\"session-caption\">{}</div></div>\
                                <div class=\"{}\">{}</div>\
                                <div class=\"session-strip\">\
                                    <div class=\"session-name\">{}</div>\
                                    <div class=\"session-menu\">{}</div>\
                                </div>\
                            </div>",
                            p.id, peer_cfg.get_option("force-always-relay"), platform_card_svg(&p.platform), crate::cm::html_escape(&caption), fav_class, heart, crate::cm::html_escape(&display_name), CARD_MENU_DOTS
                        ));
                    }
                }
            }
        }

        if should_update {
            if let Ok(Some(mut list)) = root.find_first("#sessions-list") {
                let _ = list.set_html(items_html.as_bytes(), None);
            }
        }
    }

    if let Ok(Some(mut el)) = root.find_first("#ctx-menu-flag") {
        let peer_id = el.get_text();
        if !peer_id.is_empty() {
            let _ = el.set_text("");

            if let Ok(Some(body)) = root.find_first("body") {
                let _ = body.eval_script(&format!("try {{ showCtxMenuById('{}'); }} catch(e) {{}}", peer_id));
            }
        }
    }
}
