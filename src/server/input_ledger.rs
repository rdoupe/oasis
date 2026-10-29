//! Which numbered input this server process has applied, per client token, so a
//! client that lost its connection can ask how far its input got and send the
//! rest exactly once. Shared by every connection: a client's old, stalled
//! connection may still deliver input after its new one has replayed it, and the
//! record keeps either from applying it twice.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// More client tokens than this and the longest-unused are forgotten; a client
/// that comes back after that learns its input's fate is unknown.
const MAX_TOKENS: usize = 1024;

#[derive(Default)]
pub(crate) struct InputLedger {
    applied: HashMap<String, (u64, Instant)>,
}

impl InputLedger {
    /// Whether input numbered `seq` is new for `token`, so it should be applied.
    pub(crate) fn admits(&self, token: &str, seq: u64) -> bool {
        self.applied.get(token).is_none_or(|(last, _)| seq > *last)
    }

    /// Records that input numbered `seq` for `token` was applied.
    pub(crate) fn record(&mut self, token: &str, seq: u64, now: Instant) {
        if let Some((last, used)) = self.applied.get_mut(token) {
            *last = (*last).max(seq);
            *used = now;
            return;
        }
        if self.applied.len() >= MAX_TOKENS {
            if let Some(oldest) = self
                .applied
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(token, _)| token.clone())
            {
                self.applied.remove(&oldest);
            }
        }
        self.applied.insert(token.to_owned(), (seq, now));
    }

    pub(crate) fn last_applied(&self, token: &str) -> Option<u64> {
        self.applied.get(token).map(|(last, _)| *last)
    }
}

/// This server process's ledger.
pub(crate) fn lock() -> MutexGuard<'static, InputLedger> {
    static LEDGER: OnceLock<Mutex<InputLedger>> = OnceLock::new();
    LEDGER
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Names this server process, so a client can tell a restarted server, which has
/// forgotten what it applied, from one that never saw its input.
pub(crate) fn instance() -> &'static str {
    static INSTANCE: OnceLock<String> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_is_applied_once_whatever_order_its_copies_arrive_in() {
        let now = Instant::now();
        let mut ledger = InputLedger::default();
        assert!(ledger.admits("t", 1));
        ledger.record("t", 1, now);
        assert!(!ledger.admits("t", 1), "a replayed copy");
        assert!(ledger.admits("t", 2));
        ledger.record("t", 3, now);
        assert!(!ledger.admits("t", 2), "older than what was applied");
        assert_eq!(ledger.last_applied("t"), Some(3));
        assert_eq!(ledger.last_applied("other"), None);
        assert!(ledger.admits("other", 1), "tokens are separate");
    }

    #[test]
    fn the_longest_unused_token_is_forgotten_past_the_cap() {
        let start = Instant::now();
        let mut ledger = InputLedger::default();
        for n in 0..MAX_TOKENS {
            ledger.record(
                &n.to_string(),
                1,
                start + std::time::Duration::from_millis(n as u64),
            );
        }
        ledger.record("0", 2, start + std::time::Duration::from_secs(10));
        ledger.record("new", 1, start + std::time::Duration::from_secs(11));
        assert_eq!(ledger.last_applied("0"), Some(2), "recently used is kept");
        assert_eq!(ledger.last_applied("1"), None, "oldest is forgotten");
        assert_eq!(ledger.last_applied("new"), Some(1));
    }
}
