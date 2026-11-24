//! Bounded memory-only album art. Never fetch private URLs or forward OAuth tokens.
//!
//! Covers stay stable: the cache holds more thumbnails than any page shows at once
//! (Home's hero, picks and shelves, or a track table), page changes cancel only the
//! downloads still waiting, and each URL is requested at most once per session, so
//! a visible cover is never evicted and re-fetched in a loop (Tide's jitter fix).
//! Pages ask for at most `PAGE_LIMIT` covers; later rows keep their placeholder.
use slint::{Image, Rgb8Pixel, SharedPixelBuffer};
use std::{
    collections::{HashMap, VecDeque},
    io::{Cursor, Read},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

/// Decoded square side. Cards draw at up to ~200 px, the hero at ~220 px.
pub const SIDE: u32 = 192;
/// Thumbnails kept at once. More than a page can show (about 90 on Home).
pub const CAPACITY: usize = 160;
/// Covers a single page may request, comfortably below `CAPACITY`.
pub const PAGE_LIMIT: usize = 120;
const ATTEMPT_LIMIT: usize = 4096;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Attempt {
    Waiting,
    Done,
}

pub struct Cache {
    entries: VecDeque<(String, Image)>,
    attempted: HashMap<String, Attempt>,
    tx: Option<mpsc::SyncSender<(String, u64)>>,
    rx: mpsc::Receiver<(String, u64, Option<Vec<u8>>)>,
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    task: Option<std::thread::JoinHandle<()>>,
}
impl Cache {
    pub fn new(offline: bool) -> Self {
        let (tx, requests) = mpsc::sync_channel::<(String, u64)>(8);
        let (out, rx) = mpsc::sync_channel(8);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        let task = (!offline).then(|| {
            std::thread::spawn(move || {
                let Ok(http) = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                else {
                    return;
                };
                while !stopping.load(Ordering::Relaxed) {
                    let (url, version) = match requests.recv_timeout(Duration::from_millis(200)) {
                        Ok(url) => url,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                    if version != current.load(Ordering::Relaxed) {
                        continue;
                    }
                    let pixels = fetch(&http, &url);
                    let _ = out.try_send((url, version, pixels));
                }
            })
        });
        Self {
            entries: VecDeque::new(),
            attempted: HashMap::new(),
            tx: (!offline).then_some(tx),
            rx,
            stop,
            task,
            generation,
        }
    }
    /// Collects finished downloads. A cover that arrives after a page change is
    /// still valid and kept; only the request queue is generation-scoped.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((url, _version, pixels)) = self.rx.try_recv() {
            self.attempted.insert(url.clone(), Attempt::Done);
            if let Some(pixels) = pixels {
                self.insert(url, image(&pixels));
                changed = true;
            }
        }
        changed
    }
    fn insert(&mut self, url: String, image: Image) {
        self.entries.retain(|(key, _)| *key != url);
        self.entries.push_back((url, image));
        while self.entries.len() > CAPACITY {
            // An evicted cover may be fetched again later. Callers request at most
            // `PAGE_LIMIT` covers per page, so this never becomes a refetch loop.
            if let Some((old, _)) = self.entries.pop_front() {
                self.attempted.remove(&old);
            }
        }
    }
    pub fn get(&mut self, url: &str) -> Image {
        if let Some((_, image)) = self.entries.iter().find(|(key, _)| key == url) {
            return image.clone();
        }
        if allowed(url) && !self.attempted.contains_key(url) && self.attempted.len() < ATTEMPT_LIMIT
        {
            if let Some(tx) = &self.tx {
                if tx
                    .try_send((url.to_owned(), self.generation.load(Ordering::Relaxed)))
                    .is_ok()
                {
                    self.attempted.insert(url.into(), Attempt::Waiting);
                }
            }
        }
        Image::default()
    }
    /// A cached cover, without requesting it.
    pub fn peek(&self, url: &str) -> Image {
        self.entries
            .iter()
            .find(|(key, _)| key == url)
            .map(|(_, image)| image.clone())
            .unwrap_or_default()
    }
    /// A new page: drop queued downloads so its covers go first. Decoded covers stay.
    pub fn cancel(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.attempted.retain(|_, a| *a == Attempt::Done);
    }
    /// Sign-out: forget every cover.
    pub fn clear(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.entries.clear();
        self.attempted.clear();
    }
}
impl Drop for Cache {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.tx = None;
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
fn image(pixels: &[u8]) -> Image {
    Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(
        pixels, SIDE, SIDE,
    ))
}
fn allowed(input: &str) -> bool {
    url::Url::parse(input).is_ok_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && matches!(url.host_str(), Some("i.scdn.co" | "mosaic.scdn.co"))
            && url.query().is_none()
            && url.fragment().is_none()
            && input.len() < 2048
    })
}
fn fetch(http: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    if !allowed(url) {
        return None;
    }
    let response = http.get(url).send().ok()?;
    if !response.status().is_success() || response.content_length().is_some_and(|n| n > 1024 * 1024)
    {
        return None;
    }
    let mut bytes = Vec::new();
    response
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 1024 * 1024 {
        return None;
    }
    decode(bytes)
}
fn decode(bytes: Vec<u8>) -> Option<Vec<u8>> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    Some(
        reader
            .decode()
            .ok()?
            .resize_to_fill(SIDE, SIDE, image::imageops::FilterType::Triangle)
            .to_rgb8()
            .into_raw(),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_public_spotify_cdn_art_is_fetched() {
        assert!(allowed("https://i.scdn.co/image/album"));
        for url in [
            "http://i.scdn.co/image/a",
            "https://i.scdn.co.evil.test/a",
            "https://127.0.0.1/a",
            "https://user@i.scdn.co/a",
            "https://i.scdn.co/a?token=secret",
        ] {
            assert!(!allowed(url));
        }
    }

    #[test]
    fn covers_survive_page_changes_and_never_refetch_in_a_loop() {
        let mut cache = Cache::new(true);
        let (tx, requests) = mpsc::sync_channel(8);
        cache.tx = Some(tx);
        let (out, rx) = mpsc::sync_channel(8);
        cache.rx = rx;
        let url = "https://i.scdn.co/image/one";
        assert_eq!(cache.get(url).size().width, 0);
        assert_eq!(cache.get(url).size().width, 0);
        assert_eq!(
            requests.try_iter().count(),
            1,
            "duplicate cover requests collapse"
        );
        cache.cancel();
        // A late cover from the previous page is still kept.
        out.send((url.into(), 0, Some(vec![9; (SIDE * SIDE * 3) as usize])))
            .unwrap();
        assert!(cache.poll());
        assert_eq!(cache.get(url).size().width, SIDE);
        // A waiting request cancelled by a page change may be asked for again.
        let other = "https://i.scdn.co/image/two";
        cache.get(other);
        cache.cancel();
        cache.get(other);
        assert_eq!(requests.try_iter().count(), 2);
        // The cache is bounded; an evicted cover can come back later.
        for i in 0..CAPACITY + 5 {
            cache
                .attempted
                .insert(format!("https://i.scdn.co/image/{i}"), Attempt::Done);
            cache.insert(format!("https://i.scdn.co/image/{i}"), Image::default());
        }
        assert_eq!(cache.entries.len(), CAPACITY);
        const { assert!(PAGE_LIMIT < CAPACITY) };
        cache.get("https://i.scdn.co/image/0");
        cache.get(&format!("https://i.scdn.co/image/{}", CAPACITY + 4));
        assert_eq!(requests.try_iter().count(), 1);
        cache.clear();
        assert!(cache.entries.is_empty());
    }
}
