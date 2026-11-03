# 协调服务与运维边界

本文件是未来部署设计；本次没有部署或购买服务。
仅部署设备发现辅助、在线状态、信令、必要的 STUN/穿透协调。禁止数据 relay、文件上传下载、对象存储投递与 CDN 文件分发路径。
服务可保留最少设备身份索引、短期连接会话及候选；文件清单/路径/内容摘要不入服务数据库。

主要指标：连接请求量、协商延迟、纯直连成功/失败比例及粗粒度原因、鉴权拒绝、过期会话、服务错误与资源占用。
不要把用户文件传输量写入控制面计费接口。性能诊断若需要文件规模，优先本机导出经用户审查的报告。
控制服务仍存在带宽、计算、日志和防滥用成本；取消的是文件中继带宽预算，不是所有服务器费用。
未知客户端输入严格 schema、TTL、限频、并发与总资源上限，避免成为开放扫描器/代理。

既有直连不应持续依赖服务端续租才能发文件；短期网络变化可能需要重新协调，失败就暂停。
服务提供故障状态，不假装网络一定可连。任何安全问题不能通过“临时开启中继”解决。

## 协调服务器运维（signal-server，CLI 原型现状）

本节为 `ak-cli signal-server` 当前实现的运维事实；上文为部署设计边界，两者一致：只有协调，没有文件路由。

### 启动与端口

- `ak-cli signal-server [--tcp-port 7100] [--stun-port 7101]`：单进程、双套接字——TCP 信令与 UDP STUN 响应器；Ctrl-C 优雅关停。
- 防火墙需放行 TCP 信令端口与 UDP STUN 端口各一个。`probe --server IP:7100` 隐含向 server 端口 +1 发 STUN 查询，规划端口时保持 TCP N / UDP N+1 的相邻约定即可。

### 限额默认（Limits，均为协调面资源上限，与任何文件配额无关）

- 会话 TTL 120 秒；每设备最多 4 个并发会话；Hello 新鲜度窗口 10 秒（防重放）。
- 每连接帧速率 32 帧/秒，惰性令牌桶、突发上限 64 帧，按需补充无后台任务。
- 未知客户端输入按封闭 schema 校验：类型字节仅 1..=8，候选数量、名称长度、声明帧长均有硬上限，截断、尾随字节、超限一律拒绝。

### 无文件路由的验证方式（三层）

- 消息集封闭测试：ak-signal 测试 `closed_message_set_is_bounded` 穷举 CtrlMessage 全部 8 种变体，断言每帧不超过硬上限、类型字节在 1..=8 内；新增携带 blob 的变体会先破坏穷举 match 的编译。
- 依赖扫描：`scripts/check_repository.py` 每次运行扫描 Cargo.lock，封禁 relay/turn/iroh/webrtc/str0m/datachannel 类依赖进入依赖图。
- 运行时旁证：ak-net 测试 `stats_reflect_real_loopback_transfer` 用 `DirectChannel::stats()`（ChannelStats：tx/rx 字节、帧数、丢包计数）对真实环回传输对账——帧数跨端一致、字节差 ≤10%，说明传输字节只流经直连 QUIC 通道；配合 ak-itests `t04_signal_irrelevant_to_data`（数据面零信令依赖）与 `t07_coordination_down`（信令关停前后传输照常完成）构成"协调面可有可无、从不处于数据路径"的证明对（ADR-001 不变量）。

### SameEgress 提示

- 行为：会话两端的控制面 TCP peer IP 均已知且相同时，服务器在 SessionOpened 后向双方各发一条 SameEgress 信号；任一端 IP 未知则绝不发送（不猜）。CLI probe 端只显示一次"same egress detected: prefer LAN candidates first"，服务器重复下发也只公告一次。
- 语义与误报：这只是候选排序建议（先试局域网候选）。同一出口 IP 不保证二层可达——同一企业 NAT 或 CGNAT 后的不同子网、两台经公网服务器协调但实际同网的设备都可能命中或错过该提示；LAN 候选探测失败后正常回退到其余候选，不影响传输正确性。

### 日志边界（不记录用户数据）

- 服务器实现不可见文件名、清单与字节（消息集封闭即保证），日志只记录会话与设备级事件（例如"session 共享出口，提示 LAN 优先"一类的 info 行，含会话短 id）。
- 文件清单、路径与内容摘要不进入信令消息，也不进入服务端任何存储；传输量、文件规模等用户数据不写入控制面日志或计费接口（与上文指标约束一致）。

## 持续集成

`.github/workflows/ci.yml` 已入库备用；仓库尚未推送，该工作流从未在 GitHub Actions 上实际运行过（配置与本地门槛一致，命令均为本地已验证的形态）。推送后行为：

- 触发：push 与 PR 到 `main`、`dev/**` 分支；另支持 workflow_dispatch 手动整跑。
- `checks` job（ubuntu-latest，Python 3.12）：运行 `scripts/check_repository.py`（结构、Cargo.lock 禁依赖扫描、文档链接检查）与 `python -m unittest discover -s scripts/tests`（校验器自身的 12 个单元测试）。
- `rust` job（矩阵：ubuntu-latest + windows-latest；stable 工具链含 clippy/rustfmt；Swatinem/rust-cache 缓存 cargo）：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo test -p ak-store --features sqlite`（bundled C SQLite，验证 MSVC/GCC 双平台可编译）、`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`。
- `release-deep-tests` job（仅 workflow_dispatch 手动触发，不进常规 push/PR 路径）：release-only `#[ignore]` 清单——ak-engine 5 GiB 稀疏文件传输、ak-itests 20,000/65,537 文件多文件传输与 512 MiB 吞吐、256 MiB 分块扫描基准。耗时分钟级（常规测试套件为秒级），故单独手动触发。

`deny.toml` 是与 Python 扫描互补的 cargo-deny 配置：[bans] 按精确名拒绝 iroh/iroh-net/iroh-relay/turn/rusturn/turn-rs/webrtc/str0m/libdatachannel/coturn（multiple-versions 仅警告），[licenses] 限定常见宽松许可证名单（MIT/Apache-2.0/BSD/ISC/Unicode/Zlib/BSL-1.0/CC0），[advisories] 拉取 RustSec 漏洞库、漏洞默认拒绝而 unmaintained/yanked 仅警告。本地开发机未安装 cargo-deny，该文件未在本地执行过；需要时在 CI 增加可选 job（如 EmbarkStudios/cargo-deny-action@v2），或开发者执行 `cargo install cargo-deny --locked` 后运行 `cargo deny check`。

本地等效命令（与 [CONTRIBUTING.md](../CONTRIBUTING.md) 的门槛一致，细节以该文件为准）：

```sh
python scripts/check_repository.py
python -m unittest discover -s scripts/tests
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p ak-store --features sqlite
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
# 手动深测（分钟级，默认 #[ignore]）：
cargo test --release -p ak-engine -- --ignored --nocapture
cargo test --release -p ak-itests -- --ignored --nocapture
```
