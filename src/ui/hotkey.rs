//! Global push-to-talk events write the atomic audio gate directly, never
//! through an egui frame. Unsupported/failed registrations fail closed.
use std::sync::{Arc, Mutex};

use fastframe_shell::Waker;
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};

use crate::audio::TxGate;

pub(super) const SHORTCUT: &str = "Ctrl + Shift + Space";

#[derive(Default)]
struct PressFlags {
    global: bool,
    local: bool,
    active: bool,
}

struct PressState {
    flags: Mutex<PressFlags>,
    gate: Arc<TxGate>,
}

impl PressState {
    fn update(&self, change: impl FnOnce(&mut PressFlags)) {
        // This mutex is used only by UI/OS input handlers. Audio callbacks
        // continue to read the separate lock-free TxGate. Updating a source
        // and publishing it must be one operation so focus-loss clearing
        // cannot be followed by an earlier, stale computed `true` value.
        let mut flags = self
            .flags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        change(&mut flags);
        self.gate
            .set_ptt_pressed(Some(flags.active && (flags.global || flags.local)));
    }

    fn global_pressed(&self, pressed: bool) {
        self.update(|flags| flags.global = pressed && flags.active);
    }

    fn local_pressed(&self, pressed: bool) {
        self.update(|flags| flags.local = pressed && flags.active);
    }

    fn configure(&self, active: bool) {
        self.update(|flags| {
            *flags = PressFlags {
                active,
                ..Default::default()
            }
        });
    }

    fn clear(&self) {
        self.update(|flags| {
            flags.global = false;
            flags.local = false;
        });
    }
}

pub(super) struct PushToTalk {
    shared: Arc<PressState>,
    platform: Option<PlatformHotkey>,
    status: String,
    enabled: bool,
}

impl PushToTalk {
    pub(super) fn start(gate: Arc<TxGate>, waker: Waker) -> Self {
        let shared = Arc::new(PressState {
            flags: Mutex::new(PressFlags::default()),
            gate,
        });
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space);
        let id = hotkey.id();
        let events = Arc::clone(&shared);
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.id == id {
                events.global_pressed(event.state == HotKeyState::Pressed);
                waker.wake();
            }
        }));
        Self {
            shared,
            platform: None,
            status: "Enable push to talk to register Ctrl + Shift + Space. The Hold to talk button is also available while enabled.".into(),
            enabled: false,
        }
    }

    pub(super) fn configure(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.shared.configure(enabled);
        self.platform.take();
        if !enabled {
            self.status = "Push to talk is off. Its global shortcut is not registered.".into();
            return;
        }
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space);
        match PlatformHotkey::register(hotkey, Arc::clone(&self.shared.gate)) {
            Ok(platform) => {
                self.platform = Some(platform);
                self.status =
                    format!("Hold {SHORTCUT} to talk, including while the window is closed.");
            }
            Err(reason) => {
                self.shared.gate.set_ptt_pressed(None);
                self.status = format!(
                    "Global push to talk unavailable: {reason}. Use the Hold to talk button in this window."
                );
            }
        }
    }

    pub(super) fn available(&self) -> bool {
        self.platform.is_some()
    }
    pub(super) fn status(&self) -> &str {
        &self.status
    }
    pub(super) fn local_pressed(&self, pressed: bool) {
        self.shared.local_pressed(pressed);
    }
    pub(super) fn clear(&self) {
        self.shared.clear();
    }
    pub(super) fn stop(&self) {
        self.shared.configure(false);
    }
}

impl Drop for PushToTalk {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(not(target_os = "windows"))]
struct PlatformHotkey {
    manager: GlobalHotKeyManager,
    hotkey: HotKey,
}

#[cfg(not(target_os = "windows"))]
impl PlatformHotkey {
    fn register(hotkey: HotKey, _gate: Arc<TxGate>) -> Result<Self, String> {
        // XWayland does not deliver global events from native Wayland clients.
        // Do not claim global support merely because DISPLAY is also set.
        #[cfg(target_os = "linux")]
        if std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE")
                .is_ok_and(|value| value.eq_ignore_ascii_case("wayland"))
        {
            return Err(
                "Wayland requires a compositor shortcut portal that this build does not implement"
                    .into(),
            );
        }
        let manager = GlobalHotKeyManager::new()
            .map_err(|_| "the desktop shortcut service could not start".to_string())?;
        manager.register(hotkey).map_err(|_| {
            "the shortcut is already in use or the desktop denied registration".to_string()
        })?;
        Ok(Self { manager, hotkey })
    }
}

#[cfg(not(target_os = "windows"))]
impl Drop for PlatformHotkey {
    fn drop(&mut self) {
        let _ = self.manager.unregister(self.hotkey);
    }
}

