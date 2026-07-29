use crate::capi::scdef::{
    LOAD_RESULT, OUTPUT_SEVERITY, OUTPUT_SUBSYTEMS, SCN_ATTACH_BEHAVIOR, SCN_DATA_LOADED,
    SCN_INVALIDATE_RECT, SCN_LOAD_DATA,
};
use crate::capi::sctypes::{HREQUEST, HWINDOW};
use crate::dom::event::EventHandler;
use crate::value::Value;

pub struct Host {
    hwnd: std::cell::Cell<HWINDOW>,
}

impl Host {
    pub(crate) fn new() -> Host {
        Host {
            hwnd: std::cell::Cell::new(std::ptr::null_mut()),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_hwnd(&self, hwnd: HWINDOW) {
        self.hwnd.set(hwnd);
    }

    pub fn get_hwnd(&self) -> HWINDOW {
        self.hwnd.get()
    }

    pub fn call_function(&self, name: &str, args: &[Value]) -> std::result::Result<Value, Value> {
        let _ = (name, args);
        Err(Value::error("wireui: script host is not running"))
    }
}

pub struct Archive {
    entries: Vec<(String, Vec<u8>)>,
}

impl Archive {
    pub fn open(resource: &[u8]) -> std::result::Result<Archive, ()> {
        let _ = resource;
        Ok(Archive {
            entries: Vec::new(),
        })
    }

    pub fn get(&self, uri: &str) -> Option<&[u8]> {
        let path = uri
            .trim_start_matches("this://app/")
            .trim_start_matches("//");
        self.entries
            .iter()
            .find(|(name, _)| name == path)
            .map(|(_, data)| data.as_slice())
    }
}

#[allow(unused_variables)]
pub trait HostHandler {
    fn on_data_load(&mut self, pnm: &mut SCN_LOAD_DATA) -> Option<LOAD_RESULT> {
        None
    }

    fn on_data_loaded(&mut self, pnm: &SCN_DATA_LOADED) {}

    fn on_attach_behavior(&mut self, pnm: &mut SCN_ATTACH_BEHAVIOR) -> bool {
        false
    }

    fn on_engine_destroyed(&mut self) {}

    fn on_graphics_critical_failure(&mut self) {}

    fn on_invalidate(&mut self, pnm: &SCN_INVALIDATE_RECT) {}

    fn on_debug_output(
        &mut self,
        subsystem: OUTPUT_SUBSYTEMS,
        severity: OUTPUT_SEVERITY,
        message: &str,
    ) {
        if !message.is_empty() {
            eprintln!("{:?}:{:?}: {}", severity, subsystem, message);
        }
    }

    fn data_ready(&self, hwnd: HWINDOW, uri: &str, data: &[u8], request_id: Option<HREQUEST>) {
        let _ = (hwnd, uri, data, request_id);
    }

    fn attach_behavior<Handler: EventHandler>(
        &self,
        pnm: &mut SCN_ATTACH_BEHAVIOR,
        handler: Handler,
    ) {
        let _ = (pnm, handler);
    }
}
