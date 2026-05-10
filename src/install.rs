
use std::ffi::OsStr;
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use winapi::ctypes::c_void as winapi_c_void;
use winapi::shared::minwindef::{DWORD, FALSE};
use winapi::shared::winerror::NO_ERROR;
use winapi::um::winnt::{LPCWSTR, LPWSTR, SERVICE_WIN32_OWN_PROCESS};
use winapi::um::winsvc::{
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW,
    SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_INTERROGATE,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    SERVICE_TABLE_ENTRYW,
};

pub const APP_NAME: &str = "HopToDesk";
pub const SERVICE_NAME: &str = "HopToDesk";
pub const SERVICE_DISPLAY_NAME: &str = "HopToDesk Service";
pub const EXE_NAME: &str = "HopToDesk.exe";
const UNINSTALL_REG_KEY: &str =
    "HKLM\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\HopToDesk";

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(once(0)).collect()
}

pub fn default_install_path() -> PathBuf {
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    Path::new(&pf).join(APP_NAME)
}

pub fn install_exe_path() -> PathBuf {
    default_install_path().join(EXE_NAME)
}

pub fn is_installed() -> bool {
    let path = install_exe_path();
    let result = path.exists();
    crate::config::write_log(&format!(
        "[install] is_installed check: path={} exists={}",
        path.display(),
        result
    ));
    result
}

const CREATE_NO_WINDOW: u32 = 0x08000000;

