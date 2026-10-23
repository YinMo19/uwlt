use alloy::primitives::U256;
use anyhow::{Context, Result, ensure};

pub(super) fn integer(input: &str) -> Result<U256> {
    ensure!(
        !input.is_empty() && input.bytes().all(|b| b.is_ascii_digit()),
        "invalid integer token amount"
    );
    U256::from_str_radix(input, 10).context("token amount exceeds uint256")
}

pub(super) fn parse(input: &str) -> Result<U256> {
    let (whole, fraction) = input.split_once('.').unwrap_or((input, ""));
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && fraction.bytes().all(|b| b.is_ascii_digit())
            && fraction.len() <= 6
            && (!input.contains('.') || !fraction.is_empty()),
        "USDT amount must be a positive decimal with at most six decimal places"
    );
    let units = integer(whole)?
        .checked_mul(U256::from(1_000_000))
        .and_then(|n| {
            let fractional = if fraction.is_empty() {
                U256::ZERO
            } else {
                integer(fraction).ok()? * U256::from(10u64.pow(6 - fraction.len() as u32))
            };
            n.checked_add(fractional)
        })
        .context("amount exceeds uint256")?;
    ensure!(!units.is_zero(), "bridge amount must be positive");
    Ok(units)
}

pub(super) fn format(units: U256) -> String {
    format_units(units, 6)
}

/// Render base units without floating point or a potentially overflowing scale.
pub(super) fn format_units(units: U256, decimals: u8) -> String {
    let digits = units.to_string();
    if decimals == 0 {
        return digits;
    }
    let decimals = usize::from(decimals);
    let mut result = if digits.len() <= decimals {
        format!("0.{}{digits}", "0".repeat(decimals - digits.len()))
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

pub(super) fn slippage(bps: u16) -> String {
    format!("{}.{:02}", bps / 100, bps % 100)
}

/// floor(expected * (1 - slippage)), decomposed to avoid uint256 overflow.
pub(super) fn minimum(expected: U256, bps: u16) -> Result<U256> {
    ensure!(bps < 10_000, "slippage must be below 100 percent");
    let scale = U256::from(10_000);
    let factor = U256::from(10_000 - bps);
    let whole = (expected / scale)
        .checked_mul(factor)
        .context("minimum output overflow")?;
    let fraction = (expected % scale)
        .checked_mul(factor)
        .context("minimum output overflow")?
        / scale;
    whole
        .checked_add(fraction)
        .context("minimum output overflow")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_are_exact_and_positive() {
        assert_eq!(parse("1.000001").unwrap(), U256::from(1_000_001));
        for input in ["0", "-1", "1e3", "1.0000001", "1.", ".1", " 1", "1..2"] {
            assert!(parse(input).is_err(), "{input}");
        }
        assert!(parse(&U256::MAX.to_string()).is_err());
    }

    #[test]
    fn fee_decimals_and_slippage_remain_integer_only() {
        assert_eq!(
            format_units(U256::from(37951838466816u64), 18),
            "0.000037951838466816"
        );
        assert_eq!(format_units(U256::ZERO, 18), "0");
        assert_eq!(format_units(U256::MAX, 0), U256::MAX.to_string());
        assert_eq!(slippage(1), "0.01");
        assert_eq!(slippage(50), "0.50");
        assert_eq!(slippage(125), "1.25");
        assert_eq!(slippage(0), "0.00");
        assert_eq!(
            minimum(U256::from(98_177_317), 50).unwrap(),
            U256::from(97_686_430)
        );
        assert_eq!(minimum(U256::MAX, 0).unwrap(), U256::MAX);
        assert!(minimum(U256::MAX, 10_000).is_err());
    }
}
