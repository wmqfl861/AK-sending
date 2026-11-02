# AK互传 · AK-sending

五端原文件互传：局域网优先，公网仅设备直连。服务器只用于发现、信令与 NAT 穿透协调，绝不承载文件数据或密文。

## 当前定稿

**开发蓝图 v0.2：轻量化、纯直连。** 已根据用户授权，对 Claude 2026-11-02 审计独立裁决 D1–D6；无需再次确认同一组设计选择。
主路线为 Flutter + Rust + quinn/rustls，独立窄信令与 STUN/ICE 检查；iroh/relay/TURN 不进入首版产品栈。端口映射默认关闭，先命令行原型，手机关键风险同步前置。

## 五个规范主入口

| 文档 | 责任 |
|---|---|
| 本 README | 项目入口和真实状态 |
| [BLUEPRINT](docs/BLUEPRINT.zh-CN.md) | 产品、架构、协议边界、平台、轻量预算与技术依据 |
| [ADR-001](docs/adr/001-direct-only.zh-CN.md) | 用户确认的无服务器文件传输硬约束 |
| [ROADMAP](docs/ROADMAP.zh-CN.md) | M0–M4/V1.1 顺序、待办和完成定义 |
| [TEST_PLAN](docs/TEST_PLAN.zh-CN.md) | 真实验证与发布门槛 |

旧拆分设计页仅为兼容索引，不再独立维护第二套方案。历史审计和发布证据保留，不计作活跃规范。
[Claude 审计原文](docs/REVIEW_2026-11-02.zh-CN.md) · [逐项采纳裁决](docs/REVIEW_DISPOSITION_2026-11-02.zh-CN.md) · [当前交接](docs/HANDOFF.zh-CN.md)。

## 当前实现状态

**M0-02：最小认证直连适配器已开发、测试并审核；整个 M0 尚未完成。** [先行计划](docs/modules/M0-02_PLAN.zh-CN.md) · [审核记录](reports/M0-02_REVIEW.zh-CN.md) · [验证索引](reports/M0-02_VALIDATION.json)。

`ak-core` 仍无第三方运行时依赖，负责接收授权领域状态机；新增独立 `ak-transport`，以原生 UDP + quinn/rustls TLS 1.3 实现手工地址的双向认证连接、完整已配对证书固定、私有接收许可及有界固定探测。连接就绪不等于接收确认，旧会话/连接许可不能复用。取消未完成探测会关闭连接。

最终受测代码 `f0daac8aa464e645483920adc4f51188891a17bb`；Linux、Windows、macOS 每系统 81 项 Rust 用例、24 项 Python 测试及 fmt/clippy/依赖特性/源码无变更检查通过。Android arm64、iOS arm64 通过最小 library `cargo check`，不是完整 App 编译、安装或真机验收。具体 CI、失败修复及独立审查 SHA 见审核记录。

**尚不能传输用户文件。** 当前网络实测为同机 loopback 上的真实 UDP/TLS，不证明双机局域网或公网可达；未实现配对 UI/密钥持久化、文件协议/磁盘保存、持久化续传、NAT 穿透、自动选路、Flutter UI、手机权限与生命周期。无中继是实现约束，尚无跨网络抓包验收。性能与轻量预算尚未实测。

PR #3 叠加在 M0-01 PR #2 上，均未自动合并 main。M0-01 历史验收保留在 [原审核](reports/M0-01_REVIEW.zh-CN.md) 中，不将旧测试证据改写为新提交结果。

## 工程检查与模块流程

```sh
cargo fetch --locked
python scripts/run_validation.py --output validation/attempt-001 --timeout 600
```

安装固定的 Rust 工具链及 Python 3.11+ 后运行；先按已提交锁文件预取依赖，再离线测试；每次选用新的输出目录。执行器保存实际命令、退出码、日志和摘要，缺失/失败的必要检查使整体失败。`check_repository.py` 仅是 L0 静态检查，不证明密码学身份或无中继。

**每模块必须先单独提交开发计划，再开发、实际测试、审核、修复复测和交接。** 计划存于 `docs/modules/`，审核存于 `reports/`；没有执行的检查不写通过，没有第二审查者参与不称独立审查。

修改前阅读 [AGENTS.md](AGENTS.md)和 HANDOFF。工作分支推进，不强推、不擅自合并/部署/采购；不提交凭据、真实文件路径和用户文件。暂未选择开源许可证。`scripts/publish.ps1` 是旧空仓库初始化工具，不能用于覆盖已有仓库。
