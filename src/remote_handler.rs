
use crate::client::ClientState;
use crate::file_transfer;
use crate::protocol::message_proto;
use protobuf::Message;
use sciter::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub struct RemoteHandler {
    pub is_file_transfer: bool,
    pub client_state: Arc<Mutex<ClientState>>,
    jobs: HashMap<i32, file_transfer::TransferJob>,
    next_job_id: i32,
    peer_platform: String,
}

impl RemoteHandler {
    pub fn new(
        is_file_transfer: bool,
        client_state: Arc<Mutex<ClientState>>,
    ) -> Self {
        let peer_platform = client_state.lock()
            .map(|s| s.peer_platform.clone())
            .unwrap_or_default();
        Self {
            is_file_transfer,
            client_state,
            jobs: HashMap::new(),
            next_job_id: 1,
            peer_platform,
        }
    }

    fn send_msg(&self, msg: &message_proto::Message) {
        let bytes = match msg.write_to_bytes() {
            Ok(b) => b,
            Err(_) => return,
        };
        let stream = match self.client_state.lock() {
            Ok(s) => match &s.input_stream {
                Some(st) => st.clone(),
                None => return,
            },
            Err(_) => return,
        };
        let mut guard = match stream.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let _ = guard.send_msg(&bytes);
    }

    fn send_file_action(&self, fa: message_proto::FileAction) {
        let mut msg = message_proto::Message::new();
        msg.set_file_action(fa);
        self.send_msg(&msg);
    }

    fn is_file_transfer(&self) -> bool {
        self.is_file_transfer
    }

    fn is_port_forward(&self) -> bool { false }
    fn is_view_camera(&self) -> bool { false }

    fn t(&self, key: String) -> String {
        crate::lang::translate(key)
    }

    fn read_remote_dir(&mut self, path: String, include_hidden: bool) {
        let mut rd = message_proto::ReadDir::new();
        rd.path = path;
        rd.include_hidden = include_hidden;
        let mut fa = message_proto::FileAction::new();
        fa.set_read_dir(rd);
        self.send_file_action(fa);
    }

    fn send_files(&mut self, id: i32, _file_type: i32, path: String, to: String, file_num: i32, include_hidden: bool, is_remote: bool) {
        let mut job = file_transfer::TransferJob::new(id, path.clone(), to.clone(), file_num, include_hidden, is_remote);

        if is_remote {

            let mut send_req = message_proto::FileTransferSendRequest::new();
            send_req.id = id;
            send_req.path = path;
            send_req.file_num = file_num;
            send_req.include_hidden = include_hidden;
            let mut fa = message_proto::FileAction::new();
            fa.set_send(send_req);
            self.send_file_action(fa);
        } else {

            let files = file_transfer::get_recursive_files(&path, include_hidden)
                .unwrap_or_default();
            let mut file_entries: Vec<_> = files.into_iter()
                .filter(|f| f.entry_type.enum_value_or_default() == message_proto::FileType::File)
                .map(|mut f| { f.name = f.name.replace('\\', "/"); f })
                .collect();

            if file_entries.len() == 1 && !file_entries[0].name.is_empty() && std::path::Path::new(&path).is_file() {
                file_entries[0].name = String::new();
            }
            job.files = file_entries.clone();
            let total_size: u64 = file_entries.iter().map(|f| f.size).sum();

            let mut recv = message_proto::FileTransferReceiveRequest::new();
            recv.id = id;
            recv.path = to;
            recv.files = file_entries.into();
            recv.file_num = file_num;
            recv.total_size = total_size;
            let mut fa = message_proto::FileAction::new();
            fa.set_receive(recv);
            self.send_file_action(fa);
        }

        self.jobs.insert(id, job);
    }

    fn add_job(&mut self, id: i32, _file_type: i32, path: String, to: String, file_num: i32, include_hidden: bool, is_remote: bool) {
        let job = file_transfer::TransferJob::new(id, path, to, file_num, include_hidden, is_remote);
        self.jobs.insert(id, job);
    }

    fn resume_job(&mut self, id: i32, is_remote: bool) {
        if let Some(job) = self.jobs.get(&id) {
            let path = job.path.clone();
            let to = job.to.clone();
            let file_num = job.file_num;
            let include_hidden = job.include_hidden;
            self.send_files(id, 0, path, to, file_num, include_hidden, is_remote);
        }
    }

    fn cancel_job(&mut self, id: i32) {
        if let Some(job) = self.jobs.get_mut(&id) {
            job.cancelled = true;
        }
        let mut cancel = message_proto::FileTransferCancel::new();
        cancel.id = id;
        let mut fa = message_proto::FileAction::new();
        fa.set_cancel(cancel);
        self.send_file_action(fa);
    }

