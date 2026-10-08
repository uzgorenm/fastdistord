//! OS-backed optional storage; no plaintext credential file fallback.
use anyhow::Result;
#[cfg(any(not(target_os = "macos"), test))]
use anyhow::bail;
#[cfg(target_os = "macos")]
const SERVICE: &str = "fastdistord.personal-account";
#[cfg(target_os = "macos")]
const ACCOUNT: &str = "default";
#[cfg(any(target_os = "macos", test))]
fn keychain_error(action: &str, code: i32) -> anyhow::Error {
    let reason = match code {
        -25300 => "no saved login was found",
        -128 => "access was canceled",
        -25293 => "access was denied or authentication failed",
        -25308 => "Keychain is locked or requires your approval",
        -25291 => "Keychain is unavailable",
        _ => "Keychain operation failed",
    };
    anyhow::anyhow!(
        "Could not {action}: {reason} (macOS status {code}). Approve the app's Keychain prompt yourself or unlock your login Keychain, then use Connect from Keychain. Do not delete a valid saved login."
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preference {
    Unset,
    Remembered,
    Disabled,
    Unavailable,
}
fn preference_from(bytes: Option<&[u8]>) -> Preference {
    match bytes {
        None => Preference::Unset,
        Some(b"remember-v1\n") => Preference::Remembered,
        Some(b"session-only-v1\n") => Preference::Disabled,
        Some(_) => Preference::Unavailable,
    }
}
pub fn preference() -> Preference {
    let Some(path) = preference_path() else {
        return Preference::Disabled;
    };
    match std::fs::read(path) {
        Ok(bytes) => preference_from(Some(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Preference::Unset,
        Err(_) => Preference::Unavailable,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Found,
    Missing,
    Unavailable(i32),
}
/// Attribute-only query. No password data, references or authentication UI requested.
pub fn presence() -> Presence {
    #[cfg(target_os = "macos")]
    {
        use security_framework::item::{ItemClass, ItemSearchOptions};
        match ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(SERVICE)
            .account(ACCOUNT)
            .load_attributes(true)
            .load_data(false)
            .load_refs(false)
            .skip_authenticated_items(true)
            .search()
        {
            Ok(items) if !items.is_empty() => Presence::Found,
            Ok(_) => Presence::Missing,
            Err(e) if e.code() == -25300 => Presence::Missing,
            Err(e) => Presence::Unavailable(e.code()),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Presence::Missing
    }
}
pub fn should_restore(preference: Preference, presence: Presence) -> bool {
    preference == Preference::Remembered
        || (preference == Preference::Unset && presence == Presence::Found)
}
pub fn storage_status() -> String {
    let preference = preference();
    let presence = presence();
    format!(
        "Backend: {} · startup preference: {preference:?} · saved login metadata: {presence:?}. Metadata lookup does not request password data; skipped protected items may appear absent.",
        if cfg!(target_os = "macos") {
            "macOS Security framework Keychain"
        } else {
            "session only"
        }
    )
}
pub fn store(token: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, token.as_bytes())
            .map_err(|e| keychain_error("save login", e.code()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = token;
        bail!("Credential persistence is only available with macOS Keychain");
    }
}
#[cfg(target_os = "macos")]
pub fn load() -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        let bytes = zeroize::Zeroizing::new(
            security_framework::passwords::get_generic_password(SERVICE, ACCOUNT)
                .map_err(|e| keychain_error("load saved login", e.code()))?,
        );
        std::str::from_utf8(&bytes)
            .map(str::to_owned)
            .map_err(|_| anyhow::anyhow!("Saved credential is invalid"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        bail!("Saved credentials are only available on macOS");
    }
}
pub fn forget() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        match security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == -25300 => Ok(()),
            Err(e) => Err(keychain_error("remove saved login", e.code())),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

// Nonsecret opt-in only. Credentials remain exclusively in the OS Keychain.
fn preference_path() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| {
            std::path::PathBuf::from(home)
                .join("Library/Application Support/me.uzgoren.fastdistord/remember-login")
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}
pub fn remembered() -> bool {
    preference() == Preference::Remembered
}
pub fn enable_startup() -> Result<()> {
    set_remembered(true)
}
fn set_remembered(enabled: bool) -> Result<()> {
    let path =
        preference_path().ok_or_else(|| anyhow::anyhow!("Remember me is available on macOS"))?;
    write_preference(&path, enabled)
}
fn write_preference(path: &std::path::Path, enabled: bool) -> Result<()> {
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("Login preference location unavailable"))?,
    )
    .map_err(|_| anyhow::anyhow!("Could not save Remember me preference"))?;
    // Atomic replacement: a partial write cannot turn a valid opt-in into an absent file.
    let temporary = path.with_extension("pending");
    std::fs::write(
        &temporary,
        if enabled {
            b"remember-v1\n".as_slice()
        } else {
            b"session-only-v1\n".as_slice()
        },
    )
    .map_err(|_| anyhow::anyhow!("Could not save Remember me preference"))?;
    let file = std::fs::File::open(&temporary)
        .map_err(|_| anyhow::anyhow!("Could not flush Remember me preference"))?;
    file.sync_all()
        .map_err(|_| anyhow::anyhow!("Could not flush Remember me preference"))?;
    std::fs::rename(&temporary, path)
        .map_err(|_| anyhow::anyhow!("Could not commit Remember me preference"))?;
    Ok(())
}
fn save_with(
    token: &str,
    save: impl FnOnce(&str) -> Result<()>,
    opt_in: impl FnOnce(bool) -> Result<()>,
) -> Result<()> {
    save(token)?;
    opt_in(true)
}
pub fn remember(token: &str) -> Result<()> {
    save_with(token, store, set_remembered)
}
pub fn disable_startup() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        set_remembered(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}
fn forget_with(
    disable: impl FnOnce() -> Result<()>,
    remove: impl FnOnce() -> Result<()>,
) -> Result<()> {
    // Attempt both even if one fails: a denied deletion must not keep startup enabled.
    let preference = disable();
    let credential = remove();
    preference.and(credential)
}
pub fn sign_out() -> Result<()> {
    forget_with(disable_startup, forget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    #[test]
    fn legacy_remembered_items_restore_but_explicit_session_only_never_migrates() {
        assert!(should_restore(Preference::Unset, Presence::Found));
        assert!(!should_restore(Preference::Disabled, Presence::Found));
        assert!(!should_restore(Preference::Unset, Presence::Missing));
        assert!(!should_restore(Preference::Unavailable, Presence::Found));
        assert!(should_restore(
            Preference::Remembered,
            Presence::Unavailable(-25308)
        ));
        assert_eq!(preference_from(None), Preference::Unset);
        assert_eq!(
            preference_from(Some(b"session-only-v1\n")),
            Preference::Disabled
        );
        assert_eq!(
            preference_from(Some(b"remember-v1\n")),
            Preference::Remembered
        );
    }
    #[test]
    fn load_errors_distinguish_absence_denial_and_cancellation_without_secret_values() {
        for (code, reason) in [
            (-25300, "no saved login"),
            (-128, "canceled"),
            (-25293, "denied"),
            (-25308, "locked"),
        ] {
            let text = keychain_error("load saved login", code).to_string();
            assert!(text.contains(reason));
            assert!(text.contains(&format!("status {code}")));
        }
    }
    #[test]
    fn remembered_lifecycle_survives_quit_and_explicit_logout_clears() {
        let secret = RefCell::new(None);
        let opted_in = RefCell::new(false);
        save_with(
            "synthetic-only",
            |v| {
                *secret.borrow_mut() = Some(v.to_owned());
                Ok(())
            },
            |v| {
                *opted_in.borrow_mut() = v;
                Ok(())
            },
        )
        .unwrap();
        // A new process uses the persisted opt-in and store; quitting calls neither deletion.
        assert!(*opted_in.borrow());
        assert_eq!(secret.borrow().as_deref(), Some("synthetic-only"));
        forget_with(
            || {
                *opted_in.borrow_mut() = false;
                Ok(())
            },
            || {
                *secret.borrow_mut() = None;
                Ok(())
            },
        )
        .unwrap();
        assert!(!*opted_in.borrow() && secret.borrow().is_none());
    }
    #[test]
    fn denied_save_cannot_enable_startup_and_denied_delete_disables_it() {
        let enabled = RefCell::new(false);
        assert!(
            save_with(
                "synthetic-only",
                |_| bail!("Keychain denied"),
                |v| {
                    *enabled.borrow_mut() = v;
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(!*enabled.borrow());
        *enabled.borrow_mut() = true;
        assert!(
            forget_with(
                || {
                    *enabled.borrow_mut() = false;
                    Ok(())
                },
                || bail!("Keychain denied")
            )
            .is_err()
        );
        assert!(!*enabled.borrow());
    }
    #[test]
    fn preference_contains_only_opt_in_and_cancel_does_not_save() {
        let path =
            std::env::temp_dir().join(format!("fastdistord-login-pref-{}", std::process::id()));
        write_preference(&path, true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"remember-v1\n");
        write_preference(&path, false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"session-only-v1\n");
        std::fs::remove_file(&path).unwrap();
        assert!(save_with("synthetic-only", |_| Ok(()), |_| bail!("preference denied")).is_err());
    }
}
