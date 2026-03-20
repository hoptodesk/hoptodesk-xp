
use std::mem::size_of;
use winapi::shared::minwindef::{BOOL, DWORD, FALSE, TRUE, UINT};
use winapi::shared::windef::{HBITMAP, HICON, POINT};

#[repr(C)]
struct CURSORINFO {
    cb_size: DWORD,
    flags: DWORD,
    h_cursor: HICON,
    pt_screen_pos: POINT,
}

#[repr(C)]
struct ICONINFO {
    f_icon: BOOL,
    x_hotspot: DWORD,
    y_hotspot: DWORD,
    hbm_mask: HBITMAP,
    hbm_color: HBITMAP,
}

extern "system" {
    fn GetCursorInfo(pci: *mut CURSORINFO) -> BOOL;
    fn GetIconInfo(hIcon: HICON, piconinfo: *mut ICONINFO) -> BOOL;
    fn CopyIcon(hIcon: HICON) -> HICON;
    fn DestroyIcon(hIcon: HICON) -> BOOL;
    fn BlockInput(fBlockIt: BOOL) -> BOOL;
    fn OpenInputDesktop(dwFlags: DWORD, fInherit: BOOL, dwDesiredAccess: DWORD) -> *mut std::ffi::c_void;
    fn SetThreadDesktop(hDesktop: *mut std::ffi::c_void) -> BOOL;
    fn CloseDesktop(hDesktop: *mut std::ffi::c_void) -> BOOL;
}

const CURSOR_SHOWING: DWORD = 0x00000001;

pub struct CursorState {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub cursor_handle: usize,
}

pub fn get_cursor() -> Option<CursorState> {
    unsafe {
        let mut ci: CURSORINFO = std::mem::zeroed();
        ci.cb_size = size_of::<CURSORINFO>() as DWORD;
        if GetCursorInfo(&mut ci) == FALSE {
            return None;
        }
        Some(CursorState {
            x: ci.pt_screen_pos.x,
            y: ci.pt_screen_pos.y,
            visible: (ci.flags & CURSOR_SHOWING) != 0,
            cursor_handle: ci.h_cursor as usize,
        })
    }
}

pub struct CursorData {
    pub id: u64,
    pub hotx: i32,
    pub hoty: i32,
    pub width: i32,
    pub height: i32,
    pub colors: Vec<u8>,
}

