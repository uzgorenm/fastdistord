//! Native authorization only; no TCC database access or permission changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum Permission {
    Unknown,
    NotDetermined,
    Authorized,
    Denied,
    Restricted,
    #[cfg(not(target_os = "macos"))]
    NotApplicable,
}
impl Permission {
    pub fn usable(self) -> bool {
        match self {
            Self::Authorized => true,
            #[cfg(not(target_os = "macos"))]
            Self::NotApplicable => true,
            _ => false,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Microphone permission unavailable",
            Self::NotDetermined => "Microphone permission not requested",
            Self::Authorized => "Microphone permission allowed",
            Self::Denied => "Microphone permission denied",
            Self::Restricted => "Microphone permission restricted",
            #[cfg(not(target_os = "macos"))]
            Self::NotApplicable => "Device access checked when opening audio",
        }
    }
}
#[cfg(target_os = "macos")]
pub fn status() -> Permission {
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
    unsafe {
        let Some(media) = AVMediaTypeAudio else {
            return Permission::Unknown;
        };
        match AVCaptureDevice::authorizationStatusForMediaType(media) {
            AVAuthorizationStatus::NotDetermined => Permission::NotDetermined,
            AVAuthorizationStatus::Authorized => Permission::Authorized,
            AVAuthorizationStatus::Denied => Permission::Denied,
            AVAuthorizationStatus::Restricted => Permission::Restricted,
            _ => Permission::Unknown,
        }
    }
}
#[cfg(not(target_os = "macos"))]
pub fn status() -> Permission {
    Permission::NotApplicable
}
#[cfg(target_os = "macos")]
fn begin_request() -> anyhow::Result<tokio::sync::oneshot::Receiver<bool>> {
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};
    let (tx, rx) = tokio::sync::oneshot::channel();
    let sender = std::sync::Mutex::new(Some(tx));
    let block = block2::RcBlock::new(move |allowed: objc2::runtime::Bool| {
        if let Ok(mut sender) = sender.lock()
            && let Some(tx) = sender.take()
        {
            let _ = tx.send(allowed.as_bool());
        }
    });
    unsafe {
        let media = AVMediaTypeAudio
            .ok_or_else(|| anyhow::anyhow!("Microphone authorization unavailable"))?;
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block);
    }
    // AVFoundation copies the completion block; this scope ends before await.
    Ok(rx)
}
/// Called only for the current explicit Join/Call once encryption permits audio.
pub async fn request_for_join() -> anyhow::Result<Permission> {
    #[cfg(target_os = "macos")]
    if status() == Permission::NotDetermined {
        let rx = begin_request()?;
        tokio::time::timeout(std::time::Duration::from_secs(60), rx).await
            .map_err(|_| anyhow::anyhow!("Microphone permission is still awaiting your response. Join again after responding."))?
            .map_err(|_| anyhow::anyhow!("Microphone permission response unavailable"))?;
    }
    let permission = status();
    if permission.usable() {
        Ok(permission)
    } else {
        anyhow::bail!(
            "{}. Open System Settings → Privacy & Security → Microphone, then explicitly Join again.",
            permission.label()
        )
    }
}
