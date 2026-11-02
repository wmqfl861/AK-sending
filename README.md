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

现有代码只有 Rust 领域骨架：路径策略、64 位分块和任务状态机。Claude 的 [验证日志](reports/rust-validation-2026-11-02.txt)记录 23 项 Rust 测试及 clippy 修复后通过；不代表网络、手机或性能验证。
本次是文档定稿，不是新增网络实现。真实收发、密码学身份、持久化续传、NAT 穿透、Flutter UI 与五端适配仍待开发。轻量预算是目标，不是已经测得的体积和内存。

## 工程检查

```sh
python scripts/check_repository.py
python -m unittest discover -s scripts/tests
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

现有 Python 脚本只做初始化静态一致性检查，其常量输出不代表其他执行者有没有运行 Rust。M0 要用真实命令与退出码建立证据；不得把配置相等视为无中继证明。

修改前阅读 [AGENTS.md](AGENTS.md)和 HANDOFF。工作分支推进，不强推、不擅自合并/部署/采购；不提交凭据、真实文件路径和用户文件。暂未选择开源许可证。`scripts/publish.ps1` 是旧空仓库初始化工具，不能用于覆盖已有仓库。
