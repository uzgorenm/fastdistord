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
        String::from_utf8(bytes.to_vec())
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
