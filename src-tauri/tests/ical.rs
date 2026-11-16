//! iCalendar parser tests against recorded-style fixtures with fake data
//! (`tests/fixtures/ical/`).

use aichemist_desktop_buddy_lib::domain::{
    AccountId, CalendarId, Clock, Error, Event, EventTime, FakeClock, ResponseStatus,
};
use aichemist_desktop_buddy_lib::ical::{parse_events, ParseOptions, MAX_OCCURRENCES_PER_EVENT};
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone, Utc};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("fixtures/ical/", $name))
    };
}

const USER: &str = "test.user@example.com";

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn utc_clock() -> FakeClock {
    FakeClock::new(utc(2026, 11, 16, 8, 0))
}

fn parse_with(
    ics: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    users: &[String],
    clock: &dyn Clock,
) -> Result<Vec<Event>, Error> {
    let account = AccountId::new("acc-1");
    let calendar = CalendarId::new("/calendars/test/personal/");
    let opts = ParseOptions {
        account_id: &account,
        calendar_id: &calendar,
        user_addresses: users,
        clock,
    };
    let lf = parse_events(ics, from, to, &opts);
    // Every fixture must give the same result with CRLF line endings.
    let crlf = parse_events(&ics.replace('\n', "\r\n"), from, to, &opts);
    assert_eq!(lf, crlf, "LF and CRLF inputs differ");
    lf
}

fn parse(ics: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Event> {
    parse_with(ics, from, to, &[USER.to_owned()], &utc_clock()).expect("fixture parses")
}

fn timed(e: &Event) -> (DateTime<Utc>, DateTime<Utc>) {
    match e.time {
        EventTime::Timed { start, end } => (start, end),
        EventTime::AllDay { .. } => panic!("expected a timed event"),
    }
}

fn ids(events: &[Event]) -> Vec<&str> {
    events.iter().map(|e| e.id.as_str()).collect()
}

fn starts(events: &[Event]) -> Vec<DateTime<Utc>> {
    events.iter().map(|e| timed(e).0).collect()
}

fn by_id<'a>(events: &'a [Event], id: &str) -> &'a Event {
    events
        .iter()
        .find(|e| e.id.as_str() == id)
        .unwrap_or_else(|| panic!("no event {id}"))
}

const WEEK_FROM: (i32, u32, u32) = (2026, 11, 16);

fn week() -> (DateTime<Utc>, DateTime<Utc>) {
    let (y, m, d) = WEEK_FROM;
    (utc(y, m, d, 0, 0), utc(y, m, d + 7, 0, 0))
}

#[test]
fn simple_utc_events() {
    let (from, to) = week();
    let events = parse(fixture!("simple.ics"), from, to);
    assert_eq!(
        ids(&events),
        [
            "simple-1@example.com",
            "simple-2@example.com",
            "simple-3@example.com"
        ]
    );

    let e = &events[0];
    assert_eq!(e.title, "Team sync, weekly; agenda");
    assert_eq!(e.location.as_deref(), Some("Room 4\nSecond floor"));
    assert_eq!(
        timed(e),
        (utc(2026, 11, 16, 9, 0), utc(2026, 11, 16, 9, 30))
    );
    assert_eq!(e.account_id.as_str(), "acc-1");
    assert_eq!(e.calendar_id.as_str(), "/calendars/test/personal/");
    assert_eq!(e.response, ResponseStatus::Unknown);

    // DURATION instead of DTEND; an empty LOCATION is None.
    assert_eq!(
        timed(&events[1]),
        (utc(2026, 11, 17, 13, 0), utc(2026, 11, 17, 14, 15))
    );
    assert_eq!(events[1].location, None);

    // No DTEND and no DURATION: instantaneous.
    assert_eq!(
        timed(&events[2]),
        (utc(2026, 11, 18, 8, 0), utc(2026, 11, 18, 8, 0))
    );
}

#[test]
fn all_day_single_and_multi_day() {
    let (from, to) = week();
    let events = parse(fixture!("all_day.ics"), from, to);
    assert_eq!(
        ids(&events),
        ["allday-single@example.com", "allday-multi@example.com"]
    );
    assert_eq!(
        events[0].time,
        EventTime::AllDay {
            start: date(2026, 11, 17),
            end: date(2026, 11, 18)
        }
    );
    assert_eq!(
        events[1].time,
        EventTime::AllDay {
            start: date(2026, 11, 19),
            end: date(2026, 11, 22)
        }
    );
    assert!(events.iter().all(Event::is_all_day));
}

