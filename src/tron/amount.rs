use anyhow::{Context, Result, ensure};

const SCALE: u64 = 1_000_000;

/// Parse TRX/USDT decimal units without rounding, signs, or exponent notation.
pub fn parse_amount(input: &str) -> Result<u64> {
    let (whole, fraction) = input.split_once('.').unwrap_or((input, ""));
    ensure!(
        !whole.is_empty() && whole.bytes().all(|b| b.is_ascii_digit()),
        "amount must start with unsigned decimal digits"
    );
    ensure!(
        fraction.len() <= 6 && fraction.bytes().all(|b| b.is_ascii_digit()),
        "amount permits at most six decimal places"
    );
    ensure!(
        !input.contains('.') || !fraction.is_empty(),
        "amount must have digits after the decimal point"
    );
    let whole: u64 = whole.parse().context("amount is too large")?;
    let fraction: u64 = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<u64>()
            .context("invalid fractional amount")?
            * 10_u64.pow(6 - fraction.len() as u32)
    };
    whole
        .checked_mul(SCALE)
        .and_then(|value| value.checked_add(fraction))
        .context("amount exceeds the supported range")
}

/// Format exact decimal units, omitting insignificant fractional zeroes.
pub fn format_amount(amount: u64) -> String {
    let whole = amount / SCALE;
    let fraction = amount % SCALE;
    if fraction == 0 {
        whole.to_string()
    } else {
        let mut formatted = format!("{whole}.{fraction:06}");
        while formatted.ends_with('0') {
            formatted.pop();
        }
        formatted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_decimal_boundaries() {
        for (input, expected) in [
            ("0", 0),
            ("0.000001", 1),
            ("1.2", 1_200_000),
            ("18446744073709.551615", u64::MAX),
        ] {
            assert_eq!(parse_amount(input).unwrap(), expected);
            assert_eq!(parse_amount(&format_amount(expected)).unwrap(), expected);
        }
        assert_eq!(format_amount(1_230_000), "1.23");
    }

    #[test]
    fn rejects_rounding_and_ambiguous_notation() {
        for input in [
            "",
            "-1",
            "+1",
            ".1",
            "1.",
            "1e6",
            "0.0000001",
            "18446744073709.551616",
        ] {
            assert!(parse_amount(input).is_err(), "accepted {input:?}");
        }
    }
}
