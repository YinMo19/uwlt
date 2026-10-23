use anyhow::{Result, bail};

use super::network::{Chain, Network};
use crate::{ethereum, solana, tron};

pub(super) fn run(network: Network, rpc: Option<&str>) -> Result<()> {
    print!("{}", rates(network, rpc)?);
    Ok(())
}

pub(super) fn all() -> Result<()> {
    let networks = [
        Network::Mainnet,
        Network::Ethereum,
        Network::Polygon,
        Network::Solana,
    ];
    let results = std::thread::scope(|scope| {
        networks
            .map(|network| scope.spawn(move || (network, rates(network, None))))
            .map(|worker| worker.join().expect("fee query thread panicked"))
    });
    let mut failed = false;
    for (network, result) in results {
        match result {
            Ok(output) => println!("{output}"),
            Err(error) => {
                eprintln!("{}: {error:#}", network.label());
                failed = true;
            }
        }
    }
    if failed {
        bail!("some networks could not be queried");
    }
    Ok(())
}

fn rates(network: Network, rpc: Option<&str>) -> Result<String> {
    let label = network.label();
    match network.chain() {
        Chain::Tron => {
            let fees = tron::Client::new(rpc)?.fees()?;
            Ok(format!(
                "{label} (TRX)\n  Energy           {} SUN/unit\n  Bandwidth        {} SUN/byte\n  Account creation {} TRX\n",
                fees.energy_sun,
                fees.bandwidth_sun,
                tron::format_amount(fees.activation_sun),
            ))
        }
        Chain::Ethereum(chain) => {
            let client = ethereum::Client::new(chain, rpc)?;
            let fees = client.fees()?;
            Ok(format!(
                "{label} ({})\n  Base fee         {} Gwei\n  Priority fee     {} Gwei\n  Max fee          {} Gwei\n  Native transfer  <= {} {} (21,000 gas)\n",
                client.native_symbol(),
                fees.base_gwei,
                fees.priority_gwei,
                fees.max_gwei,
                fees.native_transfer,
                client.native_symbol(),
            ))
        }
        Chain::Solana => {
            let fees = solana::Client::new(rpc)?.fees()?;
            Ok(format!(
                "{label} (SOL)\n  Native transfer  {} SOL (one signature, no priority fee)\n  Token account    {} SOL rent-exempt deposit if needed\n",
                fees.native_transfer, fees.token_account_rent,
            ))
        }
    }
}
