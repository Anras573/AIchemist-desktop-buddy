//! Merging events of all accounts (spec 6.4).

use std::collections::HashMap;

use crate::domain::{AccountConfig, Clock, Event, EventTime, ResponseStatus};

/// Merges the events of all accounts into one list for the scheduler.
///
/// 1. **Filter.** Keeps only events of a configured account whose calendar
///    is in that account's `included_calendars`.
/// 2. **De-duplicate** (best effort) by (trimmed title, start, end): the
///    same meeting seen through two accounts appears once. Timed and all-day
///    events never match each other. The copy kept is the first by:
///    * a definite response (organizer, accepted, tentative, declined)
///      before "not responded", before "unknown", so the copy that knows
///      whether the user declined wins;
///    * then account order in settings, then calendar order in
///      `included_calendars`, then event id (deterministic).
/// 3. **Sort** by start instant (all-day events at local midnight via
///    `clock`), all-day before timed at the same instant, then end, title
///    and key.
pub fn merge_events<C: Clock + ?Sized>(
    events: impl IntoIterator<Item = Event>,
    accounts: &[AccountConfig],
    clock: &C,
) -> Vec<Event> {
    // Winner per dedupe key, with its preference rank.
    let mut kept: HashMap<(String, EventTime), (Rank, Event)> = HashMap::new();
    for event in events {
        let Some(rank) = rank(&event, accounts) else {
            continue;
        };
        let key = (event.title.trim().to_owned(), event.time);
        match kept.get(&key) {
            Some((best, _)) if *best <= rank => {}
            _ => {
                kept.insert(key, (rank, event));
            }
        }
    }

    let mut merged: Vec<(chrono::DateTime<chrono::Utc>, Event)> = kept
        .into_values()
        .map(|(_, e)| (e.time.start_utc(clock), e))
        .collect();
    merged.sort_by(|(a_start, a), (b_start, b)| {
        a_start
            .cmp(b_start)
            .then_with(|| b.time.is_all_day().cmp(&a.time.is_all_day()))
            .then_with(|| a.time.end_utc(clock).cmp(&b.time.end_utc(clock)))
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.key().cmp(&b.key()))
    });
    merged.into_iter().map(|(_, e)| e).collect()
}

/// Preference among duplicates; lower wins.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    response: u8,
    account: usize,
    calendar: usize,
    event_id: String,
}

/// The event's rank, or `None` if its account or calendar is not included.
fn rank(event: &Event, accounts: &[AccountConfig]) -> Option<Rank> {
    let account = accounts.iter().position(|a| a.id == event.account_id)?;
    let calendar = accounts[account]
        .included_calendars
        .iter()
        .position(|c| *c == event.calendar_id)?;
    Some(Rank {
        response: response_rank(event.response),
        account,
        calendar,
        event_id: event.id.as_str().to_owned(),
    })
}

