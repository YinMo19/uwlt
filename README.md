# uwlt

macOS Apple Silicon 上的原生 Rust 命令行钱包，使用 Touch ID 授权私钥操作。支持 TRON、Ethereum 和 Polygon，无需 Node 或 Java 运行时。

## 安装

需要 Rust 1.94.1+、Xcode Command Line Tools；签名安装还需要完整 Xcode（已登录自己的开发者账号）和 `xcodegen`，以及可用的 Touch ID。

```sh
git clone https://github.com/YinMo19/uwlt.git
cd uwlt
brew install xcodegen
bash scripts/setup-wallet-core.sh
./scripts/install-macos.sh --team YOUR_TEAM_ID
uwlt doctor
```

将 `YOUR_TEAM_ID` 替换为自己的 Apple 开发者团队 ID。安装脚本自动配置开发签名、构建并安装；确保 `~/.cargo/bin` 在 `PATH` 中。入口直接指向 `~/Applications/uwlt.app` 内的原生可执行文件，请保留整个应用目录。升级时运行同一安装命令，保持签名团队和应用标识不变；签名配置续期时加 `--provision`。这是本机开发签名安装，不是公证发行包。

只构建、不安装：完成 Wallet Core setup 后运行 `cargo build --release --locked`，再用 `./target/release/uwlt --help` 查看帮助。钱包的 Keychain 操作需要上述签名安装。

## 钱包与网络

```sh
uwlt create                       # 交互输入钱包名
uwlt create savings               # 直接指定名称
uwlt import imported              # 隐藏输入已有私钥，保存为 imported
uwlt list
uwlt address
uwlt balance
uwlt balance "$(uwlt address)"     # 也可查询任意地址，无需拥有其私钥
uwlt --network polygon balance --all
uwlt -w savings --network ethereum address
```

钱包名只接受小写字母 `a-z`。默认钱包为 `main`，用 `-w 名称` 或 `--wallet 名称` 选择其他钱包；默认网络为 TRON 主网，用 `--network` 切换：

| `--network` | 网络 | 默认代币 / 手续费币 |
| --- | --- | --- |
| `mainnet` | TRON 主网 | USDT / TRX |
| `nile` | TRON Nile 测试网 | 测试 USDT / TRX |
| `ethereum` | Ethereum 主网 | USDT / ETH |
| `sepolia` | Ethereum Sepolia 测试网 | 自选测试代币 / ETH |
| `polygon` | Polygon PoS 主网 | USDT0 / POL |

同一私钥可用于所有支持网络，Ethereum 与 Polygon 使用相同地址。Sepolia 没有预设 USDT，查询或发送测试代币时用 `--token 合约地址` 指定六位小数的兼容合约；主网代币合约固定。

`balance --all` 按名称显示所选网络下所有本地钱包的余额，不跨网络汇总，也不能与 `-w` 或指定地址混用。

## 转账与费用

`--to` 接受收款地址或本地钱包名。下面以已有的 `savings` 钱包为收款人：

```sh
uwlt send 10 --to savings --dry-run          # 先模拟，不签名、不广播
uwlt send 10 --to savings                    # USDT
uwlt send 5 --to savings --trx               # TRX
uwlt --network ethereum send 0.01 --to savings --eth
uwlt --network polygon send 1 --to savings --pol
uwlt --network polygon send 10 --to savings  # USDT0
uwlt --network polygon status <交易哈希>
uwlt fees
uwlt fees --all
```

USDT / USDT0 / TRX 最多六位小数，ETH / POL 最多十八位。TRON 的 `send --fee-limit 100` 设置 100 TRX 的 Energy 预算（默认值），Bandwidth 另外计费。Ethereum 和 Polygon 自动估算 Gas 费用。`fees` 查看实时费率；`fees --all` 使用各网络默认 RPC，具体交易预算以 `--dry-run` 为准。

