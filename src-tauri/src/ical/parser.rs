//! Content-line level parsing (RFC 5545 section 3.1): unfolding, property
//! parameters and the component tree.
//!
//! Error messages carry line numbers and property/component names only, never
//! property values (spec section 9).

use crate::domain::{Error, Result};

/// Components nested deeper than this are rejected. Real data uses at most
/// three levels (VCALENDAR > VEVENT > VALARM, VCALENDAR > VTIMEZONE > STANDARD).
const MAX_DEPTH: usize = 8;

/// One property (content line) after unfolding.
#[derive(Debug, Clone)]
pub(crate) struct Property {
    /// Upper-cased property name.
    pub name: String,
    /// Parameters as (upper-cased name, value). Quoted values are unquoted;
    /// multi-valued parameters keep their comma-separated form.
    pub params: Vec<(String, String)>,
    /// Raw (still escaped) value.
    pub value: String,
    /// 1-based physical line number where the content line starts.
    pub line: usize,
}

impl Property {
    /// Value of the parameter `name` (upper-case), if present.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A component (`BEGIN:X` ... `END:X`) with its properties and children.
#[derive(Debug, Clone, Default)]
pub(crate) struct Component {
    /// Upper-cased component name.
    pub name: String,
    /// Properties in document order.
    pub props: Vec<Property>,
    /// Nested components in document order.
    pub children: Vec<Component>,
    /// Line of the `BEGIN`.
    pub line: usize,
}

impl Component {
    /// First property named `name`.
    pub fn prop(&self, name: &str) -> Option<&Property> {
        self.props.iter().find(|p| p.name == name)
    }

    /// All properties named `name`.
    pub fn props_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> + 'a {
        self.props.iter().filter(move |p| p.name == name)
    }

    /// Children named `name`.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Component> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
}

fn err(line: usize, what: &str) -> Error {
    Error::protocol(format!("malformed iCalendar at line {line}: {what}"))
}

/// Unfolds physical lines into logical content lines, returning each with
/// the line number it started on. Accepts CRLF and bare LF line endings.
fn unfold(input: &str) -> Vec<(usize, String)> {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut out: Vec<(usize, String)> = Vec::new();
    for (idx, raw) in input.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            if let Some((_, last)) = out.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        out.push((idx + 1, line.to_owned()));
    }
    out
}

/// Splits one content line into name, parameters and value.
fn parse_line(line_no: usize, line: &str) -> Result<Property> {
    // The name ends at the first ';' or ':'.
    let name_end = line
        .find([';', ':'])
        .ok_or_else(|| err(line_no, "content line without ':'"))?;
    let name = &line[..name_end];
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(err(line_no, "invalid property name"));
    }

    let mut params = Vec::new();
    // Byte offset of `rest` within `line`.
    let mut pos = name_end;
    let mut rest = &line[pos..];
    while let Some(after_semi) = rest.strip_prefix(';') {
        let eq = after_semi
            .find('=')
            .ok_or_else(|| err(line_no, "parameter without '='"))?;
        let pname = &after_semi[..eq];
        if pname.is_empty() || !pname.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(err(line_no, "invalid parameter name"));
        }
        // Parameter values: comma-separated, each optionally DQUOTE-quoted.
        let mut value = String::new();
        let base = pos + 1 + eq + 1;
        let mut end = None;
        let mut in_quotes = false;
        for (i, c) in line[base..].char_indices() {
            match c {
                '"' => in_quotes = !in_quotes,
                ';' | ':' if !in_quotes => {
                    end = Some(base + i);
                    break;
                }
                _ => value.push(c),
            }
        }
        if in_quotes {
            return Err(err(line_no, "unterminated quoted parameter"));
        }
        let end = end.ok_or_else(|| err(line_no, "content line without ':'"))?;
        params.push((pname.to_ascii_uppercase(), value));
        pos = end;
        rest = &line[pos..];
    }
    let value = rest
        .strip_prefix(':')
        .ok_or_else(|| err(line_no, "content line without ':'"))?;

    Ok(Property {
        name: name.to_ascii_uppercase(),
        params,
        value: value.to_owned(),
        line: line_no,
    })
}

