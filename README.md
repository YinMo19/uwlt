# uwlt

macOS Apple Silicon 原生命令行钱包，仅支持 TRON、Ethereum、Polygon、Solana 主网，使用 Touch ID 授权私钥操作。

## 安装

[Release](https://github.com/YinMo19/uwlt/releases/latest) 提供 Developer ID 签名并通过 Apple 公证的版本，无需安装 Rust 或 Xcode。

```sh
curl -fLO https://github.com/YinMo19/uwlt/releases/latest/download/uwlt-macos-arm64.zip
unzip -q uwlt-macos-arm64.zip
./uwlt-macos-arm64/install.sh
```

确保 `~/.cargo/bin` 在 `PATH` 中；保留安装后的 `~/Applications/uwlt.app`。

从源码安装使用 `./scripts/install-macos.sh`：脚本执行 `cargo install` 后，使用签名配置打包应用并更新可执行文件链接，保留钥匙串所需的应用身份。不要直接覆盖已签名应用中的二进制文件。

应用保留 macOS Hardened Runtime 和库签名校验，仅链接系统动态库，不依赖 Homebrew OpenSSL。

## 日常使用

```sh
uwlt create main                 # 创建钱包
uwlt import savings              # 隐藏输入私钥并导入
uwlt list                        # 列出钱包
uwlt address                     # 收款地址
uwlt balance                     # 所有本地钱包在所选网络上的余额
uwlt --network polygon balance
uwlt --network polygon send 10 --to savings --dry-run
uwlt --network polygon send 10 --to savings
uwlt --network polygon status <交易哈希>
uwlt fees --all                   # 各网络手续费预览
```

`balance` 默认查询所有本地钱包在所选网络上的余额，无需 `--all`；用 `balance -w savings` 只查询指定钱包，或用 `balance <地址>` 查询指定地址。其他需要选择钱包的命令默认使用 `main`，用 `-w savings` 切换。`--to` 可填地址或本地钱包名。创建、导入不选网络；其他网络相关操作未传 `--network` 时会交互询问，没有默认网络。

可选网络仅为 `mainnet`（TRON）、`ethereum`、`polygon`、`solana`；`fees --all` 只查询这四条主网。各网络固定使用主网 USDT/USDT0，不提供自定义代币配置。

## Solana

```sh
uwlt sync --network solana        # 旧钱包首次使用：Touch ID 授权补齐 Solana 地址
uwlt --network solana address
uwlt --network solana balance
uwlt --network solana send 0.01 --sol --to savings --dry-run
uwlt --network solana send 10 --to savings --dry-run # 默认发送 USDT
uwlt --network solana fees
uwlt --network solana status <Base58交易签名>
```

- `solana` 为主网；使用 `--rpc` 或 `UWLT_RPC` 指定节点时仍会校验主网 genesis hash，防止串网。SOL 使用 9 位精度，USDT 使用 6 位精度，全程使用整数金额。
- 固定使用经典 SPL Token 的 [Tether 官方 Solana USDT](https://tether.to/en/supported-protocols/)：`Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB`，不接受其他 mint。
- `--sol` 发送 SOL，不传原生币参数则发送 USDT。USDT 的 `--to` 填钱包所有者地址，不是 token account 地址；接收方关联代币账户（ATA）不存在时自动创建，预览会把网络费和账户免租押金分开显示。同链转账可合并发送方多个代币账户的余额；异常授权、冻结或不匹配的代币账户会明确报错。
- 同链转账和兑换的预览会检查余额并执行 RPC 模拟；不读取私钥、不签名、不广播。正式发送在 Touch ID 后重新检查区块哈希有效性，不偷偷更换消息或重复广播。出现广播结果不确定时，用已打印的签名查询 `status`，不要直接重发。
- 新建、导入的钱包自动保存 Solana 公共地址。现有 32 字节私钥直接作为 Ed25519 seed，TRON/EVM 地址和备份不变；不是从 TRON 地址或助记词推导 Solana 地址。备份/导入仍使用 64 个十六进制字符（32 字节），不接受 Solana CLI 的 64 字节 JSON 密钥文件。旧钱包缺少地址时需运行一次 `sync`；迁移后查询地址和余额无需 Touch ID。`sync` 会更新 iCloud 公共地址元数据，请先升级所有设备上的 uwlt。

## 兑换与跨链

```sh
uwlt --network polygon swap 5 --dry-run # 预览 USDT0 → POL
uwlt --network polygon swap 5
uwlt --network solana swap 10 --dry-run # 预览 USDT → 原生 SOL
uwlt --network solana send 100 --to savings --to-network ethereum --dry-run
uwlt --network ethereum send 100 --to savings --to-network solana --dry-run
uwlt --network ethereum send 100 --to savings --to-network mainnet --dry-run
uwlt --network ethereum send 100 --to savings --to-network mainnet --slippage 0.5
uwlt --network ethereum bridge-status <源链交易哈希> --to-network mainnet --bridge-id <Bridge_ID> --request-id <请求ID>
```

`swap` 在 Ethereum、Polygon、TRON（`mainnet`）、Solana 上统一使用 **MetaMask 报价与路由服务**，将 USDT/USDT0 换成当前链的原生手续费币 ETH/POL/TRX/SOL。不再直接请求 Uniswap、QuickSwap、SunSwap 或 Jupiter 报价，也不在 MetaMask 无路线时自动回退旧 DEX；底层执行协议仍可能是这些 DEX。无需 MetaMask 钱包或 API key，私钥和 Touch ID 签名仍在 uwlt 本机。

- 同链只执行能在兑换交易中交付原生币的受支持路线，拒绝先充值再兑付、桥接、额外签名、gasless/7702 等路线；不能把充值交易确认当作原生币到账。部分金额可能因此没有兼容路线，尤其是 TRON 返回 Bridgers/XO Swap 兑付路线时。
- Ethereum/Polygon 校验 MetaSwap 路由器和受支持适配器的精确输入、原生币输出、费用及最低值；TRON 校验 Rango 原子兑换及其受支持的 SunSwap 调用。内部 DEX 调用和适配器实现仍是信任边界，不代表支持 MetaMask 的全部协议。
- Solana 使用 MetaMask 返回的 Jupiter 路线，支持可完整解析的多跳指令及地址查找表；解码布局按 Jupiter 程序持有的链上 IDL 校准，包含 ByrealDynamicV3、JupiterLendDexSwap 等新变体，未知或未支持的动态布局仍会拒绝。USDT 需位于发送方 ATA，钱包需有 SOL 支付网络费和账户押金；若已有非零 wSOL 余额，须先处理该余额以免一并解包。兑换和解包在同一笔交易内完成，最终交付原生 SOL，而非 wSOL。
- 展示的 `Minimum` 为扣除 MetaMask 服务费后的最低原生币到账，网络费另计。EVM/Solana 对扣输出服务费的路线在本地加强链上最低值，覆盖“最低净到账 + 固定服务费”，不降低更严格的原有门槛。服务费可能从输入 USDT 或输出原生币扣除，按返回数据校验并显示，不重复扣费；不承诺 MetaMask 比直连 DEX 更便宜。
- `--dry-run` 不读取私钥、不签名、不广播。Solana 会模拟完整交易并检查精确 USDT 支出和最低原生 SOL 到账；EVM/TRON 预览校验报价、交易参数和授权预算，实际执行时逐阶段模拟后再签名。费用预算不是实际手续费，Solana 预算还可能包含兑换完成后退回的账户押金。默认滑点 0.5%，报价自请求起最多有效 90 秒。

Solana 兑换模拟接受已关闭 wSOL 账户的两种 RPC 表示：`null`，或零 lamports、空数据、不可执行且归 System Program 所有的账户对象；后者是账户关闭后尚未清除的状态，不代表留下了 wSOL。仍会检查精确 USDT 支出、关闭账户指令及扣除手续费后的最低原生 SOL 到账。

跨链转账使用 **MetaMask 报价与路由服务**，支持 Ethereum、TRON（`mainnet`）、Polygon、Solana 主网之间配置的 USDT/USDT0；具体方向、金额是否可用以实时报价为准，不支持自定义代币。私钥、钱包与 Touch ID 签名仍由 uwlt 在本机管理，不需要安装 MetaMask 或导入助记词。

- 当前直接请求 MetaMask 钱包使用的 `https://bridge.api.cx.metamask.io`，不需要 Rango API key，也不冒充 MetaMask 客户端。接口依据 [MetaMask 官方客户端源码](https://github.com/MetaMask/core/blob/main/packages/bridge-controller/src/utils/fetch.ts)接入；它不是已确认具有第三方稳定性承诺的公共 API。服务端可能调整接口、认证或限流；访问失败会明确报错，不会自动改走其他服务。
- MetaMask 会聚合底层协议，路线仍可能包含 Rango、LI.FI、Bridgers 等。uwlt 在满足本地执行与滑点要求的报价中选择预计到账最高的路线；不会为更高报价放宽安全检查。多步额外签名、目标链领取、gasless/7702、refuel 及需要独立 TRON 交易备注的路线不执行，也不会丢弃备注后发送。ERC-20/TRC-20 调用数据中的路由后缀保留原样。
- Solana 发起的跨链路线目前接受可完整解码的 XO Swap（`xoswap`）充值交易，逐项校验付款人、USDT mint、充值/服务费 ATA 和精确金额。未知路由程序、额外签名、授权、备注和地址查找表跨链路线会拒绝，不丢弃指令后发送。充值地址与跨链收款人的映射仍依赖服务方；源交易确认不等于目标链到账。
- `--dry-run` 请求预览报价并校验返回的交易数据，不读取私钥、不签名或广播；服务端可能同时生成报价 ID 和未签名交易。预览不保证余额充足或正式发送时仍有路线。正式发送重新报价，展示路线、预计到账、最低到账、费用、授权对象和本地 gas 预算，再由 Touch ID 授权。默认滑点 **0.5%**，`--slippage` 按百分比指定，最多两位小数；不接受最低到账超出该滑点的路线。报价有效期自请求开始最多 90 秒，提供方更短的期限优先；过期后需重新请求。
- 展示 MetaMask 服务费，并核对“路由净输入 + MetaMask 费 = 本次输入金额”。实测 1,000 USDT 报价的 MetaMask 费为 8.75 USDT（0.875%），不承诺固定费率或比其他服务便宜；预计到账已包含相关扣费，不再重复扣除。本项目不额外配置推荐费。
- 钱包仍需源链手续费币（ETH/TRX/POL/SOL）。报价中的额外原生币付款、本地 gas 预算及 Solana 账户免租押金单独展示；预算不是实际费用，链上实际消耗会另行显示。服务端可能因余额条件不给正式报价，即使 `--dry-run` 有路线。
- 私钥始终留在本机，授权只接受本次精确输入金额，不主动请求无限授权。交易的链、代币、发送方、收款方报价及金额均会校验；路由合约的内部调用、目标链到账约束与充值地址映射仍依赖 MetaMask 和底层协议。本地校验与模拟不能消除跨链桥风险。
- 保存 `send` 输出的请求 ID、Bridge ID、目标网络和源链哈希；广播源交易后立即打印完整跟踪命令，即使等待确认超时也可继续查询。`bridge-status` 查询 [MetaMask 状态接口](https://github.com/MetaMask/core/blob/main/packages/bridge-status-controller/src/utils/bridge-status.ts)，区分已提交、处理中、失败、未知和服务端报告完成；缺少目标交易、正确代币或正数到账金额时显示到账未核实。**源链确认不代表目标链到账**；该接口没有统一的退款状态，失败不代表已退款。旧 Allbridge/Rango 订单仍应在原服务查询，不可套用新的跟踪命令。

## 备份与同步

私钥保存在 iCloud Keychain，跨设备使用同一 Apple 账户并开启 iCloud 钥匙串。

```sh
uwlt -w savings backup savings.key # 导出明文私钥备份
uwlt import restored --file savings.key
uwlt sync
uwlt remove restored              # 输入 confirm，再验证 Touch ID
```

## 补全与帮助

在 `~/.zshrc` 加入 `eval "$(uwlt completion zsh)"`；Bash 则在 `~/.bashrc` 中将 `zsh` 改为 `bash`。

`uwlt -h` 查看命令；每个子命令的帮助都含示例，例如 `uwlt send -h`、`uwlt swap --help`。设置 `NO_COLOR=1` 可关闭帮助中的样式。

[MIT](LICENSE) · [第三方测试向量许可](LICENSE-THIRD-PARTY)
