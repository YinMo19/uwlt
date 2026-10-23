use color_print::cstr;

pub(super) const QUICK_START: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Create a wallet, then view its Polygon address and balance:
    <bold>uwlt create</bold> main
    <bold>uwlt address --network</bold> polygon
    <bold>uwlt balance --network</bold> polygon

  Show transfer options and examples:
    <bold>uwlt send --help</bold>"#
);

pub(super) const CREATE: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Create the default wallet:
    <bold>uwlt create</bold> main

  Create another wallet:
    <bold>uwlt create</bold> savings

  Choose the name interactively:
    <bold>uwlt create</bold>"#
);

pub(super) const IMPORT: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Import a key using hidden input:
    <bold>uwlt import</bold> savings

  Import a previously exported key file:
    <bold>uwlt import</bold> savings <bold>--file</bold> ~/savings.key"#
);

pub(super) const LIST: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  List local wallets with Polygon addresses:
    <bold>uwlt list --network</bold> polygon

  List the same wallets with TRON addresses:
    <bold>uwlt list --network</bold> mainnet"#
);

pub(super) const SYNC: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Sync wallets and display their Polygon addresses:
    <bold>uwlt sync --network</bold> polygon

  Derive and synchronize Solana addresses for existing wallets (Touch ID):
    <bold>uwlt sync --network</bold> solana"#
);

pub(super) const ADDRESS: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Show the default wallet's Polygon address:
    <bold>uwlt address --network</bold> polygon

  Show savings' Solana address:
    <bold>uwlt address -w</bold> savings <bold>--network</bold> solana"#
);

pub(super) const BALANCE: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Show every local wallet's USDT0 and POL balances:
    <bold>uwlt balance --network</bold> polygon

  Show savings' USDT and ETH balances:
    <bold>uwlt balance -w</bold> savings <bold>--network</bold> ethereum

  Show every local wallet's USDT and SOL balances:
    <bold>uwlt balance --network</bold> solana"#
);

pub(super) const SEND: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Send 10 USDT0 on Polygon from main to the local wallet savings:
    <bold>uwlt send</bold> 10 <bold>--network</bold> polygon <bold>-w</bold> main <bold>--to</bold> savings

  Preview a USDT0 transfer without signing or broadcasting:
    <bold>uwlt send</bold> 10 <bold>--network</bold> polygon <bold>--to</bold> savings <bold>--dry-run</bold>

  Send native POL, ETH, TRX or SOL:
    <bold>uwlt send</bold> 1 <bold>--network</bold> polygon <bold>--pol --to</bold> savings
    <bold>uwlt send</bold> 0.001 <bold>--network</bold> ethereum <bold>--eth --to</bold> savings
    <bold>uwlt send</bold> 5 <bold>--network</bold> mainnet <bold>--trx --to</bold> savings
    <bold>uwlt send</bold> 0.01 <bold>--network</bold> solana <bold>--sol --to</bold> savings

  Preview sending USDT on Solana (creates the recipient ATA when needed):
    <bold>uwlt send</bold> 10 <bold>--network</bold> solana <bold>--to</bold> savings <bold>--dry-run</bold>

  Send USDT from Ethereum to TRON through MetaMask routing:
    <bold>uwlt send</bold> 10 <bold>--network</bold> ethereum <bold>--to</bold> savings \
      <bold>--to-network</bold> mainnet

  Send USDT from TRON to Ethereum through MetaMask routing:
    <bold>uwlt send</bold> 10 <bold>--network</bold> mainnet <bold>--to</bold> savings \
      <bold>--to-network</bold> ethereum

  Preview a cross-chain route with 0.5% maximum slippage:
    <bold>uwlt send</bold> 100 <bold>--network</bold> ethereum <bold>--to</bold> savings \
      <bold>--to-network</bold> mainnet <bold>--slippage</bold> 0.5 <bold>--dry-run</bold>

  Preview USDT from Solana to Ethereum:
    <bold>uwlt send</bold> 100 <bold>--network</bold> solana <bold>--to</bold> savings \
      <bold>--to-network</bold> ethereum <bold>--dry-run</bold>

  Quotes use the MetaMask routing service; keys and Touch ID signing stay in uwlt.
  No MetaMask wallet or API key is required. Route availability is provider-dependent.

  Create the recipient wallet first with: <bold>uwlt create</bold> savings"#
);

