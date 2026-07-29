// VP8 encoder + decoder helper — compiled by cc crate to avoid Rust FFI struct layout issues
#include <vpx/vpx_encoder.h>
#include <vpx/vpx_decoder.h>
#include <vpx/vp8cx.h>
#include <vpx/vp8dx.h>
#include <vpx/vpx_image.h>
#include <string.h>
#include <stdlib.h>

// Opaque encoder handle
typedef struct {
    vpx_codec_ctx_t codec;
    vpx_image_t img;
    vpx_codec_enc_cfg_t cfg;
    int width;
    int height;
    int64_t frame_count;
} vpx_encoder_t;

// Create encoder. Returns NULL on failure, sets *err_code.
vpx_encoder_t* vpx_helper_create(int width, int height, int *err_code) {
    vpx_encoder_t *enc = (vpx_encoder_t*)calloc(1, sizeof(vpx_encoder_t));
    if (!enc) { *err_code = -1; return NULL; }

    vpx_codec_iface_t *iface = vpx_codec_vp8_cx();
    vpx_codec_enc_cfg_t cfg;
    vpx_codec_err_t res = vpx_codec_enc_config_default(iface, &cfg, 0);
    if (res != VPX_CODEC_OK) {
        *err_code = (int)res;
        free(enc);
        return NULL;
    }

    cfg.g_w = width;
    cfg.g_h = height;
    cfg.g_threads = 1;
    cfg.g_timebase.num = 1;
    cfg.g_timebase.den = 1000;
    cfg.rc_target_bitrate = width * height / 100;
    cfg.g_error_resilient = VPX_ERROR_RESILIENT_DEFAULT;
    cfg.kf_max_dist = 999999;
    cfg.g_lag_in_frames = 0;
    cfg.rc_end_usage = VPX_CBR;
    cfg.rc_min_quantizer = 4;
    cfg.rc_max_quantizer = 56;
    cfg.rc_buf_sz = 1000;
    cfg.rc_buf_initial_sz = 500;
    cfg.rc_buf_optimal_sz = 600;

    res = vpx_codec_enc_init(&enc->codec, iface, &cfg, 0);
    if (res != VPX_CODEC_OK) {
        *err_code = (int)res;
        free(enc);
        return NULL;
    }

    vpx_codec_control(&enc->codec, VP8E_SET_CPUUSED, 8);

    enc->cfg = cfg;
    enc->width = width;
    enc->height = height;
    enc->frame_count = 0;
    *err_code = 0;
    return enc;
}

int vpx_helper_set_bitrate(vpx_encoder_t *enc, int bitrate_kbps) {
    if (!enc || bitrate_kbps <= 0) return -1;
    enc->cfg.rc_target_bitrate = bitrate_kbps;
    return (int)vpx_codec_enc_config_set(&enc->codec, &enc->cfg);
}

// Encode one I420 frame. Returns number of output packets, or negative on error.
// Output data/size/key/pts are written to the out_ arrays (caller provides space for at least 8).
int vpx_helper_encode(vpx_encoder_t *enc, unsigned char *yuv_data, int yuv_len,
                      int force_keyframe,
                      const unsigned char **out_data, int *out_sizes,
                      int *out_key, int64_t *out_pts, int max_out) {
    if (!enc || !yuv_data) return -1;

    vpx_image_t *img = vpx_img_wrap(&enc->img, VPX_IMG_FMT_I420,
                                     enc->width, enc->height, 1, yuv_data);
    if (!img) return -2;

    vpx_enc_frame_flags_t flags = force_keyframe ? VPX_EFLAG_FORCE_KF : 0;
    vpx_codec_err_t res = vpx_codec_encode(&enc->codec, img,
                                            enc->frame_count, 1, flags, VPX_DL_REALTIME);
    enc->frame_count++;
    if (res != VPX_CODEC_OK) return -(int)res - 100;

    // Collect output packets
    int count = 0;
    vpx_codec_iter_t iter = NULL;
    const vpx_codec_cx_pkt_t *pkt;
    while ((pkt = vpx_codec_get_cx_data(&enc->codec, &iter)) != NULL && count < max_out) {
        if (pkt->kind == VPX_CODEC_CX_FRAME_PKT) {
            out_data[count] = (const unsigned char*)pkt->data.frame.buf;
            out_sizes[count] = (int)pkt->data.frame.sz;
            out_key[count] = (pkt->data.frame.flags & VPX_FRAME_IS_KEY) ? 1 : 0;
            out_pts[count] = pkt->data.frame.pts;
            count++;
        }
    }
    return count;
}