发送后会等待上链确认并显示交易详情；超时可用 `status` 继续查询，查询时须选择发送时的网络。**`Actual fee` 是上链执行回执中的实际扣费**；发送前的 `Fee cap` / `Fee budget` 是上限预算，`Estimated fee` 是估算，不是实际费用。失败的链上交易也可能扣费；没有回执时不显示实际费用。

## USDT 兑换手续费币

```sh
uwlt swap 5 --dry-run                  # 预览 5 USDT → TRX
uwlt swap 5                           # SunSwap V2
uwlt --network ethereum swap 5         # Uniswap V2：USDT → ETH
uwlt --network polygon swap 5 --slippage 1 # QuickSwap V2：USDT0 → POL
```

仅支持主网，兑换结果进入当前钱包。默认最大滑点 `0.5%`，`--slippage 1` 表示 `1%`。执行前显示预计和最低到账、授权笔数及网络费预算；必要授权与兑换逐笔确认并显示实际费用。**兑换本身也需要已有的 TRX / ETH / POL 支付手续费**，不能从零手续费币余额开始。已成功的授权不会因后续兑换失败而回滚。

## Allbridge 跨链

支持通过 Allbridge Core 在 **Ethereum ↔ TRON 主网之间转 USDT**，不需要 API key。`--network` 是源链，`--to-network` 是目标链；收款人也可填写目标链地址。

```sh
uwlt --network ethereum send 100 --to savings --to-network mainnet --dry-run
uwlt --network ethereum send 100 --to savings --to-network mainnet
uwlt -w savings send 100 --to main --to-network ethereum
uwlt --network ethereum bridge-status <Ethereum源链交易哈希>
uwlt bridge-status <TRON源链交易哈希>
```

报价显示目标到账、桥接费用和预计时间；源链网络费另付，不额外购买目标链手续费币。**源链交易确认不等于目标链到账**：发送命令确认源链交易后结束，用它打印的 `bridge-status` 命令继续跟踪目标链到账或退款状态。多笔回执列出的实际费用合计仅是源链网络费，不含报价中的桥接费用。

Polygon 当前无法通过此桥跨链，但同链转账和兑换可用；Nile / Sepolia 不支持跨链。跨链 `--dry-run` 不签名、不转出资金，但报价服务可能创建未付款订单。

## 私钥、备份与同步

私钥保存在 macOS **iCloud Keychain**（Data Protection Keychain）中，**不是 Apple「密码」App 中的网站密码**。本地钱包元数据不包含私钥；使用私钥前由 uwlt 验证 Touch ID。

```sh
uwlt key                          # 显示钱包名和明文私钥
uwlt -w savings backup savings.key # 新建明文备份，仅当前用户可读写，不覆盖文件
uwlt import restored --file savings.key
uwlt sync
uwlt remove restored
```

备份文件只含私钥，请妥善保管。另一台 Mac 须登录同一 Apple 账户，开启「iCloud → 密码与钥匙串」，并使用相同团队及应用标识签名的 uwlt。`sync` 迁移旧的本机条目并刷新已下载的钱包列表；实际跨设备传输由 macOS 管理，不保证即时完成。

`remove` 要求先准确输入 **`confirm`**，再通过 **Touch ID**。它删除本地记录及 iCloud Keychain 中的私钥，**删除会同步到其他设备，但链上资金仍然存在**。没有其他私钥备份将无法再操作这些资金；重新创建同名钱包不会恢复原地址。其他设备可运行 `sync` 更新本地列表。

## 补全与配置

在 `~/.zshrc` 中加入（Bash 则在 `~/.bashrc` 中将 `zsh` 改为 `bash`）：

```sh
eval "$(uwlt completion zsh)"
```

- `--rpc URL` / `UWLT_RPC`：自定义所选网络的 HTTP RPC。
- `UWLT_TRONGRID_API_KEY`：可选的 TronGrid API key。
- `UWLT_HOME`：公共钱包元数据目录，默认 `~/Library/Application Support/uwlt`，不存私钥。
- `uwlt --help`、`uwlt send --help`：查看完整命令与选项。

许可证：[MIT](LICENSE)。第三方测试向量保留其[原始许可](LICENSE-THIRD-PARTY)。
