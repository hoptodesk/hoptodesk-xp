
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

const SCITER_DLL: &[u8] = include_bytes!(env!("EMBED_SCITER_DLL"));
const INNER_EXE: &[u8] = include_bytes!(env!("EMBED_INNER_EXE"));

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    lp_reserved: *mut u16,
    lp_desktop: *mut u16,
    lp_title: *mut u16,
    dw_x: u32,
    dw_y: u32,
    dw_x_size: u32,
    dw_y_size: u32,
    dw_x_count_chars: u32,
    dw_y_count_chars: u32,
    dw_fill_attribute: u32,
    dw_flags: u32,
    w_show_window: u16,
    cb_reserved2: u16,
    lp_reserved2: *mut u8,
    h_std_input: *mut c_void,
    h_std_output: *mut c_void,
    h_std_error: *mut c_void,
}

#[repr(C)]
struct ProcessInformation {
    h_process: *mut c_void,
    h_thread: *mut c_void,
    dw_process_id: u32,
    dw_thread_id: u32,
}

#[repr(C)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; 260],
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateProcessW(
        lp_application_name: *const u16,
        lp_command_line: *mut u16,
        lp_process_attributes: *mut c_void,
        lp_thread_attributes: *mut c_void,
        b_inherit_handles: i32,
        dw_creation_flags: u32,
        lp_environment: *mut c_void,
        lp_current_directory: *const u16,
        lp_startup_info: *mut StartupInfoW,
        lp_process_information: *mut ProcessInformation,
    ) -> i32;
    fn CloseHandle(h: *mut c_void) -> i32;
    fn OpenProcess(desired_access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn TerminateProcess(process: *mut c_void, exit_code: u32) -> i32;
    fn WaitForSingleObject(h: *mut c_void, ms: u32) -> u32;
    fn GetCurrentProcessId() -> u32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
    fn Process32FirstW(snap: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snap: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
}

const TH32CS_SNAPPROCESS: u32 = 0x00000002;
const PROCESS_TERMINATE: u32 = 0x0001;
const SYNCHRONIZE: u32 = 0x00100000;
const INVALID_HANDLE: *mut c_void = -1isize as *mut c_void;

fn path_wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn wide_cstr(v: &[u16]) -> String {
    let end = v.iter().position(|&c| c == 0).unwrap_or(v.len());
    String::from_utf16_lossy(&v[..end])
}

fn kill_matching_processes(names: &[&str]) {
    let self_pid = unsafe { GetCurrentProcessId() };
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap.is_null() || snap == INVALID_HANDLE {
        return;
    }
    let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
    entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
    if unsafe { Process32FirstW(snap, &mut entry) } == 0 {
        unsafe { CloseHandle(snap) };
        return;
    }
    loop {
        let name = wide_cstr(&entry.sz_exe_file);
        let name_lower = name.to_lowercase();
        if entry.th32_process_id != self_pid
            && names
                .iter()
                .any(|n| name_lower == n.to_lowercase())
        {
            let h = unsafe {
                OpenProcess(PROCESS_TERMINATE | SYNCHRONIZE, 0, entry.th32_process_id)
            };
            if !h.is_null() {
                unsafe {
                    TerminateProcess(h, 0);
                    WaitForSingleObject(h, 3000);
                    CloseHandle(h);
                }
            }
        }
        if unsafe { Process32NextW(snap, &mut entry) } == 0 {
            break;
        }
    }
    unsafe { CloseHandle(snap) };
}

fn main() {

    let appdata = match std::env::var("APPDATA") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => std::process::exit(1),
    };
    let dir = appdata.join("HopToDesk");
    let _ = std::fs::create_dir_all(&dir);
    let exe_path = dir.join("HopToDesk.exe");
    let dll_path = dir.join("sciter.dll");

    kill_matching_processes(&["HopToDesk.exe", "hoptodesk.exe", "Uninstall.exe"]);

    let _ = write_if_different(&dll_path, SCITER_DLL);
    let _ = write_if_different(&exe_path, INNER_EXE);

    let mut cmdline = String::new();
    cmdline.push('"');
    cmdline.push_str(&exe_path.to_string_lossy());
    cmdline.push('"');
    for a in std::env::args().skip(1) {
        cmdline.push(' ');
        cmdline.push('"');
        cmdline.push_str(&a);
        cmdline.push('"');
    }
    let mut wide_cmd: Vec<u16> = cmdline.encode_utf16().chain(Some(0)).collect();
    let wide_exe = path_wide(&exe_path);
    let wide_dir = path_wide(&dir);

    let mut si: StartupInfoW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<StartupInfoW>() as u32;
    let mut pi: ProcessInformation = unsafe { std::mem::zeroed() };

    let ok = unsafe {
        CreateProcessW(
            wide_exe.as_ptr(),
            wide_cmd.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            0,
            ptr::null_mut(),
            wide_dir.as_ptr(),
            &mut si,
            &mut pi,
        )
    };
    if ok == 0 {
        std::process::exit(1);
    }
    unsafe {
        CloseHandle(pi.h_process);
        CloseHandle(pi.h_thread);
    }
}

fn write_if_different(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let same = std::fs::metadata(path)
        .map(|m| m.len() as usize == bytes.len())
        .unwrap_or(false);
    if !same {
        std::fs::write(path, bytes)?;
    }
    Ok(())
}