fn silent_cmd(program: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

fn run_cmd(label: &str, program: &str, args: &[&str]) -> Result<(), String> {
    crate::config::write_log(&format!("[install] {} -> {} {:?}", label, program, args));
    let output = silent_cmd(program)
        .args(args)
        .output()
        .map_err(|e| format!("{} spawn failed: {}", label, e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let msg = format!(
            "{} failed (exit {}): stderr={} stdout={}",
            label,
            output.status,
            stderr.trim(),
            stdout.trim()
        );
        crate::config::write_log(&format!("[install] {}", msg));
        return Err(msg);
    }
    Ok(())
}

fn write_uninstall_registry(install_path: &Path, uninstaller_exe: &Path) -> Result<(), String> {
    let install_path_str = install_path.to_string_lossy().replace('/', "\\");
    let uninstaller_str = uninstaller_exe.to_string_lossy().replace('/', "\\");
    let uninstall_cmd = format!("\"{}\" --uninstall", uninstaller_str);
    let display_icon = format!(
        "\"{}\\{}\"",
        install_path_str.trim_end_matches('\\'),
        EXE_NAME
    );
    let reg_args: &[(&str, &str, &str)] = &[
        ("DisplayName", "REG_SZ", APP_NAME),
        ("DisplayVersion", "REG_SZ", env!("CARGO_PKG_VERSION")),
        ("Publisher", "REG_SZ", "Begonia Holdings"),
        ("InstallLocation", "REG_SZ", &install_path_str),
        ("DisplayIcon", "REG_SZ", &display_icon),
        ("UninstallString", "REG_SZ", &uninstall_cmd),
        ("NoModify", "REG_DWORD", "1"),
        ("NoRepair", "REG_DWORD", "1"),
    ];
    for (name, ty, value) in reg_args {
        run_cmd(
            &format!("reg-write {}", name),
            "reg",
            &["add", UNINSTALL_REG_KEY, "/v", name, "/t", ty, "/d", value, "/f"],
        )?;
    }
    Ok(())
}

fn delete_uninstall_registry() -> Result<(), String> {
    let _ = silent_cmd("reg")
        .args(["delete", UNINSTALL_REG_KEY, "/f"])
        .output();
    Ok(())
}

fn create_shortcut(
    target: &Path,
    shortcut_path: &Path,
    args: Option<&str>,
) -> Result<(), String> {
    let target_str = target.to_string_lossy().replace('/', "\\");
    let shortcut_str = shortcut_path.to_string_lossy().replace('/', "\\");
    let temp = std::env::temp_dir().join(format!(
        "hoptodesk-mkshortcut-{}.vbs",
        std::process::id()
    ));
    let args_line = match args {
        Some(a) if !a.is_empty() => format!(
            "sc.Arguments = \"{}\"\n",
            a.replace('"', "\"\"")
        ),
        _ => String::new(),
    };
    let vbs = format!(
        r#"Set ws = CreateObject("WScript.Shell")
Set sc = ws.CreateShortcut("{shortcut}")
sc.TargetPath = "{target}"
sc.WorkingDirectory = "{wd}"
{args_line}sc.Save
"#,
        shortcut = shortcut_str.replace('"', "\"\""),
        target = target_str.replace('"', "\"\""),
        wd = target
            .parent()
            .map(|p| p.to_string_lossy().replace('/', "\\"))
            .unwrap_or_default()
            .replace('"', "\"\""),
        args_line = args_line,
    );
    std::fs::write(&temp, vbs).map_err(|e| format!("write shortcut vbs: {}", e))?;
    let res = run_cmd(
        "create-shortcut",
        "wscript",
        &["//Nologo", &temp.to_string_lossy()],
    );
    let _ = std::fs::remove_file(&temp);
    res
}

fn all_users_programs_dir() -> PathBuf {
    let xp = Path::new("C:\\Documents and Settings\\All Users\\Start Menu\\Programs");
    if xp.exists() {
        return xp.to_path_buf();
    }
    let programs = std::env::var("ProgramData")
        .unwrap_or_else(|_| "C:\\ProgramData".into());
    Path::new(&programs)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
}

fn create_start_menu_shortcuts(install_path: &Path, uninstaller_exe: &Path) -> Result<(), String> {
    let programs = all_users_programs_dir();
    let app_dir = programs.join(APP_NAME);
    std::fs::create_dir_all(&app_dir)
        .map_err(|e| format!("create start menu subfolder: {}", e))?;

    let exe_target = install_path.join(EXE_NAME);
    let app_shortcut = app_dir.join(format!("{}.lnk", APP_NAME));
    create_shortcut(&exe_target, &app_shortcut, None)?;

    let uninst_shortcut = app_dir.join(format!("Uninstall {}.lnk", APP_NAME));
    create_shortcut(uninstaller_exe, &uninst_shortcut, Some("--uninstall"))?;

    Ok(())
}

fn create_desktop_shortcut(install_path: &Path) -> Result<(), String> {
    let public = std::env::var("PUBLIC")
        .unwrap_or_else(|_| "C:\\Users\\Public".into());
    let mut desktop = Path::new(&public).join("Desktop");
    if !desktop.exists() {
        let xp = Path::new("C:\\Documents and Settings\\All Users\\Desktop");
        if xp.exists() {
            desktop = xp.to_path_buf();
        }
    }
    let _ = std::fs::create_dir_all(&desktop);
    let target = install_path.join(EXE_NAME);
    let shortcut = desktop.join(format!("{}.lnk", APP_NAME));
    create_shortcut(&target, &shortcut, None)
}

fn delete_shortcuts() {
    let programs = all_users_programs_dir();
    let app_dir = programs.join(APP_NAME);
    if app_dir.exists() {
        let _ = std::fs::remove_dir_all(&app_dir);
    }

    let legacy_candidates: Vec<PathBuf> = [
        std::env::var("ProgramData").ok().map(|p| {
            Path::new(&p)
                .join("Microsoft\\Windows\\Start Menu\\Programs")
                .join(format!("{}.lnk", APP_NAME))
        }),
        Some(
            Path::new("C:\\Documents and Settings\\All Users\\Start Menu\\Programs")
                .join(format!("{}.lnk", APP_NAME)),
        ),
        std::env::var("PUBLIC").ok().map(|p| {
            Path::new(&p)
                .join("Desktop")
                .join(format!("{}.lnk", APP_NAME))
        }),
        Some(
            Path::new("C:\\Documents and Settings\\All Users\\Desktop")
                .join(format!("{}.lnk", APP_NAME)),
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    for p in legacy_candidates {
        if p.exists() {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn register_service(install_path: &Path) -> Result<(), String> {
    let exe = install_path.join(EXE_NAME);
    let exe_str = exe.to_string_lossy().replace('/', "\\");
    let bin_path = format!("\"{}\" --service", exe_str);

    let _ = silent_cmd("sc").args(["stop", SERVICE_NAME]).output();
    let _ = silent_cmd("sc").args(["delete", SERVICE_NAME]).output();

    run_cmd(
        "sc-create",
        "sc",
        &[
            "create",
            SERVICE_NAME,
            "binPath=",
            &bin_path,
            "DisplayName=",
            SERVICE_DISPLAY_NAME,
            "start=",
            "auto",
            "type=",
            "own",
            "obj=",
            "LocalSystem",
        ],
    )?;
    Ok(())
}

fn unregister_service() -> Result<(), String> {
    let _ = silent_cmd("sc").args(["stop", SERVICE_NAME]).output();
    for _ in 0..30 {
        let q = silent_cmd("sc").args(["query", SERVICE_NAME]).output();
        let stopped = match q {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains("STOPPED") || !stdout.contains("STATE")
            }
            Err(_) => true,
        };
        if stopped {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let pid = std::process::id();
    let _ = silent_cmd("taskkill")
        .args([
            "/F",
            "/FI",
            &format!("IMAGENAME eq {}", EXE_NAME),
            "/FI",
            &format!("PID ne {}", pid),
        ])
        .output();
    let _ = silent_cmd("sc").args(["delete", SERVICE_NAME]).output();
    Ok(())
}

fn start_service() -> Result<(), String> {
    run_cmd("sc-start", "sc", &["start", SERVICE_NAME])
}

const RUN_KEY: &str = "HKLM\\Software\\Microsoft\\Windows\\CurrentVersion\\Run";

fn register_run_key(install_path: &Path) {
    let exe = install_path.join(EXE_NAME);
    let exe_str = exe.to_string_lossy().replace('/', "\\");
    let value = format!("\"{}\" --tray", exe_str);
    let _ = run_cmd(
        "reg-add-run",
        "reg",
        &[
            "add", RUN_KEY, "/f", "/v", APP_NAME, "/t", "REG_SZ", "/d", &value,
        ],
    );
}

fn delete_run_key() {
    let _ = silent_cmd("reg")
        .args(["delete", RUN_KEY, "/f", "/v", APP_NAME])
        .output();
}

fn spawn_tray_for_current_user(install_path: &Path) {
    use std::process::Stdio;
    let exe = install_path.join(EXE_NAME);
    crate::config::write_log(&format!(
        "[install] spawning tray for current user: {} --tray",
        exe.display()
    ));
    let _ = silent_cmd(exe.to_string_lossy().as_ref())
        .arg("--tray")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn add_firewall_rule(install_path: &Path) {
    let exe = install_path.join(EXE_NAME);
    let exe_str = exe.to_string_lossy().replace('/', "\\");
    let _ = silent_cmd("netsh")
        .args([
            "advfirewall",
            "firewall",
            "add",
            "rule",
            "name=HopToDesk Service",
            "dir=in",
            "action=allow",
            &format!("program={}", exe_str),
            "enable=yes",
        ])
        .output();
    let _ = silent_cmd("netsh")
        .args([
            "firewall",
            "add",
            "allowedprogram",
            &format!("program={}", exe_str),
            "name=HopToDesk Service",
            "mode=ENABLE",
        ])
        .output();
}

fn remove_firewall_rule() {
    let _ = silent_cmd("netsh")
        .args([
            "advfirewall",
            "firewall",
            "delete",
            "rule",
            "name=HopToDesk Service",
        ])
        .output();
    let _ = silent_cmd("netsh")
        .args(["firewall", "delete", "allowedprogram", "name=HopToDesk Service"])
        .output();
}

pub fn install_me(args: &str, custom_path: &str) -> Result<(), String> {
    crate::config::write_log(&format!("[install] install_me args='{}' path='{}'", args, custom_path));

    let install_path = if custom_path.trim().is_empty() {
        default_install_path()
    } else {
        PathBuf::from(custom_path.trim())
    };
    crate::config::write_log(&format!("[install] target dir: {}", install_path.display()));

    std::fs::create_dir_all(&install_path)
        .map_err(|e| format!("create install dir: {}", e))?;

    let current_exe = std::env::current_exe()
        .map_err(|e| format!("current_exe: {}", e))?;
    let dest_exe = install_path.join(EXE_NAME);
    if current_exe.canonicalize().ok() == dest_exe.canonicalize().ok()
        && dest_exe.exists()
    {
        crate::config::write_log("[install] Source and destination are the same; skipping copy");
    } else {
        crate::config::write_log(&format!(
            "[install] copy {} -> {}",
            current_exe.display(),
            dest_exe.display()
        ));
        let _ = silent_cmd("sc").args(["stop", SERVICE_NAME]).output();
        std::fs::copy(&current_exe, &dest_exe).map_err(|e| format!("copy exe: {}", e))?;
    }

    if let Some(src_dir) = current_exe.parent() {
        let src_dll = src_dir.join("sciter.dll");
        let dest_dll = install_path.join("sciter.dll");
        if src_dll.exists() {
            crate::config::write_log(&format!(
                "[install] copy {} -> {}",
                src_dll.display(),
                dest_dll.display()
            ));
            if let Err(e) = std::fs::copy(&src_dll, &dest_dll) {
                crate::config::write_log(&format!("[install] sciter.dll copy failed: {}", e));
            }
        } else {
            crate::config::write_log(&format!(
                "[install] WARNING: sciter.dll not found at {} — installed service will fail to start",
                src_dll.display()
            ));
        }
    }

    if let Some(shared) = crate::config::shared_app_dir_pub() {
        let user_appdata = std::env::var("APPDATA").ok().map(PathBuf::from);
        if let Some(user_app_dir) = user_appdata.map(|p| p.join(APP_NAME)) {
            let src_config = user_app_dir.join("config");
            let dst_config = shared.join("config");
            if src_config.exists() && !dst_config.exists() {
                let _ = std::fs::create_dir_all(&dst_config);
                if let Ok(entries) = std::fs::read_dir(&src_config) {
                    let mut copied = 0;
                    for entry in entries.flatten() {
                        let src = entry.path();
                        if src.is_file() {
                            let dst = dst_config.join(entry.file_name());
                            if std::fs::copy(&src, &dst).is_ok() {
                                copied += 1;
                            }
                        }
                    }
                    crate::config::write_log(&format!(
                        "[install] Migrated {} config file(s) from {} to {}",
                        copied,
                        src_config.display(),
                        dst_config.display()
                    ));
                }
            } else if dst_config.exists() {
                crate::config::write_log(&format!(
                    "[install] Shared config already exists at {}; not overwriting",
                    dst_config.display()
                ));
            }
        }
    } else {
        crate::config::write_log(
            "[install] WARNING: could not resolve a shared config dir; service may use a different ID than user-mode",
        );
    }

    write_uninstall_registry(&install_path, &current_exe)?;

    let _ = args;
    if let Err(e) = create_start_menu_shortcuts(&install_path, &current_exe) {
        crate::config::write_log(&format!("[install] start menu shortcuts failed: {}", e));
    }
    if let Err(e) = create_desktop_shortcut(&install_path) {
        crate::config::write_log(&format!("[install] desktop shortcut failed: {}", e));
    }

    add_firewall_rule(&install_path);
    register_service(&install_path)?;
    if let Err(e) = start_service() {
        crate::config::write_log(&format!("[install] start service warning: {}", e));
    }

    crate::config::write_log("[install] cycling service so the workload re-attaches to WinSta0\\Default cleanly");
    let _ = silent_cmd("sc").args(["stop", SERVICE_NAME]).output();
    for _ in 0..30 {
        let q = silent_cmd("sc").args(["query", SERVICE_NAME]).output();
        let stopped = match q {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains("STOPPED")
            }
            Err(_) => true,
        };
        if stopped {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    if let Err(e) = start_service() {
        crate::config::write_log(&format!("[install] restart service warning: {}", e));
    }

    let runtime = crate::cm::cm_temp_dir();
    crate::config::write_log(&format!("[install] runtime dir: {}", runtime.display()));
    crate::cm::cleanup_runtime_dir();

    register_run_key(&install_path);
    spawn_tray_for_current_user(&install_path);

    crate::config::write_log("[install] install_me complete");
    Ok(())
}

pub fn uninstall_me() -> Result<(), String> {
    crate::config::write_log("[install] uninstall_me starting");

    let _ = std::env::set_current_dir(Path::new("C:\\"));

    unregister_service()?;
    crate::config::write_log("[install] service unregistered");
    delete_run_key();
    crate::config::write_log("[install] Run key removed");
    remove_firewall_rule();
    crate::config::write_log("[install] firewall rule removed");
    delete_shortcuts();
    crate::config::write_log("[install] shortcuts deleted");
    let _ = delete_uninstall_registry();
    crate::config::write_log("[install] uninstall registry deleted");

    let install_path = default_install_path();
    let install_str = install_path.to_string_lossy().replace('/', "\\");
    let shared_dir = crate::config::shared_app_dir_pub();
    let shared_str = shared_dir
        .as_ref()
        .map(|p| p.to_string_lossy().replace('/', "\\"))
        .unwrap_or_default();

    let mut targets: Vec<String> = Vec::new();
    if install_path.exists() {
        targets.push(install_str.clone());
    }
    if !shared_str.is_empty()
        && shared_dir.as_ref().map(|p| p.exists()).unwrap_or(false)
    {
        targets.push(shared_str.clone());
    }

    if targets.is_empty() {
        crate::config::write_log("[install] nothing to clean up");
    } else {
        let bat_path = std::env::temp_dir().join(format!(
            "hoptodesk-uninstall-{}.bat",
            std::process::id()
        ));
        let bat_path_str = bat_path.to_string_lossy().replace('/', "\\");
        let debug_log = std::env::temp_dir()
            .join("hoptodesk-uninstall-debug.log")
            .to_string_lossy()
            .replace('/', "\\");

        let mut bat = String::new();
        bat.push_str(&format!(
            "@echo off\r\n\
             set LOG=\"{log}\"\r\n\
             echo [start %date% %time%] >> %LOG%\r\n\
             cd /D C:\\\r\n\
             ping 127.0.0.1 -n 4 >nul\r\n\
             echo [taskkill] >> %LOG%\r\n\
             taskkill /F /IM HopToDesk.exe >> %LOG% 2>&1\r\n\
             ping 127.0.0.1 -n 3 >nul\r\n",
            log = debug_log
        ));
        for (idx, t) in targets.iter().enumerate() {
            bat.push_str(&format!(
                "echo [target {idx}: {t}] >> %LOG%\r\n\
                 :retry{idx}\r\n\
                 if not exist \"{t}\" goto done{idx}\r\n\
                 attrib -R -S -H /S /D \"{t}\\*\" >> %LOG% 2>&1\r\n\
                 del /F /Q /S \"{t}\\*\" >> %LOG% 2>&1\r\n\
                 rmdir /S /Q \"{t}\" >> %LOG% 2>&1\r\n\
                 ping 127.0.0.1 -n 3 >nul\r\n\
                 if exist \"{t}\" goto retry{idx}\r\n\
                 :done{idx}\r\n\
                 echo [done {idx}] >> %LOG%\r\n",
                idx = idx,
                t = t
            ));
        }
        bat.push_str(&format!(
            "echo [end %date% %time%] >> %LOG%\r\n\
             del /F /Q \"{}\" 2>nul\r\n",
            bat_path_str
        ));

        match std::fs::write(&bat_path, &bat) {
            Ok(_) => crate::config::write_log(&format!(
                "[install] wrote uninstall bat ({} bytes) to {}",
                bat.len(),
                bat_path.display()
            )),
            Err(e) => crate::config::write_log(&format!(
                "[install] failed to write uninstall bat: {}",
                e
            )),
        }

        crate::config::write_log(&format!(
            "[install] cleanup targets ({}): {}",
            targets.len(),
            targets.join(", ")
        ));

        use std::process::Stdio;
        let cd_root = Path::new("C:\\");
        match silent_cmd("cmd")
            .args(["/C", &bat_path_str])
            .current_dir(cd_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => crate::config::write_log(&format!(
                "[install] uninstall bat spawned, pid={}",
                child.id()
            )),
            Err(e) => crate::config::write_log(&format!(
                "[install] uninstall bat spawn FAILED: {}",
                e
            )),
        }
    }

    crate::config::write_log("[install] uninstall_me complete");
    Ok(())
}

static SERVICE_STOP: AtomicBool = AtomicBool::new(false);

static mut SERVICE_STATUS_HANDLE_RAW: SERVICE_STATUS_HANDLE = std::ptr::null_mut();

unsafe extern "system" fn service_ctrl_handler(
    control: DWORD,
    _event_type: DWORD,
    _event_data: *mut winapi_c_void,
    _context: *mut winapi_c_void,
) -> DWORD {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            SERVICE_STOP.store(true, Ordering::SeqCst);
            set_service_state(SERVICE_STOP_PENDING, 0);
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => NO_ERROR,
    }
}

fn set_service_state(state: DWORD, accepts: DWORD) {
    unsafe {
        if SERVICE_STATUS_HANDLE_RAW.is_null() {
            return;
        }
        let mut status = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: state,
            dwControlsAccepted: accepts,
            dwWin32ExitCode: 0,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: 0,
            dwWaitHint: 0,
        };
        SetServiceStatus(SERVICE_STATUS_HANDLE_RAW, &mut status);
    }
}

unsafe extern "system" fn service_main(_argc: DWORD, _argv: *mut LPWSTR) {
    let name_w = to_wide(SERVICE_NAME);
    let handle = RegisterServiceCtrlHandlerExW(
        name_w.as_ptr() as LPCWSTR,
        Some(service_ctrl_handler),
        std::ptr::null_mut(),
    );
    if handle.is_null() {
        crate::config::write_log("[service] RegisterServiceCtrlHandlerExW failed");
        return;
    }
    SERVICE_STATUS_HANDLE_RAW = handle;
    set_service_state(SERVICE_START_PENDING, 0);

    crate::config::write_log("[service] entering service main");
    set_service_state(
        SERVICE_RUNNING,
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
    );

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_for_worker = stop_flag.clone();
    std::thread::spawn(move || {
        run_service_workload(stop_for_worker);
    });

    while !SERVICE_STOP.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    stop_flag.store(true, Ordering::SeqCst);

    crate::config::write_log("[service] stop requested, exiting service main");
    set_service_state(SERVICE_STOPPED, 0);
}

unsafe fn attach_to_interactive_desktop() {
    use winapi::um::winuser::{
        CloseDesktop, CloseWindowStation, OpenDesktopW, OpenWindowStationW,
        SetProcessWindowStation, SetThreadDesktop,
    };

    const MAXIMUM_ALLOWED: DWORD = 0x02000000;

    let winsta_name: Vec<u16> = "WinSta0\0".encode_utf16().collect();
    let desktop_name: Vec<u16> = "Default\0".encode_utf16().collect();

    let hwinsta = OpenWindowStationW(winsta_name.as_ptr(), 0, MAXIMUM_ALLOWED);
    if hwinsta.is_null() {
        let err = winapi::um::errhandlingapi::GetLastError();
        crate::config::write_log(&format!(
            "[service] OpenWindowStationW(WinSta0) failed, GetLastError={}",
            err
        ));
        return;
    }
    if SetProcessWindowStation(hwinsta) == 0 {
        let err = winapi::um::errhandlingapi::GetLastError();
        crate::config::write_log(&format!(
            "[service] SetProcessWindowStation failed, GetLastError={}",
            err
        ));
        let _ = CloseWindowStation(hwinsta);
        return;
    }
    crate::config::write_log("[service] attached to WinSta0");

    let hdesk = OpenDesktopW(desktop_name.as_ptr(), 0, 0, MAXIMUM_ALLOWED);
    if hdesk.is_null() {
        let err = winapi::um::errhandlingapi::GetLastError();
        crate::config::write_log(&format!(
            "[service] OpenDesktopW(Default) failed, GetLastError={}",
            err
        ));
        return;
    }
    if SetThreadDesktop(hdesk) == 0 {
        let err = winapi::um::errhandlingapi::GetLastError();
        crate::config::write_log(&format!(
            "[service] SetThreadDesktop failed, GetLastError={}",
            err
        ));
        let _ = CloseDesktop(hdesk);
        return;
    }
    crate::config::write_log("[service] attached to WinSta0\\Default desktop");
}

fn run_service_workload(_stop: Arc<AtomicBool>) {
    crate::config::write_log("[service] workload starting (signal + direct server)");
    unsafe { attach_to_interactive_desktop(); }
    crate::config::migrate_old_config();
    crate::cm::cleanup_runtime_dir();
    let cfg = crate::config::Config::load();
    let my_id = cfg.id.clone();
    let password = cfg.password.clone();
    let pk = cfg.key_pair.1.clone();

    {
        let my_id = my_id.clone();
        let password = password.clone();
        let pk = pk.clone();
        std::thread::spawn(move || {
            crate::server::run_direct_server(my_id, password, pk);
        });
    }

    let signal_state = Arc::new(std::sync::Mutex::new(crate::signal::SignalState::default()));
    crate::signal::run_signal_loop(my_id, password, pk, signal_state);
}

pub fn run_as_service() {
    let name_w = to_wide(SERVICE_NAME);
    let mut entries: [SERVICE_TABLE_ENTRYW; 2] = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name_w.as_ptr() as LPWSTR,
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    crate::config::write_log("[service] calling StartServiceCtrlDispatcherW");
    unsafe {
        if StartServiceCtrlDispatcherW(entries.as_mut_ptr()) == FALSE {
            let err = winapi::um::errhandlingapi::GetLastError();
            crate::config::write_log(&format!(
                "[service] StartServiceCtrlDispatcherW failed: error {}",
                err
            ));
        }
    }
}
