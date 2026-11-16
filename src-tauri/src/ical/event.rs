//! `VEVENT` → [`Event`]: time resolution, recurrence expansion, exceptions
//! and overrides.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};

use super::parser::{Component, Property};
use super::rrule::{self, Expansion, Parsed, Until};
use super::tz::{self, VTimeZone, Zone};
use super::value::{self, IcalDuration, RawDate};
use super::{protocol, ParseOptions, MAX_OCCURRENCES_PER_EVENT};
use crate::domain::{Event, EventId, EventTime, ResponseStatus, Result};

/// Everything the expansion needs besides the component itself.
pub(super) struct Context<'a> {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub opts: &'a ParseOptions<'a>,
    /// Normalized user addresses (see [`normalize_address`]).
    pub users: &'a [String],
    pub zones: &'a [VTimeZone],
}

/// A DATE or a resolved DATE-TIME.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When<'z> {
    Date(NaiveDate),
    Time {
        local: NaiveDateTime,
        zone: Zone<'z>,
    },
}

impl When<'_> {
    fn naive(&self) -> NaiveDateTime {
        match *self {
            Self::Date(d) => d.and_time(NaiveTime::MIN),
            Self::Time { local, .. } => local,
        }
    }
}

/// The identity of an occurrence slot: the original start as an instant
/// (timed series) or a date (all-day series).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Slot {
    Instant(DateTime<Utc>),
    Day(NaiveDate),
}

impl Slot {
    fn occurrence_id(&self, uid: &str) -> EventId {
        let suffix = match self {
            Self::Instant(t) => t.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            Self::Day(d) => d.format("%Y-%m-%dT00:00:00Z").to_string(),
        };
        EventId::new(format!("{uid}/{suffix}"))
    }
}

/// How long an occurrence lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Length {
    /// All-day: number of days (at least 1).
    Days(i64),
    /// Timed: nominal days (wall clock) plus exact seconds.
    Timed(IcalDuration),
}

/// Title, location and response of an occurrence.
#[derive(Debug, Clone)]
struct Content {
    title: Option<String>,
    location: Option<String>,
    response: Option<ResponseStatus>,
}

impl Content {
    fn or(&self, fallback: Option<&Content>) -> Content {
        Content {
            title: self
                .title
                .clone()
                .or_else(|| fallback.and_then(|c| c.title.clone())),
            location: self
                .location
                .clone()
                .or_else(|| fallback.and_then(|c| c.location.clone())),
            response: self.response.or_else(|| fallback.and_then(|c| c.response)),
        }
    }
}

/// A `VEVENT` with its times resolved.
struct VEvent<'c, 'z> {
    comp: &'c Component,
    uid: &'c str,
    start: When<'z>,
    /// From DTEND or DURATION; `None` if neither is present.
    length: Option<Length>,
    cancelled: bool,
    /// RECURRENCE-ID and whether it has `RANGE=THISANDFUTURE`.
    rid: Option<(When<'z>, bool)>,
    content: Content,
}

impl VEvent<'_, '_> {
    fn is_recurring(&self) -> bool {
        self.comp.prop("RRULE").is_some() || self.comp.prop("RDATE").is_some()
    }

    fn length_or_default(&self) -> Length {
        self.length.unwrap_or(match self.start {
            When::Date(_) => Length::Days(1),
            When::Time { .. } => Length::Timed(IcalDuration::default()),
        })
    }
}

/// Lower-cases an address and strips a `mailto:` prefix.
pub(super) fn normalize_address(a: &str) -> String {
    let a = a.trim();
    let a = match a.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("mailto:") => &a[7..],
        _ => a,
    };
    a.trim().to_ascii_lowercase()
}

fn is_value_date(p: &Property) -> bool {
    p.param("VALUE")
        .is_some_and(|v| v.eq_ignore_ascii_case("DATE"))
}

