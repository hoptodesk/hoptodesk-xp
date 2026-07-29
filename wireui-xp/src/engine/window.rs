// Window module: the platform-neutral pieces of the mainline engine's
// window.rs (hit testing, page sources, boot log). The winit/wgpu/softbuffer
// live-window loop is replaced by the raw Win32 layer in win32_window.rs; on
// other platforms only the headless harness bins are supported.

use super::dom::{Document, NodeKey};

pub fn hit_test(
    doc: &Document,
    rects: &std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>,
    x: f32,
    y: f32,
) -> Option<NodeKey> {
    // Document-order fallback used when no paint order is cached (tests, first
    // frame). Prefer hit_test_ordered with Engine.screen_order.
    let order = doc.descendants(doc.root);
    hit_test_ordered(doc, rects, &order, x, y)
}

/// Topmost element at a point = the LAST one containing it in PAINT order
/// (z-aware, from compute_screen_geometry). Depth- or document-order picks are
/// both wrong once overlays rely on z-index: content behind the settings panel
/// stole its clicks, and a popup's backdrop (appended after the menu) stole the
/// menu's.
pub fn hit_test_ordered(
    doc: &Document,
    rects: &std::collections::HashMap<NodeKey, (f32, f32, f32, f32)>,
    order: &[NodeKey],
    x: f32,
    y: f32,
) -> Option<NodeKey> {
    let mut best: Option<NodeKey> = None;
    for &key in order {
        match doc.arena.get(key) {
            Some(n) if !n.is_text() => {}
            _ => continue,
        }
        if let Some(&(rx, ry, rw, rh)) = rects.get(&key) {
            if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
                best = Some(key);
            }
        }
    }
    best
}

// For a custom-chrome (window-frame=extended) window: is this point on the
// draggable caption? True when walking up from the hit reaches a caption /
// role="window-caption" element without first passing an interactive control
// (so the toolbar buttons and the min/max/close controls still click).
pub fn caption_drag_target(doc: &Document, hit: NodeKey) -> bool {
    let mut k = Some(hit);
    while let Some(cur) = k {
        let node = match doc.arena.get(cur) {
            Some(n) => n,
            None => return false,
        };
        match node.tag.as_str() {
            "button" | "input" | "select" | "textarea" | "a" => return false,
            "caption" | "header" => return true,
            _ => {}
        }
        if node.attr("role") == Some("window-caption") || node.attr("role") == Some("window-icon") {
            return true;
        }
        k = node.parent;
    }
    false
}

/// role="window-minimize|window-maximize|window-close" on the hit or an
/// ancestor: Sciter treats the role itself as the window action.
pub fn window_control_role(doc: &Document, hit: NodeKey) -> Option<&'static str> {
    let mut k = Some(hit);
    while let Some(cur) = k {
        let node = doc.arena.get(cur)?;
        match node.attr("role") {
            Some("window-minimize") => return Some("minimize"),
            Some("window-maximize") => return Some("maximize"),
            Some("window-close") => return Some("close"),
            _ => {}
        }
        k = node.parent;
    }
    None
}

/// The caption's app-icon element (Sciter's role="window-icon"); a left-click on
/// it opens the native window system menu.
pub fn window_icon_at(doc: &Document, hit: NodeKey) -> Option<NodeKey> {
    let mut k = Some(hit);
    while let Some(cur) = k {
        let node = doc.arena.get(cur)?;
        if node.attr("role") == Some("window-icon") {
            return Some(cur);
        }
        k = node.parent;
    }
    None
}

pub type BehaviorFactory = std::rc::Rc<dyn Fn() -> crate::bridge::SharedHandler>;

pub enum PageSource {
    Path {
        page: std::path::PathBuf,
        base: Option<std::path::PathBuf>,
    },
    Archive {
        archive: crate::engine::archive::Archive,
        page: String,
    },
    Memory {
        html: String,
        base: String,
        archive: Option<crate::engine::archive::Archive>,
    },
}

pub fn is_shortcut_chord(alt: bool, ctrl: bool, cmd: bool, mac: bool) -> bool {
    if mac {
        cmd
    } else {
        ctrl && !alt
    }
}

pub fn modifiers_suppress_text(alt: bool, ctrl: bool, mac: bool) -> bool {
    if mac {
        alt || ctrl
    } else {
        alt != ctrl
    }
}

// Crash-surviving startup breadcrumbs: append+flush a line to a boot log in
// %TEMP% at each risky milestone. On a machine we cannot attach
// a debugger to, the last line written before an access violation names the
// exact step that died. Cheap and always-on.
pub fn boot_crumb(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("wireui-boot.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", msg);
        let _ = f.flush();
    }
}

pub fn run_window(
    page_path: &std::path::Path,
    extra_base: Option<std::path::PathBuf>,
    platform: &str,
    size: (u32, u32),
    title: &str,
    eval: Option<&str>,
) -> Result<(), String> {
    let source = PageSource::Path {
        page: page_path.to_path_buf(),
        base: extra_base,
    };
    run_window_source(source, platform, size, title, eval, None, Vec::new())
}

pub fn run_window_bridged(
    page_path: &std::path::Path,
    extra_base: Option<std::path::PathBuf>,
    platform: &str,
    size: (u32, u32),
    title: &str,
    handler: Option<crate::bridge::SharedHandler>,
    behaviors: Vec<(String, BehaviorFactory)>,
) -> Result<(), String> {
    let source = PageSource::Path {
        page: page_path.to_path_buf(),
        base: extra_base,
    };
    run_window_source(source, platform, size, title, None, handler, behaviors)
}

#[allow(clippy::too_many_arguments)]
pub fn run_window_source(
    source: PageSource,
    platform: &str,
    size: (u32, u32),
    title: &str,
    eval: Option<&str>,
    handler: Option<crate::bridge::SharedHandler>,
    behaviors: Vec<(String, BehaviorFactory)>,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        super::win32_window::run_window_source(
            source, platform, size, title, eval, handler, behaviors,
        )
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (source, platform, size, title, eval, handler, behaviors);
        Err("wireui-xp live windows are Windows-only; use wireui-render for headless".into())
    }
}