// Destroy encoder
void vpx_helper_destroy(vpx_encoder_t *enc) {
    if (enc) {
        vpx_codec_destroy(&enc->codec);
        free(enc);
    }
}

// --- VP8 Decoder ---

typedef struct {
    vpx_codec_ctx_t codec;
    int width;
    int height;
} vpx_decoder_t;

// Create decoder. Returns NULL on failure.
vpx_decoder_t* vpx_helper_dec_create(int *err_code) {
    vpx_decoder_t *dec = (vpx_decoder_t*)calloc(1, sizeof(vpx_decoder_t));
    if (!dec) { *err_code = -1; return NULL; }

    vpx_codec_iface_t *iface = vpx_codec_vp8_dx();
    vpx_codec_dec_cfg_t cfg;
    memset(&cfg, 0, sizeof(cfg));
    cfg.threads = 1;

    vpx_codec_err_t res = vpx_codec_dec_init(&dec->codec, iface, &cfg, 0);
    if (res != VPX_CODEC_OK) {
        *err_code = (int)res;
        free(dec);
        return NULL;
    }

    dec->width = 0;
    dec->height = 0;
    *err_code = 0;
    return dec;
}

// Decode one VP8 frame. Returns pointer to BGRA pixel data (caller must copy before next decode).
// Sets *out_width, *out_height, *out_stride. Returns NULL on error.
unsigned char* vpx_helper_dec_decode(vpx_decoder_t *dec, const unsigned char *data, int data_len,
                                      int *out_width, int *out_height, int *out_stride) {
    if (!dec || !data || data_len <= 0) return NULL;

    vpx_codec_err_t res = vpx_codec_decode(&dec->codec, data, (unsigned int)data_len, NULL, 0);
    if (res != VPX_CODEC_OK) return NULL;

    vpx_codec_iter_t iter = NULL;
    vpx_image_t *img = vpx_codec_get_frame(&dec->codec, &iter);
    if (!img) return NULL;

    int w = img->d_w;
    int h = img->d_h;
    dec->width = w;
    dec->height = h;

    // Allocate BGRA buffer (reuse static buffer for performance)
    static unsigned char *bgra_buf = NULL;
    static int bgra_buf_size = 0;
    int needed = w * h * 4;
    if (needed > bgra_buf_size) {
        free(bgra_buf);
        bgra_buf = (unsigned char*)malloc(needed);
        bgra_buf_size = needed;
    }
    if (!bgra_buf) return NULL;

    // Convert I420 to BGRA
    unsigned char *y_plane = img->planes[0];
    unsigned char *u_plane = img->planes[1];
    unsigned char *v_plane = img->planes[2];
    int y_stride = img->stride[0];
    int u_stride = img->stride[1];
    int v_stride = img->stride[2];

    for (int row = 0; row < h; row++) {
        for (int col = 0; col < w; col++) {
            int y_val = y_plane[row * y_stride + col];
            int u_val = u_plane[(row / 2) * u_stride + (col / 2)];
            int v_val = v_plane[(row / 2) * v_stride + (col / 2)];

            int c = y_val - 16;
            int d = u_val - 128;
            int e = v_val - 128;

            int r = (298 * c + 409 * e + 128) >> 8;
            int g = (298 * c - 100 * d - 208 * e + 128) >> 8;
            int b = (298 * c + 516 * d + 128) >> 8;

            if (r < 0) r = 0; if (r > 255) r = 255;
            if (g < 0) g = 0; if (g > 255) g = 255;
            if (b < 0) b = 0; if (b > 255) b = 255;

            int idx = (row * w + col) * 4;
            bgra_buf[idx + 0] = (unsigned char)b;
            bgra_buf[idx + 1] = (unsigned char)g;
            bgra_buf[idx + 2] = (unsigned char)r;
            bgra_buf[idx + 3] = 255;
        }
    }

    *out_width = w;
    *out_height = h;
    *out_stride = w * 4;
    return bgra_buf;
}

// Destroy decoder
void vpx_helper_dec_destroy(vpx_decoder_t *dec) {
    if (dec) {
        vpx_codec_destroy(&dec->codec);
        free(dec);
    }
}