/// Resolves a DATE / DATE-TIME property value.
fn to_when<'z>(raw: RawDate, tzid: Option<&str>, ctx: &Context<'z>) -> When<'z> {
    match raw {
        RawDate::Date(d) => When::Date(d),
        RawDate::DateTime { local, utc: true } => When::Time {
            local,
            zone: Zone::Utc,
        },
        RawDate::DateTime { local, utc: false } => When::Time {
            local,
            zone: tz::resolve(tzid, ctx.zones),
        },
    }
}

fn parse_when<'z>(p: &Property, ctx: &Context<'z>) -> Result<When<'z>> {
    let raw = value::parse_raw_date(&p.value, is_value_date(p))
        .ok_or_else(|| protocol(p.line, "invalid date"))?;
    Ok(to_when(raw, p.param("TZID"), ctx))
}

fn utc_of(w: When<'_>, ctx: &Context<'_>) -> DateTime<Utc> {
    match w {
        When::Date(d) => ctx.opts.clock.local_to_utc(d.and_time(NaiveTime::MIN)),
        When::Time { local, zone } => zone.to_utc(local, ctx.opts.clock),
    }
}

fn text(c: &Component, name: &str) -> Option<String> {
    c.prop(name)
        .map(|p| value::unescape_text(&p.value).trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn response(c: &Component, users: &[String]) -> Option<ResponseStatus> {
    let organizer = c.prop("ORGANIZER");
    let mut attendees = c.props_named("ATTENDEE").peekable();
    if organizer.is_none() && attendees.peek().is_none() {
        return None;
    }
    let is_user = |p: &Property| {
        let value = normalize_address(&p.value);
        let email = p.param("EMAIL").map(normalize_address);
        users
            .iter()
            .any(|u| *u == value || email.as_deref() == Some(u.as_str()))
    };
    if organizer.is_some_and(is_user) {
        return Some(ResponseStatus::Organizer);
    }
    let Some(a) = attendees.find(|a| is_user(a)) else {
        return Some(ResponseStatus::Unknown);
    };
    let status = a.param("PARTSTAT").unwrap_or("NEEDS-ACTION");
    Some(match status.to_ascii_uppercase().as_str() {
        "ACCEPTED" => ResponseStatus::Accepted,
        "TENTATIVE" => ResponseStatus::Tentative,
        "DECLINED" => ResponseStatus::Declined,
        "NEEDS-ACTION" => ResponseStatus::NotResponded,
        _ => ResponseStatus::Unknown,
    })
}

fn parse_vevent<'c, 'z>(c: &'c Component, ctx: &Context<'z>) -> Result<VEvent<'c, 'z>> {
    let uid = c
        .prop("UID")
        .map(|p| p.value.trim())
        .filter(|u| !u.is_empty())
        .ok_or_else(|| protocol(c.line, "VEVENT without UID"))?;
    let dtstart = c
        .prop("DTSTART")
        .ok_or_else(|| protocol(c.line, "VEVENT without DTSTART"))?;
    let start = parse_when(dtstart, ctx)?;

    let length = if let Some(p) = c.prop("DTEND") {
        let end = parse_when(p, ctx)?;
        Some(match start {
            When::Date(s) => Length::Days((end.naive().date() - s).num_days().max(1)),
            When::Time { .. } => {
                let secs = (utc_of(end, ctx) - utc_of(start, ctx)).num_seconds().max(0);
                Length::Timed(IcalDuration {
                    days: 0,
                    seconds: secs,
                })
            }
        })
    } else if let Some(p) = c.prop("DURATION") {
        let d =
            value::parse_duration(&p.value).ok_or_else(|| protocol(p.line, "invalid DURATION"))?;
        Some(match start {
            When::Date(_) => Length::Days((d.days + d.seconds / 86_400).max(1)),
            When::Time { .. } => Length::Timed(IcalDuration {
                days: d.days.max(0),
                seconds: d.seconds.max(0),
            }),
        })
    } else {
        None
    };

    let rid = match c.prop("RECURRENCE-ID") {
        Some(p) => {
            let this_and_future = p
                .param("RANGE")
                .is_some_and(|r| r.eq_ignore_ascii_case("THISANDFUTURE"));
            Some((parse_when(p, ctx)?, this_and_future))
        }
        None => None,
    };

    Ok(VEvent {
        comp: c,
        uid,
        start,
        length,
        cancelled: c
            .prop("STATUS")
            .is_some_and(|s| s.value.trim().eq_ignore_ascii_case("CANCELLED")),
        rid,
        content: Content {
            title: text(c, "SUMMARY"),
            location: text(c, "LOCATION"),
            response: response(c, ctx.users),
        },
    })
}

/// Expands every `VEVENT` of one `VCALENDAR` into `out`.
pub(super) fn expand_calendar(
    cal: &Component,
    ctx: &Context<'_>,
    out: &mut Vec<Event>,
) -> Result<()> {
    type Group<'c, 'z> = (Option<VEvent<'c, 'z>>, Vec<VEvent<'c, 'z>>);
    let mut groups: BTreeMap<&str, Group<'_, '_>> = BTreeMap::new();
    for c in cal.children_named("VEVENT") {
        let ev = parse_vevent(c, ctx)?;
        let entry = groups.entry(ev.uid).or_insert_with(|| (None, Vec::new()));
        if ev.rid.is_some() {
            entry.1.push(ev);
        } else if entry.0.is_none() {
            // A duplicate master for the same UID is ignored.
            entry.0 = Some(ev);
        }
    }
    for (uid, (master, overrides)) in &groups {
        expand_group(uid, master.as_ref(), overrides, ctx, out)?;
    }
    Ok(())
}

/// Computes the occurrence's [`EventTime`].
fn occurrence_time(start: When<'_>, length: Length, ctx: &Context<'_>) -> EventTime {
    match start {
        When::Date(d) => {
            let days = match length {
                Length::Days(n) => n,
                Length::Timed(t) => t.days + t.seconds / 86_400,
            }
            .max(1);
            EventTime::AllDay {
                start: d,
                end: d
                    .checked_add_signed(Duration::days(days))
                    .unwrap_or(d.succ_opt().unwrap_or(d)),
            }
        }
        When::Time { local, zone } => {
            let start = zone.to_utc(local, ctx.opts.clock);
            let (days, seconds) = match length {
                Length::Days(n) => (n, 0),
                Length::Timed(t) => (t.days, t.seconds),
            };
            let base = if days == 0 {
                start
            } else {
                local
                    .checked_add_signed(Duration::days(days))
                    .map_or(start, |l| zone.to_utc(l, ctx.opts.clock))
            };
            let end = base
                .checked_add_signed(Duration::seconds(seconds))
                .unwrap_or(base)
                .max(start);
            EventTime::Timed { start, end }
        }
    }
}

fn overlaps(t: &EventTime, ctx: &Context<'_>) -> bool {
    let clock = ctx.opts.clock;
    let (s, e) = (t.start_utc(clock), t.end_utc(clock));
    s < ctx.to && (e > ctx.from || (s == e && s >= ctx.from))
}

fn emit(
    id: EventId,
    start: When<'_>,
    length: Length,
    content: &Content,
    ctx: &Context<'_>,
    out: &mut Vec<Event>,
) {
    let time = occurrence_time(start, length, ctx);
    if !overlaps(&time, ctx) {
        return;
    }
    out.push(Event {
        id,
        account_id: ctx.opts.account_id.clone(),
        calendar_id: ctx.opts.calendar_id.clone(),
        title: content.title.clone().unwrap_or_default(),
        time,
        location: content.location.clone(),
        response: content.response.unwrap_or_default(),
    });
}

fn expand_group(
    uid: &str,
    master: Option<&VEvent<'_, '_>>,
    overrides: &[VEvent<'_, '_>],
    ctx: &Context<'_>,
    out: &mut Vec<Event>,
) -> Result<()> {
    if master.is_some_and(|m| m.cancelled) {
        return Ok(()); // the whole series is cancelled
    }
    let recurring = master.is_none_or(VEvent::is_recurring) || !overrides.is_empty();
    if !recurring {
        if let Some(m) = master {
            emit(
                EventId::new(uid),
                m.start,
                m.length_or_default(),
                &m.content,
                ctx,
                out,
            );
        }
        return Ok(());
    }

    let master_zone = master.map(|m| match m.start {
        When::Date(_) => None,
        When::Time { zone, .. } => Some(zone),
    });
    // Slots are normalized to the master's kind so that RECURRENCE-ID and
    // EXDATE values written in another zone (or as UTC) still match.
    let slot_of = |w: When<'_>| -> Slot {
        match (master_zone, w) {
            (Some(None), When::Date(d)) => Slot::Day(d),
            (Some(None), When::Time { local, .. }) => Slot::Day(local.date()),
            (Some(Some(mz)), When::Date(d)) => {
                Slot::Instant(mz.to_utc(d.and_time(NaiveTime::MIN), ctx.opts.clock))
            }
            (None, When::Date(d)) => Slot::Day(d),
            (_, When::Time { local, zone }) => Slot::Instant(zone.to_utc(local, ctx.opts.clock)),
        }
    };

    let mut overridden = HashSet::new();
    let mut this_and_future: Vec<(Slot, &VEvent<'_, '_>)> = Vec::new();
    for o in overrides {
        if let Some((rid, taf)) = o.rid {
            let slot = slot_of(rid);
            overridden.insert(slot);
            if taf {
                this_and_future.push((slot, o));
            }
        }
    }
    this_and_future.sort_by_key(|(s, _)| *s);

    let exclusions = match master {
        Some(m) => exclusions(m, ctx, &slot_of)?,
        None => Exclusions::default(),
    };
    let excluded = |slot: Slot, start: When<'_>| {
        exclusions.slots.contains(&slot)
            || (!exclusions.days.is_empty()
                && matches!(start, When::Time { .. })
                && exclusions.days.contains(&start.naive().date()))
    };

    if let Some(m) = master {
        let mut seen = HashSet::new();
        for start in occurrence_starts(m, ctx)? {
            let slot = slot_of(start);
            if !seen.insert(slot) || excluded(slot, start) || overridden.contains(&slot) {
                continue;
            }
            let id = slot.occurrence_id(uid);
            match this_and_future.iter().rev().find(|(s, _)| *s < slot) {
                None => emit(id, start, m.length_or_default(), &m.content, ctx, out),
                Some((_, o)) if o.cancelled => {}
                Some((rid_slot, o)) => {
                    let shifted = shift(start, *rid_slot, o, ctx);
                    let length = o.length.unwrap_or(m.length_or_default());
                    emit(
                        id,
                        shifted,
                        length,
                        &o.content.or(Some(&m.content)),
                        ctx,
                        out,
                    );
                }
            }
        }
    }

    for o in overrides {
        let Some((rid, _)) = o.rid else { continue };
        let slot = slot_of(rid);
        if o.cancelled || excluded(slot, rid) {
            continue;
        }
        let length = o
            .length
            .or(master.map(VEvent::length_or_default))
            .unwrap_or(o.length_or_default());
        let content = o.content.or(master.map(|m| &m.content));
        emit(slot.occurrence_id(uid), o.start, length, &content, ctx, out);
    }
    Ok(())
}

