//! Optional shortcuts. PTT reaches the atomic audio gate from input handlers;
//! toggles are bounded, edge-triggered events consumed by the UI coordinator.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use fastframe_shell::Waker;
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};

use crate::audio::TxGate;
use crate::shortcuts::{
    ShortcutAction, ShortcutBinding, ShortcutConfig, ShortcutKey, ShortcutModifiers, ShortcutScope,
};

const MAX_PENDING: usize = 16;

#[derive(Clone, Copy, Default, PartialEq)]
struct PressFlags {
    global: bool,
    local: bool,
    keyboard: bool,
    local_down: bool,
    active: bool,
    gate_epoch: u64,
}

struct PressState {
    flags: Mutex<PressFlags>,
    gate: Arc<TxGate>,
}

impl PressState {
    fn update(&self, change: impl FnOnce(&mut PressFlags)) {
        // Only UI/OS control handlers acquire this mutex. Audio callbacks read
        // TxGate's atomics, never this state. Publishing under the same control
        // lock prevents an old press from overtaking focus-loss revocation.
        let mut flags = self
            .flags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let observed_epoch = self.gate.epoch();
        let revoked = flags.gate_epoch != observed_epoch;
        if revoked {
            flags.global = false;
            flags.local = false;
            flags.keyboard = false;
        }
        let before = *flags;
        change(&mut flags);
        if *flags == before && !revoked {
            return;
        }
        let pressed = flags.active && (flags.global || flags.local || flags.keyboard);
        if let Some(epoch) = self
            .gate
            .set_ptt_pressed_at_epoch(Some(pressed), observed_epoch)
        {
            flags.gate_epoch = epoch;
        } else {
            // Encryption/mute/session state changed while an input handler
            // was running. Never retry an old press against the new state.
            flags.global = false;
            flags.local = false;
            flags.keyboard = false;
            self.gate.set_ptt_pressed(None);
            flags.gate_epoch = self.gate.epoch();
        }
    }

    fn global_pressed(&self, pressed: bool) {
        self.update(|flags| flags.global = pressed && flags.active);
    }

    fn local_pressed(&self, pressed: bool) {
        self.update(|flags| {
            // The on-screen hold button reports every frame. It must not
            // rearm a revoked hold until the user actually releases it.
            if flags.local_down != pressed {
                flags.local_down = pressed;
                flags.local = pressed && flags.active;
            }
        });
    }

    fn keyboard_pressed(&self, pressed: bool) {
        self.update(|flags| flags.keyboard = pressed && flags.active);
    }

    fn active(&self) -> bool {
        self.flags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
    }

    fn configure(&self, active: bool) {
        self.update(|flags| {
            *flags = PressFlags {
                active,
                local_down: flags.local_down,
                gate_epoch: flags.gate_epoch,
                ..Default::default()
            }
        });
    }

    fn clear(&self) {
        self.update(|flags| {
            flags.global = false;
            flags.local = false;
            flags.keyboard = false;
        });
    }

    fn unknown(&self) {
        let mut flags = self
            .flags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flags.global = false;
        flags.local = false;
        flags.keyboard = false;
        self.gate.set_ptt_pressed(None);
        flags.gate_epoch = self.gate.epoch();
    }
}

#[derive(Clone, Copy)]
struct Registered {
    id: u32,
    action: ShortcutAction,
    held: bool,
}

struct DispatchState {
    config: ShortcutConfig,
    global: Vec<Registered>,
    local_held: [bool; 5],
    pending: VecDeque<ShortcutAction>,
    running: bool,
}

impl Default for DispatchState {
    fn default() -> Self {
        Self {
            config: ShortcutConfig::default(),
            global: Vec::with_capacity(5),
            local_held: [false; 5],
            pending: VecDeque::with_capacity(MAX_PENDING),
            running: true,
        }
    }
}

impl DispatchState {
    fn enqueue(&mut self, action: ShortcutAction, press: &PressState) {
        if self.pending.len() < MAX_PENDING {
            self.pending.push_back(action);
        } else {
            // Never accumulate an unbounded stream of delayed toggles. Losing
            // input state also revokes PTT, even if its release is missing.
            self.pending.clear();
            press.unknown();
        }
    }

