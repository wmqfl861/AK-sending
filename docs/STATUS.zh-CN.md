# 项目状态

> 更新：2026-11-19，批次 O 复核（复核方修复 O-N1、O-N2、O-N4）合入。

## 当前状态

批次 O 由本地 agent 在 Windows 上执行，经 [O 复核](REVIEW_2026-11-19-batchO.zh-CN.md) 判定通过：

- O1：批次 N 复核方的改动在 Windows 上确认；
- O2：Windows release 深测全部通过，65,537 文件项第一次在 Windows 上通过，M6、M7 的 Windows 确认有了明文记录；
- 记录问题已由复核方更正（O-N3）。

复核方据 O2 的 Windows 数据另修了两处：

- O-N1（P1，既有缺陷）：单个大文件的整文件处理期间接收端一帧不发，处理长于发送端空闲时限时发送端放弃，而接收端照样提交。普通磁盘上几 GB 的文件就会触发。现在这类处理放到阻塞线程池上执行，接收任务照常发活动信号。O2 里 5 GiB 项的两次失败很可能也是它；
- O-N2（P2，性能）：块阶段两端的检查点写入量都随文件数平方增长，是海量小文件任务慢的主要原因。现在两端至多每秒写盘一次。

第一次快进合入 main 后，main 的 push CI 在 Windows 上有一项 CLI 集成测试失败。复核方据此又修了一处，然后再次快进合入：

- O-N4（P2，既有缺陷）：注册服务器在开始监听之前就打印就绪行，端口被占或数据库打不开时既不报错也不退出；起注册服务器的集成测试先探端口再交给子进程绑定，两步之间有竞态。现在注册服务器先监听、再打印，失败即报错退出；测试改从就绪行读端口，等待超时的报错带上子进程的 stderr。那次 CI 失败的直接原因，因为 stderr 没有留下，没能查实。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：本地 agent 执行 [批次 P](HANDOFF-BATCH-P-2026-11-19.zh-CN.md)：

- P0：清理已合入的分支，回填 main 的 CI；
- P1：Windows 上跑两项大文件 release 测试，确认 O-N1；
- P2：Windows 上重跑两个规模测试，测量 O-N2 的效果；
- P3：Windows 上的计时类测试循环；
- P4（可选）：CR-P2-08 生产代码拆分。

M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 O 的执行方证据见 [O 交接 §8](HANDOFF-BATCH-O-2026-11-18.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [O 交接 §9](HANDOFF-BATCH-O-2026-11-18.zh-CN.md#9-复核方证据2026-11-19)；批次 P 的证据由执行方记在 [P 交接 §8](HANDOFF-BATCH-P-2026-11-19.zh-CN.md#8-证据表执行方填写)。批次 N 的证据见 [N 交接 §8、§9](HANDOFF-BATCH-N-2026-11-17.zh-CN.md#9-复核方证据2026-11-18)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [P 交接（批次 P 起点，当前）](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
