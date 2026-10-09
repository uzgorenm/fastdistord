//! A fixed, nonsecret shortcut configuration. Loading preferences never opts
//! an unconfigured action into a system-wide keyboard hook.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShortcutAction {
    Mute,
    Deafen,
    Answer,
    Leave,
    PushToTalk,
}

impl ShortcutAction {
    pub const ALL: [Self; 5] = [
        Self::Mute,
        Self::Deafen,
        Self::Answer,
        Self::Leave,
        Self::PushToTalk,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mute => "Mute / unmute",
            Self::Deafen => "Deafen / undeafen",
            Self::Answer => "Answer incoming call",
            Self::Leave => "Leave call",
            Self::PushToTalk => "Hold to talk",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShortcutScope {
    #[default]
    Disabled,
    Local,
    Global,
}

impl ShortcutScope {
    pub const ALL: [Self; 3] = [Self::Disabled, Self::Local, Self::Global];

    pub fn label(self) -> &'static str {
        match self {
            Self::Disabled => "Disabled",
            Self::Local => "This window",
            Self::Global => "Global (opt in)",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShortcutKey {
    Space,
    M,
    D,
    A,
    L,
    T,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

impl ShortcutKey {
    pub const ALL: [Self; 18] = [
        Self::Space,
        Self::M,
        Self::D,
        Self::A,
        Self::L,
        Self::T,
        Self::F1,
        Self::F2,
        Self::F3,
        Self::F4,
        Self::F5,
        Self::F6,
        Self::F7,
        Self::F8,
        Self::F9,
        Self::F10,
        Self::F11,
        Self::F12,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Space => "Space",
            Self::M => "M",
            Self::D => "D",
            Self::A => "A",
            Self::L => "L",
            Self::T => "T",
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::F5 => "F5",
            Self::F6 => "F6",
            Self::F7 => "F7",
            Self::F8 => "F8",
            Self::F9 => "F9",
            Self::F10 => "F10",
            Self::F11 => "F11",
            Self::F12 => "F12",
        }
    }
}

/// Modifiers mean the same physical keys on every platform. Command is not
/// silently substituted for Control on macOS.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShortcutModifiers {
    None,
    Control,
    Shift,
    Alt,
    ControlShift,
    ControlAlt,
    AltShift,
    SuperShift,
}

impl ShortcutModifiers {
    pub const ALL: [Self; 8] = [
        Self::None,
        Self::Control,
        Self::Shift,
        Self::Alt,
        Self::ControlShift,
        Self::ControlAlt,
        Self::AltShift,
        Self::SuperShift,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Control => "Ctrl",
            Self::Shift => "Shift",
            Self::Alt => "Alt / Option",
            Self::ControlShift => "Ctrl + Shift",
            Self::ControlAlt => "Ctrl + Alt",
            Self::AltShift => "Alt + Shift",
            Self::SuperShift => "Super / Command + Shift",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShortcutBinding {
    pub scope: ShortcutScope,
    pub key: ShortcutKey,
    pub modifiers: ShortcutModifiers,
}

impl ShortcutBinding {
    fn disabled(key: ShortcutKey) -> Self {
        Self {
            scope: ShortcutScope::Disabled,
            key,
            modifiers: ShortcutModifiers::ControlShift,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShortcutConfig {
    pub mute: ShortcutBinding,
    pub deafen: ShortcutBinding,
    pub answer: ShortcutBinding,
    pub leave: ShortcutBinding,
    pub push_to_talk: ShortcutBinding,
}

impl Default for ShortcutConfig {
    fn default() -> Self {
        Self {
            mute: ShortcutBinding::disabled(ShortcutKey::M),
            deafen: ShortcutBinding::disabled(ShortcutKey::D),
            answer: ShortcutBinding::disabled(ShortcutKey::A),
            leave: ShortcutBinding::disabled(ShortcutKey::L),
            push_to_talk: ShortcutBinding::disabled(ShortcutKey::Space),
        }
    }
}

impl ShortcutConfig {
    pub fn binding(&self, action: ShortcutAction) -> &ShortcutBinding {
        match action {
            ShortcutAction::Mute => &self.mute,
            ShortcutAction::Deafen => &self.deafen,
            ShortcutAction::Answer => &self.answer,
            ShortcutAction::Leave => &self.leave,
            ShortcutAction::PushToTalk => &self.push_to_talk,
        }
    }

    pub fn binding_mut(&mut self, action: ShortcutAction) -> &mut ShortcutBinding {
        match action {
            ShortcutAction::Mute => &mut self.mute,
            ShortcutAction::Deafen => &mut self.deafen,
            ShortcutAction::Answer => &mut self.answer,
            ShortcutAction::Leave => &mut self.leave,
            ShortcutAction::PushToTalk => &mut self.push_to_talk,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        for (index, action) in ShortcutAction::ALL.iter().enumerate() {
            let binding = self.binding(*action);
            if binding.scope == ShortcutScope::Disabled {
                continue;
            }
            if !cfg!(target_os = "macos")
                && binding.scope == ShortcutScope::Local
                && binding.modifiers == ShortcutModifiers::SuperShift
            {
                return Err("Super-key local shortcuts are not supported on this platform; choose Ctrl, Alt or Shift.".into());
            }
            // Capturing ordinary typing system-wide is too easy to enable by
            // accident. Function keys can be bound without a modifier.
            if binding.scope == ShortcutScope::Global
                && binding.modifiers == ShortcutModifiers::None
                && matches!(
                    binding.key,
                    ShortcutKey::Space
                        | ShortcutKey::M
                        | ShortcutKey::D
                        | ShortcutKey::A
                        | ShortcutKey::L
                        | ShortcutKey::T
                )
            {
                return Err(format!(
                    "{} needs a modifier for a global letter or Space shortcut.",
                    action.label()
                ));
            }
            for other_action in &ShortcutAction::ALL[..index] {
                let other = self.binding(*other_action);
                if other.scope != ShortcutScope::Disabled
                    && binding.key == other.key
                    && binding.modifiers == other.modifiers
                {
                    return Err(format!(
                        "{} and {} use the same shortcut. Choose different keys or disable one.",
                        action.label(),
                        other_action.label()
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_preferences_do_not_gain_global_shortcuts() {
        let config: ShortcutConfig = serde_json::from_str("{}").unwrap();
        assert!(
            ShortcutAction::ALL
                .iter()
                .all(|action| config.binding(*action).scope == ShortcutScope::Disabled)
        );
        let roundtrip: ShortcutConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(roundtrip, config);
        assert!(
            serde_json::from_str::<ShortcutConfig>(r#"{"bindings":[{"scope":"Global"}]}"#).is_err()
        );
    }

    #[test]
    fn conflicts_across_local_and_global_scopes_are_rejected() {
        let mut config = ShortcutConfig::default();
        config.mute.scope = ShortcutScope::Local;
        config.deafen = config.mute;
        config.deafen.scope = ShortcutScope::Global;
        assert!(config.validate().is_err());
        config.deafen.scope = ShortcutScope::Disabled;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn global_typing_keys_need_modifiers_but_local_bindings_do_not() {
        let mut config = ShortcutConfig::default();
        config.mute.scope = ShortcutScope::Global;
        config.mute.modifiers = ShortcutModifiers::None;
        assert!(config.validate().is_err());
        config.mute.key = ShortcutKey::F8;
        assert!(config.validate().is_ok());
        config.mute.key = ShortcutKey::M;
        config.mute.scope = ShortcutScope::Local;
        assert!(config.validate().is_ok());
    }
}
