# 项目状态

> 更新：2026-11-21，批次 T 复核合入。应用自己作为接收端在 Android 模拟器上可用（S-N3 的应用侧确认）；发送端源文件复核的活动信号、块阶段磁盘写满的报错与接收绑定前失败的报错口径在 Windows 上确认；新旧版本互通通过。

## 当前状态

批次 T 由本地 agent 在 Windows 与 Android 模拟器上执行，经 [T 复核](REVIEW_2026-11-21-batchT.zh-CN.md) 判定执行合格，五项都通过：

- T0：上一批的分支已清理，main 的 CI 已回填，包括第一次尝试的失败与重跑；
- T1：在应用进程里经 FFI 走真实的 QUIC 回环收发，三轮都完成，每个文件的 SHA-256 相等。提交步骤在应用自己的域里执行，那里不允许创建硬链接，所以这是 S-N3 在应用一侧的确认。这是模拟器测试，不是真机验证；
- T2：Windows 上三组新单元测试连续三轮通过；三项规模测试通过，其中 4 秒空闲时限的一项说明三种活动信号在 65,537 个文件下都起作用；
- T3：新旧版本互通，四个方向、两种模式全部通过；
- T4：全部门槛通过。

复核方在本批分支上做了以下几件事：

- 把 t06 等端口释放的重试改为截止时间（T-N1）：合入前 main 的 CI 在 Windows 上因它失败过一次，实测表明这是测试余量问题，不是批次 S 引起的；
- 把 T1 测试的版本检查改为只查能力位（T-N2）；
- 登记了 Gradle wrapper 没有固定发行包校验值的问题（T-N3）；
- 核对了较老的 Android 版本允许应用调用 S-N3 用到的系统调用。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：

- 本地 agent 执行 [批次 U](HANDOFF-BATCH-U-2026-11-21.zh-CN.md)，共五项：
  - 收尾清理并核对本机的 Gradle 发行包；
  - Android 接收方向的每批回归；
  - Android 后台、锁屏、进程被回收与强制停止的走查；
  - Windows 上连续运行 t06；
  - 全部门槛。
- 复核方在 U 复核时做 CR-P2-08 的生产代码拆分，并按走查结果决定 Android 的后台策略；之后是批次 V（发布前整理），见 [T 复核 §5](REVIEW_2026-11-21-batchT.zh-CN.md#5-余下的工作与批次-u)。

正式完成还取决于所有者事项（E6 部署、C4 物理双机、E5 签名、E7 许可证等，以及 P2-02 的移动数据授权是否进 v1），见 [R 复核 §5](REVIEW_2026-11-20-batchR.zh-CN.md#5-余下的工作与批次-s)。

M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 T 的执行方证据见 [T 交接 §8](HANDOFF-BATCH-T-2026-11-20.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [T 交接 §9](HANDOFF-BATCH-T-2026-11-20.zh-CN.md#9-复核方证据2026-11-21)；批次 U 的证据由执行方记在 [U 交接 §8](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#8-证据表执行方填写)。批次 S 的执行方证据见 [S 交接 §8](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [S 交接 §9](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#9-复核方证据2026-11-20)；批次 R 的证据见 [R 交接 §8](HANDOFF-BATCH-R-2026-11-20.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-R-2026-11-20.zh-CN.md#9-复核方证据2026-11-20)，批次 Q 的见 [Q 交接 §8、§9](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md#9-复核方证据2026-11-20)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [U 交接（批次 U 起点，当前）](HANDOFF-BATCH-U-2026-11-21.zh-CN.md) · [T 复核（批次 T 验收与批次 U 计划）](REVIEW_2026-11-21-batchT.zh-CN.md) · [T 交接](HANDOFF-BATCH-T-2026-11-20.zh-CN.md) · [S 复核（批次 S 验收与批次 T 计划）](REVIEW_2026-11-20-batchS.zh-CN.md) · [S 交接](HANDOFF-BATCH-S-2026-11-20.zh-CN.md) · [R 复核（批次 R 验收与批次 S 计划）](REVIEW_2026-11-20-batchR.zh-CN.md) · [R 交接](HANDOFF-BATCH-R-2026-11-20.zh-CN.md) · [Q 复核（批次 Q 验收与批次 R 计划）](REVIEW_2026-11-20-batchQ.zh-CN.md) · [Q 交接](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 更早：[H/I 复核及 U1 清单](REVIEW_2026-11-13-batchH.zh-CN.md) · [H 交接](HANDOFF-BATCH-H-2026-11-13.zh-CN.md) · [G 复核](REVIEW_2026-11-13-batchG.zh-CN.md) · [G 交接](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [F 复核](REVIEW_2026-11-12-batchF.zh-CN.md) · [F 交接](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [审核修复轮验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [审核修复轮交接](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [方案评审（2026-11-02）](REVIEW_2026-11-02.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
