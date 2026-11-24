//! iCal feed of the public drops (F1a), written by hand to RFC 5545.

use i18n::{Amsterdam, FluentArgs, I18nError, Translator};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::calendar::{PublicDrop, PublicPickup};
use crate::status::PublicStatus;

const MAX_LINE_OCTETS: usize = 75;

/// A VCALENDAR with one VEVENT per pickup slot. UIDs come from the slot id and `domain`,
/// so they stay the same across refreshes. `generated_at` becomes every DTSTAMP.
///
/// # Errors
/// [`I18nError`] when a calendar message is missing or cannot be formatted.
pub fn render(
    drops: &[PublicDrop],
    t: &Translator,
    domain: &str,
    generated_at: Timestamp,
) -> Result<String, I18nError> {
    let mut out = String::new();
    line(&mut out, "BEGIN:VCALENDAR");
    line(&mut out, "VERSION:2.0");
    line(&mut out, "PRODID:-//Brood voor Weldadigheid//Bakdagen//NL");
    line(&mut out, "CALSCALE:GREGORIAN");
    line(&mut out, "METHOD:PUBLISH");
    text(&mut out, "X-WR-CALNAME", &t.t("calendar-name", None)?);
    line(&mut out, "REFRESH-INTERVAL;VALUE=DURATION:PT1H");
    line(&mut out, "X-PUBLISHED-TTL:PT1H");
    for drop in drops {
        let description = description(drop, t)?;
        for pickup in &drop.pickups {
            event(
                &mut out,
                t,
                drop,
                pickup,
                &description,
                domain,
                generated_at,
            )?;
        }
    }
    line(&mut out, "END:VCALENDAR");
    Ok(out)
}

fn event(
    out: &mut String,
    t: &Translator,
    drop: &PublicDrop,
    pickup: &PublicPickup,
    description: &str,
    domain: &str,
    generated_at: Timestamp,
) -> Result<(), I18nError> {
    let cancelled = drop.status == PublicStatus::Cancelled;
    let mut args = FluentArgs::new();
    args.set("location", pickup.location_name.as_str());
    let summary_id = if cancelled {
        "calendar-pickup-cancelled"
    } else {
        "calendar-pickup"
    };
    line(out, "BEGIN:VEVENT");
    line(
        out,
        &format!("UID:pickup-slot-{}@{domain}", pickup.slot_id.get()),
    );
    line(out, &format!("DTSTAMP:{}", utc(generated_at)));
    line(out, &format!("DTSTART:{}", utc(pickup.starts_at)));
    line(out, &format!("DTEND:{}", utc(pickup.ends_at)));
    text(out, "SUMMARY", &t.t(summary_id, Some(&args))?);
    text(
        out,
        "LOCATION",
        &format!("{}, {}", pickup.location_name, pickup.address),
    );
    text(out, "DESCRIPTION", description);
    line(
        out,
        if cancelled {
            "STATUS:CANCELLED"
        } else {
            "STATUS:CONFIRMED"
        },
    );
    line(
        out,
        if cancelled {
            "SEQUENCE:1"
        } else {
            "SEQUENCE:0"
        },
    );
    line(out, "TRANSP:TRANSPARENT");
    line(out, "END:VEVENT");
    Ok(())
}

