# 项目状态

> 更新：2026-11-21，批次 U 复核合入。CR-P2-08 的生产代码拆分完成（纯移动，有记号级证明）；Gradle 发行包的官方校验值已写进 wrapper；Android 后台与生命周期在模拟器上走查过，U2 的结论由复核方更正，后台传输策略等所有者决定。

## 当前状态

批次 U 由本地 agent 在 Windows 与 Android 模拟器上执行，经 [U 复核](REVIEW_2026-11-21-batchU.zh-CN.md) 判定执行合格：

- U0：上一批的分支已清理，main 的 CI 已回填；本机的 Gradle 发行包与官方校验值一致；
- U1：Android 接收方向的每批回归通过，T-N2 改过的版本检查在设备上得到确认；
- U2：后台与生命周期走查在模拟器上完成（debug 构建）。切到后台或锁屏，传输几秒内就停；进程被回收或强制停止后，对同一个文件重发会续传；无干扰时完整传完、逐字节一致。执行方原写“回到前台后不恢复”，复核方按同一记录里的数字更正为：回到前台后在同一任务里继续（U-N1），由批次 V 复测确认；
- U3：Windows 上 t06 连续运行全部通过，T-N1 稳定；
- U4：全部门槛通过。

复核方在本批分支上：

- 把 Gradle 的官方校验值写进 wrapper，T-N3 关闭，此后每次 CI 都会核对；
- 完成 CR-P2-08 的生产代码拆分：四个大文件拆成子模块，纯移动，公开路径不变，用 `scripts/tokcmp` 新增的比对模式证明；
- 把 Android 的后台传输策略交所有者决定（U-N2）。复核方建议 v1 只承诺前台传输，加界面提示与文档；另一选择是 v1 加前台服务。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：

- 本地 agent 执行 [批次 V](HANDOFF-BATCH-V-2026-11-21.zh-CN.md)，共六项：
  - 收尾清理；
  - Android 接收方向的每批回归；
  - U2 的聚焦复测；
  - 在 Windows 上确认拆分并做打包冒烟；
  - 安装、覆盖升级与卸载走查；
  - 全部门槛。
- 复核方在 V 复核时按所有者对 U-N2 的决定处理 Android 的后台策略，写发布说明，刷新 ROADMAP、支持矩阵与 BACKLOG，见 [U 复核 §5](REVIEW_2026-11-21-batchU.zh-CN.md#5-余下的工作与批次-v)。

正式完成还取决于所有者事项：U-N2 的后台策略、P2-02 的移动数据授权是否进 v1，以及 E6 部署、C4 物理双机、E5 签名、E7 许可证等，见 [R 复核 §5](REVIEW_2026-11-20-batchR.zh-CN.md#5-余下的工作与批次-s)。

M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 U 的执行方证据见 [U 交接 §8](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [U 交接 §9](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#9-复核方证据2026-11-21)；批次 V 的证据由执行方记在 [V 交接 §8](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#8-证据表执行方填写)。批次 T 的执行方证据见 [T 交接 §8](HANDOFF-BATCH-T-2026-11-20.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [T 交接 §9](HANDOFF-BATCH-T-2026-11-20.zh-CN.md#9-复核方证据2026-11-21)；批次 S 的证据见 [S 交接 §8](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-S-2026-11-20.zh-CN.md#9-复核方证据2026-11-20)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [V 交接（批次 V 起点，当前）](HANDOFF-BATCH-V-2026-11-21.zh-CN.md) · [U 复核（批次 U 验收与批次 V 计划）](REVIEW_2026-11-21-batchU.zh-CN.md) · [U 交接](HANDOFF-BATCH-U-2026-11-21.zh-CN.md) · [T 复核（批次 T 验收与批次 U 计划）](REVIEW_2026-11-21-batchT.zh-CN.md) · [T 交接](HANDOFF-BATCH-T-2026-11-20.zh-CN.md) · [S 复核（批次 S 验收与批次 T 计划）](REVIEW_2026-11-20-batchS.zh-CN.md) · [S 交接](HANDOFF-BATCH-S-2026-11-20.zh-CN.md) · [R 复核（批次 R 验收与批次 S 计划）](REVIEW_2026-11-20-batchR.zh-CN.md) · [R 交接](HANDOFF-BATCH-R-2026-11-20.zh-CN.md) · [Q 复核（批次 Q 验收与批次 R 计划）](REVIEW_2026-11-20-batchQ.zh-CN.md) · [Q 交接](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 更早：[H/I 复核及 U1 清单](REVIEW_2026-11-13-batchH.zh-CN.md) · [H 交接](HANDOFF-BATCH-H-2026-11-13.zh-CN.md) · [G 复核](REVIEW_2026-11-13-batchG.zh-CN.md) · [G 交接](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [F 复核](REVIEW_2026-11-12-batchF.zh-CN.md) · [F 交接](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [审核修复轮验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [审核修复轮交接](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [方案评审（2026-11-02）](REVIEW_2026-11-02.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
