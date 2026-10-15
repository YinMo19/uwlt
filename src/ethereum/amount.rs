use alloy::primitives::U256;
use anyhow::{Context, Result, ensure};

pub(super) fn parse(input: &str, decimals: usize) -> Result<U256> {
    let (whole, fraction) = input.split_once('.').unwrap_or((input, ""));
    ensure!(
        !whole.is_empty() && whole.bytes().all(|byte| byte.is_ascii_digit()),
        "amount must start with unsigned decimal digits"
    );
    ensure!(
        fraction.len() <= decimals && fraction.bytes().all(|byte| byte.is_ascii_digit()),
        "amount permits at most {decimals} decimal places"
    );
    ensure!(
        !input.contains('.') || !fraction.is_empty(),
        "amount must have digits after the decimal point"
    );
    let mut value = U256::ZERO;
    for digit in whole.bytes().chain(fraction.bytes()) {
        value = value
            .checked_mul(U256::from(10))
            .and_then(|value| value.checked_add(U256::from(digit - b'0')))
            .context("amount exceeds uint256 range")?;
    }
    for _ in fraction.len()..decimals {
        value = value
            .checked_mul(U256::from(10))
            .context("amount exceeds uint256 range")?;
    }
    ensure!(value != U256::ZERO, "amount must be greater than zero");
    Ok(value)
}

pub fn format(value: U256, decimals: usize) -> String {
    let digits = value.to_string();
    let mut result = if digits.len() <= decimals {
        format!("0.{:0>width$}", digits, width = decimals)
    } else {
        let split = digits.len() - decimals;
        format!("{}.{}", &digits[..split], &digits[split..])
    };
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
    fn exact_units_and_uint256_boundary() {
        assert_eq!(parse("0.000001", 6).unwrap(), U256::from(1));
        assert_eq!(parse("0.000000000000000001", 18).unwrap(), U256::from(1));
        assert_eq!(format(U256::ZERO, 18), "0");
        assert_eq!(format(parse("12.340000", 6).unwrap(), 6), "12.34");
        let maximum = format(U256::MAX, 18);
        assert_eq!(parse(&maximum, 18).unwrap(), U256::MAX);
        for invalid in ["0", "-1", "+1", "1e3", ".1", "1.", "1.0000001", "1.2.3"] {
            assert!(parse(invalid, 6).is_err(), "accepted {invalid}");
        }
        assert!(parse(&U256::MAX.to_string(), 18).is_err());
        assert!(
            parse(
                "115792089237316195423570985008687907853269984665640564039457.584007913129639936",
                18
            )
            .is_err()
        );
    }
}
