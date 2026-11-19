# 项目状态

> 更新：2026-11-19，批次 P 复核（复核方改正 P-N1，记录问题 P-N2 已更正）合入。

## 当前状态

批次 P 由本地 agent 在 Windows 上执行，经 [P 复核](REVIEW_2026-11-19-batchP.zh-CN.md) 判定通过：

- P2：O-N2 的效果在 Windows 上得到确认，两个规模测试的块阶段都大幅缩短，整项耗时降到 O2 时的几分之一；
- P1：O-N1 的 5 GiB 项在 Windows 上通过；
- P3：O-N1、O-N2 的计时类测试在 Windows 上循环全部通过。

P1 里另一项大文件测试在 Windows 上每次失败。复核方查明是测试设计问题，与 O-N1 的修复无关（P-N1）：测试的 300 ms 时限也卡住了块阶段，而在 NTFS 上，测试用 `set_len` 造出的 part 文件让续传的第一次写入要先补零整段前缀。复核方改正了测试，时限不变；CI 新增只在手动触发时运行的 Windows 深测任务，它复现了失败，也确认了改正。记录问题已由复核方更正（P-N2）。

校验阶段在 Windows 上变长了，现在占海量小文件任务的大头，已登记为 BACKLOG 工程项，复核方建议作为下一项工程，需要先定方案。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：本地 agent 执行 [批次 Q](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md)：

- Q0：清理已合入的分支，回填 main 的 CI；
- Q1：在本地 Windows 上复跑改正后的大文件测试；
- Q2（可选）：CR-P2-08 生产代码拆分。

M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 P 的执行方证据见 [P 交接 §8](HANDOFF-BATCH-P-2026-11-19.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [P 交接 §9](HANDOFF-BATCH-P-2026-11-19.zh-CN.md#9-复核方证据2026-11-19)；批次 Q 的证据由执行方记在 [Q 交接 §8](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md#8-证据表执行方填写)。批次 O 的证据见 [O 交接 §8、§9](HANDOFF-BATCH-O-2026-11-18.zh-CN.md#9-复核方证据2026-11-19)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [Q 交接（批次 Q 起点，当前）](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