pub fn get_cursor_data(cursor_handle: usize) -> Option<CursorData> {
    use winapi::um::wingdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, RGBQUAD,
    };

    unsafe {
        let hcursor = cursor_handle as HICON;
        let hicon = CopyIcon(hcursor);
        if hicon.is_null() {
            return None;
        }

        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(hicon, &mut ii) == FALSE {
            DestroyIcon(hicon);
            return None;
        }

        let mut bmp: winapi::um::wingdi::BITMAP = std::mem::zeroed();
        if winapi::um::wingdi::GetObjectW(
            ii.hbm_mask as _,
            size_of::<winapi::um::wingdi::BITMAP>() as i32,
            &mut bmp as *mut _ as _,
        ) == 0
        {
            cleanup_icon_info(&ii, hicon);
            return None;
        }

        let width = bmp.bmWidth;
        let height = if ii.hbm_color.is_null() {
            bmp.bmHeight / 2
        } else {
            bmp.bmHeight
        };

        if width <= 0 || height <= 0 || width > 256 || height > 256 {
            cleanup_icon_info(&ii, hicon);
            return None;
        }

        let mut colors = vec![0u8; (width * height * 4) as usize];

        if !ii.hbm_color.is_null() {

            let hdc = CreateCompatibleDC(std::ptr::null_mut());
            if hdc.is_null() {
                cleanup_icon_info(&ii, hicon);
                return None;
            }

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as _,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [RGBQUAD {
                    rgbBlue: 0,
                    rgbGreen: 0,
                    rgbRed: 0,
                    rgbReserved: 0,
                }],
            };

            GetDIBits(
                hdc,
                ii.hbm_color,
                0,
                height as _,
                colors.as_mut_ptr() as _,
                &mut bmi as _,
                DIB_RGB_COLORS,
            );

            let mask_size = (width * height) as usize;
            let mut mask = vec![0u8; mask_size * 4];
            bmi.bmiHeader.biHeight = -height;
            GetDIBits(
                hdc,
                ii.hbm_mask,
                0,
                height as _,
                mask.as_mut_ptr() as _,
                &mut bmi as _,
                DIB_RGB_COLORS,
            );

            for i in 0..mask_size {
                let mask_val = mask[i * 4];
                if mask_val == 0 {
                    colors[i * 4 + 3] = 255;
                }

            }

            DeleteDC(hdc);
        } else {

            let hdc = CreateCompatibleDC(std::ptr::null_mut());
            if hdc.is_null() {
                cleanup_icon_info(&ii, hicon);
                return None;
            }

            let full_height = bmp.bmHeight;
            let mut full_mask = vec![0u8; (width * full_height * 4) as usize];

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as _,
                    biWidth: width,
                    biHeight: -full_height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [RGBQUAD {
                    rgbBlue: 0,
                    rgbGreen: 0,
                    rgbRed: 0,
                    rgbReserved: 0,
                }],
            };

            GetDIBits(
                hdc,
                ii.hbm_mask,
                0,
                full_height as _,
                full_mask.as_mut_ptr() as _,
                &mut bmi as _,
                DIB_RGB_COLORS,
            );

            let pixel_count = (width * height) as usize;
            for i in 0..pixel_count {
                let and_val = full_mask[i * 4];
                let xor_offset = pixel_count * 4;
                let xor_val = full_mask[xor_offset + i * 4];

                if and_val == 0 && xor_val == 0 {

                    colors[i * 4] = 0;
                    colors[i * 4 + 1] = 0;
                    colors[i * 4 + 2] = 0;
                    colors[i * 4 + 3] = 255;
                } else if and_val == 0xFF && xor_val == 0xFF {

                    colors[i * 4] = 255;
                    colors[i * 4 + 1] = 255;
                    colors[i * 4 + 2] = 255;
                    colors[i * 4 + 3] = 255;
                } else if and_val == 0xFF && xor_val == 0 {

                    colors[i * 4] = 0;
                    colors[i * 4 + 1] = 0;
                    colors[i * 4 + 2] = 0;
                    colors[i * 4 + 3] = 0;
                } else {

                    colors[i * 4] = 255;
                    colors[i * 4 + 1] = 255;
                    colors[i * 4 + 2] = 255;
                    colors[i * 4 + 3] = 128;
                }
            }

            DeleteDC(hdc);
        }

        let data = CursorData {
            id: cursor_handle as u64,
            hotx: ii.x_hotspot as i32,
            hoty: ii.y_hotspot as i32,
            width,
            height,
            colors,
        };

        cleanup_icon_info(&ii, hicon);
        Some(data)
    }
}

unsafe fn cleanup_icon_info(ii: &ICONINFO, hicon: HICON) {
    use winapi::um::wingdi::DeleteObject;
    if !ii.hbm_mask.is_null() {
        DeleteObject(ii.hbm_mask as _);
    }
    if !ii.hbm_color.is_null() {
        DeleteObject(ii.hbm_color as _);
    }
    DestroyIcon(hicon);
}

pub fn block_input(block: bool) -> bool {
    unsafe { BlockInput(if block { TRUE } else { FALSE }) != FALSE }
}

const DESKTOP_SWITCHDESKTOP: DWORD = 0x0100;
const GENERIC_ALL: DWORD = 0x10000000;

pub fn try_change_desktop() -> bool {
    unsafe {
        let hdesk = OpenInputDesktop(0, FALSE, GENERIC_ALL);
        if hdesk.is_null() {
            return false;
        }
        let result = SetThreadDesktop(hdesk) != FALSE;
        CloseDesktop(hdesk);
        result
    }
}

pub fn blank_screen(blank: bool) {
    use winapi::um::winuser::{SendMessageW, HWND_BROADCAST, SC_MONITORPOWER, WM_SYSCOMMAND};
    unsafe {
        let param = if blank { 2isize } else { -1isize };
        SendMessageW(
            HWND_BROADCAST,
            WM_SYSCOMMAND,
            SC_MONITORPOWER as _,
            param,
        );
    }
}

pub fn get_clipboard_text() -> Option<String> {
    use winapi::um::winuser::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, CF_UNICODETEXT,
    };
    use winapi::um::winbase::{GlobalLock, GlobalUnlock};

    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == FALSE {
            return None;
        }
        if OpenClipboard(std::ptr::null_mut()) == FALSE {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle.is_null() {
            CloseClipboard();
            return None;
        }
        let ptr = GlobalLock(handle) as *const u16;
        if ptr.is_null() {
            CloseClipboard();
            return None;
        }

        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
            if len > 1024 * 1024 {
                break;
            }
        }

        let slice = std::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);

        GlobalUnlock(handle);
        CloseClipboard();
        Some(text)
    }
}