#[test]
fn all_day_overlap_uses_local_midnight() {
    // The multi-day event ends at local midnight starting 22 Nov; a window
    // starting just after it (in UTC+2) excludes it.
    let clock = FakeClock::with_offset(
        utc(2026, 11, 16, 0, 0),
        FixedOffset::east_opt(7200).unwrap(),
    );
    let events = parse_with(
        fixture!("all_day.ics"),
        utc(2026, 11, 21, 22, 0),
        utc(2026, 11, 25, 0, 0),
        &[],
        &clock,
    )
    .unwrap();
    assert!(events.is_empty(), "{:?}", ids(&events));
}

#[test]
fn floating_times_use_the_clock_zone() {
    let clock = FakeClock::with_offset(
        utc(2026, 11, 16, 0, 0),
        FixedOffset::east_opt(7200).unwrap(),
    );
    let (from, to) = week();
    let events = parse_with(fixture!("floating.ics"), from, to, &[], &clock).unwrap();
    let e = by_id(&events, "floating-1@example.com");
    assert_eq!(timed(e), (utc(2026, 11, 16, 7, 0), utc(2026, 11, 16, 8, 0)));
    // A TZID without a VTIMEZONE falls back to floating.
    let e = by_id(&events, "floating-unknown-tz@example.com");
    assert_eq!(timed(e).0, utc(2026, 11, 16, 12, 0));
    // ...unless it names UTC.
    let e = by_id(&events, "utc-alias@example.com");
    assert_eq!(timed(e).0, utc(2026, 11, 16, 16, 0));
}

#[test]
fn copenhagen_weekly_keeps_wall_clock_across_dst() {
    let events = parse(
        fixture!("tz_copenhagen_dst.ics"),
        utc(2026, 10, 12, 0, 0),
        utc(2026, 11, 3, 0, 0),
    );
    assert_eq!(
        starts(&events),
        [
            utc(2026, 10, 12, 7, 0),
            utc(2026, 10, 19, 7, 0),
            utc(2026, 10, 26, 8, 0),
            utc(2026, 11, 2, 8, 0),
        ]
    );
    assert_eq!(
        ids(&events),
        [
            "cph-weekly@example.com/2026-10-12T07:00:00Z",
            "cph-weekly@example.com/2026-10-19T07:00:00Z",
            "cph-weekly@example.com/2026-10-26T08:00:00Z",
            "cph-weekly@example.com/2026-11-02T08:00:00Z",
        ]
    );
    assert!(events
        .iter()
        .all(|e| timed(e).1 - timed(e).0 == chrono::Duration::hours(1)));
}

#[test]
fn copenhagen_time_in_dst_gap_moves_forward() {
    let events = parse(
        fixture!("tz_copenhagen_dst.ics"),
        utc(2026, 3, 29, 0, 0),
        utc(2026, 3, 30, 0, 0),
    );
    let e = by_id(&events, "cph-gap@example.com");
    // 02:30 does not exist; it is read with the pre-gap offset (+01:00),
    // i.e. 03:30 CEST.
    assert_eq!(timed(e), (utc(2026, 3, 29, 1, 30), utc(2026, 3, 29, 2, 0)));
}

#[test]
fn new_york_daily_across_dst_end_and_start() {
    let events = parse(
        fixture!("tz_new_york_dst.ics"),
        utc(2026, 10, 29, 0, 0),
        utc(2026, 11, 5, 0, 0),
    );
    assert_eq!(
        starts(&events),
        [
            utc(2026, 10, 30, 13, 0),
            utc(2026, 10, 31, 13, 0),
            utc(2026, 11, 1, 14, 0),
            utc(2026, 11, 2, 14, 0),
        ]
    );

    let events = parse(
        fixture!("tz_new_york_dst.ics"),
        utc(2026, 3, 1, 0, 0),
        utc(2026, 3, 15, 0, 0),
    );
    assert_eq!(
        starts(&events),
        [utc(2026, 3, 7, 14, 0), utc(2026, 3, 8, 13, 0)]
    );
}

#[test]
fn weekly_byday_and_biweekly() {
    let (from, to) = week();
    let events = parse(fixture!("weekly_byday.ics"), from, to);
    assert_eq!(
        ids(&events),
        [
            "weekly-byday@example.com/2026-11-16T08:00:00Z",
            "biweekly@example.com/2026-11-17T12:00:00Z",
            "weekly-byday@example.com/2026-11-18T08:00:00Z",
            "weekly-byday@example.com/2026-11-20T08:00:00Z",
        ]
    );
}

