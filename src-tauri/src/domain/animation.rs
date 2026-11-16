//! Buddy animation states and the events that drive them (spec section 5).
//!
//! The transition function `next_state` is implemented separately (task 1.C);
//! this module only defines the vocabulary.

use serde::{Deserialize, Serialize};

/// Animation state of the cat. Serialized names (`"idle"`, `"sleeping"`,
/// `"thinking"`, `"alert"`) match the state keys in a sprite pack's `pack.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BuddyState {
    /// Awake and waiting. The initial state.
    #[default]
    Idle,
    /// Asleep after a period without user interaction.
    Sleeping,
    /// A calendar sync is running (cosmetic).
    Thinking,
    /// A reminder bubble is showing. Highest priority.
    Alert,
}

impl BuddyState {
    /// All states, in declaration order.
    pub const ALL: [BuddyState; 4] = [Self::Idle, Self::Sleeping, Self::Thinking, Self::Alert];
}

/// Something that happened which may change the [`BuddyState`]. Covers every
/// row of the section 5 state table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BuddyEvent {
    /// A reminder became due and a bubble is shown (from any state to `alert`).
    ReminderDue,
    /// The user dismissed the bubble (or marked its task done) and no further
    /// bubble is queued (`alert` to `idle`).
    BubbleDismissed,
    /// The user snoozed the bubble and no further bubble is queued
    /// (`alert` to `idle`).
    BubbleSnoozed,
    /// A calendar sync started (`idle` to `thinking`).
    SyncStarted,
    /// A calendar sync finished, successfully or not (`thinking` to `idle`).
    SyncEnded,
    /// No user interaction for the configured sleep-after-idle minutes
    /// (`idle` to `sleeping`).
    IdleTimeout,
    /// The user hovered over or clicked the cat (`sleeping` to `idle`; also
    /// resets the idle timer).
    UserInteracted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_names_match_sprite_pack_keys() {
        let names: Vec<String> = BuddyState::ALL
            .iter()
            .map(|s| {
                serde_json::to_value(s)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(names, ["idle", "sleeping", "thinking", "alert"]);
        assert_eq!(BuddyState::default(), BuddyState::Idle);
    }
}
