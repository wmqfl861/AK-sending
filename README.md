# AK互传 · AK-sending

跨 Windows、macOS、Linux、Android、iOS 的原文件互传项目。

> **架构已确定：允许服务器参与设备发现、信令和 NAT 穿透协调；绝不允许任何服务器参与文件数据传输，包括加密中继。**

**v1 极简交互（wave-28 起，提交 `7458e9f`）**：一台设备生成一个随机任务码，另一台输入该码，即完成一次配对——**一个码＝一次传输任务、恰好双方、任一侧可发起**；码按需生成、自带创建方连接设置、断开即失效；接收方对每个入站请求**逐一同意**后才落盘。全程**零配置、零 IP、零端口、零指纹**：GUI 只剩"连接＋设置"两页，唯一的设置项是默认保存文件夹。

目标仓库：`wmqfl861/AK-sending`。

## 当前状态

**唯一来源：[docs/STATUS.zh-CN.md](docs/STATUS.zh-CN.md)**（2026-11-11）——审核修复阶段与批次进度、生产服务器形态、客户端交付状态、最新 CI 验证与已知边界。本 README 不再复述波次细节与逐轮验证计数。

历史：里程碑见 [CHANGELOG](CHANGELOG.md)；逐轮任务与证据见 [实施清单](docs/BACKLOG.zh-CN.md)、[验证记录与审核报告](reports/) 和 [交接说明](docs/HANDOFF.zh-CN.md)。

## 产品原则

可达局域网优先；公网只直连；直连失败暂停/重试，不启用中继。
不压缩、不转码、不重新编码；不设置商业限速或应用级文件大小、类型、总数量额度。
支持默认保存位置、单次改选、文件夹、完整性校验和恢复；移动端遵守系统授权和生命周期。

“服务器不传文件”包括自建与第三方服务。文件内容、分块、缩略图、清单、路径及内容摘要都不放入协调信令。
普通路由器和 NAT 是网络基础设施；产品不使用应用层转发节点、VPN 中继或 HTTP 上传作为替代路线。
无法仅凭应用代码证明用户网络中不存在透明代理，产品承诺边界详见 ADR-001。

## 文档入口

| 文档 | 用途 |
|---|---|
| [项目蓝图](docs/BLUEPRINT.zh-CN.md) | 完整修订方案和阶段边界 |
| [产品需求](docs/PRD.zh-CN.md) | 用户需求、范围和验收映射 |
| [ADR-001](docs/adr/001-direct-only.zh-CN.md) | 无服务器文件传输的最高优先级约束 |
| [ADR-002](docs/adr/002-protocol-stack-and-order.zh-CN.md) | QUIC+自建信令路线与先局域网原型顺序 |
| [系统架构](docs/ARCHITECTURE.zh-CN.md) | 控制面/数据面和模块责任 |
| [开发路线](docs/ROADMAP.zh-CN.md) | P0–P4 阶段门槛 |
| [实施清单](docs/BACKLOG.zh-CN.md) | 可分配任务和依赖 |
| [协议草案](docs/PROTOCOL.zh-CN.md) | 协调信息与端对端文件协议 |
| [存储与平台](docs/STORAGE_AND_PLATFORMS.zh-CN.md) | 目录授权、原件、后台和续传 |
| [安全模型](docs/SECURITY.zh-CN.md) | 配对、威胁模型与无中继证据 |
| [验收计划](docs/TEST_PLAN.zh-CN.md) | 真机/网络/大文件/无中继测试 |
| [技术选型](docs/TECH_SELECTION.zh-CN.md) | 候选方案和必须验证的风险 |
| [项目状态](docs/STATUS.zh-CN.md) | 当前状态唯一来源（阶段/服务器/交付/CI） |
| [交接说明](docs/HANDOFF.zh-CN.md) | 逐波交接记录与重启恢复手册 |
| [方案评审 2026-11-02](docs/REVIEW_2026-11-02.zh-CN.md) | 对 v0.1 方案的评审意见与待确认决策 |

## 本地检查

在装有 Rust 工具链及 Python 3.11+ 的环境中运行：

