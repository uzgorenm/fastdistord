//! Optional, generic incoming-call alerts. Call identities and caller names are
//! never sent to the OS; the runtime owns the actual call prompt and ringtone.
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Permission {
    #[default]
    Unknown,
    Granted,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Denied,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Unsupported,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Unavailable,
}

impl Permission {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Desktop notification permission has not been requested",
            Self::Granted => "Desktop notifications are allowed",
            Self::Denied => "Notifications denied; change permission in System Settings",
            Self::Unsupported => "Desktop notifications are not supported on this platform",
            Self::Unavailable => "Desktop notifications unavailable; use the packaged Mac app",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Disabled,
    Stale,
    Scheduled,
    Unavailable,
}

impl Delivery {
    pub fn label(self) -> &'static str {
        match self {
            Self::Disabled => "Desktop notifications are off",
            Self::Stale => "Incoming call has already changed",
            Self::Scheduled => "Incoming-call notification submitted to macOS",
            Self::Unavailable => "Could not submit the desktop notification",
        }
    }
}

#[derive(Default)]
struct Policy {
    enabled: bool,
    permission: Permission,
    permission_epoch: u64,
    active: Option<u64>,
    newest_generation: Option<u64>,
}

impl Policy {
    fn begin_incoming(&mut self, generation: u64) -> Result<Option<u64>, Delivery> {
        if !self.enabled || self.permission != Permission::Granted {
            return Err(Delivery::Disabled);
        }
        if self
            .newest_generation
            .is_some_and(|last| generation <= last)
        {
            return Err(Delivery::Stale);
        }
        self.newest_generation = Some(generation);
        Ok(self.active.replace(generation))
    }

    fn is_current(&self, generation: u64) -> bool {
        self.enabled && self.permission == Permission::Granted && self.active == Some(generation)
    }
}

#[async_trait::async_trait]
trait Backend: Send + Sync {
    async fn request_permission(&self) -> Permission;
    async fn refresh_permission(&self) -> Permission;
    async fn post(&self, generation: u64, gate: Arc<Mutex<Policy>>) -> bool;
    fn clear(&self, generation: u64);
}

struct Native;

/// Clone this handle into short runtime tasks. Construction never contacts the
/// OS, requests permission, or posts an alert. Keep one service per app session.
#[derive(Clone)]
pub struct NotificationService {
    gate: Arc<Mutex<Policy>>,
    backend: Arc<dyn Backend>,
}

impl Default for NotificationService {
    fn default() -> Self {
        Self {
            gate: Arc::default(),
            backend: Arc::new(Native),
        }
    }
}

impl NotificationService {
    /// Invoke exclusively for the explicit RequestNotificationPermission command.
    /// Granting OS permission does not enable the saved preference by itself.
    pub async fn request_permission(&self) -> Permission {
        let epoch = self.begin_permission_check();
        let permission = self.backend.request_permission().await;
        self.finish_permission_check(epoch, permission)
    }

    /// Read existing OS authorization without displaying a permission prompt.
    /// Use to restore a saved opt-in; only Granted can enable delivery.
    pub async fn refresh_permission(&self) -> Permission {
        let epoch = self.begin_permission_check();
        let permission = self.backend.refresh_permission().await;
        self.finish_permission_check(epoch, permission)
    }

    fn begin_permission_check(&self) -> u64 {
        let mut policy = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        policy.permission_epoch = policy.permission_epoch.wrapping_add(1);
        policy.permission_epoch
    }

    fn finish_permission_check(&self, epoch: u64, permission: Permission) -> Permission {
        let mut policy = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if policy.permission_epoch == epoch {
            policy.permission = permission;
            if permission != Permission::Granted {
                policy.enabled = false;
                if let Some(generation) = policy.active.take() {
                    self.backend.clear(generation);
                }
            }
        }
        policy.permission
    }

    /// Returns the actual enabled value. Enabling before authorization fails closed.
    /// A saved preference first needs a read-only refresh of OS authorization.
    pub fn set_enabled(&self, enabled: bool) -> bool {
        let mut policy = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        policy.enabled = enabled && policy.permission == Permission::Granted;
        if !policy.enabled {
            policy.permission_epoch = policy.permission_epoch.wrapping_add(1);
            if let Some(generation) = policy.active.take() {
                self.backend.clear(generation);
            }
        }
        policy.enabled
    }

