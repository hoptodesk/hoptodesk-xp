// OS services shim: clipboard and file dialogs. Windows uses
// raw Win32 (XP-era APIs only); other platforms keep an in-process clipboard
// so the mac harness and tests still round-trip, and no dialogs.

pub struct FileDialogOpts {
    pub save: bool,
    pub title: Option<String>,
    pub directory: Option<String>,
    pub file_name: Option<String>,
    pub filters: Vec<(String, Vec<String>)>,
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use std::cell::RefCell;

    thread_local! {
        static LOCAL_CLIPBOARD: RefCell<String> = RefCell::new(String::new());
    }

    pub fn clipboard_set_text(text: &str) {
        LOCAL_CLIPBOARD.with(|c| *c.borrow_mut() = text.to_string());
    }

    pub fn clipboard_get_text() -> Option<String> {
        LOCAL_CLIPBOARD.with(|c| Some(c.borrow().clone()))
    }

    pub fn pick_file(_opts: &super::FileDialogOpts) -> Option<std::path::PathBuf> {
        None
    }

    pub fn local_utc_offset_minutes() -> i64 {
        0
    }

    pub fn pick_folder(_title: Option<&str>, _dir: Option<&str>) -> Option<std::path::PathBuf> {
        None
    }

    pub fn message_box(_title: &str, _text: &str, _kind: &str) -> bool {
        false
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::ptr::null_mut;
    use winapi::um::winuser::{
        CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
        CF_UNICODETEXT,
    };

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn clipboard_set_text(text: &str) {
        unsafe {
            if OpenClipboard(null_mut()) == 0 {
                return;
            }
            EmptyClipboard();
            let wide = to_wide(text);
            let bytes = wide.len() * 2;
            let h = winapi::um::winbase::GlobalAlloc(winapi::um::winbase::GMEM_MOVEABLE, bytes);
            if !h.is_null() {
                let p = winapi::um::winbase::GlobalLock(h) as *mut u16;
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                    winapi::um::winbase::GlobalUnlock(h);
                    SetClipboardData(CF_UNICODETEXT, h as _);
                }
            }
            CloseClipboard();
        }
    }

    pub fn clipboard_get_text() -> Option<String> {
        unsafe {
            if OpenClipboard(null_mut()) == 0 {
                return None;
            }
            let h = GetClipboardData(CF_UNICODETEXT);
            let out = if h.is_null() {
                None
            } else {
                let p = winapi::um::winbase::GlobalLock(h as _) as *const u16;
                if p.is_null() {
                    None
                } else {
                    let mut len = 0usize;
                    while *p.add(len) != 0 {
                        len += 1;
                    }
                    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
                    winapi::um::winbase::GlobalUnlock(h as _);
                    Some(s)
                }
            };
            CloseClipboard();
            out
        }
    }

    pub fn pick_file(opts: &super::FileDialogOpts) -> Option<std::path::PathBuf> {
        use winapi::um::commdlg::{
            GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
            OFN_OVERWRITEPROMPT, OPENFILENAMEW,
        };
        unsafe {
            let mut file_buf = [0u16; 4096];
            if let Some(name) = &opts.file_name {
                let w: Vec<u16> = name.encode_utf16().collect();
                let n = w.len().min(file_buf.len() - 1);
                file_buf[..n].copy_from_slice(&w[..n]);
            }
            let mut filter: Vec<u16> = Vec::new();
            for (name, exts) in &opts.filters {
                filter.extend(name.encode_utf16());
                filter.push(0);
                let pat = exts
                    .iter()
                    .map(|e| format!("*.{}", e))
                    .collect::<Vec<_>>()
                    .join(";");
                filter.extend(pat.encode_utf16());
                filter.push(0);
            }
            filter.push(0);
            let title = opts.title.as_deref().map(to_wide);
            let dir = opts.directory.as_deref().map(to_wide);
            let mut ofn: OPENFILENAMEW = std::mem::zeroed();
            ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
            ofn.lpstrFile = file_buf.as_mut_ptr();
            ofn.nMaxFile = file_buf.len() as u32;
            if filter.len() > 1 {
                ofn.lpstrFilter = filter.as_ptr();
            }
            if let Some(t) = &title {
                ofn.lpstrTitle = t.as_ptr();
            }
            if let Some(d) = &dir {
                ofn.lpstrInitialDir = d.as_ptr();
            }
            ofn.Flags = OFN_HIDEREADONLY
                | if opts.save {
                    OFN_OVERWRITEPROMPT
                } else {
                    OFN_FILEMUSTEXIST
                };
            let ok = if opts.save {
                GetSaveFileNameW(&mut ofn)
            } else {
                GetOpenFileNameW(&mut ofn)
            };
            if ok == 0 {
                return None;
            }
            let len = file_buf.iter().position(|&c| c == 0).unwrap_or(0);
            if len == 0 {
                return None;
            }
            Some(std::path::PathBuf::from(String::from_utf16_lossy(
                &file_buf[..len],
            )))
        }
    }

