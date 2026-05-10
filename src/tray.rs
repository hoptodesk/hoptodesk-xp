use std::ffi::CString;
use std::process::{Command, Stdio};
use std::ptr;

use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HICON, HMENU, HWND, POINT};
use winapi::shared::winerror::ERROR_ALREADY_EXISTS;
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::libloaderapi::GetModuleHandleA;
use winapi::um::shellapi::{
    Shell_NotifyIconA, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAA,
};
use winapi::um::synchapi::CreateMutexA;
use winapi::um::winuser::{
    AppendMenuA, CreatePopupMenu, CreateWindowExA, DefWindowProcA, DestroyMenu, DispatchMessageA,
    GetCursorPos, GetMessageA, LoadIconA, PostQuitMessage, RegisterClassExA, RegisterWindowMessageA,
    SetForegroundWindow, TrackPopupMenu, TranslateMessage, IDI_APPLICATION, MF_SEPARATOR, MF_STRING,
    MSG, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK,
    WM_RBUTTONUP, WNDCLASSEXA,
};

const TRAY_CALLBACK_MSG: UINT = 0x0400 + 1;
const ID_OPEN: u16 = 1001;
const ID_TICKET: u16 = 1002;
const ID_EXIT: u16 = 1003;

static mut TASKBAR_CREATED_MSG: UINT = 0;

pub fn start() {
    if !acquire_single_instance() {
        crate::config::write_log("[tray] another tray instance is already running, exiting");
        return;
    }

    crate::config::write_log("[tray] starting");
    spawn_cm_watcher();
    unsafe {
        let hinst = GetModuleHandleA(ptr::null());
        let class_name = CString::new("HopToDeskTrayWnd").unwrap();
        let wnd_class = WNDCLASSEXA {
            cbSize: std::mem::size_of::<WNDCLASSEXA>() as u32,
            style: 0,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst as _,
            hIcon: load_app_icon(hinst as _),
            hCursor: ptr::null_mut(),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr() as _,
            hIconSm: load_app_icon(hinst as _),
        };
        if RegisterClassExA(&wnd_class) == 0 {
            crate::config::write_log(&format!(
                "[tray] RegisterClassExA failed (err={})",
                GetLastError()
            ));
            return;
        }

        let title = CString::new("HopToDesk").unwrap();
        let hwnd = CreateWindowExA(
            0,
            class_name.as_ptr() as _,
            title.as_ptr() as _,
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst as _,
            ptr::null_mut(),
        );
        if hwnd.is_null() {
            crate::config::write_log(&format!(
                "[tray] CreateWindowExA failed (err={})",
                GetLastError()
            ));
            return;
        }

        let taskbar_msg_name = CString::new("TaskbarCreated").unwrap();
        TASKBAR_CREATED_MSG = RegisterWindowMessageA(taskbar_msg_name.as_ptr() as _);

        if !add_tray_icon(hwnd, hinst as _) {
            crate::config::write_log("[tray] Shell_NotifyIcon NIM_ADD failed");
        }

        let mut msg: MSG = std::mem::zeroed();
        loop {
            let r = GetMessageA(&mut msg, ptr::null_mut(), 0, 0);
            if r <= 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }

        remove_tray_icon(hwnd);
    }
    crate::config::write_log("[tray] exiting");
}

