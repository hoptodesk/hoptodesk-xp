// Raw Win32 window layer: replaces the mainline engine's winit/wgpu/
// softbuffer loop with XP-era APIs only (RegisterClassW/CreateWindowExW,
// GetMessage pump, StretchDIBits present of the tiny-skia raster). One UI
// thread per process; CURRENT_INTERP/CURRENT_ENGINE are published for the
// pump's lifetime so the client's SetTimer TimerProcs can drive Element
// accessors from inside DispatchMessage.

use super::dom::NodeKey;
use super::host::{self, EngineRef};
use super::layout::TextSystem;
use super::window::{hit_test_ordered, BehaviorFactory, PageSource};
use crate::script::interp::Interp;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use winapi::shared::minwindef::{DWORD, HIWORD, LOWORD, LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT, RECT};
use winapi::um::wingdi::{
    StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use winapi::um::winuser::*;

const TICK_TIMER_ID: usize = 0x7715;
const TICK_MS: u32 = 30;
const WM_APP_WAKE: u32 = WM_APP + 1;

struct WinState {
    engine: EngineRef,
    ts: TextSystem,
    layout_cache: Option<(u64, u32, u32, super::layout::LayoutResult)>,
    last_rects: HashMap<NodeKey, (f32, f32, f32, f32)>,
    bgra: Vec<u32>,
    cursor: (f32, f32),
    buttons_down: i64,
    onmouse_over: Option<NodeKey>,
    last_onmouse_down: Option<Instant>,
    last_click: Option<(Instant, (f32, f32))>,
    last_edit_click: Option<(Instant, NodeKey)>,
    text_drag: Option<NodeKey>,
    col_drag: Option<host::ColumnDrag>,
    hover_title: Option<(NodeKey, Instant)>,
    tracking_leave: bool,
    last_window_state: i64,
    pending_statechange: u8,
    min_size: Option<(i32, i32)>,
    max_size: Option<(i32, i32)>,
    saved_rect: Option<RECT>,
    start: Instant,
    last_input: Instant,
    caret_solid: bool,
    caret_blink_phase: i64,
}

thread_local! {
    static REGISTRY: RefCell<HashMap<usize, Rc<RefCell<WinState>>>> =
        RefCell::new(HashMap::new());
    static MAIN_HWND: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static PENDING_HWND: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PENDING_HIGH_SURROGATE: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
}

fn char_from_utf16_unit(unit: u16) -> Option<char> {
    if (0xD800..0xDC00).contains(&unit) {
        PENDING_HIGH_SURROGATE.with(|c| c.set(unit));
        return None;
    }
    let high = PENDING_HIGH_SURROGATE.with(|c| c.replace(0));
    if (0xDC00..0xE000).contains(&unit) {
        if high == 0 {
            return None;
        }
        let cp = 0x10000 + (((high as u32) - 0xD800) << 10) + ((unit as u32) - 0xDC00);
        return char::from_u32(cp);
    }
    char::from_u32(unit as u32)
}

pub fn create_main_window() -> usize {
    let class = register_class();
    let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: 800,
        bottom: 600,
    };
    unsafe {
        AdjustWindowRect(&mut rc, style, 0);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            to_wide("HopToDesk").as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rc.right - rc.left,
            rc.bottom - rc.top,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null()),
            std::ptr::null_mut(),
        );
        PENDING_HWND.with(|c| c.set(hwnd as usize));
        hwnd as usize
    }
}

fn state_for(hwnd: HWND) -> Option<Rc<RefCell<WinState>>> {
    REGISTRY.with(|r| r.borrow().get(&(hwnd as usize)).cloned())
}