    /// Runtime supplies a strictly increasing, session-local generation only for
    /// real incoming events. Never use a channel/user ID or a synthetic call here.
    pub async fn incoming(&self, generation: u64) -> Delivery {
        {
            let mut policy = self.gate.lock().unwrap_or_else(|e| e.into_inner());
            match policy.begin_incoming(generation) {
                Ok(Some(old)) => self.backend.clear(old),
                Ok(None) => {}
                Err(result) => return result,
            }
        }
        let posted = self.backend.post(generation, self.gate.clone()).await;
        let mut policy = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if !policy.is_current(generation) {
            self.backend.clear(generation);
            return Delivery::Stale;
        }
        if posted {
            Delivery::Scheduled
        } else {
            policy.active = None;
            self.backend.clear(generation);
            Delivery::Unavailable
        }
    }

    /// Clear on answer, decline, remote cancellation, leaving, and expiry.
    /// An old completion cannot clear a newer call's notification.
    pub fn clear(&self, generation: u64) {
        let mut policy = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        // A cancellation may arrive before a spawned incoming task is polled.
        // Record the generation even when it has no active OS request yet.
        policy.newest_generation = Some(
            policy
                .newest_generation
                .map_or(generation, |last| last.max(generation)),
        );
        if policy.active == Some(generation) {
            policy.active = None;
        }
        self.backend.clear(generation);
    }

    /// Clear on logout and shutdown. Keep generation numbering monotonic.
    pub fn clear_all(&self) {
        let mut policy = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        policy.permission_epoch = policy.permission_epoch.wrapping_add(1);
        if let Some(generation) = policy.active.take() {
            self.backend.clear(generation);
        }
    }
}