#[test]
fn monthly_last_friday_second_monday_and_monthdays() {
    let events = parse(
        fixture!("monthly_last_friday.ics"),
        utc(2026, 11, 1, 0, 0),
        utc(2026, 12, 1, 0, 0),
    );
    assert_eq!(
        ids(&events),
        [
            "second-monday@example.com/2026-11-09T07:00:00Z",
            "monthday@example.com/2026-11-15T07:00:00Z",
            "last-friday@example.com/2026-11-27T15:00:00Z",
            "monthday@example.com/2026-11-30T07:00:00Z",
        ]
    );
}

#[test]
fn yearly_all_day_and_fourth_thursday() {
    let events = parse(
        fixture!("yearly.ics"),
        utc(2026, 11, 16, 0, 0),
        utc(2026, 12, 1, 0, 0),
    );
    assert_eq!(
        ids(&events),
        [
            "birthday@example.com/2026-11-17T00:00:00Z",
            "thanksgiving@example.com/2026-11-26T17:00:00Z",
        ]
    );
    assert_eq!(
        events[0].time,
        EventTime::AllDay {
            start: date(2026, 11, 17),
            end: date(2026, 11, 18)
        }
    );
}

#[test]
fn count_and_until() {
    let events = parse(
        fixture!("count_until.ics"),
        utc(2026, 11, 16, 0, 0),
        utc(2026, 11, 30, 0, 0),
    );
    let of = |uid: &str| {
        events
            .iter()
            .filter(|e| e.id.as_str().starts_with(uid))
            .count()
    };
    assert_eq!(of("count-3@"), 3);
    assert_eq!(of("until@"), 3, "UNTIL is inclusive");
    assert_eq!(of("until-date@"), 2);
}

#[test]
fn exdate_and_rdate() {
    let (from, to) = week();
    let events = parse(fixture!("exdate.ics"), from, to);

    let daily: Vec<&str> = ids(&events)
        .into_iter()
        .filter(|id| id.starts_with("exdate-daily@"))
        .collect();
    assert_eq!(
        daily,
        [
            "exdate-daily@example.com/2026-11-16T08:00:00Z",
            "exdate-daily@example.com/2026-11-21T08:00:00Z",
        ]
    );

    let all_day: Vec<&str> = ids(&events)
        .into_iter()
        .filter(|id| id.starts_with("exdate-allday@"))
        .collect();
    assert_eq!(
        all_day,
        [
            "exdate-allday@example.com/2026-11-16T00:00:00Z",
            "exdate-allday@example.com/2026-11-18T00:00:00Z",
        ]
    );

    let rdate: Vec<&Event> = events
        .iter()
        .filter(|e| e.id.as_str().starts_with("rdate@"))
        .collect();
    let rdate_starts: Vec<_> = rdate.iter().map(|e| timed(e).0).collect();
    assert_eq!(
        rdate_starts,
        [
            utc(2026, 11, 16, 12, 0),
            utc(2026, 11, 18, 12, 0),
            utc(2026, 11, 19, 9, 0),
            utc(2026, 11, 20, 15, 0),
        ]
    );
    assert!(rdate
        .iter()
        .all(|e| timed(e).1 - timed(e).0 == chrono::Duration::minutes(30)));
}

#[test]
fn moved_occurrence_keeps_original_id() {
    let (from, to) = week();
    let events = parse(fixture!("moved_occurrence.ics"), from, to);
    let moved: Vec<&Event> = events
        .iter()
        .filter(|e| e.id.as_str().starts_with("moved@"))
        .collect();
    assert_eq!(
        moved.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        [
            "moved@example.com/2026-11-16T10:00:00Z",
            "moved@example.com/2026-11-17T10:00:00Z",
            "moved@example.com/2026-11-18T10:00:00Z",
            "moved@example.com/2026-11-20T10:00:00Z",
        ],
        "19 Nov moved out of the window"
    );

    let e = by_id(&events, "moved@example.com/2026-11-17T10:00:00Z");
    assert_eq!(e.title, "One-on-one (moved)");
    assert_eq!(
        timed(e),
        (utc(2026, 11, 17, 15, 0), utc(2026, 11, 17, 15, 30))
    );
    assert_eq!(
        e.location.as_deref(),
        Some("Room A"),
        "inherited from the master"
    );

    // An occurrence moved INTO the window from an earlier slot keeps that
    // slot's id.
    let e = by_id(&events, "moved-in@example.com/2026-11-09T10:00:00Z");
    assert_eq!(e.title, "Weekly review (postponed)");
    assert_eq!(timed(e).0, utc(2026, 11, 18, 10, 0));
    by_id(&events, "moved-in@example.com/2026-11-16T10:00:00Z");

    // The moved-out occurrence appears in a window covering its new time,
    // still under its original id.
    let later = parse(
        fixture!("moved_occurrence.ics"),
        utc(2026, 11, 30, 0, 0),
        utc(2026, 12, 1, 0, 0),
    );
    assert_eq!(ids(&later), ["moved@example.com/2026-11-19T10:00:00Z"]);
}

