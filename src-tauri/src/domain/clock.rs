//! Injectable time source.

use std::sync::Mutex;

use chrono::{DateTime, Duration, FixedOffset, Local, LocalResult, NaiveDateTime, TimeZone, Utc};

/// Source of the current instant and of the user's local time zone.
///
/// The scheduler, sync engine and state machine timers take a `Clock` instead
/// of calling `Utc::now()` so they can be unit-tested. Local-time conversion
/// lives here too, because quiet hours and all-day events are defined in the
/// user's local time and tests must be able to pin the time zone.
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> DateTime<Utc>;

    /// Converts an instant to the user's local wall-clock time.
    fn utc_to_local(&self, instant: DateTime<Utc>) -> NaiveDateTime;

    /// Converts a local wall-clock time to an instant.
    ///
    /// Ambiguous times (DST fall-back) resolve to the earlier instant.
    /// Non-existent times (DST spring-forward gap) resolve to the first
    /// valid local time after the gap.
    fn local_to_utc(&self, local: NaiveDateTime) -> DateTime<Utc>;
}

/// The real clock: system time and the OS local time zone.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn utc_to_local(&self, instant: DateTime<Utc>) -> NaiveDateTime {
        instant.with_timezone(&Local).naive_local()
    }

    fn local_to_utc(&self, local: NaiveDateTime) -> DateTime<Utc> {
        resolve_local(&Local, local)
    }
}

/// Resolves a local time in `tz` per the rules documented on
/// [`Clock::local_to_utc`].
fn resolve_local<Tz: TimeZone>(tz: &Tz, local: NaiveDateTime) -> DateTime<Utc> {
    // Walk forward minute by minute out of a DST gap; gaps are at most a few
    // hours, so cap the walk at 24 h and fall back to treating it as UTC.
    for minutes in 0..=(24 * 60) {
        match tz.from_local_datetime(&(local + Duration::minutes(minutes))) {
            LocalResult::Single(t) => return t.with_timezone(&Utc),
            LocalResult::Ambiguous(earliest, _) => return earliest.with_timezone(&Utc),
            LocalResult::None => continue,
        }
    }
    Utc.from_utc_datetime(&local)
}

/// A manually driven clock for tests, with a fixed UTC offset as its
/// "local" time zone (default UTC).
#[derive(Debug)]
pub struct FakeClock {
    now: Mutex<DateTime<Utc>>,
    offset: FixedOffset,
}

impl FakeClock {
    /// A fake clock at `now` whose local time zone is UTC.
    pub fn new(now: DateTime<Utc>) -> Self {
        Self::with_offset(now, FixedOffset::east_opt(0).expect("zero offset is valid"))
    }

    /// A fake clock at `now` whose local time zone is the fixed `offset`.
    pub fn with_offset(now: DateTime<Utc>, offset: FixedOffset) -> Self {
        Self {
            now: Mutex::new(now),
            offset,
        }
    }

    /// Jumps to `now`.
    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.lock().unwrap_or_else(|e| e.into_inner()) = now;
    }

    /// Moves time forward (or backward, for a negative duration).
    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap_or_else(|e| e.into_inner());
        *now += by;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn utc_to_local(&self, instant: DateTime<Utc>) -> NaiveDateTime {
        instant.with_timezone(&self.offset).naive_local()
    }

    fn local_to_utc(&self, local: NaiveDateTime) -> DateTime<Utc> {
        resolve_local(&self.offset, local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, h, m, 0).unwrap()
    }

    #[test]
    fn fake_clock_set_and_advance() {
        let clock = FakeClock::new(at(9, 0));
        assert_eq!(clock.now(), at(9, 0));
        clock.advance(Duration::minutes(5));
        assert_eq!(clock.now(), at(9, 5));
        clock.set(at(12, 0));
        assert_eq!(clock.now(), at(12, 0));
    }

    #[test]
    fn fake_clock_converts_with_its_offset() {
        let clock = FakeClock::with_offset(at(9, 0), FixedOffset::east_opt(3600).unwrap());
        let local = clock.utc_to_local(at(9, 0));
        assert_eq!(
            local,
            NaiveDate::from_ymd_opt(2026, 11, 16)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap()
        );
        assert_eq!(clock.local_to_utc(local), at(9, 0));
    }

    #[test]
    fn system_clock_round_trips() {
        let clock = SystemClock;
        let now = clock.now();
        let local = clock.utc_to_local(now);
        // Round trip is exact except inside a DST fall-back hour, where the
        // earlier instant wins; it is never more than an hour off.
        let diff = (clock.local_to_utc(local) - now).num_minutes().abs();
        assert!(diff <= 60);
    }
}
