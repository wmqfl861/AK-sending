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

**M0总状态为PARTIAL：M0-01至M0-04已完成各自范围的审核；M0-05工程子范围已审核并完成五端release构建，物理设备验收仍未完成。** [M0总表](reports/M0_STATUS.zh-CN.md) · [最新审核](reports/M0-05_REVIEW.zh-CN.md) · [验证索引](reports/M0-05_VALIDATION.json)。

`ak-core`无第三方运行时依赖；`ak-transport`提供原生UDP/quinn/rustls双向认证、完整配对证书固定、私有接收许可、68字节有界协议头与16KiB合成帧、TLS/task/sequence绑定及取消/正常收尾。文件数据中继仍被禁止。

独立的原生和Flutter实验壳已接通整数句柄FFI、低频状态、取消/释放与手机目录权限实验入口。最终受测且独立复审代码 `66555214e40e779013c07cb0bda3d6dcf6bd5332`：三桌面每系统108项生产Rust、6项native Rust通过；Linux/macOS 45项Python通过，Windows43项通过+2项Unix专用测试跳过；五个构建任务各7项Dart主机测试通过。三桌面运行真实Dart FFI，同机UDP/TLS成功。两Apple主机任务执行6项Swift真实文件回归。

Windows/Linux x64、macOS/Android/iOS arm64的Flutter release构建已通过；Android用测试密钥签名，iOS未签名。构建与链接不等于实机可用或商店发布。资源记录只有极小实验壳的产物字节，未证明完整产品性能预算。

**尚不能互传用户文件。** 当前实验固定loopback；独立两设备LAN实验驱动尚未接入。真实手机网络权限、目录持久授权/撤销、后台/锁屏/杀进程、两设备断外网Wi-Fi、最低系统与CPU/内存/启动预算未验收。没有生产文件引擎、可靠保存、持久续传、配对UI/密钥生命周期、NAT、自动LAN优先选路或完整产品GUI。

最新工作分支 `dev/m0-05-flutter-risk-harness-20261102` / 草稿PR #6。所有PR仍未合并main，不部署、不采购、不强推。历史模块证据保留，不改写成当前提交的测试结果。

## 工程检查与模块流程

```sh
cargo fetch --locked
python scripts/run_validation.py --output validation/attempt-001 --timeout 600
```

安装固定的 Rust 工具链及 Python 3.11+ 后运行；先按已提交锁文件预取依赖，再离线测试；每次选用新的输出目录。执行器保存实际命令、退出码、日志和摘要，缺失/失败的必要检查使整体失败。`check_repository.py` 仅是 L0 静态检查，不证明密码学身份或无中继。

**每模块必须先单独提交开发计划，再开发、实际测试、审核、修复复测和交接。** 计划存于 `docs/modules/`，审核存于 `reports/`；没有执行的检查不写通过，没有第二审查者参与不称独立审查。

修改前阅读 [AGENTS.md](AGENTS.md)和 HANDOFF。工作分支推进，不强推、不擅自合并/部署/采购；不提交凭据、真实文件路径和用户文件。暂未选择开源许可证。`scripts/publish.ps1` 是旧空仓库初始化工具，不能用于覆盖已有仓库。