#[test]
fn this_and_future_override_shifts_later_occurrences() {
    let (from, to) = week();
    let events = parse(fixture!("this_and_future.ics"), from, to);
    let got: Vec<(&str, &str, DateTime<Utc>, DateTime<Utc>)> = events
        .iter()
        .map(|e| (e.id.as_str(), e.title.as_str(), timed(e).0, timed(e).1))
        .collect();
    assert_eq!(
        got,
        [
            (
                "taf@example.com/2026-11-16T10:00:00Z",
                "Morning sync",
                utc(2026, 11, 16, 10, 0),
                utc(2026, 11, 16, 11, 0)
            ),
            (
                "taf@example.com/2026-11-17T10:00:00Z",
                "Morning sync",
                utc(2026, 11, 17, 10, 0),
                utc(2026, 11, 17, 11, 0)
            ),
            (
                "taf@example.com/2026-11-18T10:00:00Z",
                "Late-morning sync",
                utc(2026, 11, 18, 11, 0),
                utc(2026, 11, 18, 11, 30)
            ),
            (
                "taf@example.com/2026-11-19T10:00:00Z",
                "Late-morning sync",
                utc(2026, 11, 19, 11, 0),
                utc(2026, 11, 19, 11, 30)
            ),
            (
                "taf@example.com/2026-11-20T10:00:00Z",
                "Late-morning sync",
                utc(2026, 11, 20, 11, 0),
                utc(2026, 11, 20, 11, 30)
            ),
        ]
    );
}

#[test]
fn cancelled_events_and_occurrences_are_omitted() {
    let (from, to) = week();
    let events = parse(fixture!("cancelled.ics"), from, to);
    assert_eq!(
        ids(&events),
        [
            "series@example.com/2026-11-16T10:00:00Z",
            "series@example.com/2026-11-18T10:00:00Z",
        ]
    );
}

#[test]
fn participation_status() {
    let (from, to) = week();
    let events = parse(fixture!("partstat.ics"), from, to);
    let status = |id: &str| by_id(&events, id).response;
    assert_eq!(status("declined@example.com"), ResponseStatus::Declined);
    assert!(by_id(&events, "declined@example.com").is_declined());
    assert_eq!(status("organizer@example.com"), ResponseStatus::Organizer);
    assert_eq!(status("tentative@example.com"), ResponseStatus::Tentative);
    assert_eq!(
        status("needs-action@example.com"),
        ResponseStatus::NotResponded
    );
    assert_eq!(status("not-invited@example.com"), ResponseStatus::Unknown);
    assert_eq!(status("no-attendees@example.com"), ResponseStatus::Unknown);
    assert_eq!(
        status("declined-once@example.com/2026-11-17T09:00:00Z"),
        ResponseStatus::Accepted
    );
    assert_eq!(
        status("declined-once@example.com/2026-11-18T09:00:00Z"),
        ResponseStatus::Declined
    );

    // Without user addresses nothing can be matched.
    let events = parse_with(fixture!("partstat.ics"), from, to, &[], &utc_clock()).unwrap();
    assert!(events.iter().all(|e| e.response == ResponseStatus::Unknown));

    // Addresses match with or without mailto:, case-insensitively.
    let events = parse_with(
        fixture!("partstat.ics"),
        from,
        to,
        &["MAILTO:Test.User@example.COM".to_owned()],
        &utc_clock(),
    )
    .unwrap();
    assert_eq!(
        by_id(&events, "declined@example.com").response,
        ResponseStatus::Declined
    );
}

