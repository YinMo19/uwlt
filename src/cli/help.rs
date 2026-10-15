pub(super) const QUICK_START: &str = r#"Examples:
  Create a wallet, then view its Polygon address and balance:
    uwlt create main
    uwlt address --network polygon
    uwlt balance --network polygon

  Show transfer options and examples:
    uwlt send --help"#;

pub(super) const CREATE: &str = r#"Examples:
  Create the default wallet:
    uwlt create main

  Create another wallet:
    uwlt create savings

  Choose the name interactively:
    uwlt create"#;

pub(super) const IMPORT: &str = r#"Examples:
  Import a key using hidden input:
    uwlt import savings

  Import a previously exported key file:
    uwlt import savings --file ~/savings.key"#;

pub(super) const LIST: &str = r#"Examples:
  List local wallets with Polygon addresses:
    uwlt list --network polygon

  List the same wallets with TRON addresses:
    uwlt list --network mainnet"#;

pub(super) const SYNC: &str = r#"Examples:
  Sync wallets and display their Polygon addresses:
    uwlt sync --network polygon

  Sync wallets and display their TRON addresses:
    uwlt sync --network mainnet"#;

pub(super) const ADDRESS: &str = r#"Examples:
  Show the default wallet's Polygon address:
    uwlt address --network polygon

  Show savings' TRON address:
    uwlt address -w savings --network mainnet"#;

pub(super) const BALANCE: &str = r#"Examples:
  Show the default wallet's USDT0 and POL balances:
    uwlt balance --network polygon

  Show savings' USDT and ETH balances:
    uwlt balance -w savings --network ethereum

  Show every local wallet's TRON balances:
    uwlt balance --all --network mainnet"#;

pub(super) const SEND: &str = r#"Examples:
  Send 10 USDT0 on Polygon from main to the local wallet savings:
    uwlt send 10 --network polygon -w main --to savings

  Preview a USDT0 transfer without signing or broadcasting:
    uwlt send 10 --network polygon --to savings --dry-run

  Send native POL, ETH or TRX:
    uwlt send 1 --network polygon --pol --to savings
    uwlt send 0.001 --network ethereum --eth --to savings
    uwlt send 5 --network mainnet --trx --to savings

  Send USDT from Ethereum to TRON through Allbridge:
    uwlt send 10 --network ethereum --to savings \
      --to-network mainnet

  Send USDT from TRON to Ethereum through Allbridge:
    uwlt send 10 --network mainnet --to savings \
      --to-network ethereum

  Create the recipient wallet first with: uwlt create savings"#;

pub(super) const SWAP: &str = r#"Examples:
  Preview exchanging 10 USDT0 for POL:
    uwlt swap 10 --network polygon --dry-run

  Exchange 10 USDT for ETH from savings with 1% maximum slippage:
    uwlt swap 10 -w savings --network ethereum --slippage 1

  Exchange 10 USDT for TRX:
    uwlt swap 10 --network mainnet"#;

pub(super) const FEES: &str = r#"Examples:
  Show Polygon fee rates:
    uwlt fees --network polygon

  Show TRON fee rates:
    uwlt fees --network mainnet

  Compare all supported networks:
    uwlt fees --all"#;

pub(super) const STATUS: &str = r#"Examples:
  Check a Polygon transaction (replace TX_HASH with its hash):
    uwlt status TX_HASH --network polygon

  Check a TRON transaction:
    uwlt status TX_HASH --network mainnet"#;

pub(super) const BRIDGE_STATUS: &str = r#"Examples:
  Track an Allbridge transfer sent from Ethereum:
    uwlt bridge-status SOURCE_TX_HASH --network ethereum

  Track an Allbridge transfer sent from TRON:
    uwlt bridge-status SOURCE_TX_HASH --network mainnet

  Replace SOURCE_TX_HASH with the source transaction hash from send."#;

pub(super) const KEY: &str = r#"Examples:
  Show the default wallet's private key:
    uwlt key

  Show savings' private key:
    uwlt key -w savings"#;

pub(super) const BACKUP: &str = r#"Examples:
  Export the default wallet to a new file:
    uwlt backup ~/main.key

  Export savings to a new file:
    uwlt backup ~/savings.key -w savings"#;

pub(super) const REMOVE: &str = r#"Examples:
  Remove the wallet named savings:
    uwlt remove savings"#;

pub(super) const DOCTOR: &str = r#"Examples:
  Check Wallet Core and Touch ID:
    uwlt doctor"#;

pub(super) const COMPLETION: &str = r#"Examples:
  Enable completion in the current zsh session:
    autoload -Uz compinit && compinit
    eval "$(uwlt completion zsh)"

  Enable completion in the current bash session:
    eval "$(uwlt completion bash)""#;

pub(super) const HELP: &str = r#"Examples:
  Show the command overview:
    uwlt help

  Show transfer options and examples:
    uwlt help send

  Show swap options and examples:
    uwlt help swap"#;
