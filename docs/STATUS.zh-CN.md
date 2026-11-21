# 项目状态

> 更新：2026-11-21，批次 U（批次 T 改动的确认、Gradle 发行包核对、Android 后台与生命周期走查）执行完毕，待复核。

## 当前状态

批次 U 由本地 agent 在 Windows 与 Android 模拟器上执行完毕，等待复核：

- U0：上一批的分支已清理，main 的 CI 已回填；本机 Gradle 发行包与官方校验值一致（T-N3 的疑虑排除，复核方下一步把校验值写进 wrapper）；
- U1：Android 接收方向每批回归一轮通过，T-N2 的版本检查改动随本轮确认；
- U2：后台与生命周期走查完成（模拟器、debug 构建）。四种情形一致：切后台、锁屏、系统回收、强制停止都会让传输停住，且同一任务内回前台不恢复；中断后用新任务码加保留检查点重发，两端一致确认是续传而不是从头开始；另做了一轮无干扰对照，完整传完且逐字节一致——走查记录与后台策略建议交复核方；
- U3：整份 t06 连续运行全部通过，T-N1 的截止时间等待稳定；
- U4：全量门槛跑完，新增走查 runner 的 flutter analyze 干净。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：复核方复核批次 U（含 CR-P2-08 生产代码拆分、Gradle 校验值写入 wrapper、按走查结果决定 Android 后台策略）；之后批次 V 做发布前整理，见 [U 交接 §3.2](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#32-工程项)。

正式完成还取决于所有者事项（E6 部署、C4 物理双机、E5 签名、E7 许可证等）。M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 U 的执行方证据见 [U 交接 §8](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#8-证据表执行方填写)。批次 T 的执行方证据见 [T 交接 §8](HANDOFF-BATCH-T-2026-11-20.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [T 交接 §9](HANDOFF-BATCH-T-2026-11-20.zh-CN.md#9-复核方证据2026-11-21)；批次 S 的证据见 [S 交接 §8](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#9-复核方证据2026-11-20)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [U 交接（批次 U，当前）](HANDOFF-BATCH-U-2026-11-21.zh-CN.md) · [T 复核（批次 T 验收与批次 U 计划）](REVIEW_2026-11-21-batchT.zh-CN.md) · [T 交接](HANDOFF-BATCH-T-2026-11-20.zh-CN.md) · [S 复核（批次 S 验收与批次 T 计划）](REVIEW_2026-11-20-batchS.zh-CN.md) · [S 交接](HANDOFF-BATCH-S-2026-11-20.zh-CN.md) · [R 复核（批次 R 验收与批次 S 计划）](REVIEW_2026-11-20-batchR.zh-CN.md) · [R 交接](HANDOFF-BATCH-R-2026-11-20.zh-CN.md) · [Q 复核（批次 Q 验收与批次 R 计划）](REVIEW_2026-11-20-batchQ.zh-CN.md) · [Q 交接](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 更早：[H/I 复核及 U1 清单](REVIEW_2026-11-13-batchH.zh-CN.md) · [H 交接](HANDOFF-BATCH-H-2026-11-13.zh-CN.md) · [G 复核](REVIEW_2026-11-13-batchG.zh-CN.md) · [G 交接](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [F 复核](REVIEW_2026-11-12-batchF.zh-CN.md) · [F 交接](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [审核修复轮验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [审核修复轮交接](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [方案评审（2026-11-02）](REVIEW_2026-11-02.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