pub fn set_clipboard_text(text: &str) -> bool {
    use winapi::um::winuser::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData, CF_UNICODETEXT,
    };
    use winapi::um::winbase::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let size = wide.len() * 2;

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == FALSE {
            return false;
        }
        EmptyClipboard();

        let hmem = GlobalAlloc(GMEM_MOVEABLE, size);
        if hmem.is_null() {
            CloseClipboard();
            return false;
        }

        let ptr = GlobalLock(hmem) as *mut u16;
        if ptr.is_null() {
            CloseClipboard();
            return false;
        }

        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        GlobalUnlock(hmem);

        SetClipboardData(CF_UNICODETEXT, hmem);
        CloseClipboard();
        true
    }
}

pub fn get_cursor_pos() -> Option<(i32, i32)> {
    unsafe {
        let mut pt: POINT = std::mem::zeroed();
        extern "system" {
            fn GetCursorPos(lpPoint: *mut POINT) -> BOOL;
        }
        if GetCursorPos(&mut pt) == FALSE {
            return None;
        }
        Some((pt.x, pt.y))
    }
}

pub fn lock_screen() {
    extern "system" {
        fn LockWorkStation() -> BOOL;
    }
    unsafe {
        LockWorkStation();
    }
}

static mut LAST_DESKTOP_INPUT: *mut std::ffi::c_void = std::ptr::null_mut();

pub fn desktop_changed() -> bool {
    unsafe {
        let hdesk = OpenInputDesktop(0, FALSE, DESKTOP_SWITCHDESKTOP);
        if hdesk.is_null() {
            return true;
        }
        let changed = if LAST_DESKTOP_INPUT.is_null() {
            false
        } else {
            hdesk != LAST_DESKTOP_INPUT
        };
        LAST_DESKTOP_INPUT = hdesk;
        CloseDesktop(hdesk);
        changed
    }
}

