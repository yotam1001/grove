//! Stand-ins used when the `youtube` feature is off (Grove's build).
//!
//! No extractor process is started, no cookie lease is copied and no stream is
//! ever prepared. YouTube addresses are rejected in `validate_source`, and mpv
//! runs with `--ytdl=no`. Local files and plain http/https sources still play.
#![allow(dead_code)]

pub(crate) mod prefetch {
    use super::cookie_updates::CookieUpdates;
    use serde_json::Value;
    use std::{convert::Infallible, sync::Arc};
    use tokio::sync::watch;

    #[derive(Clone, Default)]
    pub(crate) struct Request;

    /// Uninhabited: without the extractor there is never a prepared stream.
    #[derive(Clone)]
    pub(crate) struct PreparedStream(Infallible);

    impl PreparedStream {
        pub(crate) fn load_command(&self) -> Value {
            match self.0 {}
        }
    }

    pub(crate) struct Preparation;

    impl Preparation {
        pub(crate) fn new() -> (Arc<Self>, watch::Receiver<Request>) {
            let (_requests, receiver) = watch::channel(Request);
            (Arc::new(Self), receiver)
        }
        pub(crate) fn set(&self, _source: Option<String>) {}
        pub(crate) fn clear(&self) {}
        pub(crate) fn close(&self) {}
        pub(crate) fn take_for(&self, _source: &str) -> Option<PreparedStream> {
            None
        }
        pub(crate) fn is_ready(&self, _source: &str) -> bool {
            false
        }
    }

    pub(crate) async fn run(
        _runtimes: crate::RuntimePaths,
        _preparation: Arc<Preparation>,
        _requests: watch::Receiver<Request>,
        _cookie_updates: Arc<CookieUpdates>,
    ) {
    }

    /// Uninhabited: signed-in extraction is not part of this build.
    pub(crate) struct CookieCopy(Infallible);

    impl CookieCopy {
        pub(crate) fn create(_source: &std::path::Path) -> std::io::Result<Self> {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Signed-in extraction is not built in",
            ))
        }
        pub(crate) fn path(&self) -> &std::path::Path {
            match self.0 {}
        }
        pub(crate) fn publish_update(&mut self, _account: u32, _queue: &CookieUpdates) -> bool {
            match self.0 {}
        }
    }
}

pub(crate) mod cookie_updates {
    /// Never constructed in this build; kept so `PlaybackHandle`'s API is stable.
    pub struct CookieUpdate {
        pub account_index: u32,
        _never: std::convert::Infallible,
    }

    impl CookieUpdate {
        pub fn baseline(&self) -> &[u8] {
            match self._never {}
        }
        pub fn updated(&self) -> &[u8] {
            match self._never {}
        }
    }

    #[derive(Default)]
    pub(crate) struct CookieUpdates {
        _empty: (),
    }

    impl CookieUpdates {
        pub(crate) fn take(&self) -> Vec<CookieUpdate> {
            Vec::new()
        }
    }
}