    fn global_event(&mut self, id: u32, pressed: bool, press: &PressState) {
        if !self.running {
            return;
        }
        let Some(binding) = self.global.iter_mut().find(|binding| binding.id == id) else {
            return;
        };
        if binding.held == pressed {
            return;
        }
        binding.held = pressed;
        let action = binding.action;
        if action == ShortcutAction::PushToTalk {
            press.global_pressed(pressed);
        } else if pressed {
            self.enqueue(action, press);
        }
    }

    fn local_event(
        &mut self,
        key: egui::Key,
        modifiers: egui::Modifiers,
        pressed: bool,
        repeat: bool,
        usable: bool,
        press: &PressState,
    ) -> bool {
        if !self.running {
            return false;
        }
        for (index, action) in ShortcutAction::ALL.into_iter().enumerate() {
            let binding = *self.config.binding(action);
            if binding.scope != ShortcutScope::Local || local_key(binding.key) != key {
                continue;
            }
            if !pressed {
                self.local_held[index] = false;
                if action == ShortcutAction::PushToTalk {
                    press.keyboard_pressed(false);
                }
                // Key-up is accepted with any modifiers: releasing Ctrl first
                // must never strand the PTT key in the pressed state.
                continue;
            }
            if !modifiers_match(binding.modifiers, modifiers) {
                continue;
            }
            let held = std::mem::replace(&mut self.local_held[index], true);
            if held || repeat || !usable {
                return false;
            }
            if action == ShortcutAction::PushToTalk {
                if !press.active() {
                    return false;
                }
                press.keyboard_pressed(true);
            } else {
                self.enqueue(action, press);
            }
            return true;
        }
        false
    }
}

struct Shared {
    dispatch: Mutex<DispatchState>,
    press: PressState,
}

impl Shared {
    fn clear(&self) {
        // Preserve held-key knowledge until a release. An OS repeat after
        // focus loss must not reopen the microphone.
        let _dispatch = self
            .dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.press.clear();
    }

    #[cfg(any(target_os = "windows", test))]
    fn platform_lost(&self) {
        let mut state = self
            .dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.global.clear();
        self.press.unknown();
    }
}

/// Kept under the existing name so the Hold to talk control and lifecycle
/// clearing paths stay connected while Settings adds configurable shortcuts.
pub(super) struct PushToTalk {
    shared: Arc<Shared>,
    platform: Option<PlatformHotkey>,
    status: String,
    enabled: bool,
    config: ShortcutConfig,
}

