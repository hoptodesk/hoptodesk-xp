
use std::ffi::c_void;

extern "C" {
    fn vpx_helper_create(width: i32, height: i32, err_code: *mut i32) -> *mut c_void;
    fn vpx_helper_encode(
        enc: *mut c_void,
        yuv_data: *mut u8,
        yuv_len: i32,
        force_keyframe: i32,
        out_data: *mut *const u8,
        out_sizes: *mut i32,
        out_key: *mut i32,
        out_pts: *mut i64,
        max_out: i32,
    ) -> i32;
    fn vpx_helper_destroy(enc: *mut c_void);

    fn vpx_helper_dec_create(err_code: *mut i32) -> *mut c_void;
    fn vpx_helper_dec_decode(
        dec: *mut c_void,
        data: *const u8,
        data_len: i32,
        out_width: *mut i32,
        out_height: *mut i32,
        out_stride: *mut i32,
    ) -> *const u8;
    fn vpx_helper_dec_destroy(dec: *mut c_void);
}

pub struct Vp8Encoder {
    handle: *mut c_void,
    pub width: u32,
    pub height: u32,
}

impl Vp8Encoder {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        unsafe {
            let mut err: i32 = 0;
            let handle = vpx_helper_create(width as i32, height as i32, &mut err);
            if handle.is_null() {
                return Err(format!("vpx_helper_create failed: err={}", err));
            }
            Ok(Vp8Encoder { handle, width, height })
        }
    }

    pub fn encode(&mut self, yuv_data: &mut [u8], force_keyframe: bool) -> Result<Vec<EncodedFrame>, String> {
        unsafe {
            let mut out_data: [*const u8; 8] = [std::ptr::null(); 8];
            let mut out_sizes: [i32; 8] = [0; 8];
            let mut out_key: [i32; 8] = [0; 8];
            let mut out_pts: [i64; 8] = [0; 8];

            let count = vpx_helper_encode(
                self.handle,
                yuv_data.as_mut_ptr(),
                yuv_data.len() as i32,
                if force_keyframe { 1 } else { 0 },
                out_data.as_mut_ptr(),
                out_sizes.as_mut_ptr(),
                out_key.as_mut_ptr(),
                out_pts.as_mut_ptr(),
                8,
            );

            if count < 0 {
                return Err(format!("vpx_helper_encode failed: {}", count));
            }

            let mut frames = Vec::new();
            for i in 0..count as usize {
                let data = std::slice::from_raw_parts(out_data[i], out_sizes[i] as usize);
                frames.push(EncodedFrame {
                    data: data.to_vec(),
                    key: out_key[i] != 0,
                    pts: out_pts[i],
                });
            }
            Ok(frames)
        }
    }
}

impl Drop for Vp8Encoder {
    fn drop(&mut self) {
        unsafe { vpx_helper_destroy(self.handle); }
    }
}

pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub key: bool,
    pub pts: i64,
}

pub struct Vp8Decoder {
    handle: *mut c_void,
}

impl Vp8Decoder {
    pub fn new() -> Result<Self, String> {
        unsafe {
            let mut err: i32 = 0;
            let handle = vpx_helper_dec_create(&mut err);
            if handle.is_null() {
                return Err(format!("vpx_helper_dec_create failed: err={}", err));
            }
            Ok(Vp8Decoder { handle })
        }
    }

    pub fn decode(&mut self, data: &[u8]) -> Result<(Vec<u8>, i32, i32), String> {
        unsafe {
            let mut w: i32 = 0;
            let mut h: i32 = 0;
            let mut stride: i32 = 0;
            let ptr = vpx_helper_dec_decode(
                self.handle,
                data.as_ptr(),
                data.len() as i32,
                &mut w,
                &mut h,
                &mut stride,
            );
            if ptr.is_null() {
                return Err("vpx_helper_dec_decode failed".into());
            }
            let size = (h * stride) as usize;
            let bgra = std::slice::from_raw_parts(ptr, size).to_vec();
            Ok((bgra, w, h))
        }
    }
}

impl Drop for Vp8Decoder {
    fn drop(&mut self) {
        unsafe { vpx_helper_dec_destroy(self.handle); }
    }
}

pub fn bgra_to_i420(bgra: &[u8], width: usize, height: usize, yuv: &mut Vec<u8>) {
    let uv_w = width / 2;
    let uv_h = height / 2;
    let y_size = width * height;
    let uv_size = uv_w * uv_h;
    yuv.resize(y_size + uv_size * 2, 0);

    let stride = width * 4;

    let (y_plane, uv_planes) = yuv.split_at_mut(y_size);
    let (u_plane, v_plane) = uv_planes.split_at_mut(uv_size);

    for row in 0..height {
        let src_offset = row * stride;

        for col in 0..width {
            let px = src_offset + col * 4;
            let b = bgra[px] as i32;
            let g = bgra[px + 1] as i32;
            let r = bgra[px + 2] as i32;

            let y = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            y_plane[row * width + col] = y.clamp(0, 255) as u8;

            if row % 2 == 0 && col % 2 == 0 && row / 2 < uv_h && col / 2 < uv_w {
                let uv_idx = (row / 2) * uv_w + (col / 2);
                let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
                let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
                u_plane[uv_idx] = u.clamp(0, 255) as u8;
                v_plane[uv_idx] = v.clamp(0, 255) as u8;
            }
        }
    }
}
