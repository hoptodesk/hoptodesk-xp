
use std::mem::size_of;
use winapi::um::winuser::{
    SendInput, INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, VK_APPS,
    VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END, VK_HOME, VK_INSERT, VK_LEFT, VK_LWIN, VK_NEXT,
    VK_PRIOR, VK_RCONTROL, VK_RIGHT, VK_RMENU, VK_RWIN, VK_SNAPSHOT, VK_UP,
};

fn is_extended_vk(vk: u16) -> bool {
    matches!(
        vk as i32,
        VK_INSERT
            | VK_DELETE
            | VK_HOME
            | VK_END
            | VK_PRIOR
            | VK_NEXT
            | VK_LEFT
            | VK_RIGHT
            | VK_UP
            | VK_DOWN
            | VK_DIVIDE
            | VK_SNAPSHOT
            | VK_RCONTROL
            | VK_RMENU
            | VK_LWIN
            | VK_RWIN
            | VK_APPS
    )
}

fn scan_with_flags(scancode: u16) -> (u16, u32) {
    if scancode & 0xFF00 == 0xE000 {
        (scancode & 0x00FF, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY)
    } else {
        (scancode, KEYEVENTF_SCANCODE)
    }
}

fn vk_flags(vk: u16) -> u32 {
    if is_extended_vk(vk) {
        KEYEVENTF_EXTENDEDKEY
    } else {
        0
    }
}

const KEYEVENTF_UNICODE: u32 = 0x0004;
const MOUSEEVENTF_XDOWN: u32 = 0x0080;
const MOUSEEVENTF_XUP: u32 = 0x0100;
const MOUSEEVENTF_HWHEEL: u32 = 0x01000;
const XBUTTON1: i32 = 0x0001;
const XBUTTON2: i32 = 0x0002;

thread_local! {
    static HELD_VKS: std::cell::RefCell<std::collections::HashSet<u16>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
    static HELD_SCANCODES: std::cell::RefCell<std::collections::HashSet<u16>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

pub fn release_all_held_keys() {
    let vks: Vec<u16> = HELD_VKS.with(|h| h.borrow().iter().copied().collect());
    for vk in vks {
        let mut input: INPUT = unsafe { std::mem::zeroed() };
        input.type_ = INPUT_KEYBOARD;
        unsafe {
            let ki = input.u.ki_mut();
            ki.wVk = vk;
            ki.dwFlags = vk_flags(vk) | KEYEVENTF_KEYUP;
        }
        unsafe {
            SendInput(1, &mut input, size_of::<INPUT>() as i32);
        }
    }
    HELD_VKS.with(|h| h.borrow_mut().clear());

    let scancodes: Vec<u16> = HELD_SCANCODES.with(|h| h.borrow().iter().copied().collect());
    for scancode in scancodes {
        let mut input: INPUT = unsafe { std::mem::zeroed() };
        input.type_ = INPUT_KEYBOARD;
        unsafe {
            let ki = input.u.ki_mut();
            let (scan, flags) = scan_with_flags(scancode);
            ki.wScan = scan;
            ki.dwFlags = flags | KEYEVENTF_KEYUP;
        }
        unsafe {
            SendInput(1, &mut input, size_of::<INPUT>() as i32);
        }
    }
    HELD_SCANCODES.with(|h| h.borrow_mut().clear());
}

#[link(name = "user32")]
extern "system" {
    fn VkKeyScanW(ch: u16) -> i16;
    fn MapVirtualKeyW(code: u32, map_type: u32) -> u32;
}

const MAPVK_VK_TO_VSC: u32 = 0;

pub fn mouse_move_to(x: i32, y: i32) {

    let (sw, sh) = get_screen_size();
    if sw == 0 || sh == 0 {
        return;
    }
    let abs_x = ((x as i64 * 65536) / sw as i64) as i32;
    let abs_y = ((y as i64 * 65536) / sh as i64) as i32;

    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.type_ = INPUT_MOUSE;
    unsafe {
        let mi = input.u.mi_mut();
        mi.dx = abs_x;
        mi.dy = abs_y;
        mi.dwFlags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE;
    }

    unsafe {
        SendInput(1, &mut input, size_of::<INPUT>() as i32);
    }
}

#[link(name = "user32")]
extern "system" {
    fn GetSystemMetrics(index: i32) -> i32;
}

const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;

fn get_screen_size() -> (i32, i32) {
    unsafe {
        (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

pub fn mouse_down(button: MouseButton) {
    let (flag, data) = match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, 0),
        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, 0),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, 0),
        MouseButton::Back => (MOUSEEVENTF_XDOWN, XBUTTON1),
        MouseButton::Forward => (MOUSEEVENTF_XDOWN, XBUTTON2),
    };
    send_mouse_event(flag, data);
}

