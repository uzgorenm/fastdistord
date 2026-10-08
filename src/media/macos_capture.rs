//! Explicit-action native capture. Nothing is opened by construction or enumeration.
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::{
    AnyThread, DefinedClass, Message, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_av_foundation::*;
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::*;
use objc2_foundation::{
    NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSProcessInfo, NSString,
};
use objc2_screen_capture_kit::*;
use std::sync::{Arc, Mutex};

pub struct CameraSource {
    pub id: String,
    pub name: String,
}
pub struct PreviewFrame {
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
    pub sequence: u64,
}
pub struct CaptureSnapshot {
    pub status: String,
    pub active: bool,
    pub pending: bool,
}
struct State {
    generation: u64,
    active: bool,
    pending: bool,
    status: String,
    frame: Option<PreviewFrame>,
    sequence: u64,
    filter: Option<SelectedFilter>,
}
// SCContentFilter is an immutable selection descriptor in this module: after
// picker delivery we only retain it and pass it to SCStream initialization.
// Mutex ownership transfers the descriptor; no mutation or concurrent access.
struct SelectedFilter(Retained<SCContentFilter>);
unsafe impl Send for SelectedFilter {}

// Session is configured before transfer, then all running-state operations execute
// on the same serial control queue. Rust never accesses it concurrently.
struct SessionControl(Retained<AVCaptureSession>);
unsafe impl Send for SessionControl {}
impl SessionControl {
    fn start(&self) {
        unsafe {
            self.0.startRunning();
        }
    }
    fn stop(&self) {
        unsafe {
            self.0.stopRunning();
        }
    }
    fn is_running(&self) -> bool {
        unsafe { self.0.isRunning() }
    }
}

struct DelegateIvars {
    state: Arc<Mutex<State>>,
    generation: u64,
}
define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = DelegateIvars]
    struct CaptureDelegate;
    unsafe impl NSObjectProtocol for CaptureDelegate {}
    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for CaptureDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        unsafe fn camera_frame(
            &self,
            _output: &AVCaptureOutput,
            sample: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            self.receive(sample);
        }
    }
    unsafe impl SCStreamOutput for CaptureDelegate {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn screen_frame(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Screen {
                self.receive(sample);
            }
        }
    }
    unsafe impl SCStreamDelegate for CaptureDelegate {
        #[unsafe(method(stream:didStopWithError:))]
        unsafe fn stream_stopped(&self, _stream: &SCStream, _error: &NSError) {
            self.update("Window preview stopped by macOS", false);
        }
    }
    unsafe impl SCContentSharingPickerObserver for CaptureDelegate {
        #[unsafe(method(contentSharingPicker:didCancelForStream:))]
        unsafe fn cancel(&self, _picker: &SCContentSharingPicker, _stream: Option<&SCStream>) {
            self.update("Screen selection canceled", false);
        }
        #[unsafe(method(contentSharingPicker:didUpdateWithFilter:forStream:))]
        unsafe fn selected(
            &self,
            _picker: &SCContentSharingPicker,
            filter: &SCContentFilter,
            _stream: Option<&SCStream>,
        ) {
            let Ok(mut s) = self.ivars().state.lock() else {
                return;
            };
            if s.generation == self.ivars().generation && s.pending {
                s.filter = Some(SelectedFilter(filter.retain()));
                s.status = "Starting selected window preview".into();
            }
        }
        #[unsafe(method(contentSharingPickerStartDidFailWithError:))]
        unsafe fn failed(&self, _error: &NSError) {
            self.update("Screen picker failed", false);
        }
    }
);
impl CaptureDelegate {
    fn new(state: Arc<Mutex<State>>, generation: u64) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { state, generation });
        unsafe { msg_send![super(this), init] }
    }
    fn update(&self, status: &str, active: bool) {
        if let Ok(mut s) = self.ivars().state.lock()
            && s.generation == self.ivars().generation
        {
            s.status = status.into();
            s.active = active;
            s.pending = false;
            if !active {
                s.frame = None;
                s.filter = None;
            }
        }
    }
    fn receive(&self, sample: &CMSampleBuffer) {
        let Ok(mut s) = self.ivars().state.lock() else {
            return;
        };
        if !s.active || s.generation != self.ivars().generation {
            return;
        }
        // Bound retained memory to one <= 1920x1080 BGRA frame; native buffers are never retained.
        unsafe {
            let Some(buffer) = sample.image_buffer() else {
                return;
            };
            let pixel: &CVPixelBuffer = &buffer;
            let width = CVPixelBufferGetWidth(pixel);
            let height = CVPixelBufferGetHeight(pixel);
            if width == 0
                || height == 0
                || width > 1920
                || height > 1080
                || CVPixelBufferGetPixelFormatType(pixel) != kCVPixelFormatType_32BGRA
            {
                return;
            }
            let flags = CVPixelBufferLockFlags::ReadOnly;
            if CVPixelBufferLockBaseAddress(pixel, flags) != 0 {
                return;
            }
            let base = CVPixelBufferGetBaseAddress(pixel).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRow(pixel);
            if !base.is_null()
                && stride >= width * 4
                && stride
                    .checked_mul(height)
                    .is_some_and(|bytes| bytes <= CVPixelBufferGetDataSize(pixel))
            {
                let mut bgra = vec![0; width * height * 4];
                for row in 0..height {
                    std::ptr::copy_nonoverlapping(
                        base.add(row * stride),
                        bgra.as_mut_ptr().add(row * width * 4),
                        width * 4,
                    );
                }
                s.sequence = s.sequence.wrapping_add(1);
                s.frame = Some(PreviewFrame {
                    width,
                    height,
                    bgra,
                    sequence: s.sequence,
                });
            }
            CVPixelBufferUnlockBaseAddress(pixel, flags);
        }
    }
}
pub struct MacCapture {
    state: Arc<Mutex<State>>,
    delegate: Option<Retained<CaptureDelegate>>,
    session: Option<Retained<AVCaptureSession>>,
    output: Option<Retained<AVCaptureVideoDataOutput>>,
    stream: Option<Retained<SCStream>>,
    queue: DispatchRetained<DispatchQueue>,
    picker_registered: bool,
    control_queue: DispatchRetained<DispatchQueue>,
}
impl MacCapture {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                generation: 0,
                active: false,
                pending: false,
                status: "Local preview stopped".into(),
                frame: None,
                sequence: 0,
                filter: None,
            })),
            delegate: None,
            session: None,
            output: None,
            stream: None,
            queue: DispatchQueue::new("fastdistord.local-preview", DispatchQueueAttr::SERIAL),
            picker_registered: false,
            control_queue: DispatchQueue::new(
                "fastdistord.capture-control",
                DispatchQueueAttr::SERIAL,
            ),
        }
    }
    #[allow(deprecated)]
    pub fn cameras() -> Vec<CameraSource> {
        unsafe {
            AVCaptureDevice::devicesWithMediaType(
                AVMediaTypeVideo.expect("AVFoundation video media type"),
            )
            .iter()
            .take(32)
            .filter_map(|d| {
                let id = d.uniqueID().to_string();
                if id.len() > 1024 {
                    return None;
                }
                Some(CameraSource {
                    id,
                    name: d.localizedName().to_string().chars().take(128).collect(),
                })
            })
            .collect()
        }
    }
    pub fn snapshot(&self) -> CaptureSnapshot {
        let s = self.state.lock().unwrap();
        CaptureSnapshot {
            status: s.status.clone(),
            active: s.active,
            pending: s.pending,
        }
    }
    pub fn take_frame(&mut self) -> Option<PreviewFrame> {
        self.state.lock().unwrap().frame.take()
    }
    pub fn request_camera_permission(&mut self) -> Result<(), String> {
        self.stop();
        let generation = self.state.lock().unwrap().generation;
        {
            let mut s = self.state.lock().unwrap();
            s.pending = true;
            s.status = "Waiting for camera permission".into();
        }
        let state = self.state.clone();
        let block = RcBlock::new(move |granted: objc2::runtime::Bool| {
            if let Ok(mut s) = state.lock()
                && s.generation == generation
            {
                s.pending = false;
                s.status = if granted.as_bool() {
                    "Camera permission granted; choose Start preview"
                } else {
                    "Camera permission denied"
                }
                .into();
            }
        });
        unsafe {
            AVCaptureDevice::requestAccessForMediaType_completionHandler(
                AVMediaTypeVideo.expect("AVFoundation video media type"),
                &block,
            );
        }
        Ok(())
    }
    pub fn start_camera(&mut self, id: &str) -> Result<(), String> {
        self.stop();
        unsafe {
            if AVCaptureDevice::authorizationStatusForMediaType(
                AVMediaTypeVideo.expect("AVFoundation video media type"),
            ) != AVAuthorizationStatus::Authorized
            {
                return Err("Grant camera permission before starting preview".into());
            }
            let device = AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(id))
                .ok_or("Selected camera is unavailable")?;
            let input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
                .map_err(|_| "Camera input failed")?;
            let session = AVCaptureSession::new();
            session.setSessionPreset(AVCaptureSessionPreset640x480);
            let output = AVCaptureVideoDataOutput::new();
            output.setAlwaysDiscardsLateVideoFrames(true);
            let key = NSString::from_str("PixelFormatType");
            let value = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
            let settings = NSDictionary::<NSString, AnyObject>::from_slices(&[&*key], &[&*value]);
            output.setVideoSettings(Some(&settings));
            if !session.canAddInput(&input) || !session.canAddOutput(&output) {
                return Err("Camera configuration unavailable".into());
            }
            session.addInput(&input);
            session.addOutput(&output);
            let generation = self.state.lock().unwrap().generation;
            let delegate = CaptureDelegate::new(self.state.clone(), generation);
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&self.queue),
            );
            {
                let mut s = self.state.lock().unwrap();
                s.active = true;
                s.status = "Camera local preview".into();
            }
            self.delegate = Some(delegate);
            self.output = Some(output);
            let control = SessionControl(session.clone());
            self.session = Some(session);
            let state = self.state.clone();
            {
                let mut s = state.lock().unwrap();
                s.pending = true;
                s.active = false;
                s.status = "Starting camera local preview".into();
            }
            self.control_queue.exec_async(move || {
                let allowed = state
                    .lock()
                    .map(|s| s.generation == generation)
                    .unwrap_or(false);
                if !allowed {
                    return;
                }
                control.start();
                if let Ok(mut s) = state.lock()
                    && s.generation == generation
                {
                    s.pending = false;
                    s.active = control.is_running();
                    s.status = if s.active {
                        "Camera local preview"
                    } else {
                        "Camera could not start"
                    }
                    .into();
                }
            });
        }
        Ok(())
    }
    pub fn start_screen_picker(&mut self) -> Result<(), String> {
        self.stop();
        if NSProcessInfo::processInfo()
            .operatingSystemVersion()
            .majorVersion
            < 14
        {
            return Err("Native window picker requires macOS 14 or later".into());
        }
        if objc2::MainThreadMarker::new().is_none() {
            return Err("Open the picker from the app main thread".into());
        }
        unsafe {
            let delegate =
                CaptureDelegate::new(self.state.clone(), self.state.lock().unwrap().generation);
            let picker = SCContentSharingPicker::sharedPicker();
            let config = SCContentSharingPickerConfiguration::new();
            config.setAllowedPickerModes(SCContentSharingPickerMode::SingleWindow);
            config.setAllowsChangingSelectedContent(false);
            picker.setDefaultConfiguration(&config);
            picker.addObserver(ProtocolObject::from_ref(&*delegate));
            picker.setActive(true);
            self.delegate = Some(delegate);
            self.picker_registered = true;
            {
                let mut s = self.state.lock().unwrap();
                s.pending = true;
                s.status = "Choose a window for local preview".into();
            }
            picker.present();
        }
        Ok(())
    }
    pub fn poll(&mut self) {
        let terminal = {
            let s = self.state.lock().unwrap();
            if !s.active
                && !s.pending
                && (self.session.is_some() || self.stream.is_some() || self.picker_registered)
            {
                Some(s.status.clone())
            } else {
                None
            }
        };
        if let Some(status) = terminal {
            self.stop();
            self.state.lock().unwrap().status = status;
            return;
        }
        let filter = self.state.lock().unwrap().filter.take();
        let Some(filter) = filter else {
            return;
        };
        unsafe {
            let config = SCStreamConfiguration::new();
            config.setWidth(1280);
            config.setHeight(720);
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
            config.setQueueDepth(3);
            config.setCapturesAudio(false);
            config.setMinimumFrameInterval(CMTime::new(1, 15));
            let Some(delegate) = &self.delegate else {
                return;
            };
            let stream = SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter.0,
                &config,
                Some(ProtocolObject::from_ref(&**delegate)),
            );
            if stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&**delegate),
                    SCStreamOutputType::Screen,
                    Some(&self.queue),
                )
                .is_err()
            {
                delegate.update("Window preview configuration failed", false);
                return;
            }
            let state = self.state.clone();
            let generation = state.lock().unwrap().generation;
            {
                let mut s = state.lock().unwrap();
                s.active = true;
            }
            let block = RcBlock::new(move |error: *mut NSError| {
                if let Ok(mut s) = state.lock()
                    && s.generation == generation
                {
                    s.pending = false;
                    s.active = error.is_null();
                    s.status = if error.is_null() {
                        "Selected window local preview"
                    } else {
                        "Window preview could not start; check Screen Recording permission"
                    }
                    .into();
                }
            });
            stream.startCaptureWithCompletionHandler(Some(&block));
            self.stream = Some(stream);
        }
    }
    pub fn stop(&mut self) {
        {
            let mut s = self.state.lock().unwrap();
            s.generation = s.generation.wrapping_add(1);
            s.active = false;
            s.pending = false;
            s.frame = None;
            s.filter = None;
            s.status = "Local preview stopped".into();
        }
        unsafe {
            if let Some(output) = self.output.take() {
                output.setSampleBufferDelegate_queue(None, None);
            }
            if let Some(session) = self.session.take() {
                let control = SessionControl(session);
                // Drain queued start before stopping; return only once the camera stops.
                self.control_queue.exec_sync(move || control.stop());
            }
            if let Some(stream) = self.stream.take() {
                stream.stopCaptureWithCompletionHandler(None);
            }
            if self.picker_registered {
                let picker = SCContentSharingPicker::sharedPicker();
                if let Some(delegate) = &self.delegate {
                    picker.removeObserver(ProtocolObject::from_ref(&**delegate));
                }
                picker.setActive(false);
                self.picker_registered = false;
            }
        }
        self.delegate = None;
    }
}
impl Default for MacCapture {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for MacCapture {
    fn drop(&mut self) {
        self.stop();
    }
}
