# uwlt

macOS Apple Silicon 原生命令行钱包，支持 TRON、Ethereum、Polygon，使用 Touch ID 授权私钥操作。

## 安装

[Release](https://github.com/YinMo19/uwlt/releases/latest) 提供 Developer ID 签名并通过 Apple 公证的版本，无需安装 Rust 或 Xcode。

```sh
curl -fLO https://github.com/YinMo19/uwlt/releases/latest/download/uwlt-macos-arm64.zip
unzip -q uwlt-macos-arm64.zip
./uwlt-macos-arm64/install.sh
```

确保 `~/.cargo/bin` 在 `PATH` 中；保留安装后的 `~/Applications/uwlt.app`。

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

## 兑换与跨链

```sh
uwlt --network polygon swap 5 --dry-run # 预览 USDT0 → POL
uwlt --network polygon swap 5
uwlt --network ethereum send 100 --to savings --to-network mainnet --dry-run
uwlt --network ethereum send 100 --to savings --to-network mainnet --slippage 0.5
uwlt --network ethereum bridge-status <源链交易哈希> --to-network mainnet --bridge-id <Bridge_ID> --request-id <请求ID>
```

`swap` 仍将 USDT 换成当前链的手续费币。跨链转账使用 **MetaMask 报价与路由服务**，支持 Ethereum、TRON（`mainnet`）、Polygon 主网之间配置的 USDT/USDT0；具体方向、金额是否可用以实时报价为准，不支持测试网或自定义代币。私钥、钱包与 Touch ID 签名仍由 uwlt 在本机管理，不需要安装 MetaMask 或导入助记词。

- 当前直接请求 MetaMask 钱包使用的 `https://bridge.api.cx.metamask.io`，不需要 Rango API key，也不冒充 MetaMask 客户端。接口依据 [MetaMask 官方客户端源码](https://github.com/MetaMask/core/blob/main/packages/bridge-controller/src/utils/fetch.ts)接入；它不是已确认具有第三方稳定性承诺的公共 API。服务端可能调整接口、认证或限流；访问失败会明确报错，不会自动改走其他服务。
- MetaMask 会聚合底层协议，路线仍可能包含 Rango、LI.FI、Bridgers 等。uwlt 在满足本地执行与滑点要求的报价中选择预计到账最高的路线；不会为更高报价放宽安全检查。多步额外签名、目标链领取、gasless/7702、refuel 及需要独立 TRON 交易备注的路线不执行，也不会丢弃备注后发送。ERC-20/TRC-20 调用数据中的路由后缀保留原样。
- `--dry-run` 请求预览报价并校验返回的交易数据，不读取私钥、不签名或广播；服务端可能同时生成报价 ID 和未签名交易。预览不保证余额充足或正式发送时仍有路线。正式发送重新报价，展示路线、预计到账、最低到账、费用、授权对象和本地 gas 预算，再由 Touch ID 授权。默认滑点 **0.5%**，`--slippage` 按百分比指定，最多两位小数；不接受最低到账超出该滑点的路线。报价有效期自请求开始最多 90 秒，提供方更短的期限优先；过期后需重新请求。
- 展示 MetaMask 服务费，并核对“路由净输入 + MetaMask 费 = 本次输入金额”。实测 1,000 USDT 报价的 MetaMask 费为 8.75 USDT（0.875%），不承诺固定费率或比其他服务便宜；预计到账已包含相关扣费，不再重复扣除。本项目不额外配置推荐费。
- 钱包仍需源链手续费币（ETH/TRX/POL）。报价中的额外原生币付款与本地 gas 预算单独展示；预算是上限而非预计实际费用，链上实际消耗会另行显示。服务端可能因余额条件不给正式报价，即使 `--dry-run` 有路线。
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
