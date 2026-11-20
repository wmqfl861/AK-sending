# 项目状态

> 更新：2026-11-21，批次 S 复核合入，含执行方合入后补记的 S1 复测。Android 接收不可用的缺陷 S-N3 已修，模拟器上的 CLI 接收复测通过；发送端源文件复核的活动信号与块阶段磁盘写满的报错已实现。

## 当前状态

批次 S 由本地 agent 在 Windows 与 Android 模拟器上执行，经 [S 复核](REVIEW_2026-11-20-batchS.zh-CN.md) 判定执行合格：

- S0：上一批的分支已清理，main 的 CI 已回填；
- S1：在 Android 模拟器上首次失败，找出一个 P1 产品缺陷。接收端用硬链接建立最终名，而 Android 不允许普通应用创建硬链接，所以自 2026-11-04 起 Android 收不了文件。复核方改用不依赖硬链接的原子改名（S-N3）后，执行方在模拟器上用 CLI 复测通过，提交走的是新的原子改名。这是模拟器测试，不是真机验证；应用自己作为接收端还没有测过；
- S2：Windows release 打包与包内 ak-cli 的回环冒烟通过；
- S3：全部门槛通过。

复核方在本批分支上还实现了发送端源文件复核期间的活动信号（S-N1，新能力位，旧端行为不变）与块阶段磁盘写满时报 STORAGE_FULL（S-N2），修了接收绑定前失败时的报错口径（S-N4），放宽了配对端到端测试的窗口（S-N6），并加了新旧版本互通检查脚本。执行方第一次报完成时还没推送（S-N5）；合入之后又向已合入的分支推了一次记录，复核方核对后再次合入，T 交接写明此后这类记录写进下一批。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：

- 本地 agent 执行 [批次 T](HANDOFF-BATCH-T-2026-11-20.zh-CN.md)：收尾清理；在模拟器上的应用进程里经 FFI 做一次真实的回环收发，确认应用自己作为接收端可用；Windows 上运行新测试与规模测试；新旧版本互通；全部门槛；
- 之后是批次 U（生产代码拆分、Android 后台与生命周期走查）与批次 V（发布前整理），见 [S 复核 §5](REVIEW_2026-11-20-batchS.zh-CN.md#5-余下的工作与批次-t)。

正式完成还取决于所有者事项（E6 部署、C4 物理双机、E5 签名、E7 许可证等），见 [R 复核 §5](REVIEW_2026-11-20-batchR.zh-CN.md#5-余下的工作与批次-s)。

M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 S 的执行方证据见 [S 交接 §8](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [S 交接 §9](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#9-复核方证据2026-11-20)；批次 T 的证据由执行方记在 [T 交接 §8](HANDOFF-BATCH-T-2026-11-20.zh-CN.md#8-证据表执行方填写)。批次 R 的执行方证据见 [R 交接 §8](HANDOFF-BATCH-R-2026-11-20.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-R-2026-11-20.zh-CN.md#9-复核方证据2026-11-20)，批次 Q 的见 [Q 交接 §8](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md#9-复核方证据2026-11-20)，批次 P 的见 [P 交接 §8、§9](HANDOFF-BATCH-P-2026-11-19.zh-CN.md#9-复核方证据2026-11-19)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [T 交接（批次 T 起点，当前）](HANDOFF-BATCH-T-2026-11-20.zh-CN.md) · [S 复核（批次 S 验收与批次 T 计划）](REVIEW_2026-11-20-batchS.zh-CN.md) · [S 交接](HANDOFF-BATCH-S-2026-11-20.zh-CN.md) · [R 复核（批次 R 验收与批次 S 计划）](REVIEW_2026-11-20-batchR.zh-CN.md) · [R 交接](HANDOFF-BATCH-R-2026-11-20.zh-CN.md) · [Q 复核（批次 Q 验收与批次 R 计划）](REVIEW_2026-11-20-batchQ.zh-CN.md) · [Q 交接](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 更早：[H/I 复核及 U1 清单](REVIEW_2026-11-13-batchH.zh-CN.md) · [H 交接](HANDOFF-BATCH-H-2026-11-13.zh-CN.md) · [G 复核](REVIEW_2026-11-13-batchG.zh-CN.md) · [G 交接](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [F 复核](REVIEW_2026-11-12-batchF.zh-CN.md) · [F 交接](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [审核修复轮验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [审核修复轮交接](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [方案评审（2026-11-02）](REVIEW_2026-11-02.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
