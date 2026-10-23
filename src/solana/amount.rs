use anyhow::{Context, Result, ensure};

pub fn parse(input: &str, decimals: usize) -> Result<u64> {
    ensure!(!input.is_empty() && decimals <= 9, "invalid amount");
    let mut parts = input.split('.');
    let whole = parts.next().unwrap();
    let fraction = parts.next().unwrap_or("");
    ensure!(
        parts.next().is_none()
            && !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && fraction.bytes().all(|b| b.is_ascii_digit())
            && fraction.len() <= decimals,
        "amount must be a nonnegative decimal with at most {decimals} fractional digits"
    );
    let scale = 10u64.pow(decimals as u32);
    let whole: u64 = whole.parse().context("amount exceeds u64")?;
    let fraction_digits = fraction.len();
    let fraction: u64 = if fraction.is_empty() {
        0
    } else {
        fraction.parse()?
    };
    let units = whole
        .checked_mul(scale)
        .and_then(|v| v.checked_add(fraction * 10u64.pow((decimals - fraction_digits) as u32)))
        .context("amount exceeds u64")?;
    ensure!(units > 0, "amount must be greater than zero");
    Ok(units)
}

pub fn format(value: u64, decimals: usize) -> String {
    if decimals == 0 {
        return value.to_string();
    }
    let scale = 10u64.pow(decimals as u32);
    let mut result = format!(
        "{}.{:0width$}",
        value / scale,
        value % scale,
        width = decimals
    );
    while result.ends_with('0') {
        result.pop();
    }
    if result.ends_with('.') {
        result.pop();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_units_and_boundaries() {
        assert_eq!(parse("0.000000001", 9).unwrap(), 1);
        assert_eq!(parse("18446744073.709551615", 9).unwrap(), u64::MAX);
        assert_eq!(format(u64::MAX, 9), "18446744073.709551615");
        for input in [
            "18446744073.709551616",
            "0",
            "-1",
            "1e3",
            " 1",
            ".1",
            "1.0000001",
            "1..2",
        ] {
            assert!(parse(input, 6).is_err(), "{input}");
        }
    }
}
