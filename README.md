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

**M0工程已推进到M0-07：独立两端LAN原生探针、五端操作入口、设备证据工具均已实现并通过工程审核。完整M0验收仍为PARTIAL，剩余实机项目没有豁免。** [M0总表](reports/M0_STATUS.zh-CN.md) · [最新审核](reports/M0-07_REVIEW.zh-CN.md) · [验证索引](reports/M0-07_VALIDATION.json)。

`ak-core`仍无第三方运行时依赖；`ak-transport`提供原生UDP/quinn/rustls双向证书固定认证、私有接收许可、有界协议、TLS/task/sequence绑定与有界端点排空。独立LAN实验只传两块固定合成数据，不读取用户文件、不使用任何文件中继。

最终受测且独立复审代码 `253cc6c601253170e6847f59aef3c8bbd0c03d5f`：三桌面每系统110项核心Rust、16项native Rust通过；Linux/macOS61项Python通过，Windows58通过+3项Unix专用跳过；五个Flutter构建任务各20项Dart主机测试；两个Apple主机各6项Swift文件回归。

Windows/Linux x64、macOS/Android/iOS arm64 release构建通过；三桌面实际Dart FFI、两独立进程和Linux两个无默认路由网络栈正负例通过。新LAN页面支持手动地址、完整摘要核对、本机显式确认、权限回调失效和取消释放。构建不是五端实机运行，namespace不是Wi-Fi。

**尚不能互传用户文件，完整M0也尚未验收。** 真实手机权限、目录持久授权/撤销、后台/锁屏/杀进程、断外网Wi-Fi、最低系统与资源预算仍NOT_RUN。[实机操作手册](experiments/flutter_probe/LAN_DEVICE_RUNBOOK.zh-CN.md) · [设备矩阵](experiments/flutter_probe/DEVICE_MATRIX.zh-CN.md)。用户文件引擎/可靠保存/续传属于M1，公网NAT与自动选路属于后续阶段。

最新工作分支 `dev/m0-07-device-lan-harness-20261102` / 草稿PR #8；M0-06在草稿PR #7。实机缺口跟踪Issue #9。未合并main、不部署、不采购、不强推。Android用测试签名，iOS未签名；极小实验产物大小不代表完整产品性能预算。

## 工程检查与模块流程

```sh
cargo fetch --locked
python scripts/run_validation.py --output validation/attempt-001 --timeout 600
```

安装固定的 Rust 工具链及 Python 3.11+ 后运行；先按已提交锁文件预取依赖，再离线测试；每次选用新的输出目录。执行器保存实际命令、退出码、日志和摘要，缺失/失败的必要检查使整体失败。`check_repository.py` 仅是 L0 静态检查，不证明密码学身份或无中继。

**每模块必须先单独提交开发计划，再开发、实际测试、审核、修复复测和交接。** 计划存于 `docs/modules/`，审核存于 `reports/`；没有执行的检查不写通过，没有第二审查者参与不称独立审查。

修改前阅读 [AGENTS.md](AGENTS.md)和 HANDOFF。工作分支推进，不强推、不擅自合并/部署/采购；不提交凭据、真实文件路径和用户文件。暂未选择开源许可证。`scripts/publish.ps1` 是旧空仓库初始化工具，不能用于覆盖已有仓库。
