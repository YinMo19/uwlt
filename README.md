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
uwlt balance                     # 余额
uwlt --network polygon balance --all
uwlt --network polygon send 10 --to savings --dry-run
uwlt --network polygon send 10 --to savings
uwlt --network polygon status <交易哈希>
uwlt fees --all                   # 各网络手续费预览
```

默认钱包是 `main`，用 `-w savings` 切换。`--to` 可填地址或本地钱包名。创建、导入不选网络；其他网络相关操作未传 `--network` 时会交互询问，没有默认网络。

## 兑换与跨链

```sh
uwlt --network polygon swap 5 --dry-run # 预览 USDT0 → POL
uwlt --network polygon swap 5
uwlt --network ethereum send 100 --to savings --to-network mainnet
uwlt --network ethereum bridge-status <源链交易哈希>
```

`swap` 将 USDT 换成当前链的手续费币，钱包需已有少量手续费币。Allbridge 跨链目前支持 Ethereum ↔ TRON 主网；源链确认后用 `bridge-status` 跟踪到账。

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
