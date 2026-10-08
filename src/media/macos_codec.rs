//! Bounded, independent H.264 keyframes using Apple's VideoToolbox.
//!
//! Sessions are intentionally drained and destroyed for each frame. This is an
//! offline/native pipeline building block, not a tuned real-time encoder. No
//! capture, network transmission or media files are created here.
use std::{
    ffi::c_void,
    fmt,
    ptr::{self, NonNull},
    sync::Mutex,
};

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained};
use objc2_core_media::*;
use objc2_core_video::*;
use objc2_video_toolbox::*;

const MAX_BYTES: usize = crate::video::MAX_ENCODED_FRAME_BYTES;
const MAX_NALS: usize = 256;
type EncodedSlot = Mutex<Option<Result<Vec<u8>, MediaCodecError>>>;
type DecodedSlot = Mutex<Option<Result<CFRetained<CVPixelBuffer>, MediaCodecError>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCodecError {
    InvalidDimensions,
    InvalidFrame,
    FrameTooLarge,
    NativeFailure(i32),
    NoOutput,
    PixelMismatch,
}

impl fmt::Display for MediaCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidDimensions => "Unsupported video dimensions (maximum 1920 × 1080)",
            Self::InvalidFrame => "Invalid or unsupported H.264 frame",
            Self::FrameTooLarge => "Video frame exceeds the bounded media size",
            Self::NativeFailure(_) => "Native video codec operation failed",
            Self::NoOutput => "Native video codec did not produce a frame",
            Self::PixelMismatch => "Native codec roundtrip did not preserve the synthetic pattern",
        })
    }
}
impl std::error::Error for MediaCodecError {}

fn status(code: i32) -> Result<(), MediaCodecError> {
    if code == 0 {
        Ok(())
    } else {
        Err(MediaCodecError::NativeFailure(code))
    }
}

fn dimensions(width: usize, height: usize) -> Result<(), MediaCodecError> {
    if width == 0
        || height == 0
        || width > 1920
        || height > 1080
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
    {
        Err(MediaCodecError::InvalidDimensions)
    } else {
        Ok(())
    }
}

/// Accepts native buffers matching these dimensions and emits independently
/// decodable Annex B H.264 keyframes with SPS/PPS. Each call owns its session.
pub struct H264Codec {
    width: usize,
    height: usize,
}

impl H264Codec {
    pub fn new(width: usize, height: usize) -> Result<Self, MediaCodecError> {
        dimensions(width, height)?;
        Ok(Self { width, height })
    }

    pub fn encode(&self, pixels: &CVPixelBuffer) -> Result<Vec<u8>, MediaCodecError> {
        if CVPixelBufferGetWidth(pixels) != self.width
            || CVPixelBufferGetHeight(pixels) != self.height
        {
            return Err(MediaCodecError::InvalidDimensions);
        }
        let state: Box<EncodedSlot> = Box::new(Mutex::new(None));
        let mut raw = ptr::null_mut();
        // SAFETY: callback context is stable until session invalidation, and all
        // output pointers are valid. VT owns no reference to pixels after drain.
        unsafe {
            status(VTCompressionSession::create(
                None,
                self.width as i32,
                self.height as i32,
                kCMVideoCodecType_H264,
                None,
                None,
                None,
                Some(encoded_callback),
                &*state as *const _ as *mut c_void,
                NonNull::from(&mut raw),
            ))?;
            let session = CompressionSession(CFRetained::from_raw(
                NonNull::new(raw).ok_or(MediaCodecError::NativeFailure(-1))?,
            ));
            status(session.0.prepare_to_encode_frames())?;
            status(session.0.encode_frame(
                pixels,
                CMTime::new(0, 30),
                CMTime::new(1, 30),
                None,
                ptr::null_mut(),
                ptr::null_mut(),
            ))?;
            status(session.0.complete_frames(kCMTimeInvalid))?;
            // Invalidation also runs on early errors before callback context drops.
            drop(session);
        }
        state
            .lock()
            .map_err(|_| MediaCodecError::NativeFailure(-1))?
            .take()
            .ok_or(MediaCodecError::NoOutput)?
    }