    fn remove_file(&self, id: i32, path: String, file_num: i32, is_remote: bool) {
        if is_remote {
            let mut rf = message_proto::FileRemoveFile::new();
            rf.id = id;
            rf.path = path;
            rf.file_num = file_num;
            let mut fa = message_proto::FileAction::new();
            fa.set_remove_file(rf);
            self.send_file_action(fa);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }

    fn remove_dir(&self, id: i32, path: String, is_remote: bool) {
        if is_remote {
            let mut rd = message_proto::FileRemoveDir::new();
            rd.id = id;
            rd.path = path;
            rd.recursive = false;
            let mut fa = message_proto::FileAction::new();
            fa.set_remove_dir(rd);
            self.send_file_action(fa);
        } else {
            let _ = std::fs::remove_dir(&path);
        }
    }

    fn remove_dir_all(&self, id: i32, path: String, is_remote: bool, _include_hidden: bool) {
        if is_remote {
            let mut rd = message_proto::FileRemoveDir::new();
            rd.id = id;
            rd.path = path;
            rd.recursive = true;
            let mut fa = message_proto::FileAction::new();
            fa.set_remove_dir(rd);
            self.send_file_action(fa);
        } else {
            let _ = std::fs::remove_dir_all(&path);
        }
    }

    fn create_dir(&self, id: i32, path: String, is_remote: bool) {
        if is_remote {
            let mut cd = message_proto::FileDirCreate::new();
            cd.id = id;
            cd.path = path;
            let mut fa = message_proto::FileAction::new();
            fa.set_create(cd);
            self.send_file_action(fa);
        } else {
            let _ = std::fs::create_dir_all(&path);
        }
    }

    fn confirm_delete_files(&mut self, id: i32, file_num: i32) {

        if let Some(job) = self.jobs.get(&id) {
            let path = job.path.clone();
            let is_remote = job.is_remote;
            self.remove_file(id, path, file_num, is_remote);
        }
    }

    fn set_write_override(&mut self, id: i32, file_num: i32, need_override: bool, remember: bool, is_upload: bool) {
        crate::config::write_log(&format!(
            "[remote_handler] set_write_override: id={} file_num={} need_override={} remember={} is_upload={}",
            id, file_num, need_override, remember, is_upload
        ));
        let mut confirm = message_proto::FileTransferSendConfirmRequest::new();
        confirm.id = id;
        confirm.file_num = file_num;
        if need_override {
            confirm.set_offset_blk(0);
        } else {
            confirm.set_skip(true);
        }
        let mut fa = message_proto::FileAction::new();
        fa.set_send_confirm(confirm);
        self.send_file_action(fa);
    }

    fn set_no_confirm(&mut self, id: i32) {
        if let Some(job) = self.jobs.get_mut(&id) {
            job.no_confirm = true;
        }
    }

    fn rename_file(&self, _id: i32, path: String, new_name: String, is_remote: bool) {
        if is_remote {
            let mut rename = message_proto::FileRename::new();
            rename.path = path;
            rename.new_name = new_name;
            let mut fa = message_proto::FileAction::new();
            fa.set_rename(rename);
            self.send_file_action(fa);
        } else {
            let src = std::path::Path::new(&path);
            if let Some(parent) = src.parent() {
                let dest = parent.join(&new_name);
                let _ = std::fs::rename(src, &dest);
            }
        }
    }

    fn read_dir(&self, path: String, include_hidden: bool) -> Value {
        let p = if path.is_empty() { "" } else { &path };
        let fd = if p.is_empty() {
            file_transfer::get_drives()
        } else {
            file_transfer::read_dir_to_proto(p, include_hidden)
                .unwrap_or_else(|_| {
                    let mut fd = message_proto::FileDirectory::new();
                    fd.path = p.to_string();
                    fd
                })
        };
        file_transfer::file_directory_to_value(&fd)
    }

    fn get_home_dir(&self) -> String {
        file_transfer::get_home_dir()
    }

    fn get_path_sep(&self, is_remote: bool) -> String {
        file_transfer::get_path_sep(is_remote, &self.peer_platform)
    }

    fn get_next_job_id(&mut self) -> i32 {
        let id = self.next_job_id;
        self.next_job_id += 1;
        id
    }

    fn update_next_job_id(&mut self, id: i32) {
        if id > self.next_job_id {
            self.next_job_id = id;
        }
    }

    fn get_option(&self, _key: String) -> String {
        String::new()
    }

    fn set_option(&self, _key: String, _value: String) {}

    fn save_close_state(&self, _key: String, _value: String) {}

    fn save_size(&self, _x: i32, _y: i32, _w: i32, _h: i32) {}

    fn get_size(&self) -> Value { Value::null() }

    fn get_platform(&self, is_remote: bool) -> String {
        if is_remote {
            self.peer_platform.clone()
        } else {
            "Windows".to_string()
        }
    }

    fn get_icon_path(&self, _file_type: i32, _ext: String) -> String {
        String::new()
    }

    fn msgbox(&self, _tp: String, _title: String, _text: String) {

        crate::config::write_log(&format!("[remote] msgbox: {} - {}", _title, _text));
    }

    fn get_toggle_option(&self, _opt: String) -> bool { false }

    fn toggle_option(&self, _opt: String) {}

    fn get_view_style(&self) -> String { String::new() }

    fn get_keyboard_mode(&self) -> String { "legacy".to_string() }

    fn peer_platform(&self) -> String { self.peer_platform.clone() }

    fn get_default_printer(&self) -> String { String::new() }

    fn on_printer_selected(&self, _id: i32, _path: String, _name: String) {}

    fn is_screenshot_supported(&self) -> bool { false }

    fn take_screenshot(&self, _display: i32, _path: String) {}

    fn record_screen(&self, on: bool, _display: i32, _w: i32, _h: i32) {
        if let Ok(mut state) = self.client_state.lock() {
            if on && !state.recording {
                let w = state.frame_width as u32;
                let h = state.frame_height as u32;
                if w > 0 && h > 0 {
                    match crate::recording::Recorder::new(w, h, "remote") {
                        Ok(rec) => {
                            state.recorder = Some(rec);
                            state.recording = true;
                            crate::config::write_log("[recording] Started");
                        }
                        Err(e) => {
                            crate::config::write_log(&format!("[recording] Failed to start: {}", e));
                        }
                    }
                }
            } else if !on && state.recording {
                state.recording = false;
                state.recorder = None;
                crate::config::write_log("[recording] Stopped");
            }
        }
    }

    fn is_wayland_no_grab(&self) -> bool { false }

    fn send_mouse(&self, mask: i32, x: i32, y: i32, alt: bool, ctrl: bool, shift: bool, _command: bool) {
        crate::client::send_mouse_event(&self.client_state, mask, x, y, alt, ctrl, shift);
    }

    fn send_key_to_remote(&self, down_or_up: String, key_name: String, key_code: i32) {

        let down = down_or_up == "down";
        crate::client::send_key_event(&self.client_state, down, key_code as u32, false, false, false);
    }

    fn enter(&self, _keyboard_mode: String) {}
    fn leave(&self, _keyboard_mode: String) {}

    fn transfer_file(&self) {
        crate::config::write_log(&format!("[remote] transfer_file called — not supported during video session on XP"));
    }

    fn get_id(&self) -> String { String::new() }
    fn get_default_pi(&self) -> Value { Value::null() }
    fn input_os_password(&self, _pass: String, _remember: bool) {}
    fn save_view_style(&self, _style: String) {}
    fn save_keyboard_mode(&self, _mode: String) {}
    fn save_image_quality(&self, _quality: String) {}
    fn save_custom_image_quality(&self, _quality: i32) {}
    fn get_custom_image_quality(&self) -> String { String::new() }
    fn get_image_quality(&self) -> String { "balanced".to_string() }
    fn get_raw_keyboard_mode(&self) -> String { String::new() }
    fn switch_display(&self, _display: i32) {}
    fn refresh_video(&self, _display: i32) {}
    fn ctrl_alt_del(&self) {
        crate::client::send_ctrl_alt_del(&self.client_state);
    }
    fn lock_screen(&self) {}
    fn restart_remote_device(&self) {
        let mut misc = message_proto::Misc::new();
        misc.set_restart_remote_device(true);
        let mut msg = message_proto::Message::new();
        msg.set_misc(misc);
        self.send_msg(&msg);
    }
    fn send_note(&self, _msg: String) {}
    fn tunnel(&self) {}
    fn send_chat(&self, msg: String) {
        let mut misc = message_proto::Misc::new();
        misc.set_chat_message(message_proto::ChatMessage {
            text: msg,
            ..Default::default()
        });
        let mut msg_out = message_proto::Message::new();
        msg_out.set_misc(misc);
        self.send_msg(&msg_out);
    }
    fn get_chatbox(&self) -> Value { Value::null() }
    fn login(&self, _os_user: String, _os_pass: String, _pass: String, _remember: bool) {}
    fn get_icon(&self) -> String { String::new() }
    fn alternative_codecs(&self) -> Value { Value::array(0) }
    fn change_prefer_codec(&self) {}
    fn is_keyboard_mode_supported(&self, _mode: String) -> bool { false }
    fn is_remote_printing_supported(&self) -> bool { false }
    fn is_switchsides_supported(&self) -> bool { false }
    fn is_privacy_mode_supported(&self) -> bool { false }
    fn switch_sides_handler(&self) {}
    fn elevate_direct(&self) {}
    fn elevate_with_logon(&self, _user: String, _pass: String) {}
    fn version_cmp(&self, _v1: String, _v2: String) -> i32 { 0 }
    fn get_printer_names(&self) -> Value { Value::array(0) }
    fn update_supported_decodings(&self) {}
    fn is_recording(&self) -> bool { false }
    fn msgbox_retry(&self, _tp: String, _title: String, _text: String, _link: String, _retry: bool) {}
    fn set_selected_windows_session_id(&self, _id: String) {}
    fn handle_screenshot(&self, _path: String) {}
    fn check_mcp_dismiss(&self) -> bool { false }
    fn get_mcp_click_button(&self) -> String { String::new() }
    fn get_mcp_chat_msg(&self) -> String { String::new() }
    fn get_mcp_password(&self) -> String { String::new() }
    fn check_mcp_query_state(&self) -> String { String::new() }
    fn write_mcp_state(&self, _key: String, _value: String) {}
}

impl sciter::EventHandler for RemoteHandler {
    sciter::dispatch_script_call! {

        fn is_file_transfer();
        fn is_port_forward();
        fn is_view_camera();

        fn t(String);

        fn read_remote_dir(String, bool);
        fn send_files(i32, i32, String, String, i32, bool, bool);
        fn add_job(i32, i32, String, String, i32, bool, bool);
        fn resume_job(i32, bool);
        fn cancel_job(i32);
        fn remove_file(i32, String, i32, bool);
        fn remove_dir(i32, String, bool);
        fn remove_dir_all(i32, String, bool, bool);
        fn create_dir(i32, String, bool);
        fn confirm_delete_files(i32, i32);
        fn set_write_override(i32, i32, bool, bool, bool);
        fn set_no_confirm(i32);
        fn rename_file(i32, String, String, bool);

        fn read_dir(String, bool);
        fn get_home_dir();
        fn get_path_sep(bool);

        fn get_next_job_id();
        fn update_next_job_id(i32);

        fn get_option(String);
        fn set_option(String, String);
        fn save_close_state(String, String);
        fn save_size(i32, i32, i32, i32);
        fn get_size();
        fn get_platform(bool);
        fn get_icon_path(i32, String);
        fn msgbox(String, String, String);
        fn get_toggle_option(String);
        fn toggle_option(String);
        fn get_view_style();
        fn get_keyboard_mode();
        fn peer_platform();
        fn get_default_printer();
        fn on_printer_selected(i32, String, String);
        fn is_screenshot_supported();
        fn take_screenshot(i32, String);
        fn record_screen(bool, i32, i32, i32);
        fn is_wayland_no_grab();

        fn send_mouse(i32, i32, i32, bool, bool, bool, bool);
        fn send_key_to_remote(String, String, i32);
        fn enter(String);
        fn leave(String);
        fn transfer_file();

        fn get_id();
        fn get_default_pi();
        fn input_os_password(String, bool);
        fn save_view_style(String);
        fn save_keyboard_mode(String);
        fn save_image_quality(String);
        fn save_custom_image_quality(i32);
        fn get_custom_image_quality();
        fn get_image_quality();
        fn get_raw_keyboard_mode();
        fn switch_display(i32);
        fn refresh_video(i32);
        fn ctrl_alt_del();
        fn lock_screen();
        fn restart_remote_device();
        fn send_note(String);
        fn tunnel();
        fn send_chat(String);
        fn get_chatbox();
        fn login(String, String, String, bool);
        fn get_icon();
        fn alternative_codecs();
        fn change_prefer_codec();
        fn is_keyboard_mode_supported(String);
        fn is_remote_printing_supported();
        fn is_switchsides_supported();
        fn is_privacy_mode_supported();
        fn switch_sides_handler();
        fn elevate_direct();
        fn elevate_with_logon(String, String);
        fn version_cmp(String, String);
        fn get_printer_names();
        fn update_supported_decodings();
        fn is_recording();
        fn msgbox_retry(String, String, String, String, bool);
        fn set_selected_windows_session_id(String);
        fn handle_screenshot(String);
        fn check_mcp_dismiss();
        fn get_mcp_click_button();
        fn get_mcp_chat_msg();
        fn get_mcp_password();
        fn check_mcp_query_state();
        fn write_mcp_state(String, String);
    }
}
