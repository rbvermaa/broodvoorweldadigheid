use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use jiff::{SignedDuration, Timestamp};

/// The status page asks Mollie about a payment at most once per [`Self::GAP`] per server,
/// so whoever holds a status link cannot make us hammer Mollie's API.
#[derive(Debug, Default)]
pub struct PaymentLookups {
    last: Mutex<HashMap<String, Timestamp>>,
}

impl PaymentLookups {
    pub const GAP: SignedDuration = SignedDuration::from_secs(10);
    const PRUNE_ABOVE: usize = 1000;

    /// `true` when `reference` may be looked up at `now`, which then counts as its last
    /// lookup.
    pub fn claim(&self, reference: &str, now: Timestamp) -> bool {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let recent = |at: &Timestamp| now.duration_since(*at) < Self::GAP;
        if last.get(reference).is_some_and(recent) {
            return false;
        }
        if last.len() >= Self::PRUNE_ABOVE {
            last.retain(|_, at| recent(at));
        }
        last.insert(reference.to_owned(), now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn one_lookup_per_payment_per_gap() {
        let lookups = PaymentLookups::default();
        let start = ts("2026-10-28T12:00:00Z");
        assert!(lookups.claim("tr_a", start));
        assert!(!lookups.claim("tr_a", ts("2026-10-28T12:00:09Z")));
        assert!(lookups.claim("tr_b", ts("2026-10-28T12:00:09Z")));
        assert!(lookups.claim("tr_a", ts("2026-10-28T12:00:10Z")));
    }

    #[test]
    fn old_entries_are_pruned_once_there_are_many() {
        let lookups = PaymentLookups::default();
        let start = ts("2026-10-28T12:00:00Z");
        for n in 0..PaymentLookups::PRUNE_ABOVE {
            assert!(lookups.claim(&format!("tr_{n}"), start));
        }
        assert!(lookups.claim("tr_new", ts("2026-10-28T12:01:00Z")));
        let last = lookups.last.lock().unwrap();
        assert_eq!(last.len(), 1);
        assert!(last.contains_key("tr_new"));
    }
}