#[test]
fn folded_long_lines() {
    let (from, to) = week();
    let events = parse(fixture!("folded.ics"), from, to);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(
        e.id.as_str(),
        "folded-uid-that-is-quite-long-0123456789-abcdefghijklmnopqrstuvwxyz@example.com"
    );
    assert_eq!(
        e.title,
        "Quarterly planning møde with a deliberately long title that has to be folded \
         across several physical lines, because RFC 5545 limits lines to seventy-five octets"
    );
    assert_eq!(
        e.location.as_deref(),
        Some(
            "Building 7, Meeting room \"Fjord\" - enter through the north door and take the stairs"
        )
    );
    assert_eq!(timed(e).0, utc(2026, 11, 16, 9, 0));
}

#[test]
fn icloud_sample() {
    let (from, to) = week();
    let events = parse(fixture!("icloud.ics"), from, to);
    assert_eq!(
        ids(&events),
        ["3F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0/2026-11-17T09:00:00Z"]
    );
    let e = &events[0];
    assert_eq!(e.title, "Weekly status");
    assert_eq!(e.location.as_deref(), Some("Video call"));
    assert_eq!(
        timed(e),
        (utc(2026, 11, 17, 10, 0), utc(2026, 11, 17, 11, 0))
    );
    assert_eq!(e.response, ResponseStatus::Organizer);

    let next = parse(
        fixture!("icloud.ics"),
        utc(2026, 11, 23, 0, 0),
        utc(2026, 11, 30, 0, 0),
    );
    assert_eq!(next.len(), 1);
    assert_eq!(timed(&next[0]).0, utc(2026, 11, 24, 9, 0));
    assert_eq!(
        next[0].location.as_deref(),
        Some("Example Office\nExample Street 1, 1000 Example City")
    );
    assert_eq!(next[0].response, ResponseStatus::Organizer);
}

#[test]
fn nextcloud_sample() {
    let events = parse(
        fixture!("nextcloud.ics"),
        utc(2026, 11, 16, 0, 0),
        utc(2026, 12, 3, 0, 0),
    );
    assert_eq!(
        ids(&events),
        [
            "9c1a7c2e-0000-4000-8000-000000000042/2026-11-18T13:00:00Z",
            "9c1a7c2e-0000-4000-8000-000000000043",
            "9c1a7c2e-0000-4000-8000-000000000042/2026-12-02T13:00:00Z",
        ]
    );
    assert_eq!(events[0].response, ResponseStatus::Tentative);
    assert_eq!(events[0].title, "Nextcloud sync");
    assert!(events[1].is_all_day());
}

#[test]
fn outlook_windows_tzids() {
    let events = parse(
        fixture!("outlook_windows_tzid.ics"),
        utc(2026, 11, 25, 0, 0),
        utc(2026, 12, 5, 0, 0),
    );
    let close_uid = "040000008200E00074C5B7101A82E00800000000A0B1C2D3E4F5A6010000000000000000100000001234567890ABCDEF1234567890ABCDEF";
    assert_eq!(
        ids(&events),
        [
            "040000008200E00074C5B7101A82E00800000000B1C2D3E4F5A6B70100000000000000001".to_owned(),
            format!("{close_uid}/2026-11-30T15:00:00Z"),
        ]
    );
    // 09:00 "Eastern Standard Time" in winter is 14:00 UTC.
    assert_eq!(timed(&events[0]).0, utc(2026, 11, 30, 14, 0));
    assert_eq!(events[0].response, ResponseStatus::Organizer);
    assert_eq!(events[1].title, "Month-end close");
    assert_eq!(
        events[1].location.as_deref(),
        Some("Microsoft Teams Meeting")
    );

    // In summer, 16:00 "W. Europe Standard Time" is 14:00 UTC.
    let june = parse(
        fixture!("outlook_windows_tzid.ics"),
        utc(2026, 6, 29, 0, 0),
        utc(2026, 7, 1, 0, 0),
    );
    assert_eq!(ids(&june), [format!("{close_uid}/2026-06-30T14:00:00Z")]);
}

#[test]
fn multiple_vcalendars_in_one_input() {
    let (from, to) = week();
    let joined = format!("{}{}", fixture!("simple.ics"), fixture!("all_day.ics"));
    assert_eq!(parse(&joined, from, to).len(), 5);
}

#[test]
fn empty_or_inverted_window_returns_nothing() {
    let (from, _) = week();
    assert!(parse(fixture!("simple.ics"), from, from).is_empty());
    assert!(parse(
        fixture!("simple.ics"),
        from + chrono::Duration::days(1),
        from
    )
    .is_empty());
}

