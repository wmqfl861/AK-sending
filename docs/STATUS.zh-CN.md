# 项目状态

> 更新：2026-11-17，批次 L（L0 清理+CI 回填、L1 口径验证、L2 拆分、L3 UDP 缓冲）。

## 当前状态

批次 K 经 [K24 复核](REVIEW_2026-11-16-batchK24.zh-CN.md) 验收通过。K23-N1 至 N5 由复核方修复。同时新增注册中心身份连续性守卫，包括只读子命令 `ak-cli registry-id` 和部署脚本的 `--expect-pin` / `--new-identity` 两个选项；部署脚本的阻断口径从本轮起以 K24 §5 为准。批次 K 已由复核方快进合入 main，合入记录见 K24 §8。逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)，执行与复核证据见 [K 交接 §53](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#53-第-24-轮复核方修复与验收证据2026-11-16)。

批次 L 各项本地完成、推送后 CI 待验。L0 已删远端/本地旧分支并回填 §8 CI；L1 部署脚本自检+205 回归+全量 Python 363/363 通过、无 K24 §5 阻断内新发现；L2 ak-engine 测试模块拆分（纯移动，测试数不变）；L3 QUIC UDP 缓冲调大+netem sysctl 升高（A/B 对照须 CI）。下一步：CI 绿后交复核。生产部署（E6）由所有者执行。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 K 的命令、运行编号、计数、对应提交与证据层级统一见 [K 交接 §53](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#53-第-24-轮复核方修复与验收证据2026-11-16)；批次 L 的证据记在 [L 交接 §8](HANDOFF-BATCH-L-2026-11-16.zh-CN.md#8-证据表执行方填写)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [L 交接（批次 L 起点，当前）](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