impl PushToTalk {
    pub(super) fn start(gate: Arc<TxGate>, waker: Waker) -> Self {
        let shared = Arc::new(Shared {
            dispatch: Mutex::new(DispatchState::default()),
            press: PressState {
                flags: Mutex::new(PressFlags::default()),
                gate,
            },
        });
        let events = Arc::downgrade(&shared);
        // Installing a handler does not register an OS shortcut. All bindings
        // start disabled, including when PTT alone is enabled.
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if let Some(events) = events.upgrade() {
                events
                    .dispatch
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .global_event(event.id, event.state == HotKeyState::Pressed, &events.press);
                waker.wake();
            }
        }));
        Self {
            shared, platform: None, enabled: false, config: ShortcutConfig::default(),
            status: "Keyboard shortcuts are disabled. Enable a binding in Settings; Global requires explicit opt-in.".into(),
        }
    }

    pub(super) fn configure(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            let _ = self.rebuild();
        }
    }

    /// Invalid/conflicting configurations leave the previous bindings intact.
    /// A desktop registration failure disables all requested global bindings;
    /// local bindings and the Hold to talk button remain available.
    pub(super) fn set_config(&mut self, config: &ShortcutConfig) -> Result<(), String> {
        config.validate()?;
        if self.config == *config {
            return Ok(());
        }
        self.config = *config;
        self.rebuild()
    }

    fn rebuild(&mut self) -> Result<(), String> {
        let previous = {
            let mut state = self
                .shared
                .dispatch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = std::mem::take(&mut state.global);
            state.pending.clear();
            state.config = self.config;
            self.shared.press.configure(self.enabled);
            previous
        };
        self.platform.take();
        let desired: Vec<_> = ShortcutAction::ALL
            .into_iter()
            .filter(|action| {
                self.config.binding(*action).scope == ShortcutScope::Global
                    && (*action != ShortcutAction::PushToTalk || self.enabled)
            })
            .map(|action| (action, platform_key(*self.config.binding(action))))
            .collect();
        if desired.is_empty() {
            self.status = "No global shortcuts registered. Local bindings work in this window outside text fields; Hold to talk is available while PTT is enabled.".into();
            return Ok(());
        }
        let hotkeys: Vec<_> = desired.iter().map(|(_, hotkey)| *hotkey).collect();
        match PlatformHotkey::register(hotkeys, Arc::clone(&self.shared)) {
            Ok(platform) => {
                self.platform = Some(platform);
                let mut state = self
                    .shared
                    .dispatch
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.global = desired
                    .into_iter()
                    .map(|(action, hotkey)| Registered {
                        id: hotkey.id(),
                        action,
                        held: previous.iter().any(|old| old.id == hotkey.id() && old.held),
                    })
                    .collect();
                self.status = "Configured global shortcuts registered. Local shortcuts pause while typing. PTT releases on focus loss; release and press again to talk.".into();
                Ok(())
            }
            Err(reason) => {
                self.shared.press.unknown();
                self.status = format!(
                    "Global shortcuts unavailable: {reason}. Local bindings and the Hold to talk button remain available."
                );
                Err(self.status.clone())
            }
        }
    }

    /// Call once from the foreground UI logic (or background logic to drain
    /// global events). The caller maps each action to current runtime state,
    /// e.g. Answer only targets the actual current incoming call.
    pub(super) fn poll(&self, ctx: &egui::Context) -> Vec<ShortcutAction> {
        let typing = ctx.egui_wants_keyboard_input();
        let mut state = self
            .shared
            .dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ctx.input_mut(|input| {
            let usable = input.focused
                && !typing
                && !input.viewport().minimized.unwrap_or(false)
                && !input.viewport().occluded.unwrap_or(false);
            if !usable || !modifiers_match(self.config.push_to_talk.modifiers, input.modifiers) {
                self.shared.press.keyboard_pressed(false);
            }
            input.events.retain(|event| {
                if let egui::Event::Key {
                    key,
                    physical_key,
                    modifiers,
                    pressed,
                    repeat,
                    ..
                } = event
                {
                    !state.local_event(
                        physical_key.unwrap_or(*key),
                        *modifiers,
                        *pressed,
                        *repeat,
                        usable,
                        &self.shared.press,
                    )
                } else {
                    true
                }
            });
        });
        state.pending.drain(..).collect()
    }

    pub(super) fn available(&self) -> bool {
        self.enabled
            && (self.config.push_to_talk.scope == ShortcutScope::Local
                || self
                    .shared
                    .dispatch
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .global
                    .iter()
                    .any(|binding| binding.action == ShortcutAction::PushToTalk))
    }
    pub(super) fn status(&self) -> &str {
        if self.platform.is_some()
            && self
                .shared
                .dispatch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .global
                .is_empty()
        {
            "The global shortcut service stopped. PTT was released. Reconfigure the binding to retry; local controls remain available."
        } else {
            &self.status
        }
    }
    pub(super) fn local_pressed(&self, pressed: bool) {
        self.shared.press.local_pressed(pressed);
    }
    pub(super) fn clear(&self) {
        self.shared.clear();
    }
    pub(super) fn stop(&mut self) {
        self.enabled = false;
        {
            let mut state = self
                .shared
                .dispatch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.running = false;
            state.global.clear();
            state.pending.clear();
            self.shared.press.configure(false);
        }
        self.platform.take();
    }
}

impl Drop for PushToTalk {
    fn drop(&mut self) {
        self.stop();
    }
}

fn local_key(key: ShortcutKey) -> egui::Key {
    match key {
        ShortcutKey::Space => egui::Key::Space,
        ShortcutKey::M => egui::Key::M,
        ShortcutKey::D => egui::Key::D,
        ShortcutKey::A => egui::Key::A,
        ShortcutKey::L => egui::Key::L,
        ShortcutKey::T => egui::Key::T,
        ShortcutKey::F1 => egui::Key::F1,
        ShortcutKey::F2 => egui::Key::F2,
        ShortcutKey::F3 => egui::Key::F3,
        ShortcutKey::F4 => egui::Key::F4,
        ShortcutKey::F5 => egui::Key::F5,
        ShortcutKey::F6 => egui::Key::F6,
        ShortcutKey::F7 => egui::Key::F7,
        ShortcutKey::F8 => egui::Key::F8,
        ShortcutKey::F9 => egui::Key::F9,
        ShortcutKey::F10 => egui::Key::F10,
        ShortcutKey::F11 => egui::Key::F11,
        ShortcutKey::F12 => egui::Key::F12,
    }
}

