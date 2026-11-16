//! The buddy animation state machine (spec section 5, task 1.C).
//!
//! [`next_state`] is a pure function: it holds no hidden state, performs no
//! I/O and never panics. Callers keep the current [`BuddyState`] and feed it
//! every [`BuddyEvent`].

use crate::domain::{BuddyEvent, BuddyState};

/// Returns the state the buddy moves to when `event` happens in `state`.
///
/// Transitions (spec section 5):
///
/// | From       | Event                                  | To         |
/// |------------|----------------------------------------|------------|
/// | any        | `ReminderDue`                          | `Alert`    |
/// | `Alert`    | `BubbleDismissed` or `BubbleSnoozed`   | `Idle`     |
/// | `Idle`     | `SyncStarted`                          | `Thinking` |
/// | `Thinking` | `SyncEnded`                            | `Idle`     |
/// | `Idle`     | `IdleTimeout`                          | `Sleeping` |
/// | `Sleeping` | `UserInteracted`                       | `Idle`     |
///
/// Every other pair leaves the state unchanged. `Alert` has the highest
/// priority: `ReminderDue` interrupts every state, and nothing but dismissing
/// or snoozing the bubble leaves `Alert`. Notable no-ops:
///
/// - `SyncStarted` while `Sleeping` or `Alert`: stay. A background sync must
///   not wake the cat or interrupt a reminder (the thinking animation is
///   cosmetic). `SyncStarted` while `Thinking` also stays `Thinking`.
/// - `SyncEnded` while not `Thinking`: stay. In particular `Alert` stays
///   `Alert`, and a sync that ends while asleep does not wake the cat.
/// - `IdleTimeout` while `Thinking`, `Sleeping` or `Alert`: stay. The cat only
///   falls asleep from `Idle`; a pending reminder is never put to sleep.
/// - `UserInteracted` while `Alert`: stay until the bubble is dismissed or
///   snoozed. While `Idle` or `Thinking` it only resets the caller's idle
///   timer, so the state is unchanged.
/// - `BubbleDismissed` / `BubbleSnoozed` while not `Alert`: no-op (stale or
///   duplicate events).
///
/// Leaving `Alert` always lands in `Idle`, even if a sync is still running
/// (the spec table says `alert` to `idle`). This function does not remember
/// the interrupted state. A caller that wants the thinking animation back
/// should re-emit `SyncStarted` after the bubble closes while its sync is
/// still in progress; the following `SyncEnded` then returns to `Idle`.
#[must_use]
pub fn next_state(state: BuddyState, event: BuddyEvent) -> BuddyState {
    use BuddyEvent as E;
    use BuddyState as S;

    match (state, event) {
        (_, E::ReminderDue) => S::Alert,
        (S::Alert, E::BubbleDismissed | E::BubbleSnoozed) => S::Idle,
        (S::Idle, E::SyncStarted) => S::Thinking,
        (S::Thinking, E::SyncEnded) => S::Idle,
        (S::Idle, E::IdleTimeout) => S::Sleeping,
        (S::Sleeping, E::UserInteracted) => S::Idle,
        (unchanged, _) => unchanged,
    }
}

#[cfg(test)]
mod tests {
    use super::next_state;
    use crate::domain::{BuddyEvent, BuddyState};

    use crate::domain::BuddyEvent::*;
    use crate::domain::BuddyState::*;

    /// Every event variant. The `match` in `all_events_listed` makes adding a
    /// variant without updating this list a compile error.
    const ALL_EVENTS: [BuddyEvent; 7] = [
        ReminderDue,
        BubbleDismissed,
        BubbleSnoozed,
        SyncStarted,
        SyncEnded,
        IdleTimeout,
        UserInteracted,
    ];

    #[test]
    fn all_events_listed() {
        for event in ALL_EVENTS {
            match event {
                ReminderDue | BubbleDismissed | BubbleSnoozed | SyncStarted | SyncEnded
                | IdleTimeout | UserInteracted => {}
            }
        }
    }

    /// Expected result for every (state, event) pair, one row per state in
    /// `BuddyState::ALL` order, one column per event in `ALL_EVENTS` order.
    #[test]
    fn exhaustive_transition_table() {
        #[rustfmt::skip]
        let expected: [(BuddyState, [BuddyState; 7]); 4] = [
            //           ReminderDue Dismissed Snoozed  SyncStarted SyncEnded IdleTimeout UserInteracted
            (Idle,     [Alert,      Idle,     Idle,     Thinking,  Idle,     Sleeping,  Idle]),
            (Sleeping, [Alert,      Sleeping, Sleeping, Sleeping,  Sleeping, Sleeping,  Idle]),
            (Thinking, [Alert,      Thinking, Thinking, Thinking,  Idle,     Thinking,  Thinking]),
            (Alert,    [Alert,      Idle,     Idle,     Alert,     Alert,    Alert,     Alert]),
        ];

        assert_eq!(expected.map(|(s, _)| s), BuddyState::ALL);
        for (state, row) in expected {
            for (event, want) in ALL_EVENTS.into_iter().zip(row) {
                assert_eq!(
                    next_state(state, event),
                    want,
                    "next_state({state:?}, {event:?})"
                );
            }
        }
    }

    #[test]
    fn reminder_due_interrupts_every_state() {
        for state in BuddyState::ALL {
            assert_eq!(next_state(state, ReminderDue), Alert, "from {state:?}");
        }
    }

    #[test]
    fn only_dismiss_or_snooze_leaves_alert() {
        for event in ALL_EVENTS {
            let want = if matches!(event, BubbleDismissed | BubbleSnoozed) {
                Idle
            } else {
                Alert
            };
            assert_eq!(next_state(Alert, event), want, "{event:?}");
        }
    }

    #[test]
    fn alert_dismissed_or_snoozed_returns_to_idle() {
        assert_eq!(next_state(Alert, BubbleDismissed), Idle);
        assert_eq!(next_state(Alert, BubbleSnoozed), Idle);
    }

    #[test]
    fn sync_start_and_end_toggle_thinking() {
        assert_eq!(next_state(Idle, SyncStarted), Thinking);
        assert_eq!(next_state(Thinking, SyncEnded), Idle);
    }

    #[test]
    fn idle_timeout_puts_idle_cat_to_sleep() {
        assert_eq!(next_state(Idle, IdleTimeout), Sleeping);
    }

    #[test]
    fn interaction_or_reminder_wakes_sleeping_cat() {
        assert_eq!(next_state(Sleeping, UserInteracted), Idle);
        assert_eq!(next_state(Sleeping, ReminderDue), Alert);
    }

    #[test]
    fn sync_does_not_wake_or_interrupt() {
        assert_eq!(next_state(Sleeping, SyncStarted), Sleeping);
        assert_eq!(next_state(Alert, SyncStarted), Alert);
        assert_eq!(next_state(Alert, SyncEnded), Alert);
    }

    #[test]
    fn dismissing_during_a_sync_returns_to_idle() {
        let state = next_state(Idle, SyncStarted);
        let state = next_state(state, ReminderDue);
        assert_eq!(state, Alert);
        // No hidden memory of the sync: the caller re-emits SyncStarted if
        // the sync is still running.
        assert_eq!(next_state(state, BubbleDismissed), Idle);
    }
}
