//! A dashboard's time zone: every `TIMESTAMPTZ` a query returns is shown in
//! it, so the dashboard reads on the clock of whoever it is for.
//!
//! ```text
//! dashboard "Operations" {
//!   timezone = "+08:00"   // or "UTC", "local"
//! }
//! ```
//!
//! The grid writes a `TIMESTAMPTZ` as its UTC instant, with no offset. That
//! type is the only one moved: a plain `TIMESTAMP` says nothing about its
//! zone, and guessing one is how two clocks get mixed up.
//!
//! The move happens here rather than in SQL because the bundled DuckDB has no
//! ICU, so `AT TIME ZONE` is not there to do it (see `duckdb_contract.rs`).

use chrono::{FixedOffset, Local, NaiveDateTime, Offset as _, TimeZone as _, Utc};

/// The zones `timezone` may name, for the message when it names another.
pub(crate) const ZONES_HELP: &str = "\"UTC\", \"local\" or an offset such as \"+08:00\"";

/// A zone a dashboard shows its times in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Zone {
    Utc,
    /// The zone of the computer showing the dashboard.
    Local,
    Fixed(FixedOffset),
}

impl Zone {
    /// `UTC`, `local`, or an offset (`+08:00`, `-0530`, `+08`).
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        match text.to_ascii_lowercase().as_str() {
            "utc" | "z" | "gmt" => Some(Self::Utc),
            "local" => Some(Self::Local),
            _ => parse_offset(text).map(Self::Fixed),
        }
    }

    /// A UTC instant as the grid writes it, moved to this zone and written
    /// the same way, fraction kept. `None` for a cell that is not one (NULL,
    /// `infinity`, a year chrono cannot hold), which then stays as it is.
    pub(crate) fn shift(&self, utc: &str) -> Option<String> {
        let (whole, fraction) = match utc.split_once('.') {
            Some((whole, fraction)) => (whole, Some(fraction)),
            None => (utc, None),
        };
        let naive = NaiveDateTime::parse_from_str(whole, "%Y-%m-%d %H:%M:%S").ok()?;
        const FORMAT: &str = "%Y-%m-%d %H:%M:%S";
        let moved = match self {
            Self::Utc => naive.format(FORMAT).to_string(),
            Self::Local => Local.from_utc_datetime(&naive).format(FORMAT).to_string(),
            Self::Fixed(offset) => offset.from_utc_datetime(&naive).format(FORMAT).to_string(),
        };
        Some(match fraction {
            Some(fraction) => format!("{moved}.{fraction}"),
            None => moved,
        })
    }

    /// How the zone is named to the person reading: `UTC`, `UTC+08:00`, or
    /// `local, UTC+08:00`.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Utc => "UTC".into(),
            Self::Local => format!(
                "local, UTC{}",
                Local
                    .offset_from_utc_datetime(&Utc::now().naive_utc())
                    .fix()
            ),
            Self::Fixed(offset) => format!("UTC{offset}"),
        }
    }
}

/// `+08:00`, `+0800`, `-05`.
fn parse_offset(text: &str) -> Option<FixedOffset> {
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) || !matches!(digits.len(), 2 | 4) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = if digits.len() == 4 {
        digits[2..].parse().ok()?
    } else {
        0
    };
    if hours > 14 || minutes > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(seconds: i32) -> Zone {
        Zone::Fixed(FixedOffset::east_opt(seconds).unwrap())
    }

    #[test]
    fn zones_by_word_or_offset() {
        assert_eq!(Zone::parse("UTC"), Some(Zone::Utc));
        assert_eq!(Zone::parse(" local "), Some(Zone::Local));
        assert_eq!(Zone::parse("+08:00"), Some(fixed(8 * 3600)));
        assert_eq!(Zone::parse("+08"), Some(fixed(8 * 3600)));
        assert_eq!(Zone::parse("-0530"), Some(fixed(-(5 * 3600 + 1800))));
        assert_eq!(
            Zone::parse("HKT"),
            None,
            "an abbreviation names no one zone"
        );
        assert_eq!(Zone::parse("+25:00"), None);
        assert_eq!(Zone::parse("+8:00"), None);
    }

    #[test]
    fn a_utc_instant_moves_and_keeps_its_fraction() {
        let east = fixed(8 * 3600);
        assert_eq!(
            east.shift("2026-01-31 20:15:00").as_deref(),
            Some("2026-02-01 04:15:00"),
            "across a month"
        );
        assert_eq!(
            east.shift("1970-01-01 02:15:00.25").as_deref(),
            Some("1970-01-01 10:15:00.25")
        );
        assert_eq!(
            fixed(-5 * 3600).shift("2026-01-01 02:00:00").as_deref(),
            Some("2025-12-31 21:00:00")
        );
        assert_eq!(
            Zone::Utc.shift("2026-01-01 02:00:00").as_deref(),
            Some("2026-01-01 02:00:00")
        );
    }

    #[test]
    fn what_is_not_an_instant_is_left_alone() {
        let east = fixed(8 * 3600);
        for cell in ["NULL", "infinity", "2026-01-01", ""] {
            assert_eq!(east.shift(cell), None, "{cell:?}");
        }
    }

    #[test]
    fn a_zone_is_named_with_its_offset() {
        assert_eq!(Zone::Utc.label(), "UTC");
        assert_eq!(fixed(8 * 3600).label(), "UTC+08:00");
        assert!(Zone::Local.label().starts_with("local, UTC"));
    }
}