fn modifier_bits(modifiers: ShortcutModifiers) -> (bool, bool, bool, bool) {
    match modifiers {
        ShortcutModifiers::None => (false, false, false, false),
        ShortcutModifiers::Control => (true, false, false, false),
        ShortcutModifiers::Shift => (false, true, false, false),
        ShortcutModifiers::Alt => (false, false, true, false),
        ShortcutModifiers::ControlShift => (true, true, false, false),
        ShortcutModifiers::ControlAlt => (true, false, true, false),
        ShortcutModifiers::AltShift => (false, true, true, false),
        ShortcutModifiers::SuperShift => (false, true, false, true),
    }
}

fn modifiers_match(expected: ShortcutModifiers, actual: egui::Modifiers) -> bool {
    let (control, shift, alt, super_key) = modifier_bits(expected);
    // egui `command` aliases Ctrl on Windows/Linux. `mac_cmd` means the
    // physical macOS key; Super shortcuts on other platforms are global-only.
    actual.ctrl == control
        && actual.shift == shift
        && actual.alt == alt
        && actual.mac_cmd == super_key
}

fn platform_key(binding: ShortcutBinding) -> HotKey {
    let code = match binding.key {
        ShortcutKey::Space => Code::Space,
        ShortcutKey::M => Code::KeyM,
        ShortcutKey::D => Code::KeyD,
        ShortcutKey::A => Code::KeyA,
        ShortcutKey::L => Code::KeyL,
        ShortcutKey::T => Code::KeyT,
        ShortcutKey::F1 => Code::F1,
        ShortcutKey::F2 => Code::F2,
        ShortcutKey::F3 => Code::F3,
        ShortcutKey::F4 => Code::F4,
        ShortcutKey::F5 => Code::F5,
        ShortcutKey::F6 => Code::F6,
        ShortcutKey::F7 => Code::F7,
        ShortcutKey::F8 => Code::F8,
        ShortcutKey::F9 => Code::F9,
        ShortcutKey::F10 => Code::F10,
        ShortcutKey::F11 => Code::F11,
        ShortcutKey::F12 => Code::F12,
    };
    let (control, shift, alt, super_key) = modifier_bits(binding.modifiers);
    let mut modifiers = Modifiers::empty();
    if control {
        modifiers |= Modifiers::CONTROL;
    }
    if shift {
        modifiers |= Modifiers::SHIFT;
    }
    if alt {
        modifiers |= Modifiers::ALT;
    }
    if super_key {
        modifiers |= Modifiers::SUPER;
    }
    HotKey::new(Some(modifiers), code)
}

#[cfg(not(target_os = "windows"))]
struct PlatformHotkey {
    manager: GlobalHotKeyManager,
    hotkeys: Vec<HotKey>,
}

#[cfg(not(target_os = "windows"))]
impl PlatformHotkey {
    fn register(hotkeys: Vec<HotKey>, _shared: Arc<Shared>) -> Result<Self, String> {
        // XWayland cannot observe native Wayland applications. Never claim
        // support merely because DISPLAY happens to be present as well.
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
        for (index, hotkey) in hotkeys.iter().enumerate() {
            if manager.register(*hotkey).is_err() {
                for registered in &hotkeys[..index] {
                    let _ = manager.unregister(*registered);
                }
                return Err("a shortcut is already in use or the desktop denied registration; no global bindings were enabled".into());
            }
        }
        Ok(Self { manager, hotkeys })
    }
}

#[cfg(not(target_os = "windows"))]
impl Drop for PlatformHotkey {
    fn drop(&mut self) {
        for hotkey in &self.hotkeys {
            let _ = self.manager.unregister(*hotkey);
        }
    }
}

#[cfg(target_os = "windows")]
struct PlatformHotkey {
    thread_id: u32,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "windows")]