#[cfg(target_os = "windows")]
struct PlatformHotkey {
    thread_id: u32,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "windows")]
impl PlatformHotkey {
    fn register(hotkey: HotKey, gate: Arc<TxGate>) -> Result<Self, String> {
        use windows_sys::Win32::{
            System::Threading::GetCurrentThreadId,
            UI::WindowsAndMessaging::{
                DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, TranslateMessage,
            },
        };
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("global-ptt".into())
            .spawn(move || {
                // SAFETY: MSG is an ordinary initialized Win32 output structure.
                // PeekMessage creates this thread's queue before sharing its id.
                let mut message: MSG = unsafe { std::mem::zeroed() };
                unsafe {
                    PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
                }
                let manager = match GlobalHotKeyManager::new() {
                    Ok(manager) => manager,
                    Err(_) => {
                        let _ = ready_tx.send(Err(
                            "the Windows shortcut service could not start".to_string()
                        ));
                        return;
                    }
                };
                if manager.register(hotkey).is_err() {
                    let _ = ready_tx.send(Err(
                        "the shortcut is already in use or registration was denied".to_string(),
                    ));
                    return;
                }
                // SAFETY: GetCurrentThreadId takes no pointers and the message
                // queue above exists for the lifetime of this thread.
                let thread_id = unsafe { GetCurrentThreadId() };
                if ready_tx.send(Ok(thread_id)).is_err() {
                    return;
                }
                loop {
                    // SAFETY: messages are pumped on the same thread that owns
                    // the global-hotkey manager and its hidden Win32 window.
                    let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
                    if result <= 0 {
                        break;
                    }
                    unsafe {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                gate.set_ptt_pressed(None);
                let _ = manager.unregister(hotkey);
            })
            .map_err(|_| "the shortcut thread could not start".to_string())?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                worker: Some(worker),
            }),
            Ok(Err(reason)) => {
                let _ = worker.join();
                Err(reason)
            }
            Err(_) => {
                let _ = worker.join();
                Err("the shortcut thread stopped during startup".into())
            }
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for PlatformHotkey {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
        // SAFETY: this is the id of our own live thread and its message queue
        // was created before registration reported success.
        let sent = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        if sent != 0
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press_state() -> PressState {
        let gate = Arc::new(TxGate::default());
        gate.set_muted(false);
        gate.set_suppressed(false);
        gate.set_ptt_enabled(true);
        PressState {
            flags: Mutex::new(PressFlags {
                active: true,
                ..Default::default()
            }),
            gate,
        }
    }

    #[test]
    fn global_and_in_window_press_sources_do_not_cancel_each_other() {
        let state = press_state();
        state.global_pressed(true);
        assert!(state.gate.transmit_allowed());
        state.local_pressed(true);
        state.global_pressed(false);
        assert!(state.gate.transmit_allowed());
        state.local_pressed(false);
        assert!(!state.gate.transmit_allowed());
    }

    #[test]
    fn focus_loss_clear_releases_all_sources_without_another_ui_frame() {
        let state = press_state();
        state.global_pressed(true);
        state.local_pressed(true);
        state.clear();
        assert!(!state.gate.transmit_allowed());
        let flags = state.flags.lock().expect("test control mutex");
        assert!(!flags.global && !flags.local);
    }

    #[test]
    fn focus_clear_cannot_be_overwritten_by_an_older_inflight_publish() {
        let state = Arc::new(press_state());
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let pressing = Arc::clone(&state);
        let press = std::thread::spawn(move || {
            pressing.update(|flags| {
                flags.global = true;
                entered_tx.send(()).expect("notify test");
                release_rx.recv().expect("release test");
            });
        });
        entered_rx.recv().expect("press in progress");
        // The audio callback can still make a lock-free decision while an
        // OS control handler owns the small press-state mutex.
        assert!(!state.gate.transmit_allowed());
        let clearing = Arc::clone(&state);
        let (cleared_tx, cleared_rx) = std::sync::mpsc::sync_channel(1);
        let clear = std::thread::spawn(move || {
            clearing.clear();
            cleared_tx.send(()).expect("clear complete");
        });
        assert!(
            cleared_rx
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err()
        );
        release_tx.send(()).expect("allow publish");
        press.join().expect("press thread");
        clear.join().expect("clear thread");
        assert!(!state.gate.transmit_allowed());
    }

    #[test]
    fn inactive_shortcut_and_muting_always_win_over_a_press() {
        let state = press_state();
        state.configure(false);
        state.global_pressed(true);
        assert!(!state.gate.transmit_allowed());
        state.configure(true);
        state.gate.set_muted(true);
        state.global_pressed(true);
        assert!(!state.gate.transmit_allowed());
    }
}
