//! Hardware H.264 and HEVC decode on macOS through VideoToolbox, bound by
//! hand: a dozen C functions from CoreMedia, CoreVideo and VideoToolbox.
//!
//! Each access unit arrives in Annex B. Parameter sets go into a format
//! description; the slices are re-framed with 4-byte lengths (AVCC/HVCC) into
//! one sample buffer and decoded synchronously, so the output callback runs
//! on this thread before `decode` returns.

use super::{nal_units, Decoder, Frame};
use anyhow::{anyhow, bail, Result};
use std::ffi::c_void;
use std::ptr;

type OSStatus = i32;
type CFTypeRef = *const c_void;
type CFAllocatorRef = CFTypeRef;
type CFDictionaryRef = CFTypeRef;
type CFStringRef = CFTypeRef;
type CFNumberRef = CFTypeRef;
type CMFormatDescriptionRef = CFTypeRef;
type CMBlockBufferRef = CFTypeRef;
type CMSampleBufferRef = CFTypeRef;
type CVImageBufferRef = CFTypeRef;
type VTDecompressionSessionRef = CFTypeRef;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

type OutputCallback =
    unsafe extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, CVImageBufferRef, CMTime, CMTime);

#[repr(C)]
struct VTDecompressionOutputCallbackRecord {
    callback: Option<OutputCallback>,
    refcon: *mut c_void,
}

const K_CF_NUMBER_SINT32: isize = 3;
const K_CV_PIXEL_FORMAT_420V: u32 = 0x3432_3076; // '420v', video range
const K_CV_PIXEL_FORMAT_420F: u32 = 0x3432_3066; // '420f', full range
const K_CV_LOCK_READ_ONLY: u64 = 1;

