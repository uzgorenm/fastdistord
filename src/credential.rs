//! OS-backed optional storage; no plaintext credential file fallback.
use anyhow::{Result, bail};
#[cfg(target_os = "macos")]
const SERVICE: &str = "fastdistord.personal-account";
#[cfg(target_os = "macos")]
const ACCOUNT: &str = "default";
pub fn store(token: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, token.as_bytes())
            .map_err(|_| anyhow::anyhow!("Could not save credential in macOS Keychain"))
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
            security_framework::passwords::get_generic_password(SERVICE, ACCOUNT).map_err(
                |_| anyhow::anyhow!("No saved credential is available in macOS Keychain"),
            )?,
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
            Err(_) => bail!("Could not remove saved credential from macOS Keychain"),
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
    preference_path()
        .and_then(|p| std::fs::read(p).ok())
        .is_some_and(|v| v == b"remember-v1\n")
}
fn set_remembered(enabled: bool) -> Result<()> {
    let path =
        preference_path().ok_or_else(|| anyhow::anyhow!("Remember me is available on macOS"))?;
    write_preference(&path, enabled)
}
fn write_preference(path: &std::path::Path, enabled: bool) -> Result<()> {
    if enabled {
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("Login preference location unavailable"))?,
        )
        .map_err(|_| anyhow::anyhow!("Could not save Remember me preference"))?;
        std::fs::write(path, b"remember-v1\n")
            .map_err(|_| anyhow::anyhow!("Could not save Remember me preference"))?;
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => bail!("Could not disable automatic login preference"),
        }
    }
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
        assert!(!path.exists());
        assert!(save_with("synthetic-only", |_| Ok(()), |_| bail!("preference denied")).is_err());
    }
}