fn acquire_single_instance() -> bool {
    unsafe {
        let name = CString::new("Global\\HopToDeskTraySingleton").unwrap();
        let h = CreateMutexA(ptr::null_mut(), 0, name.as_ptr() as _);
        if h.is_null() {
            return false;
        }
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

unsafe fn load_app_icon(hinst: *mut std::ffi::c_void) -> HICON {
    let icon = LoadIconA(hinst as _, 1 as _);
    if !icon.is_null() {
        return icon;
    }
    LoadIconA(ptr::null_mut(), IDI_APPLICATION as _)
}

unsafe fn add_tray_icon(hwnd: HWND, hinst: *mut std::ffi::c_void) -> bool {
    let mut nid: NOTIFYICONDATAA = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAA>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = TRAY_CALLBACK_MSG;
    nid.hIcon = load_app_icon(hinst);
    let tip = b"HopToDesk - Service is running\0";
    let n = std::cmp::min(tip.len(), nid.szTip.len());
    for i in 0..n {
        nid.szTip[i] = tip[i] as i8;
    }
    Shell_NotifyIconA(NIM_ADD, &mut nid) != 0
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAA = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAA>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    Shell_NotifyIconA(NIM_DELETE, &mut nid);
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if TASKBAR_CREATED_MSG != 0 && msg == TASKBAR_CREATED_MSG {
        let hinst = GetModuleHandleA(ptr::null());
        add_tray_icon(hwnd, hinst as _);
        return 0;
    }
    match msg {
        TRAY_CALLBACK_MSG => {
            let event = lparam as u32;
            if event == WM_RBUTTONUP {
                show_context_menu(hwnd);
            } else if event == WM_LBUTTONDBLCLK {
                spawn_main_ui();
            }
            0
        }
        WM_COMMAND => {
            let cmd = (wparam & 0xFFFF) as u16;
            handle_command(hwnd, cmd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcA(hwnd, msg, wparam, lparam),
    }
}

unsafe fn show_context_menu(hwnd: HWND) {
    let menu: HMENU = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let s_open = CString::new("Open HopToDesk").unwrap();
    AppendMenuA(menu, MF_STRING, ID_OPEN as _, s_open.as_ptr() as _);

    if crate::dashboard::is_linked() {
        let s_ticket = CString::new("Submit Ticket").unwrap();
        AppendMenuA(menu, MF_STRING, ID_TICKET as _, s_ticket.as_ptr() as _);
    }

    AppendMenuA(menu, MF_SEPARATOR, 0, ptr::null());
    let s_exit = CString::new("Exit").unwrap();
    AppendMenuA(menu, MF_STRING, ID_EXIT as _, s_exit.as_ptr() as _);

    let mut pt: POINT = std::mem::zeroed();
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        ptr::null(),
    );
    DestroyMenu(menu);
}

unsafe fn handle_command(hwnd: HWND, cmd: u16) {
    match cmd {
        ID_OPEN => spawn_main_ui(),
        ID_TICKET => spawn_ticket_window(),
        ID_EXIT => {
            remove_tray_icon(hwnd);
            PostQuitMessage(0);
        }
        _ => {}
    }
}

fn spawn_main_ui() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            crate::config::write_log(&format!("[tray] current_exe failed: {}", e));
            return;
        }
    };
    crate::config::write_log(&format!("[tray] spawning main UI: {}", exe.display()));
    let _ = Command::new(&exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn spawn_ticket_window() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            crate::config::write_log(&format!("[tray] current_exe failed: {}", e));
            return;
        }
    };
    crate::config::write_log(&format!("[tray] spawning ticket window: {}", exe.display()));
    let _ = Command::new(&exe)
        .arg("--ticket")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn spawn_cm_watcher() {
    std::thread::spawn(|| cm_watcher_loop());
}

fn cm_watcher_loop() {
    use std::collections::HashSet;
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            crate::config::write_log(&format!("[tray-cm] current_exe failed: {}", e));
            return;
        }
    };

    let mut spawned: HashSet<String> = HashSet::new();
    let dir = crate::cm::cm_temp_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue,
            };
            if name.starts_with("hoptodesk_cm_") && name.ends_with(".json") {
                let sid = &name["hoptodesk_cm_".len()..name.len() - ".json".len()];
                if !sid.is_empty() {
                    spawned.insert(sid.to_string());
                }
            }
        }
    }
    crate::config::write_log(&format!(
        "[tray-cm] watcher started; ignored {} pre-existing info file(s)",
        spawned.len()
    ));

    loop {
        let dir = crate::cm::cm_temp_dir();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = match entry.file_name().into_string() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if !name.starts_with("hoptodesk_cm_") || !name.ends_with(".json") {
                    continue;
                }
                let sid = &name["hoptodesk_cm_".len()..name.len() - ".json".len()];
                if sid.is_empty() || spawned.contains(sid) {
                    continue;
                }
                spawned.insert(sid.to_string());
                crate::config::write_log(&format!(
                    "[tray-cm] spawning --cm {} for new info file",
                    sid
                ));
                let _ = Command::new(&exe)
                    .args(["--cm", sid])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}
