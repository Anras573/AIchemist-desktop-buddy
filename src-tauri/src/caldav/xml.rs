//! WebDAV `multistatus` parsing (RFC 4918 13, RFC 4791) and request bodies.
//!
//! Elements are matched by namespace URI and local name, so any prefix
//! (`d:`, `D:`, `cal:`, `C:`, a default `xmlns`, or one declared per
//! element as iCloud does) works. Only properties in a `propstat` whose
//! status is 2xx (or absent) are kept.

use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{Namespace, ResolveResult};
use quick_xml::NsReader;

use crate::domain::{Error, Result};

const NS_DAV: &str = "DAV:";
const NS_CALDAV: &str = "urn:ietf:params:xml:ns:caldav";

/// The properties of one resource that this module understands.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Props {
    /// `DAV:current-user-principal` href.
    pub current_user_principal: Option<String>,
    /// First `CALDAV:calendar-home-set` href.
    pub calendar_home_set: Option<String>,
    /// `CALDAV:calendar-user-address-set` hrefs (`mailto:...`, URLs, URNs).
    pub calendar_user_addresses: Vec<String>,
    /// `DAV:displayname`.
    pub display_name: Option<String>,
    /// `DAV:resourcetype` contains `CALDAV:calendar`.
    pub is_calendar: bool,
    /// `CALDAV:supported-calendar-component-set` names (upper case), if
    /// the property was returned.
    pub components: Option<Vec<String>>,
    /// `calendar-color` (Apple namespace, or any other).
    pub color: Option<String>,
    /// `CALDAV:calendar-data`.
    pub calendar_data: Option<String>,
}

impl Props {
    fn merge(&mut self, other: Props) {
        let Props {
            current_user_principal,
            calendar_home_set,
            calendar_user_addresses,
            display_name,
            is_calendar,
            components,
            color,
            calendar_data,
        } = other;
        self.current_user_principal = self
            .current_user_principal
            .take()
            .or(current_user_principal);
        self.calendar_home_set = self.calendar_home_set.take().or(calendar_home_set);
        self.calendar_user_addresses.extend(calendar_user_addresses);
        self.display_name = self.display_name.take().or(display_name);
        self.is_calendar |= is_calendar;
        self.components = self.components.take().or(components);
        self.color = self.color.take().or(color);
        self.calendar_data = self.calendar_data.take().or(calendar_data);
    }
}

/// One `DAV:response`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MsResponse {
    /// The resource's `DAV:href`, as sent (possibly relative, percent-encoded).
    pub href: String,
    /// Properties from successful `propstat`s.
    pub props: Props,
}

/// The elements we care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum El {
    Multistatus,
    Response,
    Href,
    Propstat,
    Prop,
    Status,
    CurrentUserPrincipal,
    CalendarHomeSet,
    CalendarUserAddressSet,
    DisplayName,
    ResourceType,
    Calendar,
    SupportedComponentSet,
    Comp,
    CalendarColor,
    CalendarData,
    Other,
}

fn classify(ns: &ResolveResult<'_>, local: &str) -> El {
    let ns = match ns {
        ResolveResult::Bound(Namespace(ns)) => *ns,
        _ => "",
    };
    match (ns, local) {
        (NS_DAV, "multistatus") => El::Multistatus,
        (NS_DAV, "response") => El::Response,
        (NS_DAV, "href") => El::Href,
        (NS_DAV, "propstat") => El::Propstat,
        (NS_DAV, "prop") => El::Prop,
        (NS_DAV, "status") => El::Status,
        (NS_DAV, "current-user-principal") => El::CurrentUserPrincipal,
        (NS_DAV, "displayname") => El::DisplayName,
        (NS_DAV, "resourcetype") => El::ResourceType,
        (NS_CALDAV, "calendar-home-set") => El::CalendarHomeSet,
        (NS_CALDAV, "calendar-user-address-set") => El::CalendarUserAddressSet,
        (NS_CALDAV, "calendar") => El::Calendar,
        (NS_CALDAV, "supported-calendar-component-set") => El::SupportedComponentSet,
        (NS_CALDAV, "comp") => El::Comp,
        (NS_CALDAV, "calendar-data") => El::CalendarData,
        // Apple's `http://apple.com/ns/ical/`; accept any namespace.
        (_, "calendar-color") => El::CalendarColor,
        _ => El::Other,
    }
}

fn malformed() -> Error {
    Error::protocol("the server sent malformed XML")
}

/// Parses the `HTTP/1.1 200 OK` of a `DAV:status`.
fn parse_status(line: &str) -> Option<u16> {
    line.split_whitespace().nth(1)?.parse().ok()
}

fn comp_name(e: &BytesStart<'_>) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        (a.key.local_name().as_ref() == "name").then(|| a.value.trim().to_ascii_uppercase())
    })
}

#[derive(Default)]
struct State {
    responses: Vec<MsResponse>,
    response: Option<(String, Props)>,
    propstat: Props,
    propstat_status: Option<u16>,
}

impl State {
    /// Handles the start of `el` whose parent is `parent`.
    fn start(&mut self, el: El, parent: Option<El>, e: &BytesStart<'_>) {
        match (el, parent) {
            (El::Response, _) => self.response = Some((String::new(), Props::default())),
            (El::Propstat, _) => {
                self.propstat = Props::default();
                self.propstat_status = None;
            }
            (El::Calendar, Some(El::ResourceType)) => self.propstat.is_calendar = true,
            (El::SupportedComponentSet, Some(El::Prop)) => {
                self.propstat.components.get_or_insert_with(Vec::new);
            }
            (El::Comp, Some(El::SupportedComponentSet)) => {
                if let (Some(name), Some(list)) = (comp_name(e), self.propstat.components.as_mut())
                {
                    list.push(name);
                }
            }
            _ => {}
        }
    }

