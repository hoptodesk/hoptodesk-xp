
use std::mem::size_of;
use winapi::{
    shared::windef::{HBITMAP, HDC},
    um::wingdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDCW, DeleteDC, DeleteObject,
        GetDIBits, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
        DIB_RGB_COLORS, HGDI_ERROR, RGBQUAD, SRCCOPY,
    },
};

const PIXEL_WIDTH: i32 = 4;

pub struct CapturerGDI {
    screen_dc: HDC,
    dc: HDC,
    bmp: HBITMAP,
    width: i32,
    height: i32,
}

impl CapturerGDI {
    pub fn new(name: &[u16], width: i32, height: i32) -> Result<Self, Box<dyn std::error::Error>> {
        unsafe {
            if name.is_empty() {
                return Err("Empty display name".into());
            }
            let screen_dc = CreateDCW(&name[0], 0 as _, 0 as _, 0 as _);
            if screen_dc.is_null() {
                return Err("Failed to create dc from monitor name".into());
            }

            let dc = CreateCompatibleDC(screen_dc);
            if dc.is_null() {
                DeleteDC(screen_dc);
                return Err("Can't get a Windows display".into());
            }

            let bmp = CreateCompatibleBitmap(screen_dc, width, height);
            if bmp.is_null() {
                DeleteDC(screen_dc);
                DeleteDC(dc);
                return Err("Can't create a Windows buffer".into());
            }

            let res = SelectObject(dc, bmp as _);
            if res.is_null() || res == HGDI_ERROR {
                DeleteDC(screen_dc);
                DeleteDC(dc);
                DeleteObject(bmp as _);
                return Err("Can't select Windows buffer".into());
            }
            Ok(Self {
                screen_dc,
                dc,
                bmp,
                width,
                height,
            })
        }
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn frame(&self, data: &mut Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
        unsafe {
            let res = BitBlt(
                self.dc,
                0,
                0,
                self.width,
                self.height,
                self.screen_dc,
                0,
                0,
                SRCCOPY | CAPTUREBLT,
            );
            if res == 0 {
                return Err("Failed to copy screen to Windows buffer".into());
            }

            let stride = self.width * PIXEL_WIDTH;
            let size: usize = (stride * self.height) as usize;
            data.resize(size, 0);

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as _,
                    biWidth: self.width as _,

                    biHeight: -(self.height as i32),
                    biPlanes: 1,
                    biBitCount: (8 * PIXEL_WIDTH) as _,
                    biCompression: BI_RGB,
                    biSizeImage: size as _,
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

            let res = GetDIBits(
                self.dc,
                self.bmp,
                0,
                self.height as _,
                data.as_mut_ptr() as _,
                &mut bmi as _,
                DIB_RGB_COLORS,
            );
            if res == 0 {
                return Err("GetDIBits failed".into());
            }

            Ok(())
        }
    }
}

impl Drop for CapturerGDI {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.screen_dc);
            DeleteDC(self.dc);
            DeleteObject(self.bmp as _);
        }
    }
}

pub struct DisplayInfo {
    pub name: Vec<u16>,
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
}

pub fn enumerate_displays() -> Vec<DisplayInfo> {
    use winapi::um::winuser::{EnumDisplayDevicesW, EnumDisplaySettingsW, ENUM_CURRENT_SETTINGS};
    use winapi::um::wingdi::DISPLAY_DEVICEW;

    let mut displays = Vec::new();
    let mut device: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
    device.cb = size_of::<DISPLAY_DEVICEW>() as u32;

    let mut i: u32 = 0;
    loop {
        let ret = unsafe { EnumDisplayDevicesW(std::ptr::null(), i, &mut device, 0) };
        if ret == 0 {
            break;
        }

        const DISPLAY_DEVICE_ACTIVE: u32 = 0x00000001;
        if device.StateFlags & DISPLAY_DEVICE_ACTIVE != 0 {
            let mut devmode: winapi::um::wingdi::DEVMODEW = unsafe { std::mem::zeroed() };
            devmode.dmSize = size_of::<winapi::um::wingdi::DEVMODEW>() as u16;

            let ret = unsafe {
                EnumDisplaySettingsW(device.DeviceName.as_ptr(), ENUM_CURRENT_SETTINGS, &mut devmode)
            };
            if ret != 0 {

                let name_len = device.DeviceName.iter().position(|&c| c == 0).unwrap_or(device.DeviceName.len());
                let mut name: Vec<u16> = device.DeviceName[..name_len].to_vec();
                name.push(0);

                displays.push(DisplayInfo {
                    name,
                    width: devmode.dmPelsWidth as i32,
                    height: devmode.dmPelsHeight as i32,
                    x: unsafe { devmode.u1.s2().dmPosition.x },
                    y: unsafe { devmode.u1.s2().dmPosition.y },
                });
            }
        }

        i += 1;
    }

    displays
}