fn with_interp<R>(f: impl FnOnce(&mut Interp) -> R) -> Option<R> {
    if host::script_busy() {
        return None;
    }
    let p = host::current_interp_ptr();
    if p.is_null() {
        None
    } else {
        Some(f(unsafe { &mut *p }))
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn key_mods() -> (bool, bool, bool, bool) {
    unsafe {
        (
            GetKeyState(VK_MENU) < 0,
            GetKeyState(VK_CONTROL) < 0,
            GetKeyState(VK_SHIFT) < 0,
            false,
        )
    }
}

fn raw_mouse(
    etype: i64,
    cursor: (f32, f32),
    buttons: i64,
    delta: (f64, f64),
    mods: (bool, bool, bool, bool),
) -> host::RawMouse {
    host::RawMouse {
        etype,
        x: cursor.0,
        y: cursor.1,
        buttons,
        wheel: delta,
        alt: mods.0,
        ctrl: mods.1,
        shift: mods.2,
        meta: mods.3,
    }
}

fn set_hover_chain(engine: &EngineRef, hovered: Option<NodeKey>) -> bool {
    let mut e = engine.borrow_mut();
    let mut want: Vec<NodeKey> = Vec::new();
    if let Some(h) = hovered {
        let mut cur = Some(h);
        while let Some(k) = cur {
            want.push(k);
            cur = e.doc.arena.get(k).and_then(|n| n.parent);
        }
    }
    if want == e.hover_chain {
        return false;
    }
    let old = std::mem::replace(&mut e.hover_chain, want);
    let mut changed = false;
    for key in old {
        if !e.hover_chain.contains(&key) {
            if let Some(node) = e.doc.arena.get_mut(key) {
                if node.states.hover {
                    node.states.hover = false;
                    changed = true;
                }
            }
        }
    }
    let chain = e.hover_chain.clone();
    for key in chain {
        if let Some(node) = e.doc.arena.get_mut(key) {
            if !node.states.hover {
                node.states.hover = true;
                changed = true;
            }
        }
    }
    changed
}

fn build_main_scene(
    dev_w: u32,
    dev_h: u32,
    engine: &EngineRef,
    ts: &mut TextSystem,
    cache: &mut Option<(u64, u32, u32, super::layout::LayoutResult)>,
    scale: f32,
) -> super::paint_backend::PaintScene {
    let styles_rc = host::cached_computed_styles(engine);
    let styles = styles_rc.as_ref();
    let pseudo_rc = host::cached_pseudo_boxes(engine, styles);
    let e = engine.borrow();
    let vw = dev_w as f32 / scale;
    let vh = dev_h as f32 / scale;
    let epoch = super::dom::layout_epoch();
    let stale = match cache {
        Some((ep, w, h, _)) => *ep != epoch || *w != dev_w || *h != dev_h,
        None => true,
    };
    if stale {
        let layout = super::layout::layout_document(&e.doc, &styles, ts, (vw, vh), scale);
        *cache = Some((epoch, dev_w, dev_h, layout));
    }
    let layout = &cache.as_ref().unwrap().3;
    let pseudo = pseudo_rc.as_ref();
    let mut scene = super::paint::paint_document_overlaid(
        &e.doc,
        &styles,
        layout,
        scale,
        &e.video_sinks,
        &pseudo,
        e.now_ms,
        e.caret_solid,
        &e.content_overlays,
    );
    let layout_rects = layout.rects.clone();
    let cursors: std::collections::HashMap<NodeKey, super::style::Cursor> =
        styles.iter().map(|(k, s)| (*k, s.cursor)).collect();
    let tooltip = e.tooltip.clone();
    drop(e);
    if let Some((text, tx, ty)) = tooltip {
        super::paint::paint_tooltip(&mut scene, ts, &text, tx, ty, (vw, vh), scale);
    }
    let (screen, order) = host::compute_screen_geometry(engine, &layout_rects, &styles);
    host::update_layout_rects(engine, &layout_rects);
    {
        let mut em = engine.borrow_mut();
        em.screen_rects = screen;
        em.screen_order = order;
        em.hover_cursors = cursors;
    }
    {
        let e = engine.borrow();
        for (iw, ih, rgba, x, y, w, h, op) in e.fg_overlays.iter() {
            super::paint::paint_overlay_image(
                &mut scene,
                *iw,
                *ih,
                rgba.clone(),
                *x as f64,
                *y as f64,
                *w as f64,
                *h as f64,
                *op,
                scale as f64,
            );
        }
    }
    scene
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    unsafe {
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut rc);
        (
            (rc.right - rc.left).max(1) as u32,
            (rc.bottom - rc.top).max(1) as u32,
        )
    }
}

fn rasterize_into(st: &mut WinState, engine: &EngineRef, w: u32, h: u32) {
    let scene = build_main_scene(w, h, engine, &mut st.ts, &mut st.layout_cache, 1.0);
    if let Some((_, _, _, layout)) = st.layout_cache.as_ref() {
        st.last_rects = layout.rects.clone();
    }
    let Some(list) = scene.as_cpu() else { return };
    let pm = super::cpu_raster::rasterize(list, w, h);
    let rgba = pm.data();
    st.bgra.clear();
    st.bgra.reserve((w * h) as usize);
    for px in rgba.chunks_exact(4) {
        st.bgra.push(
            0xFF00_0000u32 | ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | (px[2] as u32),
        );
    }
}

fn paint_now(hwnd: HWND, st: &mut WinState) {
    let (w, h) = client_size(hwnd);
    let engine = st.engine.clone();
    rasterize_into(st, &engine, w, h);
    unsafe {
        let hdc = GetDC(hwnd);
        if !hdc.is_null() {
            blit(hdc, w, h, &st.bgra);
            ReleaseDC(hwnd, hdc);
        }
    }
}

unsafe fn blit(hdc: winapi::shared::windef::HDC, w: u32, h: u32, bgra: &[u32]) {
    if bgra.len() != (w * h) as usize {
        return;
    }
    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w as i32,
        biHeight: -(h as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    StretchDIBits(
        hdc,
        0,
        0,
        w as i32,
        h as i32,
        0,
        0,
        w as i32,
        h as i32,
        bgra.as_ptr() as *const _,
        &bmi,
        DIB_RGB_COLORS,
        SRCCOPY,
    );
}

fn invalidate(hwnd: HWND) {
    unsafe {
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}

fn lparam_xy(lparam: LPARAM) -> (f32, f32) {
    let x = LOWORD(lparam as DWORD) as i16 as f32;
    let y = HIWORD(lparam as DWORD) as i16 as f32;
    (x, y)
}

fn hit_at(engine: &EngineRef, x: f32, y: f32) -> Option<NodeKey> {
    let e = engine.borrow();
    hit_test_ordered(&e.doc, &e.screen_rects, &e.screen_order, x, y)
}

fn cursor_to_idc(c: super::style::Cursor) -> *const u16 {
    use super::style::Cursor;
    match c {
        Cursor::Pointer => IDC_HAND,
        Cursor::Text => IDC_IBEAM,
        Cursor::Move => IDC_SIZEALL,
        Cursor::Wait => IDC_WAIT,
        Cursor::NotAllowed => IDC_NO,
        Cursor::ColResize => IDC_SIZEWE,
        Cursor::RowResize => IDC_SIZENS,
        _ => IDC_ARROW,
    }
}

fn update_os_metrics(hwnd: HWND) {
    unsafe {
        let sw = GetSystemMetrics(SM_CXSCREEN) as f64;
        let sh = GetSystemMetrics(SM_CYSCREEN) as f64;
        host::set_monitor_rect((0.0, 0.0, sw, sh));
        let mut wa: RECT = std::mem::zeroed();
        if SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0) != 0 {
            host::set_workarea_rect((
                wa.left as f64,
                wa.top as f64,
                (wa.right - wa.left) as f64,
                (wa.bottom - wa.top) as f64,
            ));
        }
        let mut wr: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut wr) != 0 {
            let (cw, ch) = client_size(hwnd);
            host::set_window_rect((wr.left as f64, wr.top as f64, cw as f64, ch as f64));
        }
    }
}

fn os_window_state(hwnd: HWND, fullscreen: bool) -> i64 {
    unsafe {
        if IsIconic(hwnd) != 0 {
            2
        } else if fullscreen {
            5
        } else if IsZoomed(hwnd) != 0 {
            3
        } else if IsWindowVisible(hwnd) == 0 {
            4
        } else {
            1
        }
    }
}

fn apply_window_command(hwnd: HWND, state: &Rc<RefCell<WinState>>, c: host::WindowCommand) {
    use host::WindowCommand as WC;
    unsafe {
        match c {
            WC::State(n) => match n {
                1 => {
                    if state.borrow().saved_rect.is_some() {
                        restore_from_fullscreen(hwnd, state);
                    }
                    ShowWindow(hwnd, SW_SHOWNORMAL);
                }
                2 => {
                    ShowWindow(hwnd, SW_MINIMIZE);
                }
                3 => {
                    ShowWindow(hwnd, SW_MAXIMIZE);
                }
                4 => {
                    ShowWindow(hwnd, SW_HIDE);
                }
                5 => enter_fullscreen(hwnd, state),
                _ => {}
            },
            WC::Topmost(on) => {
                SetWindowPos(
                    hwnd,
                    if on { HWND_TOPMOST } else { HWND_NOTOPMOST },
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            WC::MinSize(w, h) => state.borrow_mut().min_size = Some((w as i32, h as i32)),
            WC::MaxSize(w, h) => state.borrow_mut().max_size = Some((w as i32, h as i32)),
            WC::Resizable(on) => {
                let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
                let new = if on {
                    style | WS_THICKFRAME
                } else {
                    style & !WS_THICKFRAME
                };
                SetWindowLongW(hwnd, GWL_STYLE, new as i32);
            }
            WC::Maximizable(on) => {
                let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
                let new = if on {
                    style | WS_MAXIMIZEBOX
                } else {
                    style & !WS_MAXIMIZEBOX
                };
                SetWindowLongW(hwnd, GWL_STYLE, new as i32);
            }
            WC::Caption(t) => {
                let w = to_wide(&t);
                SetWindowTextW(hwnd, w.as_ptr());
            }
            WC::Icon(_, _, _) => {}
        }
    }
}

fn enter_fullscreen(hwnd: HWND, state: &Rc<RefCell<WinState>>) {
    unsafe {
        {
            let mut st = state.borrow_mut();
            if st.saved_rect.is_none() {
                let mut r: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut r);
                st.saved_rect = Some(r);
            }
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        SetWindowLongW(
            hwnd,
            GWL_STYLE,
            (style & !(WS_CAPTION | WS_THICKFRAME)) as i32,
        );
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        SetWindowPos(hwnd, HWND_TOP, 0, 0, sw, sh, SWP_FRAMECHANGED);
        host::set_current_window_state(5);
    }
}

fn restore_from_fullscreen(hwnd: HWND, state: &Rc<RefCell<WinState>>) {
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        SetWindowLongW(hwnd, GWL_STYLE, (style | WS_CAPTION | WS_THICKFRAME) as i32);
        let saved = state.borrow_mut().saved_rect.take();
        if let Some(r) = saved {
            SetWindowPos(
                hwnd,
                HWND_TOP,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_FRAMECHANGED,
            );
        }
        host::set_current_window_state(1);
    }
}

// Every Win32 call below can re-enter wndproc synchronously (SetWindowText,
// ShowWindow, SetWindowPos, DestroyWindow), so the window state must not be
// borrowed across them - wndproc borrows it on entry.
fn after_script(hwnd: HWND, state: &Rc<RefCell<WinState>>) {
    if host::take_view_close() {
        unsafe {
            DestroyWindow(hwnd);
        }
        return;
    }
    if let Some((x, y, w, h)) = host::take_view_move() {
        unsafe {
            let flags = if w > 0.0 && h > 0.0 {
                SWP_NOZORDER
            } else {
                SWP_NOZORDER | SWP_NOSIZE
            };
            let mut rc = RECT {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: h as i32,
            };
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            AdjustWindowRect(&mut rc, style, 0);
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                x as i32,
                y as i32,
                rc.right - rc.left,
                rc.bottom - rc.top,
                flags,
            );
        }
    }
    let id = {
        let st = state.borrow();
        host::engine_id(&st.engine)
    };
    for (target, c) in host::take_window_commands() {
        if target == id {
            apply_window_command(hwnd, state, c);
        } else {
            let other = REGISTRY.with(|r| {
                r.borrow()
                    .iter()
                    .find(|(_, s)| {
                        s.try_borrow()
                            .map_or(false, |s| host::engine_id(&s.engine) == target)
                    })
                    .map(|(h, s)| (*h, s.clone()))
            });
            if let Some((oh, os)) = other {
                apply_window_command(oh as HWND, &os, c);
            }
        }
    }
    for pending in host::take_child_windows() {
        create_child_window(pending);
    }
    if host::take_dom_mutation() {
        invalidate(hwnd);
    }
}

fn create_child_window(pending: host::PendingChildWindow) {
    let engine = pending.engine.clone();
    let mut ts = TextSystem::new_with(false);
    {
        let e = engine.borrow();
        for sheet in &e.sheets {
            for (family, data) in &sheet.font_faces {
                ts.register_font(family, data.clone());
            }
        }
    }
    register_xp_system_fonts(&mut ts);
    let class = register_class();
    let title_w = to_wide(&pending.title);
    let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: pending.width.max(200) as i32,
        bottom: pending.height.max(150) as i32,
    };
    unsafe {
        AdjustWindowRect(&mut rc, style, 0);
    }
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title_w.as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rc.right - rc.left,
            rc.bottom - rc.top,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null()),
            std::ptr::null_mut(),
        )
    };
    if hwnd.is_null() {
        return;
    }
    let state = Rc::new(RefCell::new(WinState {
        engine,
        ts,
        layout_cache: None,
        last_rects: HashMap::new(),
        bgra: Vec::new(),
        cursor: (0.0, 0.0),
        buttons_down: 0,
        onmouse_over: None,
        last_onmouse_down: None,
        last_click: None,
        last_edit_click: None,
        text_drag: None,
        col_drag: None,
        hover_title: None,
        tracking_leave: false,
        last_window_state: 1,
        pending_statechange: 0,
        min_size: None,
        max_size: None,
        saved_rect: None,
        start: Instant::now(),
        last_input: Instant::now(),
        caret_solid: false,
        caret_blink_phase: -1,
    }));
    REGISTRY.with(|r| r.borrow_mut().insert(hwnd as usize, state));
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetTimer(hwnd, TICK_TIMER_ID, TICK_MS, None);
    }
}

