//! Document-level limits around any [`Translator`].
//!
//! Two stops, both general: a wall-clock budget for the whole document, and a
//! gateway that refuses every call (`401`, `403`, `410`, `400 access denied`).
//! Once either trips, calls fail at once without touching the network, so the
//! batch and leftover passes keep the source and the job ends instead of
//! retrying until an outer watchdog kills it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::translate::{TranslateOptions, Translator};

const DEFAULT_DOWN_AFTER: usize = 6;

/// The gateway said no for a reason a retry will not change.
pub(crate) fn gateway_refused(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    if ["401", "402", "403", "404", "410"]
        .iter()
        .any(|code| lower.contains(&format!("http status {code}")))
    {
        return true;
    }
    // A pooled gateway also reports a pool hiccup as 400, which is retried.
    // Only a 400/429 that names an account problem is final.
    [
        "access denied",
        "overdue",
        "arrears",
        "insufficient balance",
        "insufficient_quota",
        "quota exceeded",
        "invalid api key",
        "invalid_api_key",
        "unauthorized",
        "forbidden",
        "account disabled",
        "suspended",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        && lower.contains("http status")
}

pub(crate) struct Guarded<'a> {
    inner: &'a dyn Translator,
    deadline: Option<Instant>,
    budget: Option<Duration>,
    down_after: usize,
    refusals: AtomicUsize,
    down: AtomicBool,
    last_refusal: Mutex<String>,
}

fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

impl<'a> Guarded<'a> {
    pub(crate) fn new(inner: &'a dyn Translator, opts: &TranslateOptions) -> Self {
        let secs = opts
            .max_translate_secs
            .or_else(|| env_number("RPT_MAX_TRANSLATE_SECS"))
            .filter(|secs| *secs > 0);
        let budget = secs.map(Duration::from_secs);
        let down_after = match opts.gateway_down_after {
            0 => env_number("RPT_GATEWAY_DOWN_AFTER")
                .map(|n| n as usize)
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_DOWN_AFTER),
            n => n,
        };
        Self {
            inner,
            deadline: budget.map(|budget| Instant::now() + budget),
            budget,
            down_after,
            refusals: AtomicUsize::new(0),
            down: AtomicBool::new(false),
            last_refusal: Mutex::new(String::new()),
        }
    }

    /// Why no further request is sent, if one of the stops tripped.
    pub(crate) fn halted(&self) -> Option<String> {
        if self.down.load(Ordering::Relaxed) {
            let last = self
                .last_refusal
                .lock()
                .map(|s| s.clone())
                .unwrap_or_default();
            return Some(format!(
                "gateway refused {} consecutive requests ({last})",
                self.down_after
            ));
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Some(format!(
                "translation budget of {}s spent (max-translate-secs)",
                self.budget.map(|b| b.as_secs()).unwrap_or(0)
            ));
        }
        None
    }
}

impl Translator for Guarded<'_> {
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        if let Some(reason) = self.halted() {
            return Err(Error::Translate(format!("not sent: {reason}")));
        }
        match self.inner.complete(system, user) {
            Ok(text) => {
                self.refusals.store(0, Ordering::Relaxed);
                Ok(text)
            }
            Err(err) => {
                let msg = err.to_string();
                if gateway_refused(&msg) {
                    if let Ok(mut last) = self.last_refusal.lock() {
                        *last = msg.chars().take(120).collect();
                    }
                    let n = self.refusals.fetch_add(1, Ordering::Relaxed) + 1;
                    if n >= self.down_after && !self.down.swap(true, Ordering::Relaxed) {
                        eprintln!(
                            "rpt: gateway refused {n} consecutive requests; not sending more ({msg})"
                        );
                    }
                } else {
                    self.refusals.store(0, Ordering::Relaxed);
                }
                Err(err)
            }
        }
    }

    fn bind_retries(&self, counter: Arc<AtomicUsize>) {
        self.inner.bind_retries(counter)
    }

    fn bind_phase(&self, phase: Arc<Mutex<String>>) {
        self.inner.bind_phase(phase)
    }

    fn arm_segment_budget(&self, budget: Duration) {
        // A segment budget never outlives the document budget.
        let budget = match self.deadline {
            Some(d) => budget.min(d.saturating_duration_since(Instant::now())),
            None => budget,
        };
        self.inner.arm_segment_budget(budget)
    }

    fn clear_segment_budget(&self) {
        self.inner.clear_segment_budget()
    }

    fn segment_budget_expired(&self) -> bool {
        self.halted().is_some() || self.inner.segment_budget_expired()
    }

    fn segment_budget_armed(&self) -> bool {
        self.inner.segment_budget_armed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_refusals_are_final_and_pool_hiccups_are_not() {
        assert!(gateway_refused("LLM HTTP status 401"));
        assert!(gateway_refused("LLM HTTP status 410: gone"));
        assert!(gateway_refused(
            "LLM HTTP status 400: {\"error\":\"Access denied, account overdue\"}"
        ));
        assert!(!gateway_refused("LLM HTTP status 400"));
        assert!(!gateway_refused("LLM HTTP status 400: upstream error"));
        assert!(!gateway_refused("LLM HTTP status 502"));
        assert!(!gateway_refused("timeout: global"));
    }
}
