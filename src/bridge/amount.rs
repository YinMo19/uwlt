use alloy::primitives::U256;
use anyhow::{Context, Result, ensure};

/// Decimal arithmetic stays integer-only, including the API's fractional prices.
pub(super) fn decimal(input: &str) -> Result<(U256, U256)> {
    let (whole, fraction) = input.split_once('.').unwrap_or((input, ""));
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && fraction.bytes().all(|b| b.is_ascii_digit())
            && (!input.contains('.') || !fraction.is_empty()),
        "invalid unsigned decimal {input:?}"
    );
    let mut numerator = U256::ZERO;
    for digit in whole.bytes().chain(fraction.bytes()) {
        numerator = numerator
            .checked_mul(U256::from(10))
            .and_then(|n| n.checked_add(U256::from(digit - b'0')))
            .context("decimal exceeds uint256")?;
    }
    let mut denominator = U256::from(1);
    for _ in fraction.bytes() {
        denominator = denominator
            .checked_mul(U256::from(10))
            .context("decimal precision exceeds uint256")?;
    }
    Ok((numerator, denominator))
}

pub(super) fn parse(input: &str) -> Result<U256> {
    ensure!(
        input
            .split_once('.')
            .is_none_or(|(_, fraction)| fraction.len() <= 6),
        "USDT amounts support at most six decimal places"
    );
    let (numerator, denominator) = decimal(input)?;
    let units = numerator
        .checked_mul(U256::from(1_000_000) / denominator)
        .context("amount exceeds uint256")?;
    ensure!(units > U256::ZERO, "bridge amount must be positive");
    Ok(units)
}

pub(super) fn format(units: U256) -> String {
    let scale = U256::from(1_000_000);
    let whole = units / scale;
    let fraction = (units % scale).to::<u64>();
    if fraction == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{}", format!("{fraction:06}").trim_end_matches('0'))
    }
}

pub(super) fn ceil_div(numerator: U256, denominator: U256) -> Result<U256> {
    ensure!(denominator > U256::ZERO, "invalid zero fee denominator");
    let quotient = numerator / denominator;
    if numerator % denominator == U256::ZERO {
        Ok(quotient)
    } else {
        quotient
            .checked_add(U256::from(1))
            .context("fee exceeds uint256")
    }
}

/// SDK getGasFeeOptions: ceil(fee * nativePrice * 10^(6-nativeDecimals)).
pub(super) fn stable_fee(fee: &str, price: &str, native_decimals: u32) -> Result<U256> {
    ensure!(
        fee.bytes().all(|b| b.is_ascii_digit()) && !fee.is_empty(),
        "invalid native relay fee"
    );
    let native = U256::from_str_radix(fee, 10).context("invalid native relay fee")?;
    let (price, precision) = decimal(price)?;
    ensure!(
        price > U256::ZERO,
        "Allbridge does not offer stablecoin fee payment for this route"
    );
    let divisor = precision
        .checked_mul(U256::from(10).pow(U256::from(native_decimals - 6)))
        .context("fee precision exceeds uint256")?;
    ceil_div(
        native
            .checked_mul(price)
            .context("relay fee exceeds uint256")?,
        divisor,
    )
}

/// SDK getAmountToBeReceivedComputeOft: ceil(net * (1-adminFeeShareWithExtras)).
pub(super) fn received(net: U256, share: &str) -> Result<U256> {
    let (numerator, denominator) = decimal(share)?;
    ensure!(numerator < denominator, "invalid Allbridge admin fee share");
    ensure!(
        net > U256::from(1),
        "amount is too small after the relay fee"
    );
    ceil_div(
        net.checked_mul(denominator - numerator)
            .context("received amount exceeds uint256")?,
        denominator,
    )
}
