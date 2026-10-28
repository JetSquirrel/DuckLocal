//! A plot's `format`: what its numbers are, named the way Evidence names
//! them — `usd` money, `pct1` a share, `num2` a plain number to two places —
//! so a spec says what a number means rather than how to draw it.
//!
//! One format reads three ways. In full where a number stands alone, a
//! card's value or a tooltip's row: `$1,234.50`, `42.1%`. Short where room
//! is tight, a bar's label: past ten thousand in K or M, as unformatted
//! labels are, but in the format's units: `$1.9M`. And as an axis, whose
//! ticks share one unit: `$0 $5K $10K`.

use crate::ui::chart::{format_ticks, format_value};

/// The names a `format` attribute takes.
pub(crate) const FORMATS: &[&str] = &[
    "num0", "num1", "num2", "pct", "pct0", "pct1", "pct2", "usd", "usd0", "eur", "eur0", "gbp",
    "gbp0", "cny", "cny0", "jpy",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NumberFormat {
    /// Grouped by thousands, to a fixed number of places.
    Number { decimals: u8 },
    /// A share: 0.421 reads `42.1%`.
    Percent { decimals: u8 },
    /// Money: the symbol before the number, a minus before the symbol.
    Currency { symbol: &'static str, decimals: u8 },
}

impl NumberFormat {
    /// The format a `format` attribute names; `None` for a name not in
    /// [`FORMATS`].
    pub(crate) fn parse(name: &str) -> Option<Self> {
        use NumberFormat::*;
        let currency = |symbol, decimals| Some(Currency { symbol, decimals });
        match name {
            "num0" => Some(Number { decimals: 0 }),
            "num1" => Some(Number { decimals: 1 }),
            "num2" => Some(Number { decimals: 2 }),
            "pct0" => Some(Percent { decimals: 0 }),
            "pct" | "pct1" => Some(Percent { decimals: 1 }),
            "pct2" => Some(Percent { decimals: 2 }),
            "usd" => currency("$", 2),
            "usd0" => currency("$", 0),
            "eur" => currency("€", 2),
            "eur0" => currency("€", 0),
            "gbp" => currency("£", 2),
            "gbp0" => currency("£", 0),
            "cny" => currency("¥", 2),
            "cny0" | "jpy" => currency("¥", 0),
            _ => None,
        }
    }

    /// A number in full, where it stands alone: `$1,234.50`, `42.1%`.
    pub(crate) fn full(self, value: f64) -> String {
        match self {
            NumberFormat::Number { decimals } => grouped(value, decimals),
            NumberFormat::Percent { decimals } => format!("{}%", grouped(value * 100., decimals)),
            NumberFormat::Currency { symbol, decimals } => money(symbol, &grouped(value, decimals)),
        }
    }

    /// A number where room is short: in full below ten thousand, in K or M
    /// above, as unformatted labels are. A share is never that large.
    pub(crate) fn short(self, value: f64) -> String {
        if value.abs() < 10_000. {
            return self.full(value);
        }
        match self {
            NumberFormat::Percent { .. } => self.full(value),
            NumberFormat::Number { .. } => format_value(value),
            NumberFormat::Currency { symbol, .. } => money(symbol, &format_value(value)),
        }
    }

    /// An axis's tick labels, in one unit as [`format_ticks`] picks it, in
    /// this format.
    pub(crate) fn ticks(self, ticks: &[f64]) -> Vec<String> {
        match self {
            NumberFormat::Number { .. } => format_ticks(ticks),
            NumberFormat::Percent { .. } => {
                // Rounded back to the tick: 0.07 × 100 is 7.000000000000001.
                let percents: Vec<f64> = ticks
                    .iter()
                    .map(|t| (t * 100. * 1e6).round() / 1e6)
                    .collect();
                format_ticks(&percents)
                    .into_iter()
                    .map(|label| format!("{label}%"))
                    .collect()
            }
            NumberFormat::Currency { symbol, .. } => format_ticks(ticks)
                .iter()
                .map(|label| money(symbol, label))
                .collect(),
        }
    }
}

/// A number in full in `format`, or as unformatted labels read it.
pub(crate) fn full_text(format: Option<NumberFormat>, value: f64) -> String {
    format.map_or_else(|| format_value(value), |format| format.full(value))
}

/// A number where room is short, in `format` or as unformatted labels read it.
pub(crate) fn short_text(format: Option<NumberFormat>, value: f64) -> String {
    format.map_or_else(|| format_value(value), |format| format.short(value))
}

/// `-$1,234`, not `$-1,234`.
fn money(symbol: &str, number: &str) -> String {
    match number.strip_prefix('-') {
        Some(magnitude) => format!("-{symbol}{magnitude}"),
        None => format!("{symbol}{number}"),
    }
}

/// `value` to `decimals` places, its whole part grouped by thousands.
fn grouped(value: f64, decimals: u8) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    // Half away from zero, as money is rounded: `{:.0}` alone rounds half
    // to even, and $1,234.50 would read $1,234.
    let scale = 10f64.powi(decimals as i32);
    let text = format!(
        "{:.*}",
        decimals as usize,
        (value.abs() * scale).round() / scale
    );
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (text.as_str(), None),
    };
    let mut out = String::with_capacity(text.len() + whole.len() / 3 + 1);
    for (i, digit) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    if let Some(fraction) = fraction {
        out.push('.');
        out.push_str(fraction);
    }
    // A minus on a number that rounds to nothing says what `0.00` does not.
    if value < 0. && out.bytes().any(|b| matches!(b, b'1'..=b'9')) {
        out.insert(0, '-');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{NumberFormat, FORMATS};

    fn format(name: &str) -> NumberFormat {
        NumberFormat::parse(name).unwrap()
    }

    #[test]
    fn every_listed_name_parses() {
        for name in FORMATS {
            assert!(NumberFormat::parse(name).is_some(), "{name}");
        }
        assert_eq!(NumberFormat::parse("dollars"), None);
    }

    #[test]
    fn a_number_in_full() {
        assert_eq!(format("usd").full(1234.5), "$1,234.50");
        assert_eq!(format("usd").full(-1234.5), "-$1,234.50");
        assert_eq!(format("usd0").full(999.5), "$1,000");
        assert_eq!(format("eur").full(0.), "€0.00");
        assert_eq!(format("pct").full(0.4213), "42.1%");
        assert_eq!(format("pct0").full(1.), "100%");
        assert_eq!(format("num2").full(1234567.891), "1,234,567.89");
        assert_eq!(format("num0").full(-0.4), "0");
        assert_eq!(format("jpy").full(12345.6), "¥12,346");
    }

    #[test]
    fn a_short_number_abbreviates_past_ten_thousand() {
        assert_eq!(format("usd").short(1_900_000.), "$1.9M");
        assert_eq!(format("usd").short(-12_345.), "-$12K");
        assert_eq!(format("usd").short(9_999.), "$9,999.00");
        assert_eq!(format("num2").short(12.3456), "12.35");
        assert_eq!(format("pct1").short(0.5), "50.0%");
    }

    #[test]
    fn axis_ticks_share_one_unit_in_the_format() {
        assert_eq!(
            format("usd").ticks(&[0., 5000., 10000.]),
            ["$0", "$5K", "$10K"]
        );
        assert_eq!(
            format("pct").ticks(&[0., 0.05, 0.07, 0.1]),
            ["0%", "5%", "7%", "10%"]
        );
        assert_eq!(format("usd").ticks(&[-20000., 0.]), ["-$20K", "$0"]);
    }
}
