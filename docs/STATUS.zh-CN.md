# 项目状态

> 更新：2026-11-20，批次 Q 复核（记录问题 Q-N1 已更正并新增仓库检查；复核方完成校验阶段的并行化 Q-N2）合入。

## 当前状态

批次 Q 由本地 agent 在 Windows 上执行，经 [Q 复核](REVIEW_2026-11-20-batchQ.zh-CN.md) 判定通过：

- Q1：P-N1 改正后的大文件测试在本地 Windows 上通过，与 CI 的 Windows 深测结果一致；
- Q0：上一批的分支已清理，main 的 CI 已回填；
- Q2（可选）没有做；CR-P2-08 的拆分不再列给执行方，由复核方另行安排。

记录问题与批次 O、P 同类（Q-N1），复核方已更正。`scripts/check_repository.py` 新增两项检查：STATUS 要链接全部交接与复核文档；状态表批次行与 STATUS 正文不写运行编号、计数和实测耗时。

复核方做了 BACKLOG 推荐的下一项工程（Q-N2）：校验阶段同时处理至多 8 个文件，每个文件仍是先落盘、再改名。Linux 上海量小文件任务的校验阶段明显缩短；Windows CI 上效果小。复核方新加的基准测试显示，Windows 上的主要开销来自逐个文件交替落盘与建立最终名，先把一批文件落盘、再建立它们的最终名要便宜几倍；这是下一步工程，先由批次 R 在本地 Windows 上复测。CI 上一项任务码测试在 Windows 上间歇超时（Q-N3），复核方补全了它的超时报错。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：本地 agent 执行 [批次 R](HANDOFF-BATCH-R-2026-11-20.zh-CN.md)：

- R0：清理已合入的分支，回填 main 的 CI；
- R1：在本地 Windows 上测 Q-N2 的效果；
- R2：在本地 Windows 上复跑 ak-engine 的 release 深测；
- R3：在本地 Windows 上跑每文件落盘开销的基准测试。

M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 Q 的执行方证据见 [Q 交接 §8](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [Q 交接 §9](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md#9-复核方证据2026-11-20)；批次 R 的证据由执行方记在 [R 交接 §8](HANDOFF-BATCH-R-2026-11-20.zh-CN.md#8-证据表执行方填写)。批次 P 的证据见 [P 交接 §8](HANDOFF-BATCH-P-2026-11-19.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-P-2026-11-19.zh-CN.md#9-复核方证据2026-11-19)，批次 O 的见 [O 交接 §8、§9](HANDOFF-BATCH-O-2026-11-18.zh-CN.md#9-复核方证据2026-11-19)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [R 交接（批次 R 起点，当前）](HANDOFF-BATCH-R-2026-11-20.zh-CN.md) · [Q 复核（批次 Q 验收与批次 R 计划）](REVIEW_2026-11-20-batchQ.zh-CN.md) · [Q 交接](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 更早：[H/I 复核及 U1 清单](REVIEW_2026-11-13-batchH.zh-CN.md) · [H 交接](HANDOFF-BATCH-H-2026-11-13.zh-CN.md) · [G 复核](REVIEW_2026-11-13-batchG.zh-CN.md) · [G 交接](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [F 复核](REVIEW_2026-11-12-batchF.zh-CN.md) · [F 交接](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [审核修复轮验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [审核修复轮交接](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [方案评审（2026-11-02）](REVIEW_2026-11-02.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
