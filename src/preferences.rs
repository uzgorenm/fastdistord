//! Bounded, nonsecret application preferences, separate from credential storage.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 32 * 1024;
pub const MAX_PARTICIPANT_VOLUMES: usize = 128;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageDensity {
    #[default]
    Compact,
    Comfortable,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub density: MessageDensity,
    pub text_size: f32,
    pub light_theme: bool,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            density: MessageDensity::Compact,
            text_size: 14.0,
            light_theme: false,
        }
    }
}
impl Appearance {
    pub fn normalize(mut self) -> Self {
        self.text_size = if self.text_size.is_finite() {
            self.text_size.clamp(12.0, 20.0)
        } else {
            14.0
        };
        self
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub processing: crate::audio_processing::ProcessingOptions,
    pub appearance: Appearance,
    pub shortcuts: crate::shortcuts::ShortcutConfig,
    pub desktop_notifications: bool,
    pub check_updates: bool,
    pub participant_volumes: BTreeMap<u64, f32>,
}
impl Preferences {
    fn normalize(&mut self) {
        self.appearance = self.appearance.normalize();
        if self.shortcuts.validate().is_err() {
            self.shortcuts = Default::default();
        }
        self.participant_volumes
            .retain(|id, gain| *id != 0 && gain.is_finite());
        for gain in self.participant_volumes.values_mut() {
            *gain = gain.clamp(0.0, 1.0);
        }
        while self.participant_volumes.len() > MAX_PARTICIPANT_VOLUMES {
            self.participant_volumes.pop_last();
        }
    }
    pub fn set_participant_volume(&mut self, id: u64, gain: f32) -> bool {
        if id == 0 || !gain.is_finite() {
            return false;
        }
        if gain >= 1.0 {
            self.participant_volumes.remove(&id);
            return true;
        }
        if !self.participant_volumes.contains_key(&id)
            && self.participant_volumes.len() >= MAX_PARTICIPANT_VOLUMES
        {
            return false;
        }
        self.participant_volumes.insert(id, gain.clamp(0.0, 1.0));
        true
    }
}
fn path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|base| {
            PathBuf::from(base)
                .join("Library/Application Support/me.uzgoren.fastdistord/preferences-v1.json")
        })
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(|base| PathBuf::from(base).join("Fastdistord/preferences-v1.json"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|base| PathBuf::from(base).join(".config")))
            .map(|base| base.join("fastdistord/preferences-v1.json"))
    }
}
fn read_from(path: &Path) -> Result<Preferences> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!("Preferences exceed the size limit");
    }
    let mut value: Preferences = serde_json::from_slice(&bytes)?;
    value.normalize();
    Ok(value)
}
pub fn load() -> (Preferences, String) {
    let Some(path) = path() else {
        return (
            Preferences::default(),
            "Preferences are available for this launch only".into(),
        );
    };
    if !path.exists() {
        return (Preferences::default(), String::new());
    }
    match read_from(&path) {
        Ok(value) => (value, String::new()),
        Err(_) => (
            Preferences::default(),
            "Saved preferences could not be loaded; using defaults".into(),
        ),
    }
}
pub fn save(value: &Preferences) -> Result<()> {
    write_to(
        &path().context("Preferences directory is unavailable")?,
        value,
    )
}
fn write_to(path: &Path, value: &Preferences) -> Result<()> {
    let mut value = value.clone();
    value.normalize();
    let bytes = serde_json::to_vec(&value)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!("Preferences exceed the size limit");
    }
    std::fs::create_dir_all(path.parent().context("Invalid preferences path")?)?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.context("Preferences could not be saved")
}

#[cfg(test)]
mod tests {
    use super::*;
    // Protects privacy defaults and persisted bounds against malformed settings.
    #[test]
    fn preference_bounds_and_privacy_survive_round_trip() {
        let mut value = Preferences::default();
        assert!(!value.desktop_notifications && !value.check_updates);
        assert!(!value.processing.noise_suppression && !value.processing.automatic_gain);
        for id in 1..=MAX_PARTICIPANT_VOLUMES as u64 {
            assert!(value.set_participant_volume(id, 0.5));
        }
        assert!(!value.set_participant_volume(999, 0.5));
        assert!(!value.set_participant_volume(1, f32::NAN));
        value.appearance.text_size = 200.0;
        let dir = std::env::temp_dir().join(format!(
            "fastdistord-preferences-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("preferences.json");
        write_to(&path, &value).unwrap();
        let loaded = read_from(&path).unwrap();
        assert_eq!(loaded.participant_volumes.len(), MAX_PARTICIPANT_VOLUMES);
        assert_eq!(loaded.appearance.text_size, 20.0);
        std::fs::write(&path, vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert!(read_from(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