    /// Decodes one independent Annex B keyframe. Inter-frame streams are not
    /// accepted: the frame must contain SPS, PPS and an IDR slice.
    pub fn decode(&self, frame: &[u8]) -> Result<CFRetained<CVPixelBuffer>, MediaCodecError> {
        let nals = annex_b_nals(frame)?;
        let sps = nals
            .iter()
            .find(|n| n[0] & 31 == 7)
            .ok_or(MediaCodecError::InvalidFrame)?;
        let pps = nals
            .iter()
            .find(|n| n[0] & 31 == 8)
            .ok_or(MediaCodecError::InvalidFrame)?;
        if !nals.iter().any(|n| n[0] & 31 == 5) {
            return Err(MediaCodecError::InvalidFrame);
        }
        let mut pointers = [
            NonNull::new(sps.as_ptr().cast_mut()).unwrap(),
            NonNull::new(pps.as_ptr().cast_mut()).unwrap(),
        ];
        let mut lengths = [sps.len(), pps.len()];
        let mut raw_format = ptr::null();
        // SAFETY: format construction copies parameter sets, which remain live
        // for the call. Native dimensions are checked before allocating decoder.
        let format = unsafe {
            status(CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                2,
                NonNull::from(&mut pointers[0]),
                NonNull::from(&mut lengths[0]),
                4,
                NonNull::from(&mut raw_format),
            ))?;
            CFRetained::from_raw(
                NonNull::new(raw_format.cast_mut()).ok_or(MediaCodecError::NativeFailure(-1))?,
            )
        };
        let size = unsafe { CMVideoFormatDescriptionGetDimensions(&format) };
        if size.width != self.width as i32 || size.height != self.height as i32 {
            return Err(MediaCodecError::InvalidDimensions);
        }
        let mut avcc = Vec::with_capacity(frame.len());
        for nal in nals {
            avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            avcc.extend_from_slice(nal);
        }
        let mut raw_block = ptr::null_mut();
        let mut raw_sample = ptr::null_mut();
        let state: Box<DecodedSlot> = Box::new(Mutex::new(None));
        let mut raw_session = ptr::null_mut();
        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(decoded_callback),
            decompressionOutputRefCon: &*state as *const _ as *mut c_void,
        };
        let pixel_format = CFNumber::new_i32(kCVPixelFormatType_32BGRA as i32);
        let destination = unsafe {
            CFDictionary::from_slices(&[kCVPixelBufferPixelFormatTypeKey], &[&*pixel_format])
        };
        // SAFETY: CMBlockBuffer allocates its own bytes. Sample, format, context
        // and all pointers remain alive until decoder drain/invalidation.
        unsafe {
            status(CMBlockBuffer::create_with_memory_block(
                None,
                ptr::null_mut(),
                avcc.len(),
                None,
                ptr::null(),
                0,
                avcc.len(),
                0,
                NonNull::from(&mut raw_block),
            ))?;
            let block = CFRetained::from_raw(
                NonNull::new(raw_block).ok_or(MediaCodecError::NativeFailure(-1))?,
            );
            status(CMBlockBuffer::replace_data_bytes(
                NonNull::new(avcc.as_mut_ptr().cast()).unwrap(),
                &block,
                0,
                avcc.len(),
            ))?;
            let timing = CMSampleTimingInfo {
                duration: CMTime::new(1, 30),
                presentationTimeStamp: CMTime::new(0, 30),
                decodeTimeStamp: kCMTimeInvalid,
            };
            let sample_size = avcc.len();
            status(CMSampleBuffer::create_ready(
                None,
                Some(&block),
                Some(&format),
                1,
                1,
                &timing,
                1,
                &sample_size,
                NonNull::from(&mut raw_sample),
            ))?;
            let sample = CFRetained::from_raw(
                NonNull::new(raw_sample).ok_or(MediaCodecError::NativeFailure(-1))?,
            );
            status(VTDecompressionSession::create(
                None,
                &format,
                None,
                Some(destination.as_opaque()),
                &callback,
                NonNull::from(&mut raw_session),
            ))?;
            let session = DecompressionSession(CFRetained::from_raw(
                NonNull::new(raw_session).ok_or(MediaCodecError::NativeFailure(-1))?,
            ));
            status(session.0.decode_frame(
                &sample,
                VTDecodeFrameFlags(0),
                ptr::null_mut(),
                ptr::null_mut(),
            ))?;
            status(session.0.finish_delayed_frames())?;
            status(session.0.wait_for_asynchronous_frames())?;
            drop(session);
        }
        state
            .lock()
            .map_err(|_| MediaCodecError::NativeFailure(-1))?
            .take()
            .ok_or(MediaCodecError::NoOutput)?
    }
}

struct CompressionSession(CFRetained<VTCompressionSession>);
impl Drop for CompressionSession {
    fn drop(&mut self) {
        unsafe {
            // Drain even on early errors before callback state is released.
            let _ = self.0.complete_frames(kCMTimeInvalid);
            self.0.invalidate();
        }
    }
}
struct DecompressionSession(CFRetained<VTDecompressionSession>);
impl Drop for DecompressionSession {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.finish_delayed_frames();
            let _ = self.0.wait_for_asynchronous_frames();
            self.0.invalidate();
        }
    }
}