    #[repr(C)]
    #[allow(non_snake_case)]
    struct BROWSEINFOW {
        hwndOwner: winapi::shared::windef::HWND,
        pidlRoot: *const winapi::ctypes::c_void,
        pszDisplayName: *mut u16,
        lpszTitle: *const u16,
        ulFlags: u32,
        lpfn: *const winapi::ctypes::c_void,
        lParam: isize,
        iImage: i32,
    }

    const BIF_RETURNONLYFSDIRS: u32 = 0x0001;
    const BIF_NEWDIALOGSTYLE: u32 = 0x0040;

    #[link(name = "shell32")]
    extern "system" {
        fn SHBrowseForFolderW(lpbi: *mut BROWSEINFOW) -> *mut winapi::ctypes::c_void;
    }

    pub fn local_utc_offset_minutes() -> i64 {
        use winapi::um::timezoneapi::GetTimeZoneInformation;
        unsafe {
            let mut tzi = std::mem::zeroed();
            let extra = match GetTimeZoneInformation(&mut tzi) {
                2 => tzi.DaylightBias,
                1 => tzi.StandardBias,
                _ => 0,
            };
            -((tzi.Bias + extra) as i64)
        }
    }

    pub fn message_box(title: &str, text: &str, kind: &str) -> bool {
        use winapi::um::winuser::{
            GetForegroundWindow, MessageBoxW, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION,
            MB_ICONWARNING, MB_OK, MB_YESNO, IDOK,
        };
        unsafe {
            let (flags, ok_ids): (u32, &[i32]) = match kind {
                "question" => (MB_YESNO | MB_ICONQUESTION, &[IDYES]),
                "warning" => (MB_OK | MB_ICONWARNING, &[IDOK]),
                _ => (MB_OK | MB_ICONINFORMATION, &[IDOK]),
            };
            let r = MessageBoxW(
                GetForegroundWindow(),
                to_wide(text).as_ptr(),
                to_wide(title).as_ptr(),
                flags,
            );
            ok_ids.contains(&r)
        }
    }

    pub fn pick_folder(title: Option<&str>, _dir: Option<&str>) -> Option<std::path::PathBuf> {
        use winapi::um::shlobj::SHGetPathFromIDListW;
        unsafe {
            let _ = winapi::um::objbase::CoInitialize(null_mut());
            let title_w = title.map(to_wide);
            let mut bi: BROWSEINFOW = std::mem::zeroed();
            if let Some(t) = &title_w {
                bi.lpszTitle = t.as_ptr();
            }
            bi.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
            let pidl = SHBrowseForFolderW(&mut bi);
            if pidl.is_null() {
                return None;
            }
            let mut path = [0u16; 1024];
            let ok = SHGetPathFromIDListW(pidl as _, path.as_mut_ptr());
            winapi::um::combaseapi::CoTaskMemFree(pidl as _);
            if ok == 0 {
                return None;
            }
            let len = path.iter().position(|&c| c == 0).unwrap_or(0);
            if len == 0 {
                return None;
            }
            Some(std::path::PathBuf::from(String::from_utf16_lossy(
                &path[..len],
            )))
        }
    }
}

pub use imp::{clipboard_get_text, clipboard_set_text, local_utc_offset_minutes, message_box, pick_file, pick_folder};