fn response_rank(response: ResponseStatus) -> u8 {
    match response {
        ResponseStatus::Organizer
        | ResponseStatus::Accepted
        | ResponseStatus::Tentative
        | ResponseStatus::Declined => 0,
        ResponseStatus::NotResponded => 1,
        ResponseStatus::Unknown => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AccountConnection, AccountId, CalendarId, EventId, FakeClock, ResponseStatus,
    };
    use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone, Utc};

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, h, m, 0).unwrap()
    }

    fn account(id: &str, calendars: &[&str]) -> AccountConfig {
        AccountConfig {
            id: AccountId::new(id),
            display_name: id.into(),
            connection: AccountConnection::CalDav {
                server_url: "https://example.com/".into(),
                username: "me".into(),
            },
            included_calendars: calendars.iter().map(|c| CalendarId::new(*c)).collect(),
        }
    }

    fn timed(acc: &str, cal: &str, id: &str, title: &str, start: DateTime<Utc>) -> Event {
        Event {
            id: EventId::new(id),
            account_id: AccountId::new(acc),
            calendar_id: CalendarId::new(cal),
            title: title.into(),
            time: EventTime::Timed {
                start,
                end: start + chrono::Duration::hours(1),
            },
            location: None,
            response: ResponseStatus::Unknown,
        }
    }

    fn all_day(acc: &str, cal: &str, id: &str, day: u32) -> Event {
        Event {
            time: EventTime::AllDay {
                start: NaiveDate::from_ymd_opt(2026, 11, day).unwrap(),
                end: NaiveDate::from_ymd_opt(2026, 11, day + 1).unwrap(),
            },
            ..timed(acc, cal, id, "Holiday", at(0, 0))
        }
    }

    fn ids(events: &[Event]) -> Vec<&str> {
        events.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn sorts_by_start_across_accounts() {
        let accounts = [account("a", &["c"]), account("b", &["c"])];
        let clock = FakeClock::new(at(8, 0));
        let merged = merge_events(
            vec![
                timed("a", "c", "a3", "Three", at(15, 0)),
                timed("b", "c", "b1", "One", at(9, 0)),
                timed("a", "c", "a2", "Two", at(11, 30)),
            ],
            &accounts,
            &clock,
        );
        assert_eq!(ids(&merged), ["b1", "a2", "a3"]);
    }

    #[test]
    fn all_day_events_sort_at_local_midnight() {
        let accounts = [account("a", &["c"])];
        // Local time is UTC+2: 17 Nov local midnight is 16 Nov 22:00 UTC.
        let clock = FakeClock::with_offset(at(8, 0), FixedOffset::east_opt(2 * 3600).unwrap());
        let merged = merge_events(
            vec![
                timed("a", "c", "late", "Late", at(23, 0)),
                all_day("a", "c", "tomorrow", 17),
                timed("a", "c", "evening", "Evening", at(21, 0)),
                // Exactly at local midnight: the all-day event comes first.
                timed("a", "c", "midnight", "Midnight", at(22, 0)),
                all_day("a", "c", "today", 16),
            ],
            &accounts,
            &clock,
        );
        assert_eq!(
            ids(&merged),
            ["today", "evening", "tomorrow", "midnight", "late"]
        );
    }

    #[test]
    fn dedupes_same_event_in_two_accounts() {
        let accounts = [account("work", &["c"]), account("home", &["c"])];
        let clock = FakeClock::new(at(8, 0));
        let mut home = timed("home", "c", "h", "Standup", at(9, 0));
        home.response = ResponseStatus::Accepted;
        let work = timed("work", "c", "w", " Standup ", at(9, 0));
        let merged = merge_events(vec![work, home], &accounts, &clock);
        // The copy with a definite response wins over the earlier account.
        assert_eq!(ids(&merged), ["h"]);

        // Same response quality: the first account in settings wins.
        let merged = merge_events(
            vec![
                timed("home", "c", "h", "Standup", at(9, 0)),
                timed("work", "c", "w", "Standup", at(9, 0)),
            ],
            &accounts,
            &clock,
        );
        assert_eq!(ids(&merged), ["w"]);
    }

    #[test]
    fn different_title_or_time_is_not_a_duplicate() {
        let accounts = [account("a", &["c"]), account("b", &["c"])];
        let clock = FakeClock::new(at(8, 0));
        let mut longer = timed("b", "c", "b2", "Standup", at(9, 0));
        longer.time = EventTime::Timed {
            start: at(9, 0),
            end: at(9, 30),
        };
        let merged = merge_events(
            vec![
                timed("a", "c", "a1", "Standup", at(9, 0)),
                timed("b", "c", "b1", "Review", at(9, 0)),
                longer,
                all_day("b", "c", "b3", 16),
                all_day("a", "c", "a3", 16),
            ],
            &accounts,
            &clock,
        );
        // The two all-day "Holiday" events are duplicates; the rest are not.
        assert_eq!(ids(&merged), ["a3", "b2", "b1", "a1"]);
    }

    #[test]
    fn filters_by_included_calendars_and_accounts() {
        let accounts = [account("a", &["work"]), account("b", &[])];
        let clock = FakeClock::new(at(8, 0));
        let merged = merge_events(
            vec![
                timed("a", "work", "in", "In", at(9, 0)),
                timed("a", "home", "other-calendar", "X", at(9, 0)),
                timed("b", "work", "no-calendars", "Y", at(9, 0)),
                timed("gone", "work", "removed-account", "Z", at(9, 0)),
            ],
            &accounts,
            &clock,
        );
        assert_eq!(ids(&merged), ["in"]);
    }
}
