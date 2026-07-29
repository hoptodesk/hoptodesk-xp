#![allow(non_camel_case_types)]

#[macro_use]
pub mod macros;

pub mod bridge;
pub mod capi;
pub mod dom;
pub mod engine;
pub mod host;
pub mod script;
pub mod value;
pub mod video;
pub mod window;

pub mod types {
    pub use crate::capi::sctypes::*;
}

pub use capi::scdef::{GFX_LAYER, SCRIPT_RUNTIME_FEATURES};
pub use capi::scdom::HELEMENT;
pub use dom::event::EventHandler;
pub use dom::Element;
pub use host::{Archive, Host, HostHandler};
pub use value::{FromValue, Value};
pub use window::Window;

pub type WindowBuilder = window::Builder;

#[derive(Copy, Clone)]
pub enum RuntimeOptions<'a> {
    LibraryPath(&'a str),
    GfxLayer(GFX_LAYER),
    UxTheming(bool),
    DebugMode(bool),
    ScriptFeatures(u8),
    ConnectionTimeout(u32),
    OnHttpsError(u8),
    InitScript(&'a str),
    MaxHttpDataLength(usize),
    LogicalPixel(bool),
}

pub fn set_library(custom_path: &str) -> std::result::Result<(), String> {
    let _ = custom_path;
    Ok(())
}

pub fn set_options(options: RuntimeOptions) -> std::result::Result<(), ()> {
    let _ = options;
    Ok(())
}

pub fn version() -> String {
    "wireui 0.0.1".to_owned()
}

pub fn version_num() -> u64 {
    0x0401_0A00
}
