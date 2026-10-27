//! Worker thread that owns the model, so the UI never blocks on inference.
//!
//! The UI sends word keys; the worker loads the model once, then answers
//! the newest requests first and posts `msg` to `notify` after each event.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use ezhuthu_xlit::Transliterator;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub enum Event {
    /// Model loaded. The UI keeps the handle for synchronous fallbacks.
    Ready(Arc<Transliterator>),
    Failed(String),
    Done {
        key: String,
        candidates: Vec<String>,
        /// A confident whole-word completion from the dictionary (from
        /// `COMPLETE_FROM` letters on).
        completion: Option<String>,
    },
}

/// Word completion starts once this many letters are typed.
pub const COMPLETE_FROM: usize = 3;
/// A dictionary completion is offered only when it makes up at least this
/// share of the words starting the same way (a confident guess).
const MIN_SHARE: f32 = 0.5;

/// Candidates for `key`, and a dictionary completion once it is long enough.
pub fn suggest(xlit: &Transliterator, key: &str) -> (Vec<String>, Option<String>) {
    let cands = xlit.transliterate(key).unwrap_or_default();
    let completion = if key.len() >= COMPLETE_FROM {
        xlit.complete(&cands, MIN_SHARE)
    } else {
        None
    };
    (cands.into_iter().map(|c| c.text).collect(), completion)
}

pub struct Engine {
    requests: Sender<String>,
    pub events: Receiver<Event>,
}

impl Engine {
    pub fn start(assets: PathBuf, beam: usize, notify: HWND, msg: u32) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<String>();
        let (ev_tx, ev_rx) = mpsc::channel();
        // HWND is a raw pointer (not Send); the window outlives the worker.
        let notify = notify.0 as isize;
        std::thread::Builder::new()
            .name("ezhuthu-engine".into())
            .spawn(move || {
                let send = |ev: Event| {
                    if ev_tx.send(ev).is_ok() {
                        // SAFETY: posting to a window owned by the UI thread is
                        // thread-safe; if it is gone the call just fails.
                        unsafe {
                            let _ = PostMessageW(
                                Some(HWND(notify as *mut _)),
                                msg,
                                WPARAM(0),
                                LPARAM(0),
                            );
                        }
                    }
                };
                let xlit = match Transliterator::load(&assets) {
                    Ok(x) => Arc::new(x.with_beam(beam)),
                    Err(e) => {
                        send(Event::Failed(format!("{e:#}")));
                        return;
                    }
                };
                send(Event::Ready(xlit.clone()));
                while let Ok(first) = req_rx.recv() {
                    // Newest first, each key once.
                    let mut batch: Vec<String> =
                        std::iter::once(first).chain(req_rx.try_iter()).collect();
                    batch.reverse();
                    let mut seen = HashSet::new();
                    for key in batch.into_iter().filter(|k| seen.insert(k.clone())) {
                        let (candidates, completion) = suggest(&xlit, &key);
                        send(Event::Done {
                            key,
                            candidates,
                            completion,
                        });
                    }
                }
            })
            .expect("spawning the engine thread");
        Self {
            requests: req_tx,
            events: ev_rx,
        }
    }

    pub fn request(&self, key: String) {
        let _ = self.requests.send(key);
    }
}