/// Applies a THISANDFUTURE override's time shift to a later occurrence.
fn shift<'z>(start: When<'z>, rid_slot: Slot, o: &VEvent<'_, 'z>, ctx: &Context<'_>) -> When<'z> {
    match (start, rid_slot, o.start) {
        (When::Time { .. }, Slot::Instant(rid), When::Time { .. }) => {
            let delta = utc_of(o.start, ctx) - rid;
            let moved = utc_of(start, ctx)
                .checked_add_signed(delta)
                .unwrap_or_else(|| utc_of(start, ctx));
            When::Time {
                local: moved.naive_utc(),
                zone: Zone::Utc,
            }
        }
        (When::Date(d), Slot::Day(rid), When::Date(new)) => {
            When::Date(d.checked_add_signed(new - rid).unwrap_or(d))
        }
        _ => start,
    }
}

#[derive(Default)]
struct Exclusions {
    slots: HashSet<Slot>,
    /// DATE-valued EXDATEs on a timed series: exclude the whole local day.
    days: HashSet<NaiveDate>,
}

fn exclusions<'z>(
    m: &VEvent<'_, 'z>,
    ctx: &Context<'z>,
    slot_of: &dyn Fn(When<'_>) -> Slot,
) -> Result<Exclusions> {
    let mut ex = Exclusions::default();
    for p in m.comp.props_named("EXDATE") {
        for v in value::split_list(&p.value) {
            let raw = value::parse_raw_date(v, is_value_date(p))
                .ok_or_else(|| protocol(p.line, "invalid EXDATE"))?;
            let when = to_when(raw, p.param("TZID"), ctx);
            match (m.start, when) {
                (When::Time { .. }, When::Date(d)) => {
                    ex.days.insert(d);
                }
                _ => {
                    ex.slots.insert(slot_of(when));
                }
            }
        }
    }
    Ok(ex)
}

