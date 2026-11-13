# 贡献与开发

先读 [AGENTS.md](AGENTS.md)、[README.md](README.md)、[docs/HANDOFF.zh-CN.md](docs/HANDOFF.zh-CN.md) 与 [ADR-001](docs/adr/001-direct-only.zh-CN.md)。
需求/设计变更同步更新文档；新增真实能力必须有对应测试，不将模型输入当实际网络证据。

## 工作区布局（Cargo workspace，11 个 crate）

| crate | 职责 |
|---|---|
| ak-core | 领域模型（路径策略/分块/任务），保持零依赖 |
| ak-identity | 设备密钥、指纹、配对码、配对簿、自签证书钉扎 |
| ak-wire | 端到端线协议帧（封闭消息集，含显示字符串清洗） |
| ak-storage | 落盘、.akpart 分片、摘要校验、路径/符号链接防护 |
| ak-net | QUIC 直连通道（无公开构造）、路径分类、UDP 打洞 |
| ak-discovery | mDNS 发现（设备名/配对状态/browse_with/watch） |
| ak-store | 检查点存储（文件后端；可选 sqlite 后端与迁移工具） |
| ak-signal | 协调面信令 TCP + STUN UDP（封闭消息集，无文件路由） |
| ak-engine | 端到端传输引擎（协议时序、续传、并发接收、统计） |
| ak-cli | 命令行（keygen/pair/receive/send/jobs/discover/signal-server/probe/bench/doctor/completions） |
| ak-itests | 跨 crate 场景测试（T 系列环回形态 + 产品全流程 e2e） |

## 验证门槛

每次提交前在装有 Rust 工具链与 Python 3.11+ 的环境运行：

```sh
python scripts/check_repository.py                       # L0 仓库检查（含 Cargo.lock 禁 relay/TURN 扫描）
cargo fmt --all -- --check                               # 格式化门槛（零 diff，与 CI 第一步一致）
cargo test --workspace                                   # 全部单元/集成测试
cargo clippy --workspace --all-targets -- -D warnings    # 静态检查零警告
bash scripts/clippy_unix_probe.sh                        # unix 路径 clippy 本地预警（可选，需 Android NDK；缺工具自动 SKIP；动机见 6235bec）
cargo test -p ak-store --features sqlite                 # 可选特性后端（默认依赖图不启用）
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps   # rustdoc 门槛（零警告；Windows PowerShell 用 $env:RUSTDOCFLAGS = "-D warnings"）
```

可选特性：`sqlite`（rusqlite bundled 检查点后端）默认关闭，默认依赖图零泄漏；改动 ak-store 时两配置都要过。

release 专属重载荷测试默认 `#[ignore]`，按需显式运行（不计入常规门槛）：

```sh
cargo test --release -p ak-engine --lib five_gib -- --ignored            # 5 GiB 稀疏大文件
cargo test --release -p ak-itests --test t13_many_files -- --ignored     # 20,000 / 65,537 文件边界
cargo test --release -p ak-itests --test bench_throughput -- --ignored --nocapture   # 吞吐基准
cargo test --release -p ak-itests --test bench_chunk_sweep -- --ignored --nocapture  # 块尺寸扫描
```

## 测试层级口径

按 [验收计划](docs/TEST_PLAN.zh-CN.md)：L0 文档/仓库静态检查；L1 领域模型单测；L2 真实传输集成；L3 网络故障注入；L4 五端真机与发布。
当前全部 L2 为本机环回形态（单机环回＋本机网卡，双进程真实 QUIC），不等价于跨机器/跨网段传输；L3/L4 未执行。表述测试结论时必须注明层级与形态，L0 成功不能替代 L1–L4。

## 并行开发惯例（所有权边界）

多轮并行开发的实际惯例：一个 crate 同一时间只归一个代理/开发者所有，接口先按冻结的模块契约约定，再并行实现；跨 crate 接线、依赖与冲突修复由单一集成者完成；ak-itests 场景测试可与模块开发并行推进（发现缺陷→修复→回归钉死）。新工作请先在 [HANDOFF](docs/HANDOFF.zh-CN.md) 与 [BACKLOG](docs/BACKLOG.zh-CN.md) 认领条目，避免同 crate 撞车。

## 提交纪律

证据规则以 [AGENTS.md](AGENTS.md) 为准：文档、代码、实际测试结果分别标记，禁止把设计、模拟测试或静态检查说成真机验证；完成状态必须有可复现命令与记录（reports/）支撑。
提交合成测试数据，避免真实用户文件、凭据、绝对路径和未经脱敏的抓包。当前包没有自动部署和自动发布。
许可证尚未由所有者选择，不自行假定仓库已经采用某开源许可证。