unsafe extern "C-unwind" fn encoded_callback(
    context: *mut c_void,
    _: *mut c_void,
    code: i32,
    _: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let result = if code != 0 {
        Err(MediaCodecError::NativeFailure(code))
    } else if sample.is_null() {
        Err(MediaCodecError::NoOutput)
    } else {
        unsafe { sample_to_annex_b(&*sample) }
    };
    // SAFETY: owner keeps the context allocated until session invalidation.
    let state = unsafe { &*(context as *const EncodedSlot) };
    if let Ok(mut slot) = state.lock()
        && slot.is_none()
    {
        *slot = Some(result);
    }
}

unsafe fn sample_to_annex_b(sample: &CMSampleBuffer) -> Result<Vec<u8>, MediaCodecError> {
    let format = unsafe { sample.format_description() }.ok_or(MediaCodecError::InvalidFrame)?;
    let block = unsafe { sample.data_buffer() }.ok_or(MediaCodecError::InvalidFrame)?;
    let len = unsafe { block.data_length() };
    if len == 0 || len > MAX_BYTES {
        return Err(MediaCodecError::FrameTooLarge);
    }
    let mut out = Vec::new();
    let mut header_len = 0;
    for index in 0..2 {
        let mut pointer = ptr::null();
        let mut len = 0;
        unsafe {
            status(CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                &format,
                index,
                &mut pointer,
                &mut len,
                ptr::null_mut(),
                &mut header_len,
            ))?;
        }
        if pointer.is_null() || len == 0 || len > MAX_BYTES || out.len() + len + 4 > MAX_BYTES {
            return Err(MediaCodecError::InvalidFrame);
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(pointer, len) });
    }
    if header_len != 4 {
        return Err(MediaCodecError::InvalidFrame);
    }
    let mut avcc = vec![0; len];
    unsafe {
        status(block.copy_data_bytes(0, len, NonNull::new(avcc.as_mut_ptr().cast()).unwrap()))?;
    }
    let mut position = 0;
    let mut count = 0;
    while position < avcc.len() {
        count += 1;
        if count > MAX_NALS || avcc.len() - position < 4 {
            return Err(MediaCodecError::InvalidFrame);
        }
        let size = u32::from_be_bytes(avcc[position..position + 4].try_into().unwrap()) as usize;
        position += 4;
        if size == 0 || size > avcc.len() - position {
            return Err(MediaCodecError::InvalidFrame);
        }
        if out.len() + size + 4 > MAX_BYTES {
            return Err(MediaCodecError::FrameTooLarge);
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&avcc[position..position + size]);
        position += size;
    }
    Ok(out)
}

unsafe extern "C-unwind" fn decoded_callback(
    context: *mut c_void,
    _: *mut c_void,
    code: i32,
    _: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _: CMTime,
    _: CMTime,
) {
    let result = if code != 0 {
        Err(MediaCodecError::NativeFailure(code))
    } else {
        NonNull::new(image)
            .map(|pointer| unsafe { CFRetained::retain(pointer) })
            .ok_or(MediaCodecError::NoOutput)
    };
    let state = unsafe { &*(context as *const DecodedSlot) };
    if let Ok(mut slot) = state.lock()
        && slot.is_none()
    {
        *slot = Some(result);
    }
}

fn annex_b_nals(frame: &[u8]) -> Result<Vec<&[u8]>, MediaCodecError> {
    if frame.len() > MAX_BYTES {
        return Err(MediaCodecError::FrameTooLarge);
    }
    // This component emits four-byte prefixes and accepts that canonical form.
    if !frame.starts_with(&[0, 0, 0, 1]) {
        return Err(MediaCodecError::InvalidFrame);
    }
    let mut nals = Vec::new();
    let mut start = 4;
    let mut cursor = start;
    while cursor + 4 <= frame.len() {
        if frame[cursor..cursor + 4] == [0, 0, 0, 1] {
            if cursor == start {
                return Err(MediaCodecError::InvalidFrame);
            }
            nals.push(&frame[start..cursor]);
            start = cursor + 4;
            cursor = start;
            if nals.len() >= MAX_NALS {
                return Err(MediaCodecError::FrameTooLarge);
            }
        } else {
            cursor += 1;
        }
    }
    if start == frame.len() {
        return Err(MediaCodecError::InvalidFrame);
    }
    nals.push(&frame[start..]);
    if nals
        .iter()
        .any(|nal| nal[0] & 0x80 != 0 || nal[0] & 31 == 0)
    {
        return Err(MediaCodecError::InvalidFrame);
    }
    Ok(nals)
}