/// DTSTART, the RRULE occurrences near the window, and the RDATEs, as
/// start times in the master's frame. May contain duplicates.
fn occurrence_starts<'z>(m: &VEvent<'_, 'z>, ctx: &Context<'z>) -> Result<Vec<When<'z>>> {
    let clock = ctx.opts.clock;
    let mut starts = vec![m.start];
    let zone = match m.start {
        When::Date(_) => Zone::Floating,
        When::Time { zone, .. } => zone,
    };
    let make = |naive: NaiveDateTime| match m.start {
        When::Date(_) => When::Date(naive.date()),
        When::Time { zone, .. } => When::Time { local: naive, zone },
    };

    let span = match m.length_or_default() {
        Length::Days(n) => Duration::days(n),
        Length::Timed(t) => Duration::days(t.days) + Duration::seconds(t.seconds),
    };
    let margin = Duration::days(2);
    let from_local = zone.to_local(ctx.from, clock);
    let to_local = zone.to_local(ctx.to, clock);
    let skip_before = from_local
        .checked_sub_signed(span)
        .and_then(|t| t.checked_sub_signed(margin));
    let stop_after = to_local
        .checked_add_signed(margin)
        .unwrap_or(NaiveDateTime::MAX);

    for p in m.comp.props_named("RRULE") {
        let Parsed::Rule(rule) = rrule::parse(&p.value, p.line)? else {
            continue; // unsupported rule parts: DTSTART and RDATEs only
        };
        let until = rule.until.map(|u| match u {
            Until::Date(d) => d.and_hms_opt(23, 59, 59).unwrap_or_default(),
            Until::Local(l) => l,
            Until::Utc(u) => zone.to_local(Utc.from_utc_datetime(&u), clock),
        });
        let x = Expansion {
            dtstart: m.start.naive(),
            until,
            skip_before,
            stop_after,
            max: MAX_OCCURRENCES_PER_EVENT,
        };
        starts.extend(rrule::expand(&rule, &x).into_iter().skip(1).map(make));
    }

    for p in m.comp.props_named("RDATE") {
        let is_period = p
            .param("VALUE")
            .is_some_and(|v| v.eq_ignore_ascii_case("PERIOD"));
        for v in value::split_list(&p.value) {
            let v = if is_period || v.contains('/') {
                v.split('/').next().unwrap_or(v)
            } else {
                v
            };
            let raw = value::parse_raw_date(v, is_value_date(p))
                .ok_or_else(|| protocol(p.line, "invalid RDATE"))?;
            let when = to_when(raw, p.param("TZID"), ctx);
            starts.push(match (m.start, when) {
                (When::Date(_), When::Time { local, .. }) => When::Date(local.date()),
                (When::Time { zone, .. }, When::Date(d)) => When::Time {
                    local: d.and_time(NaiveTime::MIN),
                    zone,
                },
                _ => when,
            });
        }
    }
    if starts.len() > MAX_OCCURRENCES_PER_EVENT {
        starts.truncate(MAX_OCCURRENCES_PER_EVENT);
    }
    Ok(starts)
}
