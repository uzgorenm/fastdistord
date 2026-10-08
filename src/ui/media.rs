//! Explicit local camera/window preview. No Discord send path exists here.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use fastdistord::media::{
    macos_capture::{CameraSource, MacCapture},
    macos_codec,
};

pub struct MediaView {
    capture: MacCapture,
    cameras: Vec<CameraSource>,
    camera: Option<String>,
    texture: Option<egui::TextureHandle>,
    error: Option<String>,
    codec_busy: Arc<AtomicBool>,
    codec_result: Arc<Mutex<Option<String>>>,
}

impl MediaView {
    pub fn new() -> Self {
        Self {
            capture: MacCapture::new(),
            cameras: vec![],
            camera: None,
            texture: None,
            error: None,
            codec_busy: Arc::new(AtomicBool::new(false)),
            codec_result: Arc::new(Mutex::new(None)),
        }
    }

    pub fn stop(&mut self) {
        self.capture.stop();
        self.texture.take();
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.capture.poll();
        let state = self.capture.snapshot();
        if !state.active && !state.pending {
            self.texture.take();
        }
        ui.heading("Local preview");
        ui.label(
            "Camera and screen content stays on this Mac. Discord video sending is unavailable.",
        );
        ui.add_space(10.0);
        ui.label(&state.status);
        if let Some(error) = &self.error {
            ui.colored_label(super::theme::DANGER, error);
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !state.active && !state.pending,
                    egui::Button::new("Find cameras"),
                )
                .clicked()
            {
                self.cameras = MacCapture::cameras();
                self.error = None;
            }
            if ui
                .add_enabled(
                    !state.active && !state.pending,
                    egui::Button::new("Allow camera access…"),
                )
                .clicked()
            {
                self.error = self.capture.request_camera_permission().err();
            }
            if ui
                .add_enabled(
                    !state.active && !state.pending,
                    egui::Button::new("Choose window to preview…"),
                )
                .clicked()
            {
                self.texture.take();
                self.error = self.capture.start_screen_picker().err();
            }
            if ui
                .add_enabled(
                    state.active || state.pending,
                    egui::Button::new("Stop preview"),
                )
                .clicked()
            {
                self.stop();
            }
        });
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("preview_camera")
                .selected_text(
                    self.cameras
                        .iter()
                        .find(|c| Some(&c.id) == self.camera.as_ref())
                        .map(|c| c.name.as_str())
                        .unwrap_or("Choose camera"),
                )
                .show_ui(ui, |ui| {
                    for camera in &self.cameras {
                        ui.selectable_value(
                            &mut self.camera,
                            Some(camera.id.clone()),
                            &camera.name,
                        );
                    }
                });
            if ui
                .add_enabled(
                    self.camera.is_some() && !state.active && !state.pending,
                    egui::Button::new("Start camera preview"),
                )
                .clicked()
            {
                self.texture.take();
                if let Some(camera) = &self.camera {
                    self.error = self.capture.start_camera(camera).err();
                }
            }
        });
        if let Some(frame) = self.capture.take_frame() {
            let expected = frame
                .width
                .checked_mul(frame.height)
                .and_then(|n| n.checked_mul(4));
            if expected == Some(frame.bgra.len()) && frame.bgra.len() <= 1920 * 1080 * 4 {
                let mut pixels = frame.bgra;
                for pixel in pixels.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                let image =
                    egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &pixels);
                if let Some(texture) = &mut self.texture {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    self.texture = Some(ui.ctx().load_texture(
                        "local_media_preview",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            } else {
                self.error = Some("Preview frame exceeded its limits; preview stopped.".into());
                self.stop();
            }
        }
        if let Some(texture) = &self.texture {
            ui.add(
                egui::Image::new(texture)
                    .max_size(egui::vec2(
                        ui.available_width(),
                        (ui.available_height() - 85.0).max(80.0),
                    ))
                    .maintain_aspect_ratio(true),
            );
        } else {
            ui.label("Choose a source and start preview when ready.");
        }
        ui.add_space(8.0);
        let busy = self.codec_busy.load(Ordering::Acquire);
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(if busy {
                    "Checking codec…"
                } else {
                    "Check codec with synthetic frame"
                }),
            )
            .clicked()
            && self
                .codec_busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            let busy = self.codec_busy.clone();
            let result = self.codec_result.clone();
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let message = match macos_codec::synthetic_roundtrip(320, 180) {
                    Ok(_) => {
                        "Synthetic H.264 encode/decode passed. No camera or screen captured.".into()
                    }
                    Err(error) => format!("Codec check failed: {error}"),
                };
                if let Ok(mut result) = result.lock() {
                    *result = Some(message);
                }
                busy.store(false, Ordering::Release);
                ctx.request_repaint();
            });
        }
        if let Ok(result) = self.codec_result.lock()
            && let Some(result) = &*result
        {
            ui.label(result);
        }
        if state.active || state.pending {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

impl Drop for MediaView {
    fn drop(&mut self) {
        self.stop();
    }
}
