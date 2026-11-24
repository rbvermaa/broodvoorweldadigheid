use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;

/// Stored status of a bake day. See ADR 0008 for how it maps to the public drop status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BakeDayStatus {
    Draft,
    Open,
    Closed,
    Cancelled,
    Done,
}

impl BakeDayStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
            Self::Done => "done",
        }
    }

    /// Done and cancelled days can no longer change.
    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Cancelled | Self::Done)
    }
}

impl fmt::Display for BakeDayStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown bake day status: {0}")]
pub struct UnknownStatus(pub String);

impl FromStr for BakeDayStatus {
    type Err = UnknownStatus;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [
            Self::Draft,
            Self::Open,
            Self::Closed,
            Self::Cancelled,
            Self::Done,
        ]
        .into_iter()
        .find(|status| status.as_str() == s)
        .ok_or_else(|| UnknownStatus(s.to_owned()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Open,
    Close,
    Finish,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransitionRefused {
    #[error("cannot {transition:?} a {from} bake day")]
    NotAllowed {
        from: BakeDayStatus,
        transition: Transition,
    },
    #[error("a production-only bake day never opens online")]
    ProductionOnly,
    #[error("the order deadline has passed")]
    DeadlinePassed,
    #[error("baking has not started yet")]
    NotBaked,
}

/// The facts about a bake day that decide whether a transition is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusFacts {
    pub status: BakeDayStatus,
    pub online_ordering: bool,
    pub cutoff_at: Option<Timestamp>,
    pub bake_starts_at: Timestamp,
}

/// `draft -> open -> closed -> done`, any status but done to cancelled. A production-only
/// day goes from draft straight to closed. Opening is refused once the deadline has
/// passed; finishing is refused before baking starts.
///
/// # Errors
/// [`TransitionRefused`] with the reason.
pub fn next_status(
    facts: StatusFacts,
    transition: Transition,
    now: Timestamp,
) -> Result<BakeDayStatus, TransitionRefused> {
    use BakeDayStatus::{Cancelled, Closed, Done, Draft, Open};
    let refused = TransitionRefused::NotAllowed {
        from: facts.status,
        transition,
    };
    match (facts.status, transition) {
        (Draft, Transition::Open) if !facts.online_ordering => {
            Err(TransitionRefused::ProductionOnly)
        }
        (Draft, Transition::Open) => match facts.cutoff_at {
            Some(cutoff) if now < cutoff => Ok(Open),
            _ => Err(TransitionRefused::DeadlinePassed),
        },
        (Open, Transition::Close) => Ok(Closed),
        (Draft, Transition::Close) if !facts.online_ordering => Ok(Closed),
        (Closed, Transition::Finish) if now < facts.bake_starts_at => {
            Err(TransitionRefused::NotBaked)
        }
        (Closed, Transition::Finish) => Ok(Done),
        (Draft | Open | Closed, Transition::Cancel) => Ok(Cancelled),
        _ => Err(refused),
    }
}

/// The status shown on the public drop calendar (F1a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicStatus {
    Planned,
    MenuAnnounced,
    Open,
    SoldOut,
    Closed,
    Cancelled,
}

/// `None` for production-only days, which never appear on the public calendar. A draft or
/// open day past its deadline shows closed. `opens_at` plays no part: a day shows open only
/// once it is stored as open (ADR 0008).
#[must_use]
pub fn public_status(
    facts: StatusFacts,
    menu_publish_at: Option<Timestamp>,
    now: Timestamp,
    sold_out: bool,
) -> Option<PublicStatus> {
    if !facts.online_ordering {
        return None;
    }
    let past_cutoff = facts.cutoff_at.is_none_or(|cutoff| now >= cutoff);
    Some(match facts.status {
        BakeDayStatus::Draft | BakeDayStatus::Open if past_cutoff => PublicStatus::Closed,
        BakeDayStatus::Draft => match menu_publish_at {
            Some(at) if at <= now => PublicStatus::MenuAnnounced,
            _ => PublicStatus::Planned,
        },
        BakeDayStatus::Open if sold_out => PublicStatus::SoldOut,
        BakeDayStatus::Open => PublicStatus::Open,
        BakeDayStatus::Closed | BakeDayStatus::Done => PublicStatus::Closed,
        BakeDayStatus::Cancelled => PublicStatus::Cancelled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use BakeDayStatus::{Cancelled, Closed, Done, Draft, Open};

    const ALL: [BakeDayStatus; 5] = [Draft, Open, Closed, Cancelled, Done];
    const TRANSITIONS: [Transition; 4] = [
        Transition::Open,
        Transition::Close,
        Transition::Finish,
        Transition::Cancel,
    ];

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn facts(status: BakeDayStatus) -> StatusFacts {
        StatusFacts {
            status,
            online_ordering: true,
            cutoff_at: Some(ts("2026-10-29T19:00:00Z")),
            bake_starts_at: ts("2026-10-31T03:00:00Z"),
        }
    }

    const BEFORE_CUTOFF: &str = "2026-10-28T12:00:00Z";
    const AFTER_BAKE: &str = "2026-10-31T09:00:00Z";

    #[test]
    fn statuses_round_trip_through_text() {
        for status in ALL {
            assert_eq!(status.as_str().parse(), Ok(status));
            assert_eq!(status.to_string(), status.as_str());
        }
        assert_eq!(
            "sold_out".parse::<BakeDayStatus>(),
            Err(UnknownStatus("sold_out".to_owned()))
        );
        assert_eq!(
            UnknownStatus("x".to_owned()).to_string(),
            "unknown bake day status: x"
        );
    }

    #[test]
    fn only_done_and_cancelled_are_final() {
        let finals: Vec<_> = ALL.into_iter().filter(|s| s.is_final()).collect();
        assert_eq!(finals, [Cancelled, Done]);
    }

    #[test]
    fn allowed_transitions() {
        let allowed = |from, transition, now| next_status(facts(from), transition, ts(now)).ok();
        let mut table = Vec::new();
        for from in ALL {
            for transition in TRANSITIONS {
                let now = if transition == Transition::Finish {
                    AFTER_BAKE
                } else {
                    BEFORE_CUTOFF
                };
                if let Some(to) = allowed(from, transition, now) {
                    table.push((from, transition, to));
                }
            }
        }
        assert_eq!(
            table,
            [
                (Draft, Transition::Open, Open),
                (Draft, Transition::Cancel, Cancelled),
                (Open, Transition::Close, Closed),
                (Open, Transition::Cancel, Cancelled),
                (Closed, Transition::Finish, Done),
                (Closed, Transition::Cancel, Cancelled),
            ]
        );
    }

    #[test]
    fn refused_transitions_say_why() {
        assert_eq!(
            next_status(facts(Done), Transition::Cancel, ts(AFTER_BAKE)),
            Err(TransitionRefused::NotAllowed {
                from: Done,
                transition: Transition::Cancel
            })
        );
        assert_eq!(
            next_status(facts(Draft), Transition::Open, ts("2026-10-29T19:00:00Z")),
            Err(TransitionRefused::DeadlinePassed)
        );
        assert_eq!(
            next_status(
                StatusFacts {
                    cutoff_at: None,
                    ..facts(Draft)
                },
                Transition::Open,
                ts(BEFORE_CUTOFF)
            ),
            Err(TransitionRefused::DeadlinePassed)
        );
        assert_eq!(
            next_status(
                facts(Closed),
                Transition::Finish,
                ts("2026-10-31T02:59:59Z")
            ),
            Err(TransitionRefused::NotBaked)
        );
        assert_eq!(
            TransitionRefused::NotAllowed {
                from: Done,
                transition: Transition::Open
            }
            .to_string(),
            "cannot Open a done bake day"
        );
    }

    #[test]
    fn production_only_days_skip_open() {
        let production_only = StatusFacts {
            online_ordering: false,
            cutoff_at: None,
            ..facts(Draft)
        };
        let now = ts(BEFORE_CUTOFF);
        assert_eq!(
            next_status(production_only, Transition::Open, now),
            Err(TransitionRefused::ProductionOnly)
        );
        assert_eq!(
            next_status(production_only, Transition::Close, now),
            Ok(Closed)
        );
        assert_eq!(
            next_status(facts(Draft), Transition::Close, now),
            Err(TransitionRefused::NotAllowed {
                from: Draft,
                transition: Transition::Close
            })
        );
    }

    #[test]
    fn public_status_follows_the_day() {
        let menu = Some(ts("2026-10-28T08:00:00Z"));
        let public = |status, now, sold_out| public_status(facts(status), menu, ts(now), sold_out);
        assert_eq!(
            public(Draft, "2026-10-27T12:00:00Z", false),
            Some(PublicStatus::Planned)
        );
        assert_eq!(
            public(Draft, "2026-10-28T08:00:00Z", false),
            Some(PublicStatus::MenuAnnounced)
        );
        assert_eq!(public(Open, BEFORE_CUTOFF, false), Some(PublicStatus::Open));
        assert_eq!(
            public(Open, BEFORE_CUTOFF, true),
            Some(PublicStatus::SoldOut)
        );
        assert_eq!(
            public(Open, "2026-10-29T19:00:00Z", false),
            Some(PublicStatus::Closed)
        );
        assert_eq!(
            public(Closed, BEFORE_CUTOFF, true),
            Some(PublicStatus::Closed)
        );
        assert_eq!(public(Done, AFTER_BAKE, false), Some(PublicStatus::Closed));
        assert_eq!(
            public(Cancelled, BEFORE_CUTOFF, false),
            Some(PublicStatus::Cancelled)
        );
        assert_eq!(
            public_status(facts(Draft), None, ts(BEFORE_CUTOFF), false),
            Some(PublicStatus::Planned)
        );
        assert_eq!(
            public_status(facts(Draft), menu, ts("2026-10-29T19:00:00Z"), false),
            Some(PublicStatus::Closed)
        );
    }

    #[test]
    fn production_only_days_are_not_public() {
        let production_only = StatusFacts {
            online_ordering: false,
            ..facts(Closed)
        };
        assert_eq!(
            public_status(production_only, None, ts(BEFORE_CUTOFF), false),
            None
        );
    }
}
