use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::channel;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use winapi::shared::minwindef::{LPARAM, LRESULT, WPARAM};
use winapi::shared::windef::{HHOOK, HWND, RECT};
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::libloaderapi::GetModuleHandleW;
use winapi::um::wingdi::{CreateSolidBrush, DeleteObject, RGB};
use winapi::um::winuser::*;

const PRIVACY_CLASS: &str = "HopToDeskPrivacy";
const WM_USER_QUIT_PRIVACY: u32 = WM_USER + 200;
const TOPMOST_TIMER_ID: usize = 1;
const TOPMOST_TIMER_MS: u32 = 100;
const LLKHF_INJECTED_FLAG: u32 = 0x10;
const LLKHF_ALTDOWN_FLAG: u32 = 0x20;
const LLMHF_INJECTED_FLAG: u32 = 0x01;
const VK_P: u32 = 0x50;
const VK_LCONTROL_CODE: u32 = 0xA2;
const VK_RCONTROL_CODE: u32 = 0xA3;

static CAPTURE_LAYERED: AtomicBool = AtomicBool::new(true);
static THREAD_ID: AtomicU32 = AtomicU32::new(0);
static TURNED_OFF_LOCALLY: AtomicBool = AtomicBool::new(false);
static SWITCH: Mutex<()> = Mutex::new(());

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> u32;
}

pub fn capture_layered_windows() -> bool {
    CAPTURE_LAYERED.load(Ordering::Relaxed)
}

pub fn is_on() -> bool {
    THREAD_ID.load(Ordering::Relaxed) != 0
}

pub fn take_turned_off_locally() -> bool {
    TURNED_OFF_LOCALLY.swap(false, Ordering::Relaxed)
}

pub fn turn_on() -> Result<(), String> {
    let _guard = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    if is_on() {
        post_quit();
        wait_until_off(Duration::from_secs(1));
    }
    CAPTURE_LAYERED.store(false, Ordering::Relaxed);
    let (tx, rx) = channel::<Result<(), String>>();
    std::thread::spawn(move || unsafe {
        crate::platform::try_change_desktop();
        match create_privacy_window() {
            Ok((hwnd, hook_keyboard, hook_mouse)) => {
                THREAD_ID.store(GetCurrentThreadId(), Ordering::Relaxed);
                let _ = tx.send(Ok(()));
                SetTimer(hwnd, TOPMOST_TIMER_ID, TOPMOST_TIMER_MS, None);
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    if msg.message == WM_USER_QUIT_PRIVACY {
                        break;
                    }
                    if msg.message == WM_TIMER && msg.hwnd == hwnd {
                        keep_covering(hwnd);
                        continue;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                KillTimer(hwnd, TOPMOST_TIMER_ID);
                UnhookWindowsHookEx(hook_keyboard);
                UnhookWindowsHookEx(hook_mouse);
                DestroyWindow(hwnd);
                CAPTURE_LAYERED.store(true, Ordering::Relaxed);
                THREAD_ID.store(0, Ordering::Relaxed);
            }
            Err(e) => {
                CAPTURE_LAYERED.store(true, Ordering::Relaxed);
                let _ = tx.send(Err(e));
            }
        }
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {
            crate::config::write_log("[privacy] Privacy mode on");
            Ok(())
        }
        Ok(Err(e)) => {
            crate::config::write_log(&format!("[privacy] Privacy mode failed: {}", e));
            Err(e)
        }
        Err(_) => {
            CAPTURE_LAYERED.store(true, Ordering::Relaxed);
            Err("Timeout creating the privacy window".to_string())
        }
    }
}

pub fn turn_off() {
    if !is_on() {
        return;
    }
    post_quit();
    crate::config::write_log("[privacy] Privacy mode off");
}

fn post_quit() {
    let tid = THREAD_ID.load(Ordering::Relaxed);
    if tid != 0 {
        unsafe {
            PostThreadMessageW(tid, WM_USER_QUIT_PRIVACY, 0, 0);
        }
    }
}

fn wait_until_off(timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while is_on() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

unsafe fn keep_covering(hwnd: HWND) {
    let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let cx = GetSystemMetrics(SM_CXVIRTUALSCREEN);
    let cy = GetSystemMetrics(SM_CYVIRTUALSCREEN);
    let mut rect: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut rect);
    let same_place = rect.left == x
        && rect.top == y
        && rect.right - rect.left == cx
        && rect.bottom - rect.top == cy;
    let flags = if same_place {
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE
    } else {
        SWP_NOACTIVATE | SWP_SHOWWINDOW
    };
    SetWindowPos(hwnd, HWND_TOPMOST, x, y, cx, cy, flags);
}

unsafe fn create_privacy_window() -> Result<(HWND, HHOOK, HHOOK), String> {
    let hinstance = GetModuleHandleW(std::ptr::null());
    let class_name: Vec<u16> = PRIVACY_CLASS.encode_utf16().chain(std::iter::once(0)).collect();
    let brush = CreateSolidBrush(RGB(24, 24, 24));
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(DefWindowProcW),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: brush,
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    if RegisterClassExW(&wc) == 0 {
        let err = GetLastError();
        DeleteObject(brush as _);
        if err != winapi::shared::winerror::ERROR_CLASS_ALREADY_EXISTS {
            return Err(format!("RegisterClassExW failed: {}", err));
        }
    }

    let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let cx = GetSystemMetrics(SM_CXVIRTUALSCREEN);
    let cy = GetSystemMetrics(SM_CYVIRTUALSCREEN);
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
        class_name.as_ptr(),
        class_name.as_ptr(),
        WS_POPUP | WS_VISIBLE,
        x,
        y,
        cx,
        cy,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        hinstance,
        std::ptr::null_mut(),
    );
    if hwnd.is_null() {
        return Err(format!("CreateWindowExW failed: {}", GetLastError()));
    }
    SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
    SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW | SWP_NOACTIVATE,
    );

    let hook_keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), hinstance, 0);
    if hook_keyboard.is_null() {
        let err = GetLastError();
        DestroyWindow(hwnd);
        return Err(format!("Keyboard hook failed: {}", err));
    }
    let hook_mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), hinstance, 0);
    if hook_mouse.is_null() {
        let err = GetLastError();
        UnhookWindowsHookEx(hook_keyboard);
        DestroyWindow(hwnd);
        return Err(format!("Mouse hook failed: {}", err));
    }
    Ok((hwnd, hook_keyboard, hook_mouse))
}

unsafe extern "system" fn keyboard_hook(code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param);
    }
    let ks = &*(l_param as *const KBDLLHOOKSTRUCT);
    if ks.flags & LLKHF_INJECTED_FLAG != 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param);
    }
    if ks.flags & LLKHF_ALTDOWN_FLAG != 0 {
        return 1;
    }
    let message = w_param as u32;
    if message == WM_KEYDOWN || message == WM_SYSKEYDOWN {
        if ks.vkCode != VK_P && ks.vkCode != VK_LCONTROL_CODE && ks.vkCode != VK_RCONTROL_CODE {
            return 1;
        }
        let ctrl_down = (GetKeyState(VK_CONTROL) as u16) & 0x8000 != 0;
        if ks.vkCode == VK_P && ctrl_down {
            crate::config::write_log("[privacy] Ctrl+P pressed locally, turning privacy mode off");
            TURNED_OFF_LOCALLY.store(true, Ordering::Relaxed);
            turn_off();
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param)
}

unsafe extern "system" fn mouse_hook(code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param);
    }
    let ms = &*(l_param as *const MSLLHOOKSTRUCT);
    if ms.flags & LLMHF_INJECTED_FLAG != 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param);
    }
    1
}