pub(super) const SWAP: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Preview exchanging 10 USDT0 for POL:
    <bold>uwlt swap</bold> 10 <bold>--network</bold> polygon <bold>--dry-run</bold>

  Exchange 10 USDT for ETH from savings with 1% maximum slippage:
    <bold>uwlt swap</bold> 10 <bold>-w</bold> savings <bold>--network</bold> ethereum <bold>--slippage</bold> 1

  Preview USDT to native SOL through MetaMask routing:
    <bold>uwlt swap</bold> 10 <bold>--network</bold> solana <bold>--dry-run</bold>

  All networks use MetaMask quotes; keys and signing remain local to uwlt.
  Provider fees are included in quoted output/input; network gas is additional.
  Only validated atomic native-coin swaps execute. Deposit/bridge routes are rejected.
  No compatible quote means an error, not a fallback to a direct DEX.
  Solana requires USDT in the wallet's associated token account and SOL for fees.
  Solana previews simulate the transaction; other networks simulate stages before signing."#
);

pub(super) const FEES: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Show Polygon fee rates:
    <bold>uwlt fees --network</bold> polygon

  Show TRON fee rates:
    <bold>uwlt fees --network</bold> mainnet

  Compare all supported networks:
    <bold>uwlt fees --all</bold>"#
);

pub(super) const STATUS: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Check a Polygon transaction (replace TX_HASH with its hash):
    <bold>uwlt status</bold> TX_HASH <bold>--network</bold> polygon

  Check a Solana transaction (use its Base58 signature, not a hex hash):
    <bold>uwlt status</bold> SIGNATURE <bold>--network</bold> solana"#
);

pub(super) const BRIDGE_STATUS: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Track a MetaMask-routed transfer from Ethereum to TRON:
    <bold>uwlt bridge-status</bold> SOURCE_TX_HASH <bold>--network</bold> ethereum \
      <bold>--to-network</bold> mainnet <bold>--bridge-id</bold> BRIDGE_ID <bold>--request-id</bold> REQUEST_ID

  Track a MetaMask-routed transfer from Solana to Polygon:
    <bold>uwlt bridge-status</bold> SOURCE_SIGNATURE <bold>--network</bold> solana \
      <bold>--to-network</bold> polygon <bold>--bridge-id</bold> xoswap <bold>--request-id</bold> REQUEST_ID

  Copy the complete tracking command printed immediately after source broadcast.
  Tracking uses MetaMask's service; source confirmation alone does not prove delivery.
  Failed or unknown status does not confirm a refund. Check both chains before retrying."#
);

pub(super) const KEY: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Show the default wallet's private key:
    <bold>uwlt key</bold>

  Show savings' private key:
    <bold>uwlt key -w</bold> savings"#
);

pub(super) const BACKUP: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Export the default wallet to a new file:
    <bold>uwlt backup</bold> ~/main.key

  Export savings to a new file:
    <bold>uwlt backup</bold> ~/savings.key <bold>-w</bold> savings"#
);

pub(super) const REMOVE: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Remove the wallet named savings:
    <bold>uwlt remove</bold> savings"#
);

pub(super) const DOCTOR: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Check Wallet Core and Touch ID:
    <bold>uwlt doctor</bold>"#
);

pub(super) const COMPLETION: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Enable completion in the current zsh session:
    <bold>autoload -Uz</bold> compinit && <bold>compinit</bold>
    <bold>eval</bold> "$(<bold>uwlt completion</bold> zsh)"

  Enable completion in the current bash session:
    <bold>eval</bold> "$(<bold>uwlt completion</bold> bash)""#
);

pub(super) const HELP: &str = cstr!(
    r#"<bold><underline>Examples:</underline></bold>
  Show the command overview:
    <bold>uwlt help</bold>

  Show transfer options and examples:
    <bold>uwlt help</bold> send

  Show swap options and examples:
    <bold>uwlt help</bold> swap"#
);