fn engine_tick(hwnd: HWND, st: &mut WinState) {
    let engine = st.engine.clone();
    let now = st.start.elapsed().as_millis() as f64;
    let osst = os_window_state(hwnd, st.saved_rect.is_some());
    host::set_current_window_state(osst);
    if osst != st.last_window_state {
        st.last_window_state = osst;
        with_interp(|interp| host::fire_view_event(interp, &engine, "statechange"));
        invalidate(hwnd);
    }
    update_os_metrics(hwnd);
    {
        let mut e = engine.borrow_mut();
        e.now_ms = now;
    }
    let due = {
        let e = engine.borrow();
        e.timers.iter().any(|(t, _)| *t <= now)
    };
    if due {
        with_interp(|interp| {
            if let Err(e) = host::pump_timers(interp, &engine, now) {
                eprintln!("timer error: {}", e.0);
            }
        });
        invalidate(hwnd);
    }
    host::pump_video_events(&engine);
    let (progress, caret) = {
        let e = engine.borrow();
        (
            host::has_active_progress(&e),
            host::focused_input(&e).is_some(),
        )
    };
    let repaint = std::mem::take(&mut engine.borrow_mut().repaint_requested);
    if host::animate_scroll(&engine) || progress || repaint {
        invalidate(hwnd);
    }
    // The caret repaints only when its blink phase actually flips, and goes
    // solid after a few idle seconds; unconditional invalidation here repainted
    // the full window every tick while any input had focus (the home window's
    // ID box always does), pinning XP-era CPUs at idle.
    if caret {
        let solid_now = st.last_input.elapsed().as_secs() >= 5;
        if solid_now != st.caret_solid {
            st.caret_solid = solid_now;
            engine.borrow_mut().caret_solid = solid_now;
            invalidate(hwnd);
        }
        if !solid_now {
            let phase = (now / 530.0) as i64 % 2;
            if phase != st.caret_blink_phase {
                st.caret_blink_phase = phase;
                invalidate(hwnd);
            }
        }
    }
    if let Some((el, since)) = st.hover_title {
        let shown = engine.borrow().tooltip.is_some();
        if !shown && since.elapsed().as_millis() >= 600 {
            let title = {
                let e = engine.borrow();
                e.doc
                    .arena
                    .get(el)
                    .and_then(|n| n.attr("title"))
                    .map(|s| s.to_string())
            };
            if let Some(t) = title {
                let c = st.cursor;
                engine.borrow_mut().tooltip = Some((t, c.0, c.1));
                invalidate(hwnd);
            }
        }
    }
}

fn fire_change_on_focused(engine: &EngineRef) {
    let focused = {
        let e = engine.borrow();
        host::focused_input(&e)
    };
    if let Some(fk) = focused {
        with_interp(|interp| {
            host::dispatch_dom_event(interp, engine, "change", fk).ok();
            host::drain_events(interp, engine);
        });
    }
}