#[async_trait::async_trait]
impl Backend for Native {
    async fn request_permission(&self) -> Permission {
        #[cfg(target_os = "macos")]
        {
            let Some(rx) = macos::begin_permission() else {
                return Permission::Unavailable;
            };
            match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
                Ok(Ok(permission)) => permission,
                _ => Permission::Unavailable,
            }
        }
        #[cfg(not(target_os = "macos"))]
        Permission::Unsupported
    }

    async fn refresh_permission(&self) -> Permission {
        #[cfg(target_os = "macos")]
        {
            let Some(rx) = macos::begin_status() else {
                return Permission::Unavailable;
            };
            match tokio::time::timeout(std::time::Duration::from_secs(3), rx).await {
                Ok(Ok(permission)) => permission,
                _ => Permission::Unavailable,
            }
        }
        #[cfg(not(target_os = "macos"))]
        Permission::Unsupported
    }

    async fn post(&self, generation: u64, gate: Arc<Mutex<Policy>>) -> bool {
        #[cfg(target_os = "macos")]
        {
            let Some(rx) = macos::begin_post(generation, gate) else {
                return false;
            };
            matches!(
                tokio::time::timeout(std::time::Duration::from_secs(3), rx).await,
                Ok(Ok(true))
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (generation, gate);
            false
        }
    }

    fn clear(&self, generation: u64) {
        #[cfg(target_os = "macos")]
        macos::clear(generation);
        #[cfg(not(target_os = "macos"))]
        let _ = generation;
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use block2::RcBlock;
    use objc2::{
        msg_send,
        rc::Retained,
        runtime::{AnyClass, AnyObject, Bool},
    };
    use objc2_foundation::{NSArray, NSBundle, NSError, NSString};
    use tokio::sync::oneshot;

    #[link(name = "UserNotifications", kind = "framework")]
    unsafe extern "C" {}

    fn center() -> Option<Retained<AnyObject>> {
        // UserNotifications can abort in an unbundled CLI process. Refuse that
        // context before asking for the singleton. No OS calls run in tests.
        let bundle = NSBundle::mainBundle();
        if bundle.bundleIdentifier()?.to_string() != "me.uzgoren.fastdistord"
            || !bundle.bundlePath().to_string().ends_with(".app")
        {
            return None;
        }
        let class = AnyClass::get(c"UNUserNotificationCenter")?;
        // SAFETY: UserNotifications framework is linked; this is the documented
        // class getter returning a retained Objective-C object through msg_send.
        unsafe { msg_send![class, currentNotificationCenter] }
    }

    pub(super) fn begin_permission() -> Option<oneshot::Receiver<Permission>> {
        let center = center()?;
        let (tx, rx) = oneshot::channel();
        let sender = Mutex::new(Some(tx));
        let block = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            let permission = if !error.is_null() {
                Permission::Unavailable
            } else if granted.as_bool() {
                Permission::Granted
            } else {
                Permission::Denied
            };
            if let Ok(mut sender) = sender.lock()
                && let Some(tx) = sender.take()
            {
                let _ = tx.send(permission);
            }
        });
        // SAFETY: Exact Apple selector and block signature. Alert is bit 2;
        // sounds and badges are deliberately excluded. Apple copies the block.
        unsafe {
            let _: () = msg_send![&*center, requestAuthorizationWithOptions: 4usize, completionHandler: &*block];
        }
        Some(rx)
    }

    pub(super) fn begin_status() -> Option<oneshot::Receiver<Permission>> {
        let center = center()?;
        let (tx, rx) = oneshot::channel();
        let sender = Mutex::new(Some(tx));
        let block = RcBlock::new(move |settings: *mut AnyObject| {
            let permission = if settings.is_null() {
                Permission::Unavailable
            } else {
                // SAFETY: The API passes a live UNNotificationSettings object
                // for the callback's duration. UNAuthorizationStatus is NSInteger.
                let status: isize = unsafe { msg_send![settings, authorizationStatus] };
                match status {
                    0 => Permission::Unknown,
                    1 => Permission::Denied,
                    2 | 3 => Permission::Granted,
                    _ => Permission::Unavailable,
                }
            };
            if let Ok(mut sender) = sender.lock()
                && let Some(tx) = sender.take()
            {
                let _ = tx.send(permission);
            }
        });
        // SAFETY: This documented query copies the completion block and never
        // requests authorization or presents UI.
        unsafe {
            let _: () = msg_send![&*center, getNotificationSettingsWithCompletionHandler: &*block];
        }
        Some(rx)
    }

    fn identifier(generation: u64) -> Retained<NSString> {
        NSString::from_str(&format!("fastdistord-incoming-{generation}"))
    }

    pub(super) fn begin_post(
        generation: u64,
        gate: Arc<Mutex<Policy>>,
    ) -> Option<oneshot::Receiver<bool>> {
        // Serialize the final validity check with clear/disable. After this
        // submission any concurrent cancellation performs native removal.
        let policy = gate.lock().ok()?;
        if !policy.is_current(generation) {
            return None;
        }
        let center = center()?;
        let content_class = AnyClass::get(c"UNMutableNotificationContent")?;
        let request_class = AnyClass::get(c"UNNotificationRequest")?;
        let (tx, rx) = oneshot::channel();
        let sender = Mutex::new(Some(tx));
        let callback_gate = gate.clone();
        let block = RcBlock::new(move |error: *mut NSError| {
            // Cancellation can race the OS submission. Clear again after its
            // completion, including after our receiver's timeout/drop.
            let current = callback_gate
                .lock()
                .is_ok_and(|policy| policy.is_current(generation));
            if !current {
                clear(generation);
            }
            if let Ok(mut sender) = sender.lock()
                && let Some(tx) = sender.take()
            {
                let _ = tx.send(error.is_null() && current);
            }
        });
        // SAFETY: These signatures match UserNotifications' Objective-C API.
        // All objects stay alive through submission; the center copies content
        // and block. No Objective-C object crosses a Rust await point.
        unsafe {
            let content: Retained<AnyObject> = msg_send![content_class, new];
            let _: () = msg_send![&*content, setTitle: &*NSString::from_str("Incoming call")];
            let _: () = msg_send![&*content, setBody: &*NSString::from_str("Open Fastdistord to answer or decline.")];
            let request: Retained<AnyObject> = msg_send![request_class,
                requestWithIdentifier: &*identifier(generation),
                content: &*content,
                trigger: std::ptr::null::<AnyObject>()];
            let _: () = msg_send![&*center, addNotificationRequest: &*request, withCompletionHandler: &*block];
        }
        drop(policy);
        Some(rx)
    }

    pub(super) fn clear(generation: u64) {
        let Some(center) = center() else { return };
        let ids = NSArray::from_retained_slice(&[identifier(generation)]);
        // SAFETY: Documented selectors take NSArray<NSString>; removal is safe
        // even if the request was never delivered or has already been removed.
        unsafe {
            let _: () =
                msg_send![&*center, removePendingNotificationRequestsWithIdentifiers: &*ids];
            let _: () = msg_send![&*center, removeDeliveredNotificationsWithIdentifiers: &*ids];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::Notify;

    #[derive(Default)]
    struct Fake {
        prompts: AtomicUsize,
        posts: AtomicUsize,
        cleared: Mutex<Vec<u64>>,
        delay_permission: AtomicBool,
        permission_entered: Notify,
        permission_finish: Notify,
        fail_post: AtomicBool,
        entered: Notify,
        finish: Notify,
    }
    #[async_trait::async_trait]
    impl Backend for Fake {
        async fn request_permission(&self) -> Permission {
            self.prompts.fetch_add(1, Ordering::SeqCst);
            self.permission_entered.notify_one();
            if self.delay_permission.load(Ordering::SeqCst) {
                self.permission_finish.notified().await;
            }
            Permission::Granted
        }
        async fn refresh_permission(&self) -> Permission {
            Permission::Granted
        }
        async fn post(&self, _: u64, _: Arc<Mutex<Policy>>) -> bool {
            self.posts.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            self.finish.notified().await;
            !self.fail_post.load(Ordering::SeqCst)
        }
        fn clear(&self, generation: u64) {
            self.cleared.lock().unwrap().push(generation);
        }
    }
    fn fake() -> (NotificationService, Arc<Fake>) {
        let backend = Arc::new(Fake::default());
        (
            NotificationService {
                gate: Arc::default(),
                backend: backend.clone(),
            },
            backend,
        )
    }

    #[tokio::test]
    async fn default_and_preference_cannot_request_permission_or_post() {
        let (service, fake) = fake();
        assert!(!service.set_enabled(true));
        assert_eq!(service.incoming(1).await, Delivery::Disabled);
        assert_eq!(fake.prompts.load(Ordering::SeqCst), 0);
        assert_eq!(fake.posts.load(Ordering::SeqCst), 0);
        assert_eq!(service.request_permission().await, Permission::Granted);
        assert_eq!(service.incoming(1).await, Delivery::Disabled);
        assert_eq!(fake.prompts.load(Ordering::SeqCst), 1);
        assert!(service.set_enabled(true));
    }

    #[tokio::test]
    async fn cancellation_during_submission_clears_the_late_result() {
        let (service, fake) = fake();
        service.request_permission().await;
        service.set_enabled(true);
        let task = tokio::spawn({
            let service = service.clone();
            async move { service.incoming(10).await }
        });
        fake.entered.notified().await;
        service.clear(10);
        fake.finish.notify_one();
        assert_eq!(task.await.unwrap(), Delivery::Stale);
        assert_eq!(service.incoming(10).await, Delivery::Stale);
        assert_eq!(*fake.cleared.lock().unwrap(), [10, 10]);
    }

    #[tokio::test]
    async fn cancellation_before_the_task_starts_prevents_submission() {
        let (service, fake) = fake();
        service.request_permission().await;
        service.set_enabled(true);
        service.clear(10);
        assert_eq!(service.incoming(10).await, Delivery::Stale);
        assert_eq!(fake.posts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stale_call_cannot_replace_or_clear_the_current_generation() {
        let (service, fake) = fake();
        service.request_permission().await;
        service.set_enabled(true);
        fake.finish.notify_one();
        assert_eq!(service.incoming(20).await, Delivery::Scheduled);
        assert_eq!(service.incoming(19).await, Delivery::Stale);
        service.clear(19);
        assert_eq!(service.gate.lock().unwrap().active, Some(20));
        service.set_enabled(false);
        assert_eq!(service.gate.lock().unwrap().active, None);
        assert_eq!(service.incoming(21).await, Delivery::Disabled);
    }

    #[tokio::test]
    async fn only_one_call_is_retained_and_logout_clears_it() {
        let (service, fake) = fake();
        service.request_permission().await;
        service.set_enabled(true);
        for generation in 0..100 {
            fake.finish.notify_one();
            assert_eq!(service.incoming(generation).await, Delivery::Scheduled);
        }
        assert_eq!(service.gate.lock().unwrap().active, Some(99));
        service.clear_all();
        assert_eq!(service.gate.lock().unwrap().active, None);
        assert_eq!(fake.cleared.lock().unwrap().len(), 100);
    }

    #[tokio::test]
    async fn disabling_during_authorization_discards_the_late_grant() {
        let (service, fake) = fake();
        fake.delay_permission.store(true, Ordering::SeqCst);
        let task = tokio::spawn({
            let service = service.clone();
            async move { service.request_permission().await }
        });
        fake.permission_entered.notified().await;
        service.set_enabled(false);
        fake.permission_finish.notify_one();
        assert_eq!(task.await.unwrap(), Permission::Unknown);
        assert!(!service.set_enabled(true));
        assert_eq!(service.incoming(1).await, Delivery::Disabled);
    }

    #[tokio::test]
    async fn failed_os_submission_is_not_reported_as_scheduled() {
        let (service, fake) = fake();
        service.request_permission().await;
        service.set_enabled(true);
        fake.fail_post.store(true, Ordering::SeqCst);
        fake.finish.notify_one();
        assert_eq!(service.incoming(1).await, Delivery::Unavailable);
        assert_eq!(service.gate.lock().unwrap().active, None);
    }

    #[tokio::test]
    async fn restoring_a_saved_opt_in_queries_permission_without_prompting() {
        let (service, fake) = fake();
        assert_eq!(service.refresh_permission().await, Permission::Granted);
        assert_eq!(service.incoming(1).await, Delivery::Disabled);
        assert!(service.set_enabled(true));
        assert_eq!(fake.prompts.load(Ordering::SeqCst), 0);
        assert_eq!(fake.posts.load(Ordering::SeqCst), 0);
    }
}