pub fn mouse_up(button: MouseButton) {
    let (flag, data) = match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTUP, 0),
        MouseButton::Right => (MOUSEEVENTF_RIGHTUP, 0),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEUP, 0),
        MouseButton::Back => (MOUSEEVENTF_XUP, XBUTTON1),
        MouseButton::Forward => (MOUSEEVENTF_XUP, XBUTTON2),
    };
    send_mouse_event(flag, data);
}

pub fn mouse_scroll(delta: i32) {
    send_mouse_event(MOUSEEVENTF_WHEEL, delta);
}

pub fn mouse_hscroll(delta: i32) {
    send_mouse_event(MOUSEEVENTF_HWHEEL, delta);
}

fn send_mouse_event(flags: u32, data: i32) {
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.type_ = INPUT_MOUSE;
    unsafe {
        let mi = input.u.mi_mut();
        mi.dwFlags = flags;
        mi.mouseData = data as u32;
    }
    unsafe {
        SendInput(1, &mut input, size_of::<INPUT>() as i32);
    }
}

pub fn key_event(scancode: u16, down: bool) {
    HELD_SCANCODES.with(|h| {
        let mut set = h.borrow_mut();
        if down {
            set.insert(scancode);
        } else {
            set.remove(&scancode);
        }
    });
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.type_ = INPUT_KEYBOARD;
    unsafe {
        let ki = input.u.ki_mut();
        let (scan, flags) = scan_with_flags(scancode);
        ki.wScan = scan;
        ki.dwFlags = flags | if down { 0 } else { KEYEVENTF_KEYUP };
    }
    unsafe {
        SendInput(1, &mut input, size_of::<INPUT>() as i32);
    }
}

pub fn vk_event(vk: u16, down: bool) {
    HELD_VKS.with(|h| {
        let mut set = h.borrow_mut();
        if down {
            set.insert(vk);
        } else {
            set.remove(&vk);
        }
    });
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.type_ = INPUT_KEYBOARD;
    unsafe {
        let ki = input.u.ki_mut();
        ki.wVk = vk;
        ki.dwFlags = vk_flags(vk) | if down { 0 } else { KEYEVENTF_KEYUP };
    }
    unsafe {
        SendInput(1, &mut input, size_of::<INPUT>() as i32);
    }
}

pub fn unicode_char_down(ch: u16) {
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.type_ = INPUT_KEYBOARD;
    unsafe {
        let ki = input.u.ki_mut();
        ki.wScan = ch;
        ki.dwFlags = KEYEVENTF_UNICODE;
    }
    unsafe {
        SendInput(1, &mut input, size_of::<INPUT>() as i32);
    }
}

pub fn unicode_char_up(ch: u16) {
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.type_ = INPUT_KEYBOARD;
    unsafe {
        let ki = input.u.ki_mut();
        ki.wScan = ch;
        ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
    }
    unsafe {
        SendInput(1, &mut input, size_of::<INPUT>() as i32);
    }
}

pub fn unicode_sequence(text: &str) {
    for ch in text.encode_utf16() {
        unicode_char_down(ch);
        unicode_char_up(ch);
    }
}

pub fn char_to_vk(ch: char) -> Option<(u16, bool)> {
    let result = unsafe { VkKeyScanW(ch as u16) };
    if result == -1 {
        return None;
    }
    let vk = (result & 0xFF) as u16;
    let shift = (result >> 8) & 0x01 != 0;
    Some((vk, shift))
}

pub fn vk_to_scancode_event(vk: u16, down: bool) {
    if is_extended_vk(vk) {
        vk_event(vk, down);
        return;
    }
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
    if scan != 0 {
        key_event(scan, down);
    } else {
        vk_event(vk, down);
    }
}

pub fn legacy_char_event(chr: u32, down: bool, has_modifiers: bool) {
    if let Ok(c) = char::try_from(chr) {
        if has_modifiers {

            if let Some((vk, _shift)) = char_to_vk(c) {
                vk_event(vk, down);
            }
        } else if down {

            unicode_char_down(c as u16);
        } else {
            unicode_char_up(c as u16);
        }
    }
}

pub fn translate_chr_event(code: u32, down: bool) {
    let vk_code = (code >> 16) as u16;
    let scancode = (code & 0xFFFF) as u16;
    if vk_code != 0 {

        vk_event(vk_code, down);
    } else if scancode != 0 {

        key_event(scancode, down);
    }
}

pub fn win2win_hotkey_event(code: u32, down: bool) {
    let unicode = (code & 0x0000FFFF) as u16;
    if down && unicode != 0 {

        unicode_char_down(unicode);
        unicode_char_up(unicode);
        return;
    }
    let vk = ((code >> 16) & 0x0000FFFF) as u16;
    if vk != 0 {
        vk_to_scancode_event(vk, down);
    }
}
