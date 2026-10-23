//! Stderr progress for a translation job.
//!
//! A quiet process is indistinguishable from a stalled socket. Text (the CLI
//! default) and JSON lines both go to stderr so stdout stays the result
//! document. `Off` is the library default.

use std::collections::HashMap;
use std::io::{stderr, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::ProgressMode;
use crate::segment::Segment;

struct InFlight {
    token: u64,
    ids: Vec<u32>,
    /// 0-based page index.
    page: u32,
    started: Instant,
    reported: bool,
}

pub struct Watch<'a> {
    progress: &'a Progress,
    token: u64,
}

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        self.progress.clear_inflight(self.token);
    }
}

pub struct Progress {
    mode: ProgressMode,
    started: Instant,
    segments_total: usize,
    pages_total: usize,
    segments_done: AtomicUsize,
    pages_done: AtomicUsize,
    calls: AtomicUsize,
    retries: Arc<AtomicUsize>,
    last_advance_ms: AtomicU64,
    remaining: Mutex<HashMap<u32, usize>>,
    stop: Arc<AtomicBool>,
    /// `connect`, `recv-body`, `retry`, or `request`. Shared with the LLM client.
    phase: Arc<Mutex<String>>,
    inflight: Mutex<Vec<InFlight>>,
    next_token: AtomicU64,
    /// Per-request deadline. A batch still running after this is aborted.
    timeout_secs: u64,
    watchdog: Mutex<Vec<String>>,
}

pub struct Heartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Progress {
    pub fn start(
        mode: ProgressMode,
        segments: &[Segment],
        done: &[bool],
        timeout_secs: u64,
    ) -> Arc<Self> {
        let mut remaining: HashMap<u32, usize> = HashMap::new();
        let mut segments_done = 0usize;
        for (segment, is_done) in segments.iter().zip(done.iter().copied()) {
            if is_done {
                segments_done += 1;
            } else {
                *remaining.entry(segment.page_index).or_default() += 1;
            }
        }
        let pages_total = segments
            .iter()
            .map(|segment| segment.page_index)
            .collect::<std::collections::HashSet<_>>()
            .len();
        let pages_done = pages_total.saturating_sub(remaining.len());
        let progress = Arc::new(Self {
            mode,
            started: Instant::now(),
            segments_total: segments.len(),
            pages_total,
            segments_done: AtomicUsize::new(segments_done),
            pages_done: AtomicUsize::new(pages_done),
            calls: AtomicUsize::new(0),
            retries: Arc::new(AtomicUsize::new(0)),
            last_advance_ms: AtomicU64::new(0),
            remaining: Mutex::new(remaining),
            stop: Arc::new(AtomicBool::new(false)),
            phase: Arc::new(Mutex::new("idle".into())),
            inflight: Mutex::new(Vec::new()),
            next_token: AtomicU64::new(1),
            timeout_secs: timeout_secs.max(1),
            watchdog: Mutex::new(Vec::new()),
        });
        if mode != ProgressMode::Off {
            progress.emit(false);
        }
        progress
    }