extern "C" {
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;

    fn CFRelease(cf: CFTypeRef);
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        num: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: isize,
        ptr: *const c_void,
    ) -> CFNumberRef;

    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFAllocatorRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_unit_header_length: i32,
        out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: CFAllocatorRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_unit_header_length: i32,
        extensions: CFDictionaryRef,
        out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;
    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: CFAllocatorRef,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        out: *mut CMBlockBufferRef,
    ) -> OSStatus;
    fn CMBlockBufferReplaceDataBytes(
        source: *const c_void,
        destination: CMBlockBufferRef,
        offset: usize,
        length: usize,
    ) -> OSStatus;
    fn CMSampleBufferCreateReady(
        allocator: CFAllocatorRef,
        data_buffer: CMBlockBufferRef,
        format: CMFormatDescriptionRef,
        num_samples: isize,
        num_timing_entries: isize,
        timing: *const c_void,
        num_size_entries: isize,
        sizes: *const usize,
        out: *mut CMSampleBufferRef,
    ) -> OSStatus;

    fn VTDecompressionSessionCreate(
        allocator: CFAllocatorRef,
        format: CMFormatDescriptionRef,
        decoder_specification: CFDictionaryRef,
        destination_attributes: CFDictionaryRef,
        callback: *const VTDecompressionOutputCallbackRecord,
        out: *mut VTDecompressionSessionRef,
    ) -> OSStatus;
    fn VTDecompressionSessionDecodeFrame(
        session: VTDecompressionSessionRef,
        sample: CMSampleBufferRef,
        flags: u32,
        frame_refcon: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;
    fn VTDecompressionSessionInvalidate(session: VTDecompressionSessionRef);
    fn VTDecompressionSessionCanAcceptFormatDescription(
        session: VTDecompressionSessionRef,
        format: CMFormatDescriptionRef,
    ) -> u8;

    fn CVPixelBufferLockBaseAddress(pb: CVImageBufferRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: CVImageBufferRef, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddressOfPlane(pb: CVImageBufferRef, plane: usize) -> *const u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(pb: CVImageBufferRef, plane: usize) -> usize;
    fn CVPixelBufferGetWidthOfPlane(pb: CVImageBufferRef, plane: usize) -> usize;
    fn CVPixelBufferGetHeightOfPlane(pb: CVImageBufferRef, plane: usize) -> usize;
    fn CVPixelBufferGetPixelFormatType(pb: CVImageBufferRef) -> u32;
    fn CVPixelBufferGetPlaneCount(pb: CVImageBufferRef) -> usize;
}

pub struct VideoToolbox {
    hevc: bool,
    full_range: bool,
    format: CMFormatDescriptionRef,
    session: VTDecompressionSessionRef,
    /// Parameter sets the current format description was built from.
    params: Vec<Vec<u8>>,
    avcc: Vec<u8>,
}

unsafe impl Send for VideoToolbox {}

impl VideoToolbox {
    pub fn new(format: i32, _width: u32, _height: u32) -> Result<Self> {
        let hevc = format & crate::ffi::VIDEO_FORMAT_MASK_H265 != 0;
        if !hevc && format & crate::ffi::VIDEO_FORMAT_MASK_H264 == 0 {
            bail!("unsupported video format {format:#x}");
        }
        Ok(Self {
            hevc,
            full_range: false,
            format: ptr::null(),
            session: ptr::null(),
            params: Vec::new(),
            avcc: Vec::new(),
        })
    }

    fn nal_type(&self, nal: &[u8]) -> u8 {
        if self.hevc {
            (nal[0] >> 1) & 0x3f
        } else {
            nal[0] & 0x1f
        }
    }

    fn is_parameter_set(&self, t: u8) -> bool {
        if self.hevc {
            (32..=34).contains(&t)
        } else {
            t == 7 || t == 8
        }
    }

    /// Rebuild the format description (and, if needed, the session) from a
    /// fresh set of parameter NALs.
    fn set_parameters(&mut self, sets: Vec<Vec<u8>>) -> Result<()> {
        if sets == self.params && !self.format.is_null() {
            // A sleep/wake or GPU reset invalidates the session while the
            // parameter sets stay the same. Recreate rather than decoding
            // into a dead session until the user reconnects.
            if self.session.is_null() {
                self.create_session()?;
            }
            return Ok(());
        }
        let ptrs: Vec<*const u8> = sets.iter().map(|s| s.as_ptr()).collect();
        let sizes: Vec<usize> = sets.iter().map(|s| s.len()).collect();
        let mut fmt: CMFormatDescriptionRef = ptr::null();
        let status = unsafe {
            if self.hevc {
                CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    ptr::null(),
                    sets.len(),
                    ptrs.as_ptr(),
                    sizes.as_ptr(),
                    4,
                    ptr::null(),
                    &mut fmt,
                )
            } else {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    ptr::null(),
                    sets.len(),
                    ptrs.as_ptr(),
                    sizes.as_ptr(),
                    4,
                    &mut fmt,
                )
            }
        };
        if status != 0 || fmt.is_null() {
            bail!("format description failed ({status})");
        }
        unsafe {
            if !self.session.is_null()
                && VTDecompressionSessionCanAcceptFormatDescription(self.session, fmt) == 0
            {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
                self.session = ptr::null();
            }
            if !self.format.is_null() {
                CFRelease(self.format);
            }
        }
        self.format = fmt;
        self.params = sets;
        if self.session.is_null() {
            self.create_session()?;
        }
        Ok(())
    }

    fn create_session(&mut self) -> Result<()> {
        unsafe {
            let pixel_format: i32 = if self.full_range {
                K_CV_PIXEL_FORMAT_420F
            } else {
                K_CV_PIXEL_FORMAT_420V
            } as i32;
            let number = CFNumberCreate(
                ptr::null(),
                K_CF_NUMBER_SINT32,
                &pixel_format as *const i32 as *const c_void,
            );
            let keys = [kCVPixelBufferPixelFormatTypeKey];
            let values = [number];
            let attrs = CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let record = VTDecompressionOutputCallbackRecord {
                callback: Some(output),
                refcon: ptr::null_mut(),
            };
            let mut session: VTDecompressionSessionRef = ptr::null();
            let status = VTDecompressionSessionCreate(
                ptr::null(),
                self.format,
                ptr::null(),
                attrs,
                &record,
                &mut session,
            );
            CFRelease(attrs);
            CFRelease(number);
            if status != 0 || session.is_null() {
                bail!("VideoToolbox session failed ({status})");
            }
            self.session = session;
        }
        Ok(())
    }
}

impl VideoToolbox {
    fn drop_session(&mut self) {
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
                self.session = ptr::null();
            }
        }
    }
}

impl Drop for VideoToolbox {
    fn drop(&mut self) {
        self.drop_session();
        unsafe {
            if !self.format.is_null() {
                CFRelease(self.format);
            }
        }
    }
}