```sh
python scripts/check_repository.py
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## v1 极简交互（GUI 主线路径，wave-28 起）

普通用户的全部操作（Android/Windows GUI，Linux GUI 待构建）：

1. **生成任务码**：连接页点"生成码"并选择本机角色（发送方或接收方）——码按需生成（600 秒有效、仅可兑换一次），显示为 `AKI1-XXXX-…-XXXX` 完整格式，复制给对端；
2. **输入任务码**：对端在连接页输入该码，双方经部署的注册服务器（122.51.121.24）自动交换连接设置并完成配对——全程无需输入 IP、端口或指纹；任一侧断开，码即失效；
3. **收发**：配对成功后连接出现在列表中，进入详情用系统文件选择器挑文件或文件夹发送；接收方对每个入站请求先看到横幅，**点"接受"后才开始落盘**（拒绝则零写入）；
4. **设置**：仅一项——默认保存文件夹。

**两条产品路径（如实标注）**：v1 一次性任务码模型为 GUI 与 CLI 双入口——GUI 走 `ak_task_*` FFI（→Dart→Flutter），CLI 走 `ak-cli task` 命令族（第三十二波 `0cf1382` 起，不依赖 ak-ffi，Linux 的"GUI"即终端）；下述高级用法中的旧邀请码/配对簿多码模型保留为 CLI 高级能力，与 v1 并存。

## 高级 CLI 用法（旧邀请码/配对簿多码模型与运维工具）

以下命令行能力全部保留，适用于自动化、服务器运维与高级场景；其中 `invite`/`daemon --pairing` 属旧"多码调度"模型，与 v1 一次性任务码并存为两条产品路径（v1 的 CLI 入口是 `ak-cli task` 命令族，见上一节）：

```sh
cargo run -p ak-cli -- keygen            # 生成本机密钥并显示 device id/指纹/配对码（fingerprint 子命令同输出）
cargo run -p ak-cli -- pair code         # 显示本机指纹与 6 位配对码（在对端设备上配对时引用）
cargo run -p ak-cli -- pair add <64hex> --name NAME   # 配对簿：添加/改名对端
cargo run -p ak-cli -- pair list | pair remove <64hex|唯一前缀> | pair export | pair import "<AKPAIR1文本>"
cargo run -p ak-cli -- receive --out DIR --port 7100 [--bind IP|IP:PORT] [--expect any|指纹|唯一前缀|配对码] [--once] [--jobs N] [--no-internet] [--conflict error|rename] [--timeout 30]  # 接收端（默认仅接受配对簿内指纹，循环接单；--jobs N 为并发接单模式）
cargo run -p ak-cli -- send PATH [PATH...] --to IP:7100 --fp <完整指纹|唯一前缀> [--port 0] [--multistream] [--no-internet] [--conflict error|rename] [--timeout 30]  # 多路径同任务，成功后 TOFU 记入配对簿
cargo run -p ak-cli -- jobs [list] | jobs status <id> | jobs clean --all|--completed  # 检查点记录查看/清理（.akpart 分片永不删除，保持可续传）
cargo run -p ak-cli -- discover [--seconds N] [--json]  # mDNS 发现（带配对状态与设备名）
cargo run -p ak-cli -- signal-server [--tcp-port 7100] [--stun-port 7101] [--bind IP|IP:PORT] [--stun-bind IP|IP:PORT]  # 协调服务器（信令+STUN，无文件路由）
cargo run -p ak-cli -- registry-server [--port 7200] [--bind IP|IP:PORT] [--db-path PATH]  # v2 权威邀请码注册服务（TLS 1.3＋SQLite；启动打印 registry id 供对端 --registry-fp 钉扎；无文件路由）
cargo run -p ak-cli -- invite create [--ttl-secs 600 | --long-lived] --server IP:7200 --registry-fp <64hex>  # 生成并登记配对邀请码（输出 AKI1-...；bearer 凭证，像密码一样保管；--server/--registry-fp 首次成对提供后持久化到 registry.pin）
cargo run -p ak-cli -- invite list | invite revoke <code-id-32hex>  # 列出/撤销本机邀请码（list 只显示状态/到期/掩码，不输出明文码）
cargo run -p ak-cli -- invite redeem AKI1-... --out DIR [--once] [--conflict error|rename]  # 兑换邀请码：与 owner 完成配对证明并落簿，随后按簿准入接收
cargo run -p ak-cli -- daemon --pairing [--invite-code AKI1-...] [--same-code serial|parallel] [--cross-code serial|parallel] --server IP:7200 --registry-fp <64hex>  # 常驻 daemon：owner 侧自动应答兑换＋两维并发策略（码本身即授权，owner 需在线）
cargo run -p ak-cli -- probe --server IP:7100 --peer <对端指纹64hex> [--no-stun] [--dual-stack] [--json]  # 公网直连探测（STUN 隐含使用 server 端口+1；--json 输出机器可读结果）
cargo run -p ak-cli -- bench [--bytes N] [--files N] [--chunk-kb N] [--multistream] [--once] [--json]  # 进程内环回吞吐基准（默认单流 vs 多流各一轮并对比）
cargo run -p ak-cli -- doctor [--stun IP:PORT] [--mdns-seconds N] [--json]  # 环境自检（身份/网络/UDP 绑定/mDNS/可选 STUN/磁盘余量）
cargo run -p ak-cli -- completions bash|zsh|fish|powershell  # 生成 shell 补全脚本（stdout，自行管道到补全目录）
```

安卓端（先产引擎 so 再打包 APK；前置、目录布局与故障排查见 [Android 交叉编译指南](docs/ANDROID_BUILD.zh-CN.md)）：

```sh
bash scripts/build_android_ffi.sh   # 交叉编译 x86_64+arm64 双 ABI libak_ffi.so 放入 jniLibs（含 llvm-nm 符号验证）
cd flutter_app && flutter build apk --debug
```

首次初始化空仓库可使用 [发布说明](docs/PUBLISHING.zh-CN.md) 及 `scripts/publish.ps1`；脚本不包含凭据、不强推，也不处理已有提交的仓库。

## 开发规则

阅读 [AGENTS.md](AGENTS.md) 后再修改。完成状态必须有证据；未实现和未测试事项保留为未完成。
未经项目所有者选择，不擅自添加开源许可证。受控依赖新增须经 ADR-002 复核（当前：quinn/rustls(ring)/ed25519-dalek/blake3/rcgen/mdns-sd/if-addrs/tokio/clap/fs2/filetime 等，全部无 relay/TURN 类）。