impl PlatformHotkey {
    fn register(hotkeys: Vec<HotKey>, shared: Arc<Shared>) -> Result<Self, String> {
        use windows_sys::Win32::{
            System::Threading::GetCurrentThreadId,
            UI::WindowsAndMessaging::{
                DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, TranslateMessage,
            },
        };
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new().name("global-shortcuts".into()).spawn(move || {
            // SAFETY: PeekMessage creates this thread's queue before its id is
            // shared. Dispatch starts global-hotkey's existing key-up observer.
            let mut message: MSG = unsafe { std::mem::zeroed() };
            unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE); }
            let manager = match GlobalHotKeyManager::new() {
                Ok(manager) => manager,
                Err(_) => { let _ = ready_tx.send(Err("the Windows shortcut service could not start".to_string())); return; }
            };
            for (index, hotkey) in hotkeys.iter().enumerate() {
                if manager.register(*hotkey).is_err() {
                    for registered in &hotkeys[..index] { let _ = manager.unregister(*registered); }
                    let _ = ready_tx.send(Err("a shortcut is already in use or registration was denied; no global bindings were enabled".to_string()));
                    return;
                }
            }
            // SAFETY: This is the current thread with its initialized queue.
            let thread_id = unsafe { GetCurrentThreadId() };
            if ready_tx.send(Ok(thread_id)).is_err() {
                for hotkey in &hotkeys { let _ = manager.unregister(*hotkey); }
                return;
            }
            loop {
                // SAFETY: Pump the same thread that owns the native manager.
                let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
                if result <= 0 { break; }
                unsafe { TranslateMessage(&message); DispatchMessageW(&message); }
            }
            shared.platform_lost();
            for hotkey in &hotkeys { let _ = manager.unregister(*hotkey); }
        }).map_err(|_| "the shortcut thread could not start".to_string())?;
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
        // SAFETY: This is our own worker's initialized message queue. PTT is
        // revoked before requesting shutdown, even if posting WM_QUIT fails.
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
                gate_epoch: gate.epoch(),
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
        state.keyboard_pressed(true);
        state.clear();
        assert!(!state.gate.transmit_allowed());
        let flags = state.flags.lock().expect("test control mutex");
        assert!(!flags.global && !flags.local && !flags.keyboard);
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

    #[test]
    fn repeated_button_frames_do_not_rearm_after_encryption_revocation() {
        let state = press_state();
        state.local_pressed(true);
        assert!(state.gate.transmit_allowed());
        state.gate.set_encryption_pending(true);
        state.gate.set_encryption_pending(false);
        state.local_pressed(true);
        assert!(!state.gate.transmit_allowed());
        state.local_pressed(false);
        state.local_pressed(true);
        assert!(state.gate.transmit_allowed());
    }

    #[test]
    fn encryption_revocation_wins_over_an_inflight_input_publish() {
        let state = Arc::new(press_state());
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
        let pressed_state = Arc::clone(&state);
        let pressed = std::thread::spawn(move || {
            pressed_state.update(|flags| {
                flags.global = true;
                entered_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
            });
        });
        entered_rx.recv().unwrap();
        state.gate.set_encryption_pending(true);
        state.gate.set_encryption_pending(false);
        resume_tx.send(()).unwrap();
        pressed.join().unwrap();
        assert!(!state.gate.transmit_allowed());
    }

    #[test]
    fn unchanged_ui_frames_do_not_republish_a_revoked_global_hold() {
        let state = press_state();
        let mut dispatch = DispatchState::default();
        dispatch.global.push(Registered {
            id: 9,
            action: ShortcutAction::PushToTalk,
            held: false,
        });
        dispatch.global_event(9, true, &state);
        assert!(state.gate.transmit_allowed());
        state.gate.set_remote_muted(true);
        state.gate.set_remote_muted(false);
        state.local_pressed(false);
        dispatch.global_event(9, true, &state);
        assert!(!state.gate.transmit_allowed());
        dispatch.global_event(9, false, &state);
        dispatch.global_event(9, true, &state);
        assert!(state.gate.transmit_allowed());
    }

    #[test]
    fn enabling_ptt_alone_never_registers_a_global_shortcut() {
        let gate = Arc::new(TxGate::default());
        let mut shortcuts = PushToTalk::start(gate, Waker::default());
        shortcuts.configure(true);
        assert!(shortcuts.platform.is_none());
        let mut config = ShortcutConfig::default();
        config.push_to_talk.scope = ShortcutScope::Local;
        shortcuts.set_config(&config).unwrap();
        assert!(shortcuts.platform.is_none());
        assert!(shortcuts.available());
        shortcuts.stop();
        shortcuts.local_pressed(true);
        assert!(!shortcuts.shared.press.gate.transmit_allowed());
    }

    #[test]
    fn reconfiguration_revokes_a_held_button_and_invalid_config_keeps_previous_bindings() {
        let gate = press_state().gate;
        let mut shortcuts = PushToTalk::start(Arc::clone(&gate), Waker::default());
        shortcuts.configure(true);
        shortcuts.local_pressed(true);
        assert!(gate.transmit_allowed());
        let mut config = ShortcutConfig::default();
        config.push_to_talk.scope = ShortcutScope::Local;
        shortcuts.set_config(&config).unwrap();
        shortcuts.local_pressed(true);
        assert!(!gate.transmit_allowed());
        shortcuts.local_pressed(false);
        shortcuts.local_pressed(true);
        assert!(gate.transmit_allowed());
        let mut invalid = config;
        invalid.mute = config.push_to_talk;
        assert!(shortcuts.set_config(&invalid).is_err());
        assert_eq!(shortcuts.config, config);
        shortcuts.stop();
        assert!(!gate.transmit_allowed());
    }

    #[test]
    fn global_repeat_does_not_rearm_after_focus_loss_and_stale_ids_are_ignored() {
        let state = press_state();
        let mut dispatch = DispatchState::default();
        dispatch.global.push(Registered {
            id: 17,
            action: ShortcutAction::PushToTalk,
            held: false,
        });
        dispatch.global_event(17, true, &state);
        assert!(state.gate.transmit_allowed());
        state.clear();
        dispatch.global_event(17, true, &state);
        assert!(!state.gate.transmit_allowed());
        dispatch.global_event(17, false, &state);
        dispatch.global_event(17, true, &state);
        assert!(state.gate.transmit_allowed());
        state.configure(true);
        dispatch.global.clear();
        dispatch.global_event(17, true, &state);
        assert!(!state.gate.transmit_allowed());
    }

    #[test]
    fn typing_repeat_and_lost_modifier_cannot_open_local_ptt() {
        let state = press_state();
        let mut dispatch = DispatchState::default();
        dispatch.config.push_to_talk.scope = ShortcutScope::Local;
        let modifiers = egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        dispatch.local_event(egui::Key::Space, modifiers, true, false, false, &state);
        assert!(!state.gate.transmit_allowed());
        dispatch.local_event(egui::Key::Space, modifiers, true, true, true, &state);
        assert!(!state.gate.transmit_allowed());
        dispatch.local_event(egui::Key::Space, modifiers, false, false, true, &state);
        dispatch.local_event(egui::Key::Space, modifiers, true, false, true, &state);
        assert!(state.gate.transmit_allowed());
        dispatch.local_event(
            egui::Key::Space,
            egui::Modifiers::default(),
            false,
            false,
            true,
            &state,
        );
        assert!(!state.gate.transmit_allowed());
    }

    #[test]
    fn toggles_are_once_per_press_and_overflow_is_bounded_and_fail_closed() {
        let press = press_state();
        let mut dispatch = DispatchState::default();
        dispatch.global.push(Registered {
            id: 11,
            action: ShortcutAction::Mute,
            held: false,
        });
        dispatch.global_event(11, true, &press);
        dispatch.global_event(11, true, &press);
        assert_eq!(dispatch.pending.pop_front(), Some(ShortcutAction::Mute));
        assert!(dispatch.pending.is_empty());
        press.global_pressed(true);
        for _ in 0..=MAX_PENDING {
            dispatch.global_event(11, false, &press);
            dispatch.global_event(11, true, &press);
        }
        assert!(dispatch.pending.is_empty());
        assert!(!press.gate.transmit_allowed());
    }

    #[test]
    fn lost_platform_and_stop_reject_inflight_global_presses() {
        let shared = Shared {
            dispatch: Mutex::new(DispatchState::default()),
            press: press_state(),
        };
        shared.dispatch.lock().unwrap().global.push(Registered {
            id: 6,
            action: ShortcutAction::PushToTalk,
            held: false,
        });
        shared
            .dispatch
            .lock()
            .unwrap()
            .global_event(6, true, &shared.press);
        assert!(shared.press.gate.transmit_allowed());
        shared.platform_lost();
        shared
            .dispatch
            .lock()
            .unwrap()
            .global_event(6, true, &shared.press);
        assert!(!shared.press.gate.transmit_allowed());
        let mut dispatch = shared.dispatch.lock().unwrap();
        dispatch.running = false;
        dispatch.global.push(Registered {
            id: 6,
            action: ShortcutAction::PushToTalk,
            held: false,
        });
        dispatch.global_event(6, true, &shared.press);
        assert!(!shared.press.gate.transmit_allowed());
    }
}