#[test]
fn runaway_rules_are_capped() {
    let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:r@example.com\nDTSTART:20261116T000000Z\n\
               DTEND:20261116T000100Z\nRRULE:FREQ=DAILY\nEND:VEVENT\nEND:VCALENDAR\n";
    let events = parse(ics, utc(2026, 11, 16, 0, 0), utc(2036, 11, 16, 0, 0));
    assert_eq!(events.len(), MAX_OCCURRENCES_PER_EVENT);

    // An old daily series is expanded cheaply and correctly for a window
    // decades later.
    let ics = ics.replace("20261116T", "19700101T");
    let (from, to) = week();
    assert_eq!(parse(&ics, from, to).len(), 7);
}

#[test]
fn unsupported_rule_parts_fall_back_to_dtstart() {
    let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:h@example.com\nDTSTART:20261116T090000Z\n\
               RRULE:FREQ=HOURLY;COUNT=3\nEND:VEVENT\nEND:VCALENDAR\n";
    let (from, to) = week();
    assert_eq!(
        ids(&parse(ics, from, to)),
        ["h@example.com/2026-11-16T09:00:00Z"]
    );
}

#[test]
fn malformed_input_is_a_protocol_error_without_contents() {
    let (from, to) = week();
    let clock = utc_clock();
    let check = |ics: &str| {
        let err = parse_with(ics, from, to, &[USER.to_owned()], &clock)
            .expect_err("malformed input must fail");
        assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
        let msg = err.to_string();
        for secret in ["Confidential", "board", "example.com", "Secret"] {
            assert!(!msg.contains(secret), "error leaks contents: {msg}");
        }
    };

    check(fixture!("malformed.ics"));
    let event = |body: &str| {
        format!(
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\n{body}\nSUMMARY:Secret\nEND:VEVENT\nEND:VCALENDAR\n"
        )
    };
    for body in [
        "DTSTART:20261116T090000Z",                    // no UID
        "UID:x@example.com",                           // no DTSTART
        "UID:x@example.com\nDTSTART:2026-11-16 09:00", // bad date
        "UID:x@example.com\nDTSTART:20261116T090000Z\nDTEND:Secret",
        "UID:x@example.com\nDTSTART:20261116T090000Z\nDURATION:Secret",
        "UID:x@example.com\nDTSTART:20261116T090000Z\nRRULE:FREQ=Secret",
        "UID:x@example.com\nDTSTART:20261116T090000Z\nRRULE:COUNT=2",
        "UID:x@example.com\nDTSTART:20261116T090000Z\nRRULE:FREQ=DAILY\nEXDATE:Secret",
        "UID:x@example.com\nDTSTART:20261116T090000Z\nRDATE:Secret",
        "UID:x@example.com\nDTSTART:20261116T090000Z\nRECURRENCE-ID:Secret",
        "Secret line without a colon",
    ] {
        check(&event(body));
    }
    check("");
    check("Secret: not iCalendar at all");
    check("BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:Secret\nBEGIN:STANDARD\nDTSTART:19700101T000000\nEND:STANDARD\nEND:VTIMEZONE\nEND:VCALENDAR\n");
}

#[test]
fn truncated_input_never_panics() {
    let (from, to) = week();
    let clock = utc_clock();
    for ics in [
        fixture!("icloud.ics"),
        fixture!("outlook_windows_tzid.ics"),
        fixture!("this_and_future.ics"),
    ] {
        for (i, _) in ics.char_indices().step_by(7) {
            // Ok or Err are both fine; panicking is not.
            let _ = parse_with(&ics[..i], from, to, &[], &clock);
        }
    }
}

#[test]
fn extreme_values_do_not_panic() {
    let clock = utc_clock();
    let far = [
        "RRULE:FREQ=YEARLY;INTERVAL=2147483647",
        "RRULE:FREQ=DAILY;INTERVAL=4294967295",
        "RRULE:FREQ=MONTHLY;BYMONTHDAY=31;BYMONTH=2",
        "RRULE:FREQ=WEEKLY;COUNT=4294967295",
        "RRULE:FREQ=MONTHLY;BYDAY=53MO,-53FR;BYSETPOS=366,-366",
        "DURATION:P36600D",
    ];
    for extra in far {
        for start in ["99991231T235959Z", "00000101T000000Z", "20261116T090000Z"] {
            let ics = format!(
                "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x@example.com\nDTSTART:{start}\n{extra}\nEND:VEVENT\nEND:VCALENDAR\n"
            );
            for (from, to) in [
                (utc(2026, 11, 16, 0, 0), utc(2026, 11, 23, 0, 0)),
                (DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC),
            ] {
                let _ = parse_with(&ics, from, to, &[], &clock);
            }
        }
    }
}