// Pop the native window system menu on the caption's app icon, and close the
// window on a double-click -- the Sciter role="window-icon" behaviour for a
// custom-chrome window with no OS title bar. `icon` is the icon rect (x,y,w,h)
// in client px; the menu opens at its bottom-left. The double-click is
// reconstructed from a fast dismiss (the second click dismisses the just-opened
// modal menu) with the button still down and the pointer still over the icon,
// so a stray dismiss never closes a live session. Returns true when shown.
unsafe fn show_window_system_menu(hwnd: HWND, icon: (f32, f32, f32, f32)) -> bool {
    let menu = GetSystemMenu(hwnd, 0);
    if menu.is_null() {
        return false;
    }
    let maximized = IsZoomed(hwnd) != 0;
    let en = |on: bool| MF_BYCOMMAND | (if on { MF_ENABLED } else { MF_GRAYED });
    EnableMenuItem(menu, SC_RESTORE as UINT, en(maximized));
    EnableMenuItem(menu, SC_MOVE as UINT, en(!maximized));
    EnableMenuItem(menu, SC_SIZE as UINT, en(!maximized));
    EnableMenuItem(menu, SC_MINIMIZE as UINT, MF_BYCOMMAND | MF_ENABLED);
    EnableMenuItem(menu, SC_MAXIMIZE as UINT, en(!maximized));
    EnableMenuItem(menu, SC_CLOSE as UINT, MF_BYCOMMAND | MF_ENABLED);

    let mut tl = POINT {
        x: icon.0 as i32,
        y: icon.1 as i32,
    };
    ClientToScreen(hwnd, &mut tl);
    let (ix, iy) = (tl.x, tl.y);
    let (iw, ih) = (icon.2 as i32, icon.3 as i32);

    SetForegroundWindow(hwnd);
    let opened = Instant::now();
    let cmd = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_LEFTBUTTON,
        ix,
        iy + ih,
        0,
        hwnd,
        std::ptr::null(),
    );
    PostMessageW(hwnd, WM_NULL, 0, 0);
    if cmd != 0 {
        PostMessageW(hwnd, WM_SYSCOMMAND, cmd as WPARAM, 0);
        return true;
    }
    // No selection: the second click of a double-click dismissed the menu.
    // Reconstruct it as a fast dismiss with the button still down and the
    // pointer still over the icon -> close. The guards stop an Escape or
    // click-elsewhere dismiss from closing a live session.
    let fast = opened.elapsed().as_millis() as u32 <= GetDoubleClickTime();
    let btn_down = (GetAsyncKeyState(VK_LBUTTON) as u16 & 0x8000) != 0;
    let mut pt = POINT { x: 0, y: 0 };
    let on_icon = GetCursorPos(&mut pt) != 0
        && pt.x >= ix
        && pt.x < ix + iw
        && pt.y >= iy
        && pt.y < iy + ih;
    if fast && btn_down && on_icon {
        PostMessageW(hwnd, WM_SYSCOMMAND, SC_CLOSE, 0);
    }
    true
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(state) = state_for(hwnd) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    // A modal (view.msgbox) opened from a script handler pumps the message
    // queue, so this procedure re-enters while an outer message still holds the
    // window state. Let the default handler take those rather than panicking on
    // the borrow.
    let Ok(state_ref) = state.try_borrow() else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    let engine = state_ref.engine.clone();
    drop(state_ref);
    host::set_current_engine(&engine);
    host::set_current_window_hwnd(hwnd as usize);
    if matches!(
        msg,
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_CHAR | WM_LBUTTONDOWN | WM_LBUTTONUP
            | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_LBUTTONDBLCLK | WM_MOUSEMOVE | WM_MOUSEWHEEL
    ) {
        if let Ok(mut st) = state.try_borrow_mut() {
            st.last_input = Instant::now();
        }
    }
    let mods = key_mods();

    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let fire_state = {
                let mut st = state.borrow_mut();
                let (w, h) = client_size(hwnd);
                rasterize_into(&mut st, &engine, w, h);
                blit(hdc, w, h, &st.bgra);
                if st.pending_statechange > 0 {
                    st.pending_statechange -= 1;
                    if st.pending_statechange == 0 {
                        true
                    } else {
                        invalidate(hwnd);
                        false
                    }
                } else {
                    false
                }
            };
            if fire_state {
                with_interp(|interp| host::fire_view_event(interp, &engine, "statechange"));
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_SIZE => {
            with_interp(|interp| {
                host::fire_view_event(interp, &engine, "size");
                host::drain_events(interp, &engine);
            });
            let osst = os_window_state(hwnd, state.borrow().saved_rect.is_some());
            host::set_current_window_state(osst);
            {
                let mut st = state.borrow_mut();
                if osst != st.last_window_state {
                    st.last_window_state = osst;
                    if osst == 2 {
                        drop(st);
                        with_interp(|interp| {
                            host::fire_view_event(interp, &engine, "statechange")
                        });
                    } else {
                        st.pending_statechange = 3;
                    }
                }
            }
            after_script(hwnd, &state);
            invalidate(hwnd);
            0
        }
        WM_GETMINMAXINFO => {
            let mmi = lparam as *mut MINMAXINFO;
            let st = state.borrow();
            if let Some((w, h)) = st.min_size {
                (*mmi).ptMinTrackSize = POINT { x: w, y: h };
            }
            if let Some((w, h)) = st.max_size {
                (*mmi).ptMaxTrackSize = POINT { x: w, y: h };
            }
            0
        }
        WM_SETCURSOR => {
            if LOWORD(lparam as DWORD) as isize == HTCLIENT {
                let c = {
                    let st = state.borrow();
                    let hit = hit_at(&engine, st.cursor.0, st.cursor.1);
                    host::cursor_at(&engine, hit)
                };
                SetCursor(LoadCursorW(std::ptr::null_mut(), cursor_to_idc(c)));
                1
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_MOUSEMOVE => {
            let (x, y) = lparam_xy(lparam);
            let mut st = state.borrow_mut();
            st.cursor = (x, y);
            if !st.tracking_leave {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tme);
                st.tracking_leave = true;
            }
            if let Some(d) = &st.col_drag {
                host::column_resize_apply(&engine, d, x);
                drop(st);
                invalidate(hwnd);
                return 0;
            }
            let buttons = st.buttons_down;
            engine.borrow_mut().pointer = (x, y);
            if engine.borrow().edit_menu.is_some() {
                let hit = hit_at(&engine, x, y);
                if host::edit_menu_hover(&engine, hit) {
                    invalidate(hwnd);
                }
            }
            if buttons & 1 != 0 {
                if let Some(t) = st.text_drag {
                    let styles = host::cached_computed_styles(&engine);
                    let rect = engine.borrow().screen_rects.get(&t).copied();
                    if let Some(r) = rect {
                        let is_ta = {
                            let e = engine.borrow();
                            e.doc.arena.get(t).map_or(false, |n| n.tag == "textarea")
                        };
                        let pad = if is_ta { 5.0 } else { host::input_pad_left(styles.get(&t)) };
                        let (cy, wrap) = if is_ta {
                            (y - r.1 - pad, Some((r.2 - 2.0 * pad).max(8.0)))
                        } else {
                            (2.0, None)
                        };
                        host::caret_click(
                            &engine,
                            &mut st.ts,
                            t,
                            x - r.0 - pad,
                            cy,
                            wrap,
                            host::CaretClick::Drag,
                            &styles,
                        );
                        drop(st);
                        invalidate(hwnd);
                        return 0;
                    }
                }
            }
            let hit = hit_at(&engine, x, y);
            with_interp(|interp| {
                host::dispatch_mouse_subscriptions(interp, &engine, 0x1003, hit, x, y);
            });
            let om_target = host::on_mouse_target(&engine, hit);
            let old_over = st.onmouse_over;
            if om_target != old_over {
                with_interp(|interp| {
                    if let Some(old) = old_over {
                        host::dispatch_on_mouse(
                            interp,
                            &engine,
                            old,
                            &raw_mouse(0x1005, (x, y), buttons, (0.0, 0.0), mods),
                        );
                    }
                    if let Some(new) = om_target {
                        host::dispatch_on_mouse(
                            interp,
                            &engine,
                            new,
                            &raw_mouse(0x1004, (x, y), buttons, (0.0, 0.0), mods),
                        );
                    }
                });
                st.onmouse_over = om_target;
            }
            if let Some(t) = om_target {
                with_interp(|interp| {
                    host::dispatch_on_mouse(
                        interp,
                        &engine,
                        t,
                        &raw_mouse(0x1003, (x, y), buttons, (0.0, 0.0), mods),
                    );
                    host::drain_events(interp, &engine);
                });
            }
            let title_el = {
                let e = engine.borrow();
                host::element_title(&e, hit).map(|(k, _)| k)
            };
            match (st.hover_title, title_el) {
                (Some((old, _)), Some(new)) if old == new => {}
                (_, Some(new)) => st.hover_title = Some((new, Instant::now())),
                (_, None) => {
                    st.hover_title = None;
                    if engine.borrow_mut().tooltip.take().is_some() {
                        invalidate(hwnd);
                    }
                }
            }
            if set_hover_chain(&engine, hit) {
                invalidate(hwnd);
            }
            drop(st);
            after_script(hwnd, &state);
            0
        }
        WM_MOUSELEAVE => {
            let mut st = state.borrow_mut();
            st.tracking_leave = false;
            let old = st.onmouse_over.take();
            let cursor = st.cursor;
            let buttons = st.buttons_down;
            drop(st);
            if let Some(old) = old {
                with_interp(|interp| {
                    host::dispatch_on_mouse(
                        interp,
                        &engine,
                        old,
                        &raw_mouse(0x1005, cursor, buttons, (0.0, 0.0), mods),
                    );
                    host::drain_events(interp, &engine);
                });
            }
            if set_hover_chain(&engine, None) {
                invalidate(hwnd);
            }
            0
        }
        WM_LBUTTONDOWN => {
            SetCapture(hwnd);
            let (x, y) = lparam_xy(lparam);
            let mut st = state.borrow_mut();
            st.cursor = (x, y);
            if engine.borrow().edit_menu.is_some() {
                let hit = hit_at(&engine, x, y);
                host::edit_menu_click(&engine, hit);
                drop(st);
                invalidate(hwnd);
                return 0;
            }
            st.hover_title = None;
            if engine.borrow_mut().tooltip.take().is_some() {
                invalidate(hwnd);
            }
            let hit = hit_at(&engine, x, y);
            let consumed = with_interp(|interp| {
                host::dispatch_mouse_subscriptions(interp, &engine, 0x1001, hit, x, y)
            })
            .unwrap_or(false);
            if consumed {
                drop(st);
                invalidate(hwnd);
                return 0;
            }
            if engine.borrow().open_select.is_some() {
                with_interp(|interp| {
                    host::select_dropdown_click(interp, &engine, hit);
                });
                drop(st);
                invalidate(hwnd);
                return 0;
            }
            if let Some(t) = hit {
                let sel = {
                    let e = engine.borrow();
                    e.doc.closest("select", t)
                };
                if let Some(sel) = sel {
                    if host::is_native_select(&engine, sel) {
                        host::open_select_dropdown(&engine, sel);
                        drop(st);
                        invalidate(hwnd);
                        return 0;
                    }
                    let chevron = {
                        let e = engine.borrow();
                        e.screen_rects
                            .get(&sel)
                            .map_or(false, |r| x >= r.0 + r.2 - 22.0)
                    };
                    if chevron {
                        host::open_select_dropdown(&engine, sel);
                        drop(st);
                        invalidate(hwnd);
                        return 0;
                    }
                }
            }
            if hit.map_or(false, |t| {
                let e = engine.borrow();
                e.doc.closest("thead", t).is_some()
            }) {
                if let Some(d) = host::column_resize_hit(&engine, x, y) {
                    st.col_drag = Some(d);
                    st.buttons_down |= 1;
                    return 0;
                }
            }
            // A left-click on the caption's app icon opens the native window
            // system menu; a double-click closes the window (Sciter's
            // role="window-icon"). The menu is modal and re-enters wndproc, so
            // release the window-state borrow and the mouse capture first.
            let icon = hit.and_then(|t| {
                let e = engine.borrow();
                super::window::window_icon_at(&e.doc, t)
                    .and_then(|k| e.screen_rects.get(&k).copied())
            });
            if let Some(rect) = icon {
                drop(st);
                ReleaseCapture();
                show_window_system_menu(hwnd, rect);
                invalidate(hwnd);
                return 0;
            }
            st.buttons_down |= 1;
            if let Some(t) = host::on_mouse_target(&engine, hit) {
                let dclick = st
                    .last_onmouse_down
                    .map_or(false, |i| i.elapsed().as_millis() < 400);
                let etype = if dclick { 0x1006 } else { 0x1001 };
                st.last_onmouse_down = if dclick { None } else { Some(Instant::now()) };
                let consumed = with_interp(|interp| {
                    let c = host::dispatch_on_mouse(
                        interp,
                        &engine,
                        t,
                        &raw_mouse(etype, (x, y), 1, (0.0, 0.0), mods),
                    );
                    host::drain_events(interp, &engine);
                    c
                })
                .unwrap_or(false);
                if consumed {
                    drop(st);
                    invalidate(hwnd);
                    return 0;
                }
            } else {
                st.last_onmouse_down = None;
            }
            host::set_active(&engine, hit);
            host::set_focus(&engine, hit);
            if let Some(t) = hit {
                let editable = {
                    let e = engine.borrow();
                    e.doc.arena.get(t).map_or(false, host::is_text_editable)
                };
                if editable {
                    let styles = host::cached_computed_styles(&engine);
                    let rect = engine.borrow().screen_rects.get(&t).copied();
                    if let Some(r) = rect {
                        let is_ta = {
                            let e = engine.borrow();
                            e.doc.arena.get(t).map_or(false, |n| n.tag == "textarea")
                        };
                        let pad = if is_ta { 5.0 } else { host::input_pad_left(styles.get(&t)) };
                        let (cy, wrap) = if is_ta {
                            (y - r.1 - pad, Some((r.2 - 2.0 * pad).max(8.0)))
                        } else {
                            (2.0, None)
                        };
                        let word = st
                            .last_edit_click
                            .map_or(false, |(i, k)| k == t && i.elapsed().as_millis() < 400);
                        st.last_edit_click = Some((Instant::now(), t));
                        let mode = if word {
                            host::CaretClick::Word
                        } else {
                            host::CaretClick::Place { extend: mods.2 }
                        };
                        host::caret_click(
                            &engine,
                            &mut st.ts,
                            t,
                            x - r.0 - pad,
                            cy,
                            wrap,
                            mode,
                            &styles,
                        );
                        st.text_drag = if word { None } else { Some(t) };
                    }
                } else {
                    st.last_edit_click = None;
                }
            }
            if let Some(target) = hit {
                with_interp(|interp| {
                    if let Err(e) = host::dispatch_dom_event(interp, &engine, "mousedown", target)
                    {
                        eprintln!("mousedown handler error: {}", e.0);
                    }
                });
            }
            drop(st);
            after_script(hwnd, &state);
            invalidate(hwnd);
            0
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            let (x, y) = lparam_xy(lparam);
            let mut pending_show: Option<i32> = None;
            let mut st = state.borrow_mut();
            st.cursor = (x, y);
            if let Some(k) = st.text_drag.take() {
                host::finish_text_drag(&engine, k);
                invalidate(hwnd);
            }
            if st.col_drag.take().is_some() {
                st.buttons_down &= !1;
                return 0;
            }
            let active_menu = engine.borrow().active_context_menu;
            if let Some(m) = active_menu {
                let hit = hit_at(&engine, x, y);
                let inside = hit.map_or(false, |h| {
                    let e = engine.borrow();
                    host::node_in_subtree(&e.doc, m, h)
                });
                if inside {
                    if let Some(target) = hit {
                        with_interp(|interp| {
                            host::dispatch_dom_event(interp, &engine, "click", target).ok();
                        });
                    }
                }
                host::close_context_menu(&engine);
                drop(st);
                after_script(hwnd, &state);
                invalidate(hwnd);
                return 0;
            }
            host::set_active(&engine, None);
            let hit = hit_at(&engine, x, y);
            st.buttons_down &= !1;
            with_interp(|interp| {
                host::dispatch_mouse_subscriptions(interp, &engine, 0x1002, hit, x, y);
            });
            if let Some(t) = host::on_mouse_target(&engine, hit) {
                let consumed = with_interp(|interp| {
                    let c = host::dispatch_on_mouse(
                        interp,
                        &engine,
                        t,
                        &raw_mouse(0x1002, (x, y), 1, (0.0, 0.0), mods),
                    );
                    host::drain_events(interp, &engine);
                    c
                })
                .unwrap_or(false);
                if consumed {
                    drop(st);
                    invalidate(hwnd);
                    return 0;
                }
            }
            if let Some(target) = hit {
                use crate::script::interp::SV;
                with_interp(|interp| {
                    host::dispatch_dom_event_with(
                        interp,
                        &engine,
                        "mouseup",
                        target,
                        &[
                            ("propButton".into(), SV::Bool(false)),
                            ("mainButton".into(), SV::Bool(true)),
                        ],
                    )
                    .ok();
                    if let Err(e) = host::dispatch_dom_event(interp, &engine, "click", target) {
                        eprintln!("click handler error: {}", e.0);
                    }
                });
                // Double-click detection is positional (time + the system
                // double-click rectangle), matching native Windows semantics.
                // Keying on the clicked node breaks whenever the first click's
                // handler re-renders its list: the second click lands on a
                // fresh NodeKey and a dblclick could never fire (the FT folder
                // rows). The event still targets whatever is under the cursor
                // NOW, so a re-rendered row receives it correctly.
                let now = Instant::now();
                let (slop_x, slop_y) = (
                    (GetSystemMetrics(SM_CXDOUBLECLK).max(4) as f32) / 2.0,
                    (GetSystemMetrics(SM_CYDOUBLECLK).max(4) as f32) / 2.0,
                );
                if st.last_click.map_or(false, |(t0, (px, py))| {
                    now.duration_since(t0).as_millis() < GetDoubleClickTime().max(1) as u128
                        && (x - px).abs() <= slop_x
                        && (y - py).abs() <= slop_y
                }) {
                    with_interp(|interp| {
                        host::dispatch_dom_event(interp, &engine, "dblclick", target).ok();
                    });
                    st.last_click = None;
                } else {
                    st.last_click = Some((now, (x, y)));
                }
                let role = {
                    let e = engine.borrow();
                    super::window::window_control_role(&e.doc, target)
                };
                match role {
                    Some("close") => host::request_view_close(),
                    Some("minimize") => pending_show = Some(SW_MINIMIZE),
                    Some("maximize") => {
                        pending_show = Some(if IsZoomed(hwnd) != 0 {
                            SW_SHOWNORMAL
                        } else {
                            SW_MAXIMIZE
                        });
                    }
                    _ => {}
                }
            }
            drop(st);
            if let Some(cmd) = pending_show {
                ShowWindow(hwnd, cmd);
            }
            after_script(hwnd, &state);
            invalidate(hwnd);
            0
        }
        WM_RBUTTONDOWN => {
            let (x, y) = lparam_xy(lparam);
            let mut st = state.borrow_mut();
            st.cursor = (x, y);
            st.buttons_down |= 2;
            let hit = hit_at(&engine, x, y);
            if let Some(t) = host::on_mouse_target(&engine, hit) {
                with_interp(|interp| {
                    host::dispatch_on_mouse(
                        interp,
                        &engine,
                        t,
                        &raw_mouse(0x1001, (x, y), 2, (0.0, 0.0), mods),
                    );
                    host::drain_events(interp, &engine);
                });
            }
            0
        }
        WM_RBUTTONUP => {
            let (x, y) = lparam_xy(lparam);
            let mut st = state.borrow_mut();
            st.cursor = (x, y);
            st.buttons_down &= !2;
            let hit = hit_at(&engine, x, y);
            let editable = hit.map_or(false, |t| {
                let e = engine.borrow();
                e.doc.arena.get(t).map_or(false, host::is_text_editable)
            });
            if editable {
                if let Some(t) = hit {
                    host::open_edit_menu(&engine, t, x, y);
                    drop(st);
                    invalidate(hwnd);
                    return 0;
                }
            }
            if host::close_edit_menu(&engine) {
                drop(st);
                invalidate(hwnd);
                return 0;
            }
            if let Some(t) = host::on_mouse_target(&engine, hit) {
                let consumed = with_interp(|interp| {
                    let c = host::dispatch_on_mouse(
                        interp,
                        &engine,
                        t,
                        &raw_mouse(0x1002, (x, y), 2, (0.0, 0.0), mods),
                    );
                    host::drain_events(interp, &engine);
                    c
                })
                .unwrap_or(false);
                if consumed {
                    drop(st);
                    invalidate(hwnd);
                    return 0;
                }
            }
            let mut handled = false;
            if let Some(t) = hit {
                use crate::script::interp::SV;
                handled = with_interp(|interp| {
                    if host::dispatch_dom_event_with(
                        interp,
                        &engine,
                        "mouseup",
                        t,
                        &[
                            ("propButton".into(), SV::Bool(true)),
                            ("mainButton".into(), SV::Bool(false)),
                        ],
                    )
                    .unwrap_or(false)
                    {
                        host::drain_events(interp, &engine);
                        return true;
                    }
                    if host::dispatch_dom_event(interp, &engine, "contextmenu", t)
                        .unwrap_or(false)
                    {
                        host::drain_events(interp, &engine);
                        return true;
                    }
                    false
                })
                .unwrap_or(false);
            }
            if !handled {
                let opened = hit.map_or(false, |hh| host::open_context_menu(&engine, hh, x, y));
                if !opened {
                    host::close_context_menu(&engine);
                }
            }
            drop(st);
            after_script(hwnd, &state);
            invalidate(hwnd);
            0
        }
        WM_MBUTTONDOWN | WM_MBUTTONUP => {
            let (x, y) = lparam_xy(lparam);
            let mut st = state.borrow_mut();
            st.cursor = (x, y);
            let (etype, add) = if msg == WM_MBUTTONDOWN {
                (0x1001, true)
            } else {
                (0x1002, false)
            };
            if add {
                st.buttons_down |= 4;
            } else {
                st.buttons_down &= !4;
            }
            let hit = hit_at(&engine, x, y);
            if let Some(t) = host::on_mouse_target(&engine, hit) {
                with_interp(|interp| {
                    host::dispatch_on_mouse(
                        interp,
                        &engine,
                        t,
                        &raw_mouse(etype, (x, y), 4, (0.0, 0.0), mods),
                    );
                    host::drain_events(interp, &engine);
                });
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = HIWORD(wparam as DWORD) as i16 as f64 / 120.0;
            let mut pt = POINT {
                x: LOWORD(lparam as DWORD) as i16 as i32,
                y: HIWORD(lparam as DWORD) as i16 as i32,
            };
            ScreenToClient(hwnd, &mut pt);
            let (x, y) = (pt.x as f32, pt.y as f32);
            {
                let mut st = state.borrow_mut();
                st.cursor = (x, y);
            }
            let hit = hit_at(&engine, x, y);
            let raw_consumed = host::on_mouse_target(&engine, hit)
                .and_then(|t| {
                    with_interp(|interp| {
                        let c = host::dispatch_on_mouse(
                            interp,
                            &engine,
                            t,
                            &raw_mouse(0x1007, (x, y), 0, (0.0, delta), mods),
                        );
                        host::drain_events(interp, &engine);
                        c
                    })
                })
                .unwrap_or(false);
            let mut dom_consumed = false;
            if !raw_consumed {
                if let Some(t) = hit {
                    use crate::script::interp::{sv_array, SV};
                    let extra: Vec<(String, SV)> = vec![(
                        "wheelDeltas".into(),
                        sv_array(vec![SV::Float(0.0), SV::Float(delta)]),
                    )];
                    dom_consumed = with_interp(|interp| {
                        let c = host::dispatch_dom_event_with(
                            interp, &engine, "mousewheel", t, &extra,
                        )
                        .unwrap_or(false);
                        if c {
                            host::drain_events(interp, &engine);
                        }
                        c
                    })
                    .unwrap_or(false);
                }
            }
            if !raw_consumed && !dom_consumed {
                let styles = host::cached_computed_styles(&engine);
                let screen = engine.borrow().screen_rects.clone();
                host::scroll_at(&engine, &screen, x, y, -(delta as f32) * host::scroll_step(), &styles);
                host::animate_scroll(&engine);
            }
            invalidate(hwnd);
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let code = wparam as i64;
            let consumed = with_interp(|interp| {
                let c = host::dispatch_on_key(interp, &engine, 0x2001, code, mods);
                if c {
                    host::drain_events(interp, &engine);
                }
                c
            })
            .unwrap_or(false);
            if consumed {
                invalidate(hwnd);
                return 0;
            }
            {
                use crate::script::interp::SV;
                let focus_target = {
                    let e = engine.borrow();
                    host::focused_element(&e).unwrap_or(e.doc.root)
                };
                let extra = vec![
                    ("keyCode".into(), SV::Int(code)),
                    ("altKey".into(), SV::Bool(mods.0)),
                    ("ctrlKey".into(), SV::Bool(mods.1)),
                    ("shiftKey".into(), SV::Bool(mods.2)),
                    ("commandKey".into(), SV::Bool(mods.3)),
                    ("metaKey".into(), SV::Bool(mods.3)),
                ];
                let dom_consumed = with_interp(|interp| {
                    let c = host::dispatch_dom_event_with(
                        interp,
                        &engine,
                        "keydown",
                        focus_target,
                        &extra,
                    )
                    .unwrap_or(false);
                    if c {
                        host::drain_events(interp, &engine);
                    }
                    c
                })
                .unwrap_or(false);
                if dom_consumed {
                    invalidate(hwnd);
                    return 0;
                }
            }
            let shift = mods.2;
            let ctrl = super::window::is_shortcut_chord(mods.0, mods.1, mods.3, false);
            let mut dirty = false;
            match wparam as i32 {
                VK_BACK => dirty = host::backspace(&engine),
                VK_DELETE => dirty = host::delete_forward(&engine),
                VK_LEFT => {
                    if host::move_caret(&engine, -1, false, false, shift) {
                        invalidate(hwnd);
                    }
                }
                VK_RIGHT => {
                    if host::move_caret(&engine, 1, false, false, shift) {
                        invalidate(hwnd);
                    }
                }
                VK_HOME => {
                    if host::move_caret(&engine, 0, true, false, shift) {
                        invalidate(hwnd);
                    }
                }
                VK_END => {
                    if host::move_caret(&engine, 0, false, true, shift) {
                        invalidate(hwnd);
                    }
                }
                0x41 if ctrl => {
                    if host::select_all(&engine) {
                        invalidate(hwnd);
                    }
                }
                0x43 if ctrl => {
                    let sel = {
                        let e = engine.borrow();
                        host::selected_text(&e)
                    };
                    if let Some(text) = sel {
                        super::platform::clipboard_set_text(&text);
                    }
                }
                0x58 if ctrl => {
                    let sel = {
                        let e = engine.borrow();
                        host::selected_text(&e)
                    };
                    if let Some(text) = sel {
                        super::platform::clipboard_set_text(&text);
                        dirty = host::delete_forward(&engine);
                    }
                }
                0x56 if ctrl => {
                    if let Some(text) = super::platform::clipboard_get_text() {
                        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
                        if !clean.is_empty() {
                            dirty = host::insert_text(&engine, &clean);
                        }
                    }
                }
                _ => {}
            }
            if dirty {
                fire_change_on_focused(&engine);
                invalidate(hwnd);
            }
            after_script(hwnd, &state);
            if msg == WM_SYSKEYDOWN {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            } else {
                0
            }
        }
        WM_KEYUP | WM_SYSKEYUP => {
            let code = wparam as i64;
            with_interp(|interp| {
                if host::dispatch_on_key(interp, &engine, 0x2002, code, mods) {
                    host::drain_events(interp, &engine);
                }
                use crate::script::interp::SV;
                let focus_target = {
                    let e = engine.borrow();
                    host::focused_element(&e).unwrap_or(e.doc.root)
                };
                host::dispatch_dom_event_with(
                    interp,
                    &engine,
                    "keyup",
                    focus_target,
                    &[
                        ("keyCode".into(), SV::Int(code)),
                        ("altKey".into(), SV::Bool(mods.0)),
                        ("ctrlKey".into(), SV::Bool(mods.1)),
                        ("shiftKey".into(), SV::Bool(mods.2)),
                        ("commandKey".into(), SV::Bool(mods.3)),
                        ("metaKey".into(), SV::Bool(mods.3)),
                    ],
                )
                .ok();
            });
            after_script(hwnd, &state);
            if msg == WM_SYSKEYUP {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            } else {
                0
            }
        }
        WM_CHAR => {
            if let Some(ch) = char_from_utf16_unit(wparam as u16) {
                if !ch.is_control()
                    && !super::window::modifiers_suppress_text(mods.0, mods.1, false)
                {
                    let consumed = with_interp(|interp| {
                        if host::dispatch_on_key(interp, &engine, 0x2003, ch as i64, mods) {
                            host::drain_events(interp, &engine);
                            return true;
                        }
                        use crate::script::interp::SV;
                        let focus_target = {
                            let e = engine.borrow();
                            host::focused_element(&e).unwrap_or(e.doc.root)
                        };
                        let c = host::dispatch_dom_event_with(
                            interp,
                            &engine,
                            "keypress",
                            focus_target,
                            &[
                                ("keyCode".into(), SV::Int(ch as i64)),
                                ("altKey".into(), SV::Bool(mods.0)),
                                ("ctrlKey".into(), SV::Bool(mods.1)),
                                ("shiftKey".into(), SV::Bool(mods.2)),
                            ],
                        )
                        .unwrap_or(false);
                        if c {
                            host::drain_events(interp, &engine);
                        }
                        c
                    })
                    .unwrap_or(false);
                    if !consumed && host::type_char(&engine, ch) {
                        fire_change_on_focused(&engine);
                    }
                    invalidate(hwnd);
                }
            }
            after_script(hwnd, &state);
            0
        }
        WM_TIMER => {
            if wparam == TICK_TIMER_ID {
                {
                    let mut st = state.borrow_mut();
                    engine_tick(hwnd, &mut st);
                }
                after_script(hwnd, &state);
                0
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_APP_WAKE => {
            let epoch_before = super::dom::layout_epoch();
            with_interp(|interp| {
                host::drain_offthread_calls(interp, &engine);
                host::drain_events(interp, &engine);
            });
            host::pump_video_events(&engine);
            after_script(hwnd, &state);
            // Idle wakeups (signal traffic, status polls) repainted the full
            // window per message on the CPU raster; only a document change, an
            // explicit repaint request, or a live video sink needs a paint.
            let needs_paint = super::dom::layout_epoch() != epoch_before
                || std::mem::take(&mut engine.borrow_mut().repaint_requested)
                || !engine.borrow().video_sinks.is_empty();
            if needs_paint {
                invalidate(hwnd);
            }
            0
        }
        WM_CLOSE => {
            with_interp(|interp| {
                host::run_window_closing(interp, &engine);
            });
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, TICK_TIMER_ID);
            REGISTRY.with(|r| r.borrow_mut().remove(&(hwnd as usize)));
            if MAIN_HWND.with(|m| m.get()) == hwnd as usize {
                PostQuitMessage(0);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn register_class() -> Vec<u16> {
    let name = to_wide("WireuiXPWindow");
    unsafe {
        let hinst = winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null());
        let icon = LoadIconW(hinst, 1 as *const u16);
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: if icon.is_null() {
                LoadIconW(std::ptr::null_mut(), IDI_APPLICATION)
            } else {
                icon
            },
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: name.as_ptr(),
        };
        RegisterClassW(&wc);
    }
    name
}

fn register_xp_system_fonts(ts: &mut TextSystem) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let fonts = [
        ("Tahoma", "tahoma.ttf"),
        ("Tahoma", "tahomabd.ttf"),
        ("Verdana", "verdana.ttf"),
        ("Verdana", "verdanab.ttf"),
        ("Arial", "arial.ttf"),
        ("Arial", "arialbd.ttf"),
        ("Courier New", "cour.ttf"),
        ("Courier New", "courbd.ttf"),
        ("Times New Roman", "times.ttf"),
    ];
    for (family, file) in fonts {
        let path = std::path::Path::new(&windir).join("Fonts").join(file);
        if let Ok(data) = std::fs::read(&path) {
            ts.register_font(family, data);
        }
    }
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
    super::window::boot_crumb("win32 run_window_source: enter");
    unsafe {
        let sw = GetSystemMetrics(SM_CXSCREEN) as f64;
        let sh = GetSystemMetrics(SM_CYSCREEN) as f64;
        host::set_monitor_rect((0.0, 0.0, sw, sh));
        let mut wa: RECT = std::mem::zeroed();
        if SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0) != 0 {
            host::set_workarea_rect((
                wa.left as f64,
                wa.top as f64,
                (wa.right - wa.left) as f64,
                (wa.bottom - wa.top) as f64,
            ));
        }
    }
    let loaded = match source {
        PageSource::Path { page, base } => {
            host::load_page_bridged_with_behaviors(&page, base, platform, handler, behaviors)?
        }
        PageSource::Archive { archive, page } => {
            host::load_page_archive_with_behaviors(archive, &page, platform, handler, behaviors)?
        }
        PageSource::Memory {
            html,
            base: _,
            archive,
        } => host::load_page_memory_with_behaviors(
            &html,
            archive.into_iter().collect(),
            None,
            platform,
            handler,
            behaviors,
        )?,
    };
    let host::LoadedPage {
        engine, mut interp, ..
    } = loaded;
    super::window::boot_crumb("win32 run_window_source: page loaded");
    if let Some(code) = eval {
        interp
            .run_source(code)
            .map_err(|e| format!("eval: {}", e.0))?;
        host::drain_events(&mut interp, &engine);
    }

    let mut ts = TextSystem::new_with(false);
    {
        let e = engine.borrow();
        for sheet in &e.sheets {
            for (family, data) in &sheet.font_faces {
                ts.register_font(family, data.clone());
            }
        }
    }
    register_xp_system_fonts(&mut ts);
    super::window::boot_crumb("win32 run_window_source: fonts registered");

    let initial_move = host::take_view_move();
    let (mut w, mut h) = (size.0 as i32, size.1 as i32);
    if let Some((_, _, mw, mh)) = initial_move {
        if mw > 0.0 && mh > 0.0 {
            w = mw as i32;
            h = mh as i32;
        }
    }
    let title_w = to_wide(title);
    let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    unsafe {
        AdjustWindowRect(&mut rc, style, 0);
    }
    let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
    let pending = PENDING_HWND.with(|c| c.replace(0));
    let hwnd = if pending != 0 {
        let hwnd = pending as HWND;
        unsafe {
            SetWindowTextW(hwnd, title_w.as_ptr());
            match initial_move {
                Some((mx, my, _, _)) => {
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        mx as i32,
                        my as i32,
                        ww,
                        wh,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                None => {
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        0,
                        0,
                        ww,
                        wh,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE,
                    );
                }
            }
        }
        hwnd
    } else {
        let class = register_class();
        let (x, y) = match initial_move {
            Some((mx, my, _, _)) => (mx as i32, my as i32),
            None => (CW_USEDEFAULT, CW_USEDEFAULT),
        };
        unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title_w.as_ptr(),
                style,
                x,
                y,
                ww,
                wh,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null()),
                std::ptr::null_mut(),
            )
        }
    };
    if hwnd.is_null() {
        return Err("CreateWindowExW failed".into());
    }
    super::window::boot_crumb("win32 run_window_source: window created");

    {
        let hwnd_u = hwnd as usize;
        let waker: crate::video::FrameWaker = std::sync::Arc::new(move || unsafe {
            PostMessageW(hwnd_u as HWND, WM_APP_WAKE, 0, 0);
        });
        host::install_video_wake(&engine, waker);
    }
    {
        let hwnd_u = hwnd as usize;
        let waker: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || unsafe {
            PostMessageW(hwnd_u as HWND, WM_APP_WAKE, 0, 0);
        });
        host::set_offthread_waker(waker);
    }

    host::set_current_engine(&engine);
    host::set_current_interp(&mut interp as *mut _);
    host::set_current_window_hwnd(hwnd as usize);
    MAIN_HWND.with(|m| m.set(hwnd as usize));

    host::pump_timers(&mut interp, &engine, 5.0).ok();
    host::focus_first_input(&engine);

    let state = Rc::new(RefCell::new(WinState {
        engine: engine.clone(),
        ts,
        layout_cache: None,
        last_rects: HashMap::new(),
        bgra: Vec::new(),
        cursor: (0.0, 0.0),
        buttons_down: 0,
        onmouse_over: None,
        last_onmouse_down: None,
        last_click: None,
        last_edit_click: None,
        text_drag: None,
        col_drag: None,
        hover_title: None,
        tracking_leave: false,
        last_window_state: 1,
        pending_statechange: 0,
        min_size: None,
        max_size: None,
        saved_rect: None,
        start: Instant::now(),
        last_input: Instant::now(),
        caret_solid: false,
        caret_blink_phase: -1,
    }));
    host::set_scroll_ease_default(1.0);
    REGISTRY.with(|r| r.borrow_mut().insert(hwnd as usize, state.clone()));

    // Layout-only refresh for script geometry reads (element.box) that land
    // between a DOM rebuild and the next paint.
    {
        let hook_state = state.clone();
        let hook_hwnd = hwnd as usize;
        host::set_layout_hook(Box::new(move |target| {
            let Ok(mut st) = hook_state.try_borrow_mut() else {
                return;
            };
            if !Rc::ptr_eq(target, &st.engine) {
                return;
            }
            let engine = st.engine.clone();
            let (w, h) = client_size(hook_hwnd as HWND);
            let epoch = super::dom::layout_epoch();
            let stale = match &st.layout_cache {
                Some((ep, cw, ch, _)) => *ep != epoch || *cw != w || *ch != h,
                None => true,
            };
            let styles = host::cached_computed_styles(&engine);
            let styles = styles.as_ref();
            if stale {
                let e = engine.borrow();
                let layout = super::layout::layout_document(
                    &e.doc,
                    &styles,
                    &mut st.ts,
                    (w as f32, h as f32),
                    1.0,
                );
                drop(e);
                st.layout_cache = Some((epoch, w, h, layout));
            }
            let rects = match &st.layout_cache {
                Some((_, _, _, l)) => l.rects.clone(),
                None => return,
            };
            let (screen, order) = host::compute_screen_geometry(&engine, &rects, &styles);
            host::update_layout_rects(&engine, &rects);
            let mut em = engine.borrow_mut();
            em.screen_rects = screen;
            em.screen_order = order;
        }));
    }

    {
        let mut st = state.borrow_mut();
        paint_now(hwnd, &mut st);
    }
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetTimer(hwnd, TICK_TIMER_ID, TICK_MS, None);
    }
    update_os_metrics(hwnd);
    after_script(hwnd, &state);
    super::window::boot_crumb("win32 run_window_source: entering pump");

    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if r == 0 || r == -1 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if let Some(st) = state_for(hwnd) {
                host::set_current_engine(&engine);
                after_script(hwnd, &st);
            } else {
                break;
            }
        }
    }
    host::set_current_interp(std::ptr::null_mut());
    host::clear_window_hooks();
    super::window::boot_crumb("win32 run_window_source: pump exited");
    Ok(())
}