    pub fn retries_handle(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.retries)
    }

    pub fn phase_handle(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.phase)
    }

    /// The batch is in flight until the guard drops. The heartbeat aborts it
    /// in the log once `timeout_secs` passes without the worker returning.
    pub fn watch(&self, segments: &[Segment], indexes: &[usize]) -> Watch<'_> {
        let ids: Vec<u32> = indexes
            .iter()
            .filter_map(|index| segments.get(*index).map(|segment| segment.id))
            .collect();
        let page = indexes
            .iter()
            .find_map(|index| segments.get(*index).map(|segment| segment.page_index))
            .unwrap_or(0);
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut phase) = self.phase.lock() {
            if phase.is_empty() || *phase == "idle" {
                *phase = "request".into();
            }
        }
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.push(InFlight {
                token,
                ids,
                page,
                started: Instant::now(),
                reported: false,
            });
        }
        Watch {
            progress: self,
            token,
        }
    }

    fn clear_inflight(&self, token: u64) {
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.retain(|item| item.token != token);
        }
        if self
            .inflight
            .lock()
            .map(|items| items.is_empty())
            .unwrap_or(false)
        {
            if let Ok(mut phase) = self.phase.lock() {
                *phase = "idle".into();
            }
        }
    }

    /// Name a batch that is still running after the per-request deadline.
    pub fn poll_watchdog(&self) {
        let phase = self
            .phase
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| "request".into());
        let mut notes = Vec::new();
        if let Ok(mut inflight) = self.inflight.lock() {
            for item in inflight.iter_mut() {
                if item.reported || item.ids.is_empty() {
                    continue;
                }
                let age = item.started.elapsed().as_secs();
                if age < self.timeout_secs {
                    continue;
                }
                item.reported = true;
                let shown: Vec<String> = item.ids.iter().take(4).map(|id| id.to_string()).collect();
                let extra = item.ids.len().saturating_sub(shown.len());
                let ids = if extra > 0 {
                    format!("{} +{extra}", shown.join(","))
                } else {
                    shown.join(",")
                };
                notes.push(format!(
                    "rpt: watchdog: aborting segment {ids} page {} stuck in {phase} for {age}s",
                    item.page + 1
                ));
            }
        }
        if notes.is_empty() {
            return;
        }
        if let Ok(mut saved) = self.watchdog.lock() {
            saved.extend(notes.iter().cloned());
        }
        if self.mode != ProgressMode::Off {
            let mut err = stderr().lock();
            for note in &notes {
                let _ = writeln!(err, "{note}");
            }
        }
    }

    #[cfg(test)]
    pub fn watchdog_messages(&self) -> Vec<String> {
        self.watchdog
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    pub fn spawn(self: &Arc<Self>) -> Heartbeat {
        if self.mode == ProgressMode::Off {
            return Heartbeat {
                stop: Arc::clone(&self.stop),
                handle: None,
            };
        }
        let progress = Arc::clone(self);
        let stop = Arc::clone(&self.stop);
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut ticks = 0u32;
            while !thread_stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                ticks += 1;
                if ticks < 25 {
                    continue;
                }
                ticks = 0;
                progress.poll_watchdog();
                let since = progress.since_advance_ms();
                if since >= 5_000 {
                    progress.emit(true);
                }
            }
        });
        Heartbeat {
            stop,
            handle: Some(handle),
        }
    }

    pub fn add_calls(&self, calls: usize) {
        if calls > 0 {
            self.calls.fetch_add(calls, Ordering::Relaxed);
        }
    }

    /// One finished batch. `pages` is one entry per segment in that batch.
    pub fn finish_batch(&self, pages: &[u32], calls: usize) {
        self.add_calls(calls);
        if self.mode == ProgressMode::Off || pages.is_empty() {
            return;
        }
        let mut pages_finished = 0usize;
        if let Ok(mut remaining) = self.remaining.lock() {
            for page in pages {
                self.segments_done.fetch_add(1, Ordering::Relaxed);
                if let Some(left) = remaining.get_mut(page) {
                    *left = left.saturating_sub(1);
                    if *left == 0 {
                        remaining.remove(page);
                        pages_finished += 1;
                    }
                }
            }
        }
        if pages_finished > 0 {
            self.pages_done.fetch_add(pages_finished, Ordering::Relaxed);
        }
        self.last_advance_ms
            .store(self.elapsed_ms(), Ordering::Relaxed);
        self.emit(false);
    }

    pub fn emit_done(&self) {
        if self.mode != ProgressMode::Off {
            self.emit(false);
        }
    }

    fn since_advance_ms(&self) -> u64 {
        self.elapsed_ms()
            .saturating_sub(self.last_advance_ms.load(Ordering::Relaxed))
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn emit(&self, waiting: bool) {
        let segments_done = self.segments_done.load(Ordering::Relaxed);
        let pages_done = self.pages_done.load(Ordering::Relaxed);
        let calls = self.calls.load(Ordering::Relaxed);
        let retries = self.retries.load(Ordering::Relaxed);
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        let rate = segments_done as f64 / elapsed;
        let waiting_secs = if waiting {
            self.since_advance_ms() / 1000
        } else {
            0
        };
        let phase = self
            .phase
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| "idle".into());
        let focus = self.inflight.lock().ok().and_then(|items| {
            items.iter().min_by_key(|item| item.started).map(|item| {
                let id = item.ids.first().copied().unwrap_or(0);
                (id, item.page + 1, item.ids.len())
            })
        });
        let (segment_id, page, batch) = focus.unwrap_or((0, 0, 0));
        let mut err = stderr().lock();
        match self.mode {
            ProgressMode::Off => {}
            ProgressMode::Json => {
                let line = serde_json::json!({
                    "event": if waiting { "waiting" } else { "progress" },
                    "pages_done": pages_done,
                    "pages_total": self.pages_total,
                    "segments_done": segments_done,
                    "segments_total": self.segments_total,
                    "calls": calls,
                    "retries": retries,
                    "elapsed_secs": (elapsed * 10.0).round() / 10.0,
                    "rate_segments_per_sec": (rate * 10.0).round() / 10.0,
                    "waiting_secs": waiting_secs,
                    "phase": phase,
                    "segment_id": segment_id,
                    "page": page,
                    "batch": batch,
                });
                let _ = writeln!(err, "{line}");
            }
            ProgressMode::Text => {
                let focus = if batch == 0 {
                    String::new()
                } else {
                    format!(", phase {phase}, segment {segment_id} p.{page}")
                };
                if waiting {
                    let _ = writeln!(
                        err,
                        "rpt: waiting {waiting_secs}s, {pages_done}/{pages} pages, {segments_done}/{segments} segments, calls {calls}, retries {retries}{focus}",
                        pages = self.pages_total,
                        segments = self.segments_total,
                    );
                } else {
                    let _ = writeln!(
                        err,
                        "rpt: {pages_done}/{pages} pages, {segments_done}/{segments} segments, {rate:.1}/s, calls {calls}, retries {retries}{focus}",
                        pages = self.pages_total,
                        segments = self.segments_total,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_names_the_segment_past_the_deadline() {
        let segments = vec![Segment {
            id: 7,
            page_index: 2,
            glyph_ids: Vec::new(),
            text: "The body paragraph stays in English.".into(),
        }];
        let progress = Progress::start(ProgressMode::Off, &segments, &[false], 1);
        let watch = progress.watch(&segments, &[0]);
        std::thread::sleep(Duration::from_millis(1100));
        progress.poll_watchdog();
        let notes = progress.watchdog_messages();
        assert!(
            notes.iter().any(|note| {
                note.contains("segment 7") && note.contains("page 3") && note.contains("aborting")
            }),
            "{notes:?}"
        );
        drop(watch);
        progress.poll_watchdog();
        assert_eq!(progress.watchdog_messages().len(), notes.len());
    }
}