fn description(drop: &PublicDrop, t: &Translator) -> Result<String, I18nError> {
    let mut lines = Vec::new();
    if drop.status == PublicStatus::Cancelled {
        lines.push(t.t("calendar-cancelled", None)?);
    }
    match &drop.charity {
        Some(charity) => {
            let mut args = FluentArgs::new();
            args.set("charity", charity.name.as_str());
            lines.push(t.t("calendar-charity", Some(&args))?);
            if !charity.description.trim().is_empty() {
                lines.push(charity.description.trim().to_owned());
            }
            lines.push(charity.url.clone());
        }
        None => lines.push(t.t("calendar-normal-sale", None)?),
    }
    if let Some(cutoff) = drop.cutoff_at
        && drop.status != PublicStatus::Cancelled
    {
        let zone = Amsterdam::new();
        let mut at = FluentArgs::new();
        at.set("date", t.long_date(zone.date(cutoff)));
        at.set("time", zone.time(cutoff));
        let mut args = FluentArgs::new();
        args.set("deadline", t.t("calendar-deadline-at", Some(&at))?);
        lines.push(t.t("order-deadline", Some(&args))?);
    }
    if let Some(menu) = drop.menu.as_ref().filter(|menu| !menu.is_empty()) {
        lines.push(t.t("calendar-menu", None)?);
        lines.extend(menu.iter().map(|item| item.name.clone()));
    }
    Ok(lines.join("\n"))
}

fn utc(at: Timestamp) -> String {
    at.to_zoned(TimeZone::UTC)
        .strftime("%Y%m%dT%H%M%SZ")
        .to_string()
}

fn line(out: &mut String, content: &str) {
    out.push_str(&fold(content));
    out.push_str("\r\n");
}

fn text(out: &mut String, name: &str, value: &str) {
    line(out, &format!("{name}:{}", escape(value)));
}

/// Escapes a TEXT value (RFC 5545 3.3.11): backslash, semicolon, comma and line breaks.
/// Other control characters except tab are not allowed and are dropped.
#[must_use]
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                chars.next_if_eq(&'\n');
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// Folds a content line so no physical line is longer than 75 octets (RFC 5545 3.1).
/// Continuation lines start with one space, which counts toward their 75. A UTF-8
/// character is never split.
#[must_use]
pub fn fold(content: &str) -> String {
    let mut out = String::with_capacity(content.len() + content.len() / 64 * 3);
    let mut used = 0;
    for c in content.chars() {
        if used + c.len_utf8() > MAX_LINE_OCTETS {
            out.push_str("\r\n ");
            used = 1;
        }
        out.push(c);
        used += c.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_text_specials() {
        assert_eq!(escape(r"a\b;c,d"), r"a\\b\;c\,d");
        assert_eq!(escape("een\ntwee\r\ndrie\rvier"), r"een\ntwee\ndrie\nvier");
        assert_eq!(escape("gewoon: tekst"), "gewoon: tekst");
        assert_eq!(escape("a\u{7}b\tc\u{1b}"), "ab\tc");
    }

    #[test]
    fn short_lines_are_not_folded() {
        let line = "x".repeat(75);
        assert_eq!(fold(&line), line);
    }

    #[test]
    fn long_lines_fold_at_75_octets() {
        let folded = fold(&"x".repeat(200));
        let lines: Vec<&str> = folded.split("\r\n").collect();
        assert_eq!(
            lines.iter().map(|l| l.len()).collect::<Vec<_>>(),
            [75, 75, 52]
        );
        assert!(lines[1..].iter().all(|l| l.starts_with(' ')));
        let unfolded: String = folded.replace("\r\n ", "");
        assert_eq!(unfolded, "x".repeat(200));
    }

    #[test]
    fn folding_never_splits_a_character() {
        let content = format!("{}€€", "x".repeat(73));
        let folded = fold(&content);
        let lines: Vec<&str> = folded.split("\r\n").collect();
        assert_eq!(lines[0], "x".repeat(73));
        assert_eq!(lines[1], " €€");
        for content in ["é".repeat(100), "€".repeat(100), "🥖".repeat(100)] {
            let folded = fold(&content);
            assert!(folded.split("\r\n").all(|l| l.len() <= MAX_LINE_OCTETS));
            assert_eq!(folded.replace("\r\n ", ""), content);
        }
    }

    #[test]
    fn utc_times_end_in_z() {
        assert_eq!(
            utc("2026-10-31T10:00:00Z".parse().unwrap()),
            "20261031T100000Z"
        );
    }
}