/// Runs inside `VTDecompressionSessionDecodeFrame`; copies the picture out
/// into the `Option<Frame>` passed as the frame refcon.
unsafe extern "C" fn output(
    _decoder_refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: OSStatus,
    _flags: u32,
    image: CVImageBufferRef,
    _pts: CMTime,
    _duration: CMTime,
) {
    if status != 0 || image.is_null() || frame_refcon.is_null() {
        return;
    }
    let out = &mut *(frame_refcon as *mut Option<Frame>);
    if CVPixelBufferGetPlaneCount(image) < 2 {
        return;
    }
    if CVPixelBufferLockBaseAddress(image, K_CV_LOCK_READ_ONLY) != 0 {
        return;
    }
    let w = CVPixelBufferGetWidthOfPlane(image, 0);
    let h = CVPixelBufferGetHeightOfPlane(image, 0);
    let ys = CVPixelBufferGetBytesPerRowOfPlane(image, 0);
    let uvh = CVPixelBufferGetHeightOfPlane(image, 1);
    let uvs = CVPixelBufferGetBytesPerRowOfPlane(image, 1);
    let yp = CVPixelBufferGetBaseAddressOfPlane(image, 0);
    let uvp = CVPixelBufferGetBaseAddressOfPlane(image, 1);
    if !yp.is_null() && !uvp.is_null() {
        let y = std::slice::from_raw_parts(yp, ys * h).to_vec();
        let uv = std::slice::from_raw_parts(uvp, uvs * uvh).to_vec();
        *out = Some(Frame {
            width: w as u32,
            height: h as u32,
            y,
            y_stride: ys,
            uv,
            uv_stride: uvs,
            full_range: CVPixelBufferGetPixelFormatType(image) == K_CV_PIXEL_FORMAT_420F,
        });
    }
    CVPixelBufferUnlockBaseAddress(image, K_CV_LOCK_READ_ONLY);
}

impl Decoder for VideoToolbox {
    fn decode(&mut self, annexb: &[u8], _idr: bool) -> Result<Option<Frame>> {
        let nals = nal_units(annexb);
        let mut sets = Vec::new();
        self.avcc.clear();
        for nal in nals {
            let t = self.nal_type(nal);
            if self.is_parameter_set(t) {
                sets.push(nal.to_vec());
            } else {
                self.avcc
                    .extend_from_slice(&(nal.len() as u32).to_be_bytes());
                self.avcc.extend_from_slice(nal);
            }
        }
        if !sets.is_empty() {
            self.set_parameters(sets)?;
        }
        if self.session.is_null() {
            bail!("no parameter sets yet");
        }
        if self.avcc.is_empty() {
            return Ok(None);
        }
        let len = self.avcc.len();
        let mut frame: Option<Frame> = None;
        unsafe {
            let mut block: CMBlockBufferRef = ptr::null();
            let status = CMBlockBufferCreateWithMemoryBlock(
                ptr::null(),
                ptr::null_mut(),
                len,
                ptr::null(),
                ptr::null(),
                0,
                len,
                0,
                &mut block,
            );
            if status != 0 {
                bail!("block buffer failed ({status})");
            }
            let status =
                CMBlockBufferReplaceDataBytes(self.avcc.as_ptr() as *const c_void, block, 0, len);
            if status != 0 {
                CFRelease(block);
                bail!("block buffer copy failed ({status})");
            }
            let mut sample: CMSampleBufferRef = ptr::null();
            let status = CMSampleBufferCreateReady(
                ptr::null(),
                block,
                self.format,
                1,
                0,
                ptr::null(),
                1,
                &len,
                &mut sample,
            );
            CFRelease(block);
            if status != 0 {
                bail!("sample buffer failed ({status})");
            }
            let status = VTDecompressionSessionDecodeFrame(
                self.session,
                sample,
                0,
                &mut frame as *mut Option<Frame> as *mut c_void,
                ptr::null_mut(),
            );
            CFRelease(sample);
            if status != 0 {
                // -12903 kVTInvalidSessionErr, -12911 kVTVideoDecoderMalfunctionErr
                if status == -12903 || status == -12911 {
                    self.drop_session();
                    if !self.format.is_null() {
                        let _ = self.create_session();
                    }
                }
                return Err(anyhow!("decode failed ({status})"));
            }
        }
        Ok(frame)
    }

    fn name(&self) -> &'static str {
        if self.hevc {
            "VideoToolbox HEVC"
        } else {
            "VideoToolbox H.264"
        }
    }
}