/// Allocates a synthetic BGRA checkerboard and encodes it entirely in memory.
/// Never touches a camera, display, network or media file.
pub fn synthetic_encoded_frame(width: usize, height: usize) -> Result<Vec<u8>, MediaCodecError> {
    let codec = H264Codec::new(width, height)?;
    let mut raw = ptr::null_mut();
    let pixels = unsafe {
        status(CVPixelBufferCreate(
            None,
            width,
            height,
            kCVPixelFormatType_32BGRA,
            None,
            NonNull::from(&mut raw),
        ))?;
        CFRetained::from_raw(NonNull::new(raw).ok_or(MediaCodecError::NativeFailure(-1))?)
    };
    unsafe {
        status(CVPixelBufferLockBaseAddress(
            &pixels,
            CVPixelBufferLockFlags(0),
        ))?;
        let base = CVPixelBufferGetBaseAddress(&pixels);
        let stride = CVPixelBufferGetBytesPerRow(&pixels);
        if base.is_null() || stride < width * 4 {
            let _ = CVPixelBufferUnlockBaseAddress(&pixels, CVPixelBufferLockFlags(0));
            return Err(MediaCodecError::NativeFailure(-1));
        }
        for y in 0..height {
            for x in 0..width {
                let pixel = (base as *mut u8).add(y * stride + x * 4);
                ptr::copy_nonoverlapping(
                    [
                        checkerboard(x, y),
                        checkerboard(x, y),
                        checkerboard(x, y),
                        255,
                    ]
                    .as_ptr(),
                    pixel,
                    4,
                );
            }
        }
        status(CVPixelBufferUnlockBaseAddress(
            &pixels,
            CVPixelBufferLockFlags(0),
        ))?;
    }
    codec.encode(&pixels)
}

/// Encodes and decodes the synthetic checkerboard, checking bounded decoded pixel error.
pub fn synthetic_roundtrip(
    width: usize,
    height: usize,
) -> Result<(usize, usize, usize), MediaCodecError> {
    let codec = H264Codec::new(width, height)?;
    let bytes = synthetic_encoded_frame(width, height)?;
    let decoded = codec.decode(&bytes)?;
    verify_synthetic_frame(&decoded, width, height)?;
    Ok((
        bytes.len(),
        CVPixelBufferGetWidth(&decoded),
        CVPixelBufferGetHeight(&decoded),
    ))
}

fn checkerboard(x: usize, y: usize) -> u8 {
    if (x / 32 + y / 32).is_multiple_of(2) {
        32
    } else {
        224
    }
}

/// Checks decoded dimensions, format and sampled checkerboard RGB error.
/// The mean absolute channel error must be at most 24/255.
pub fn verify_synthetic_frame(
    pixels: &CVPixelBuffer,
    width: usize,
    height: usize,
) -> Result<(), MediaCodecError> {
    if CVPixelBufferGetWidth(pixels) != width
        || CVPixelBufferGetHeight(pixels) != height
        || CVPixelBufferGetPixelFormatType(pixels) != kCVPixelFormatType_32BGRA
    {
        return Err(MediaCodecError::PixelMismatch);
    }
    // SAFETY: buffer is locked for the whole read with verified dimensions and
    // stride. Sample block interiors to allow normal lossy edge artifacts.
    unsafe {
        status(CVPixelBufferLockBaseAddress(
            pixels,
            CVPixelBufferLockFlags::ReadOnly,
        ))?;
        let base = CVPixelBufferGetBaseAddress(pixels).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRow(pixels);
        let mut error = 0u64;
        let mut samples = 0u64;
        if !base.is_null() && stride >= width * 4 {
            for y in (4..height).step_by(8) {
                for x in (4..width).step_by(8) {
                    let pixel = base.add(y * stride + x * 4);
                    let expected = checkerboard(x, y);
                    for channel in 0..3 {
                        error += (*pixel.add(channel)).abs_diff(expected) as u64;
                        samples += 1;
                    }
                }
            }
        }
        status(CVPixelBufferUnlockBaseAddress(
            pixels,
            CVPixelBufferLockFlags::ReadOnly,
        ))?;
        if samples == 0 || error > samples * 24 {
            Err(MediaCodecError::PixelMismatch)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unbounded_dimensions_and_malformed_frames() {
        assert!(H264Codec::new(1922, 1080).is_err());
        assert!(H264Codec::new(0, 2).is_err());
        assert!(annex_b_nals(&[0, 0, 0, 1]).is_err());
        assert!(annex_b_nals(&[0, 0, 0, 1, 0x80]).is_err());
        assert!(annex_b_nals(&[0; MAX_BYTES + 1]).is_err());
    }
    #[test]
    #[ignore = "requires a macOS VideoToolbox encoder/decoder"]
    fn native_synthetic_h264_roundtrip() {
        let (bytes, width, height) =
            synthetic_roundtrip(320, 240).expect("native synthetic roundtrip");
        assert!(bytes > 0 && bytes <= MAX_BYTES);
        assert_eq!((width, height), (320, 240));
    }
}
