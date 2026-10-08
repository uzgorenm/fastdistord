//! Small memory-only static Discord CDN cache. Fetch and decode happen off the UI thread.
use egui::{ColorImage, TextureHandle};
use std::{
    collections::{HashMap, HashSet},
    sync::mpsc,
};
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Key {
    pub guild: bool,
    pub id: u64,
    pub hash: String,
}
impl Key {
    fn url(&self) -> Option<String> {
        let plain = self.hash.strip_prefix("a_").unwrap_or(&self.hash);
        if self.id == 0 || plain.len() != 32 || !plain.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(format!(
            "https://cdn.discordapp.com/{}/{}/{}.png?size=64",
            if self.guild { "icons" } else { "avatars" },
            self.id,
            self.hash
        ))
    }
}
#[derive(Default)]
pub struct Cache {
    account: Option<u64>,
    suspended: bool,
    tx: Option<tokio::sync::mpsc::Sender<Key>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    rx: Option<mpsc::Receiver<(Key, Option<ColorImage>)>>,
    pending: HashSet<Key>,
    textures: HashMap<Key, TextureHandle>,
    attempted: HashSet<Key>,
}
impl Drop for Cache {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
impl Cache {
    pub fn suspend(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        *self = Self::default();
        self.suspended = true;
    }
    pub fn sync(&mut self, account: Option<u64>, ctx: &egui::Context) {
        if self.suspended {
            if account.is_none() {
                self.suspended = false;
            } else {
                return;
            }
        }
        if self.account != account {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            *self = Self::default();
            self.account = account;
            if account.is_some() {
                let (tx, mut jobs) = tokio::sync::mpsc::channel::<Key>(32);
                let (out, rx) = mpsc::channel();
                let (stop, mut stopped) = tokio::sync::oneshot::channel();
                self.tx = Some(tx);
                self.stop = Some(stop);
                self.rx = Some(rx);
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        return;
                    };
                    rt.block_on(async move {
                        let Ok(client) = reqwest::Client::builder()
                            .redirect(reqwest::redirect::Policy::none())
                            .timeout(std::time::Duration::from_secs(8))
                            .build()
                        else {
                            return;
                        };
                        loop {
                            tokio::select! {
                                biased;
                                _ = &mut stopped => break,
                                job = jobs.recv() => {
                                    let Some(key) = job else { break };
                                    let result = tokio::select! {
                                        biased;
                                        _ = &mut stopped => break,
                                        result = fetch(&client, &key) => result,
                                    };
                                    if out.send((key, result)).is_err() { break }
                                    ctx.request_repaint();
                                }
                            }
                        }
                    });
                });
            }
        }
        if let Some(rx) = &self.rx {
            for (key, pixels) in rx.try_iter() {
                self.pending.remove(&key);
                if let Some(pixels) = pixels
                    && self.textures.len() < 128
                {
                    self.textures.insert(
                        key,
                        ctx.load_texture("Discord portrait", pixels, egui::TextureOptions::LINEAR),
                    );
                }
            }
        }
    }
    pub fn get(&mut self, key: Key) -> Option<TextureHandle> {
        if let Some(texture) = self.textures.get(&key) {
            return Some(texture.clone());
        }
        // A bounded one-shot attempt set also prevents broken portraits retrying on every repaint.
        if self.attempted.len() < 128
            && !self.attempted.contains(&key)
            && let Some(tx) = &self.tx
            && tx.try_send(key.clone()).is_ok()
        {
            self.attempted.insert(key.clone());
            self.pending.insert(key);
        }
        None
    }
}
async fn fetch(client: &reqwest::Client, key: &Key) -> Option<ColorImage> {
    let mut response = client.get(key.url()?).send().await.ok()?;
    if !response.status().is_success() || response.content_length().is_some_and(|n| n > 262144) {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > 262144 {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    // Explicit PNG decoder, bounded dimensions/allocation, no format guessing.
    use image::ImageDecoder;
    let mut decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || w > 128 || h > 128 {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(1024 * 1024);
    decoder.set_limits(limits).ok()?;
    let rgba = image::DynamicImage::from_decoder(decoder)
        .ok()?
        .into_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        rgba.as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logout_cancels_and_cannot_reschedule_from_a_stale_ui_snapshot() {
        let mut cache = Cache::default();
        let ctx = egui::Context::default();
        cache.suspend();
        cache.sync(Some(42), &ctx);
        assert!(cache.tx.is_none());
        assert!(cache.textures.is_empty());
        assert!(cache.suspended);
        cache.sync(None, &ctx);
        assert!(!cache.suspended);
    }
    #[test]
    fn cdn_key_cannot_change_origin_path_or_image_size() {
        let key = Key {
            guild: false,
            id: 42,
            hash: "a_0123456789abcdef0123456789abcdef".into(),
        };
        assert_eq!(
            key.url().unwrap(),
            "https://cdn.discordapp.com/avatars/42/a_0123456789abcdef0123456789abcdef.png?size=64"
        );
        for invalid in [
            "../token",
            "abc?size=4096",
            "https://evil.example",
            "0123456789abcdef0123456789abcde/ff",
        ] {
            assert!(
                Key {
                    hash: invalid.into(),
                    ..key.clone()
                }
                .url()
                .is_none()
            );
        }
    }
}