pub fn set_prevent_sleep(prevent: bool) {

    const ES_CONTINUOUS: DWORD = 0x80000000;
    const ES_SYSTEM_REQUIRED: DWORD = 0x00000001;
    const ES_DISPLAY_REQUIRED: DWORD = 0x00000002;

    extern "system" {
        fn SetThreadExecutionState(esFlags: DWORD) -> DWORD;
    }

    unsafe {
        if prevent {
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED);
        } else {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}

pub fn get_double_click_time() -> u32 {
    extern "system" {
        fn GetDoubleClickTime() -> UINT;
    }
    unsafe { GetDoubleClickTime() }
}

pub fn wide_string(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[repr(C)]
struct PROCESSENTRY32 {
    dw_size: DWORD,
    cnt_usage: DWORD,
    th32_process_id: DWORD,
    th32_default_heap_id: usize,
    th32_module_id: DWORD,
    cnt_threads: DWORD,
    th32_parent_process_id: DWORD,
    pc_pri_class_base: i32,
    dw_flags: DWORD,
    sz_exe_file: [u8; 260],
}

extern "system" {
    fn CreateToolhelp32Snapshot(dwFlags: DWORD, th32ProcessID: DWORD) -> *mut std::ffi::c_void;
    fn Process32First(hSnapshot: *mut std::ffi::c_void, lppe: *mut PROCESSENTRY32) -> BOOL;
    fn Process32Next(hSnapshot: *mut std::ffi::c_void, lppe: *mut PROCESSENTRY32) -> BOOL;
}

const TH32CS_SNAPPROCESS: DWORD = 0x00000002;

pub fn is_exe_running(exe_name: &str) -> bool {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot == -1isize as *mut std::ffi::c_void {
            return false;
        }

        let mut pe32: PROCESSENTRY32 = std::mem::zeroed();
        pe32.dw_size = size_of::<PROCESSENTRY32>() as DWORD;

        if Process32First(snapshot, &mut pe32) != FALSE {
            loop {
                let name = std::ffi::CStr::from_ptr(pe32.sz_exe_file.as_ptr() as *const i8)
                    .to_string_lossy();
                if name.to_lowercase() == exe_name.to_lowercase() {
                    CloseHandle(snapshot);
                    return true;
                }
                if Process32Next(snapshot, &mut pe32) == FALSE {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        false
    }
}

pub fn get_current_process_session_id() -> Option<u32> {
    extern "system" {
        fn GetCurrentProcessId() -> DWORD;
        fn ProcessIdToSessionId(dwProcessId: DWORD, pSessionId: *mut DWORD) -> BOOL;
    }
    let mut sid: DWORD = 0;
    unsafe {
        if ProcessIdToSessionId(GetCurrentProcessId(), &mut sid) != FALSE {
            Some(sid)
        } else {
            None
        }
    }
}

#[repr(C)]
struct DEVMODEW {
    dm_device_name: [u16; 32],
    dm_spec_version: u16,
    dm_driver_version: u16,
    dm_size: u16,
    dm_driver_extra: u16,
    dm_fields: DWORD,

    dm_position_x: i32,
    dm_position_y: i32,
    dm_display_orientation: DWORD,
    dm_display_fixed_output: DWORD,

    dm_color: i16,
    dm_duplex: i16,
    dm_y_resolution: i16,
    dm_tt_option: i16,
    dm_collate: i16,
    dm_form_name: [u16; 32],
    dm_log_pixels: u16,
    dm_bits_per_pel: DWORD,
    dm_pels_width: DWORD,
    dm_pels_height: DWORD,
    dm_display_flags: DWORD,
    dm_display_frequency: DWORD,

    dm_icm_method: DWORD,
    dm_icm_intent: DWORD,
    dm_media_type: DWORD,
    dm_dither_type: DWORD,
    dm_reserved1: DWORD,
    dm_reserved2: DWORD,
    dm_panning_width: DWORD,
    dm_panning_height: DWORD,
}

extern "system" {
    fn EnumDisplaySettingsW(
        lpszDeviceName: *const u16,
        iModeNum: DWORD,
        lpDevMode: *mut DEVMODEW,
    ) -> BOOL;
    fn ChangeDisplaySettingsExW(
        lpszDeviceName: *const u16,
        lpDevMode: *mut DEVMODEW,
        hwnd: *mut std::ffi::c_void,
        dwflags: DWORD,
        lParam: *mut std::ffi::c_void,
    ) -> i32;
}

const ENUM_CURRENT_SETTINGS: DWORD = 0xFFFFFFFF;
const DM_PELSWIDTH: DWORD = 0x00080000;
const DM_PELSHEIGHT: DWORD = 0x00100000;
const CDS_UPDATEREGISTRY: DWORD = 0x00000001;
const CDS_GLOBAL: DWORD = 0x00000008;
const CDS_RESET: DWORD = 0x40000000;
const DISP_CHANGE_SUCCESSFUL: i32 = 0;

pub struct DisplayResolution {
    pub width: u32,
    pub height: u32,
}

pub fn current_resolution() -> Option<DisplayResolution> {
    unsafe {
        let mut dm: DEVMODEW = std::mem::zeroed();
        dm.dm_size = size_of::<DEVMODEW>() as u16;
        if EnumDisplaySettingsW(std::ptr::null(), ENUM_CURRENT_SETTINGS, &mut dm) == FALSE {
            return None;
        }
        Some(DisplayResolution {
            width: dm.dm_pels_width,
            height: dm.dm_pels_height,
        })
    }
}

pub fn available_resolutions() -> Vec<DisplayResolution> {
    let mut v = Vec::new();
    unsafe {
        let mut dm: DEVMODEW = std::mem::zeroed();
        dm.dm_size = size_of::<DEVMODEW>() as u16;
        let mut num = 0u32;
        loop {
            if EnumDisplaySettingsW(std::ptr::null(), num, &mut dm) == FALSE {
                break;
            }
            let r = DisplayResolution {
                width: dm.dm_pels_width,
                height: dm.dm_pels_height,
            };
            if !v.iter().any(|x: &DisplayResolution| x.width == r.width && x.height == r.height) {
                v.push(r);
            }
            num += 1;
        }
    }
    v
}

pub fn change_resolution(width: u32, height: u32) -> bool {
    unsafe {
        let mut dm: DEVMODEW = std::mem::zeroed();
        dm.dm_size = size_of::<DEVMODEW>() as u16;
        dm.dm_pels_width = width;
        dm.dm_pels_height = height;
        dm.dm_fields = DM_PELSWIDTH | DM_PELSHEIGHT;
        ChangeDisplaySettingsExW(
            std::ptr::null(),
            &mut dm,
            std::ptr::null_mut(),
            CDS_UPDATEREGISTRY | CDS_GLOBAL | CDS_RESET,
            std::ptr::null_mut(),
        ) == DISP_CHANGE_SUCCESSFUL
    }
}

pub fn is_elevated() -> bool {
    extern "system" {
        fn IsUserAnAdmin() -> BOOL;
    }
    unsafe { IsUserAnAdmin() != FALSE }
}

pub fn get_username() -> String {
    std::env::var("USERNAME").unwrap_or_default()
}

pub fn get_hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "XP-PC".into())
}

pub fn is_installed() -> bool {

    false
}

extern "system" {
    fn CloseHandle(hObject: *mut std::ffi::c_void) -> BOOL;
}