/// Parses iCalendar text into its top-level components (normally one or more
/// `VCALENDAR`s).
pub(crate) fn parse_components(input: &str) -> Result<Vec<Component>> {
    let mut roots = Vec::new();
    let mut stack: Vec<Component> = Vec::new();

    for (line_no, line) in unfold(input) {
        if line.trim().is_empty() {
            continue;
        }
        let prop = parse_line(line_no, &line)?;
        match prop.name.as_str() {
            "BEGIN" => {
                if stack.len() >= MAX_DEPTH {
                    return Err(err(line_no, "components nested too deeply"));
                }
                let name = prop.value.trim().to_ascii_uppercase();
                if name.is_empty() {
                    return Err(err(line_no, "BEGIN without component name"));
                }
                stack.push(Component {
                    name,
                    line: line_no,
                    ..Component::default()
                });
            }
            "END" => {
                let name = prop.value.trim().to_ascii_uppercase();
                let done = stack
                    .pop()
                    .ok_or_else(|| err(line_no, "END without BEGIN"))?;
                if done.name != name {
                    return Err(err(line_no, "END does not match BEGIN"));
                }
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => roots.push(done),
                }
            }
            _ => match stack.last_mut() {
                Some(current) => current.props.push(prop),
                None => return Err(err(line_no, "property outside of a component")),
            },
        }
    }

    if let Some(open) = stack.last() {
        return Err(err(open.line, "component is never closed"));
    }
    if roots.is_empty() {
        return Err(Error::protocol("malformed iCalendar: no VCALENDAR found"));
    }
    if let Some(bad) = roots.iter().find(|c| c.name != "VCALENDAR") {
        return Err(err(bad.line, "top-level component is not VCALENDAR"));
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfolds_crlf_and_lf_with_space_or_tab() {
        let lines = unfold("A:12\r\n 34\r\nB:x\n\ty\nC:z");
        assert_eq!(
            lines,
            vec![
                (1, "A:1234".to_owned()),
                (3, "B:xy".to_owned()),
                (5, "C:z".to_owned())
            ]
        );
    }

    #[test]
    fn parses_params_with_quotes() {
        let p = parse_line(
            1,
            r#"ATTENDEE;CN="Doe; Jane: Esq";PARTSTAT=ACCEPTED;MEMBER="a","b":mailto:j@example.com"#,
        )
        .unwrap();
        assert_eq!(p.name, "ATTENDEE");
        assert_eq!(p.param("CN"), Some("Doe; Jane: Esq"));
        assert_eq!(p.param("PARTSTAT"), Some("ACCEPTED"));
        assert_eq!(p.param("MEMBER"), Some("a,b"));
        assert_eq!(p.value, "mailto:j@example.com");
    }

    #[test]
    fn names_are_case_insensitive() {
        let p = parse_line(1, "dtStart;tzid=Europe/Copenhagen:20261116T090000").unwrap();
        assert_eq!(p.name, "DTSTART");
        assert_eq!(p.param("TZID"), Some("Europe/Copenhagen"));
    }

    #[test]
    fn rejects_structural_errors() {
        for bad in [
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nEND:VCALENDAR\r\n",
            "BEGIN:VCALENDAR\r\n",
            "END:VCALENDAR\r\n",
            "BEGIN:VEVENT\r\nEND:VEVENT\r\n",
            "SUMMARY:x\r\n",
            "BEGIN:VCALENDAR\r\nno colon here\r\nEND:VCALENDAR\r\n",
            "BEGIN:VCALENDAR\r\nX;P=\"open:END:VCALENDAR\r\n",
            "",
        ] {
            let e = parse_components(bad).unwrap_err();
            assert!(matches!(e, Error::Protocol { .. }), "{bad:?}");
        }
    }

    #[test]
    fn error_messages_do_not_leak_values() {
        let e = parse_components("BEGIN:VCALENDAR\r\nSecret meeting with Bob\r\nEND:VCALENDAR")
            .unwrap_err();
        assert!(!e.to_string().contains("Bob"));
    }

    #[test]
    fn rejects_deep_nesting() {
        let input = "BEGIN:X\n".repeat(20);
        assert!(parse_components(&input).is_err());
    }
}