    /// Handles the end of `el` whose parent is `parent`, with its text.
    fn end(&mut self, el: El, parent: Option<El>, text: &str) {
        let p = &mut self.propstat;
        match (el, parent) {
            (El::Href, Some(El::Response)) => {
                if let Some((href, _)) = self.response.as_mut() {
                    *href = text.trim().to_owned();
                }
            }
            (El::Href, Some(El::CurrentUserPrincipal)) => {
                p.current_user_principal = Some(text.trim().to_owned());
            }
            (El::Href, Some(El::CalendarHomeSet)) => {
                if p.calendar_home_set.is_none() {
                    p.calendar_home_set = Some(text.trim().to_owned());
                }
            }
            (El::Href, Some(El::CalendarUserAddressSet)) => {
                p.calendar_user_addresses.push(text.trim().to_owned());
            }
            (El::Status, Some(El::Propstat)) => self.propstat_status = parse_status(text),
            (El::DisplayName, Some(El::Prop)) => p.display_name = Some(text.to_owned()),
            (El::CalendarColor, Some(El::Prop)) => p.color = Some(text.trim().to_owned()),
            (El::CalendarData, Some(El::Prop)) => p.calendar_data = Some(text.to_owned()),
            (El::Propstat, _) => {
                let props = std::mem::take(&mut self.propstat);
                let ok = self.propstat_status.is_none_or(|s| (200..300).contains(&s));
                if let (true, Some((_, merged))) = (ok, self.response.as_mut()) {
                    merged.merge(props);
                }
            }
            (El::Response, _) => {
                if let Some((href, props)) = self.response.take() {
                    if !href.is_empty() {
                        self.responses.push(MsResponse { href, props });
                    }
                }
            }
            _ => {}
        }
    }
}

/// Parses a `DAV:multistatus` document.
///
/// # Errors
///
/// [`Error::Protocol`] if the XML is malformed or the root element is not
/// `DAV:multistatus`. The message never contains document contents.
pub(crate) fn parse_multistatus(xml: &str) -> Result<Vec<MsResponse>> {
    let mut reader = NsReader::from_str(xml);
    let mut stack: Vec<El> = Vec::new();
    let mut state = State::default();
    let mut text = String::new();
    let mut seen_root = false;

    loop {
        let (ns, event) = reader.read_resolved_event().map_err(|_| malformed())?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let el = classify(&ns, e.local_name().as_ref());
                if !seen_root {
                    if el != El::Multistatus {
                        return Err(Error::protocol(
                            "the server did not send a WebDAV multistatus response",
                        ));
                    }
                    seen_root = true;
                }
                text.clear();
                let parent = stack.last().copied();
                state.start(el, parent, e);
                if matches!(event, Event::Start(_)) {
                    stack.push(el);
                } else {
                    state.end(el, parent, "");
                }
            }
            Event::End(_) => {
                let el = stack.pop().ok_or_else(malformed)?;
                state.end(el, stack.last().copied(), &text);
                text.clear();
            }
            Event::Text(t) => text.push_str(&t.xml10_content()),
            Event::CData(t) => text.push_str(&t.into_inner()),
            Event::GeneralRef(r) => {
                let ch = match r.resolve_char_ref().map_err(|_| malformed())? {
                    Some(ch) => ch,
                    None => match &*r {
                        "lt" => '<',
                        "gt" => '>',
                        "amp" => '&',
                        "apos" => '\'',
                        "quot" => '"',
                        _ => return Err(malformed()),
                    },
                };
                text.push(ch);
            }
            Event::Eof => break,
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
        }
    }
    if !seen_root || !stack.is_empty() {
        return Err(malformed());
    }
    Ok(state.responses)
}

/// `PROPFIND` body asking for `DAV:current-user-principal`.
pub(crate) const PROPFIND_PRINCIPAL: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop><d:current-user-principal/></d:prop>
</d:propfind>"#;

/// `PROPFIND` body asking a principal for its calendar home and addresses.
pub(crate) const PROPFIND_HOME: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><c:calendar-home-set/><c:calendar-user-address-set/></d:prop>
</d:propfind>"#;

/// `PROPFIND Depth: 1` body listing the calendars in a home.
pub(crate) const PROPFIND_CALENDARS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:a="http://apple.com/ns/ical/">
  <d:prop>
    <d:resourcetype/>
    <d:displayname/>
    <c:supported-calendar-component-set/>
    <a:calendar-color/>
  </d:prop>
</d:propfind>"#;

/// `REPORT calendar-query` body for VEVENTs overlapping `[start, end)`,
/// both formatted as iCalendar UTC (`20261116T000000Z`).
pub(crate) fn calendar_query(start: &str, end: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:getetag/><c:calendar-data/></d:prop>
  <c:filter>
    <c:comp-filter name="VCALENDAR">
      <c:comp-filter name="VEVENT">
        <c:time-range start="{start}" end="{end}"/>
      </c:comp-filter>
    </c:comp-filter>
  </c:filter>
</c:calendar-query>"#
    )
}
