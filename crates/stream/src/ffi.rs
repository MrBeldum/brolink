//! Bindings to the C shim (`csrc/shim.h`) and to the moonlight-common-c input
//! functions, which take only primitive arguments and need no shim.

#![allow(non_camel_case_types, dead_code)]

use std::os::raw::{c_char, c_int, c_uint, c_void};

#[repr(C)]
pub struct Callbacks {
    pub video_setup: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int) -> c_int,
    pub video_cleanup: unsafe extern "C" fn(*mut c_void),
    pub video_frame:
        unsafe extern "C" fn(*mut c_void, *const u8, c_int, c_int, c_int, u16, u64, u64) -> c_int,
    pub audio_setup:
        unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int, c_int, *const u8) -> c_int,
    pub audio_cleanup: unsafe extern "C" fn(*mut c_void),
    pub audio_packet: unsafe extern "C" fn(*mut c_void, *const u8, c_int),
    pub stage: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int),
    pub connected: unsafe extern "C" fn(*mut c_void),
    pub terminated: unsafe extern "C" fn(*mut c_void, c_int),
    pub status: unsafe extern "C" fn(*mut c_void, c_int),
    pub log: unsafe extern "C" fn(*mut c_void, *const c_char),
}

#[repr(C)]
pub struct StreamConfig {
    pub width: c_int,
    pub height: c_int,
    pub fps: c_int,
    pub bitrate_kbps: c_int,
    pub packet_size: c_int,
    pub remote: c_int,
    pub video_formats: c_int,
    pub color_space: c_int,
    pub color_range: c_int,
    pub encryption_flags: c_int,
    pub video_capabilities: c_int,
    pub audio_capabilities: c_int,
    pub ri_key: *const u8,
    pub ri_iv: *const u8,
}

#[repr(C)]
pub struct ServerInfo {
    pub address: *const c_char,
    pub app_version: *const c_char,
    pub gfe_version: *const c_char,
    pub rtsp_url: *const c_char,
    pub codec_mode_support: c_int,
}

pub const STREAM_CFG_LOCAL: c_int = 0;
pub const STREAM_CFG_REMOTE: c_int = 1;
pub const STREAM_CFG_AUTO: c_int = 2;
pub const COLORSPACE_REC_709: c_int = 1;
pub const COLOR_RANGE_LIMITED: c_int = 0;
pub const COLOR_RANGE_FULL: c_int = 1;
pub const ENCFLG_ALL: c_int = -1;

pub const VIDEO_FORMAT_H264: c_int = 0x0001;
pub const VIDEO_FORMAT_H265: c_int = 0x0100;
pub const VIDEO_FORMAT_MASK_H264: c_int = 0x000F;
pub const VIDEO_FORMAT_MASK_H265: c_int = 0x0F00;

pub const CAPABILITY_DIRECT_SUBMIT: c_int = 0x1;
/// The decoder copes with a frame that references one older than the last
/// (the host then repairs a lost frame without a whole new keyframe).
pub const CAPABILITY_REFERENCE_FRAME_INVALIDATION_AVC: c_int = 0x2;
pub const CAPABILITY_REFERENCE_FRAME_INVALIDATION_HEVC: c_int = 0x4;
pub const CAPABILITY_SUPPORTS_ARBITRARY_AUDIO_DURATION: c_int = 0x10;

pub const FRAME_TYPE_IDR: c_int = 1;
pub const DR_OK: c_int = 0;
pub const DR_NEED_IDR: c_int = -1;

pub const ML_ERROR_GRACEFUL_TERMINATION: c_int = 0;
pub const ML_ERROR_NO_VIDEO_TRAFFIC: c_int = -100;
pub const ML_ERROR_NO_VIDEO_FRAME: c_int = -101;
pub const ML_ERROR_UNEXPECTED_EARLY_TERMINATION: c_int = -102;
pub const ML_ERROR_PROTECTED_CONTENT: c_int = -103;
pub const ML_ERROR_FRAME_CONVERSION: c_int = -104;

pub const CONN_STATUS_OKAY: c_int = 0;
pub const CONN_STATUS_POOR: c_int = 1;

pub const BUTTON_ACTION_PRESS: c_char = 0x07;
pub const BUTTON_ACTION_RELEASE: c_char = 0x08;
pub const BUTTON_LEFT: c_int = 0x01;
pub const BUTTON_MIDDLE: c_int = 0x02;
pub const BUTTON_RIGHT: c_int = 0x03;
pub const BUTTON_X1: c_int = 0x04;
pub const BUTTON_X2: c_int = 0x05;

pub const KEY_ACTION_DOWN: c_char = 0x03;
pub const KEY_ACTION_UP: c_char = 0x04;
pub const MODIFIER_SHIFT: c_char = 0x01;
pub const MODIFIER_CTRL: c_char = 0x02;
pub const MODIFIER_ALT: c_char = 0x04;
pub const MODIFIER_META: c_char = 0x08;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RtpVideoStats {
    pub packet_count_video: u32,
    pub packet_count_fec: u32,
    pub packet_count_fec_recovered: u32,
    pub packet_count_fec_failed: u32,
    pub packet_count_oos: u32,
    pub packet_count_invalid: u32,
    pub packet_count_fec_invalid: u32,
}

extern "C" {
    pub fn bl_start(
        server: *const ServerInfo,
        cfg: *const StreamConfig,
        cb: *const Callbacks,
        ctx: *mut c_void,
    ) -> c_int;
    pub fn bl_stop();
    pub fn bl_interrupt();
    pub fn bl_stage_name(stage: c_int) -> *const c_char;
    pub fn bl_launch_query() -> *const c_char;

    pub fn LiSendMouseMoveEvent(dx: i16, dy: i16) -> c_int;
    pub fn LiSendMousePositionEvent(x: i16, y: i16, ref_w: i16, ref_h: i16) -> c_int;
    pub fn LiSendMouseButtonEvent(action: c_char, button: c_int) -> c_int;
    pub fn LiSendKeyboardEvent(key: i16, action: c_char, modifiers: c_char) -> c_int;
    pub fn LiSendUtf8TextEvent(text: *const c_char, len: c_uint) -> c_int;
    pub fn LiSendHighResScrollEvent(amount: i16) -> c_int;
    pub fn LiSendHighResHScrollEvent(amount: i16) -> c_int;
    pub fn LiGetEstimatedRttInfo(rtt: *mut u32, variance: *mut u32) -> bool;
    pub fn LiGetMicroseconds() -> u64;
    pub fn LiGetPendingVideoFrames() -> c_int;
    pub fn LiGetRTPVideoStats() -> *const RtpVideoStats;
    pub fn LiRequestIdrFrame();
}

pub fn stage_name(stage: c_int) -> String {
    unsafe {
        let p = bl_stage_name(stage);
        if p.is_null() {
            return format!("stage {stage}");
        }
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

pub fn launch_query() -> String {
    unsafe {
        let p = bl_launch_query();
        if p.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}
