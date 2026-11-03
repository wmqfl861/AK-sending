# AK互传 · AK-sending

跨 Windows、macOS、Linux、Android、iOS 的原文件互传项目。

> **架构已确定：允许服务器参与设备发现、信令和 NAT 穿透协调；绝不允许任何服务器参与文件数据传输，包括加密中继。**

目标仓库：`wmqfl861/AK-sending`。本包是 v0.1 项目初始化材料，不是五端可用软件。

## 当前真实状态

- 已整理：需求、修订蓝图、ADR-001/ADR-002、协议草案、平台与存储方案、开发路线、验收规范和交接说明。
- 2026-11-02：完成 [方案评审](docs/REVIEW_2026-11-02.zh-CN.md)；按所有者"继续本地开发"指示采纳其 D1/D5/D6 路线，见 [ADR-002](docs/adr/002-protocol-stack-and-order.zh-CN.md)。
- 2026-11-02（第二阶段，分支 `dev/local-parallel`，未推送）：完成局域网 CLI 原型全栈——10 个 crate 并行开发 + 集成：
  - 真实 QUIC 直连通道（quinn + rustls/ring，自签 Ed25519 证书双向钉扎，无任何 relay 路径）；
  - 端到端文件协议引擎（Hello/Offer/Accept/ManifestPage/Chunk/ChunkAck/ResumeRequest/Finalize/FinalAck，确定性任务 ID，窗口化确认，检查点续传，完成标记严格晚于校验与落盘）；
  - 协调面信令服务器/客户端 + 最小 STUN（封闭消息集、限频、会话 TTL，无文件路由，测试锁定）；
  - 命令行（keygen/receive/send/discover/signal-server/probe，公网直连探测工具）。
- 已实际执行并通过：workspace 242 项 Rust 测试、clippy `-D warnings`、仓库检查与 12 项 Python 测试；5 GiB 稀疏文件 release 环回传输（74 秒，u64 偏移）；真实双进程 CLI 环回传输逐字节一致。证据见 [开发验证记录](reports/dev-validation-2026-11-02.txt)。
- 2026-11-03（第四轮，同分支）：ResumeRequest 多帧分页解除单任务条目上限（65,537 文件 release 实测）、SQLite 检查点可选后端（特性 sqlite，默认依赖图零泄漏）、连接统计 API（tx/rx 字节/帧数/丢包，无中继运行时旁证）、T06 身份替换与 T08 端口变更重钉扎续传场景自动化。第四轮后 workspace 338 项测试通过（含 sqlite 特性 +23）、clippy 双配置全绿；逐项见 [实施清单](docs/BACKLOG.zh-CN.md) 第四轮节。
- 仍未完成（如实标注）：跨机器/跨网段真实传输、NAT 穿透实测、抓包级无中继证明（L3）、Flutter 五端 UI、移动原生适配、真机验收。当前全部验证为本机环回（L2 形态）。
- 历史初始化记录见 [上传验收记录](reports/GITHUB_PUBLISH_VERIFICATION.zh-CN.md)、[Rust 验证记录](reports/rust-validation-2026-11-02.txt) 和 [交接说明](docs/HANDOFF.zh-CN.md)。

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
| [交接说明](docs/HANDOFF.zh-CN.md) | 当前状态和下一执行节点 |
| [方案评审 2026-11-02](docs/REVIEW_2026-11-02.zh-CN.md) | 对 v0.1 方案的评审意见与待确认决策 |

## 本地检查

在装有 Rust 工具链及 Python 3.11+ 的环境中运行：

```sh
python scripts/check_repository.py
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

命令行原型（局域网，环回或同网段两台机器）：

```sh
cargo run -p ak-cli -- keygen            # 生成本机密钥并显示 device id/指纹/配对码（fingerprint 子命令同输出）
cargo run -p ak-cli -- pair code         # 显示本机指纹与 6 位配对码（在对端设备上配对时引用）
cargo run -p ak-cli -- pair add <64hex> --name NAME   # 配对簿：添加/改名对端
cargo run -p ak-cli -- pair list | pair remove <64hex|唯一前缀>
cargo run -p ak-cli -- receive --out DIR --port 7100 [--expect any|指纹|唯一前缀|配对码] [--once] [--conflict error|rename] [--timeout 30]  # 接收端（默认仅接受配对簿内指纹，循环接单）
cargo run -p ak-cli -- send PATH [PATH...] --to IP:7100 --fp <完整指纹|唯一前缀> [--multistream] [--conflict error|rename] [--timeout 30]  # 多路径同任务，成功后 TOFU 记入配对簿
cargo run -p ak-cli -- jobs [list] | jobs clean --all|--completed  # 检查点记录查看/清理（.akpart 分片永不删除，保持可续传）
cargo run -p ak-cli -- discover [--seconds N] [--json]  # mDNS 发现（带配对状态与设备名）
cargo run -p ak-cli -- signal-server [--tcp-port 7100] [--stun-port 7101]  # 协调服务器（信令+STUN，无文件路由）
cargo run -p ak-cli -- probe --server IP:7100 --peer <对端指纹64hex> [--no-stun]  # 公网直连探测（STUN 隐含使用 server 端口+1）
cargo run -p ak-cli -- bench [--bytes N] [--files N] [--chunk-kb N] [--multistream] [--once] [--json]  # 进程内环回吞吐基准（默认单流 vs 多流各一轮并对比）
cargo run -p ak-cli -- doctor            # 环境诊断（开发中）
```

首次初始化空仓库可使用 [发布说明](docs/PUBLISHING.zh-CN.md) 及 `scripts/publish.ps1`；脚本不包含凭据、不强推，也不处理已有提交的仓库。

## 开发规则

阅读 [AGENTS.md](AGENTS.md) 后再修改。完成状态必须有证据；未实现和未测试事项保留为未完成。
未经项目所有者选择，不擅自添加开源许可证。受控依赖新增须经 ADR-002 复核（当前：quinn/rustls(ring)/ed25519-dalek/blake3/rcgen/mdns-sd/if-addrs/tokio/clap/fs2/filetime 等，全部无 relay/TURN 类）。
