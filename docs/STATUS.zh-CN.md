# 项目状态

> 更新：2026-11-17，批次 M（复核方执行）合入。

## 当前状态

批次 L 经 [L 复核](REVIEW_2026-11-17-batchL.zh-CN.md) 验收通过并合入。复核中发现一个早已存在的 P2（L-N3）：TLS 帧写入后没有 flush，空闲连接上的帧可能一直发不出去。

批次 M 由复核方执行，并由复核方在批次 M 最终文档提交的 CI 全绿后快进合入 main，内容见 [M 记录](HANDOFF-BATCH-M-2026-11-17.zh-CN.md)：

- M1：检查脚本检查状态表的每一行，并拦下表外行；
- M2：修复 L-N3，帧写入后 flush，附回归测试；
- M3：补齐 netem 证据，实际获得的 UDP 缓冲进入日志；
- M4：三个测试模块移到单独文件，已证明是纯移动；
- M5：首次在 CI 上运行 release 深测（Linux），结果见 M 记录 §3；
- M6：修复深测揭出的多流接收流控死锁（P2），失败时显式关闭连接；
- M7：被取消的多流接收不再让发送方空等（P3），同时改正中断续传测试里的过时断言。

深测还揭出一个新的既有缺陷 M-N1（P1）：接收端为每个文件各开一个文件句柄并一直持有，文件数超过进程的打开文件上限就会失败。在 Linux 常见的 1024 上限下，2,000 个文件就会失败；Windows 上测不出来。修复列为批次 N 的 N4。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：本地 agent 执行 [批次 N](HANDOFF-BATCH-N-2026-11-17.zh-CN.md)：

- N0：清理已合入的分支，回填 main 的 CI；
- N1：在 Windows 上反复运行，确认 M2；
- N2：Windows release 深测；
- N3（可选）：CR-P2-08 生产代码拆分；
- N4：修复 M-N1。

M2 的修复要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 M 的命令、运行编号与计数见 [M 记录 §3](HANDOFF-BATCH-M-2026-11-17.zh-CN.md#3-证据表)。批次 L 的执行方证据见 [L 交接 §8](HANDOFF-BATCH-L-2026-11-16.zh-CN.md#8-证据表执行方填写)，复核方证据见 [L 交接 §9](HANDOFF-BATCH-L-2026-11-16.zh-CN.md#9-复核方证据2026-11-17)。批次 N 的证据由执行方记在 [N 交接 §8](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#8-证据表执行方填写)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [N 交接（批次 N 起点，当前）](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
