# 项目状态

> 更新：2026-11-22，批次 V 复核合入，同日完成全项目审核。核心工程扎实，重心转向产品体验：批次 W 修复走查发现的僵尸连接行、黑屏与错误文案，并做第一批界面改进。

## 当前状态

批次 V 由本地 agent 在 Windows 与 Android 模拟器上执行，经 [V 复核](REVIEW_2026-11-22-batchV.zh-CN.md) 判定执行合格：

- V0：上一批的分支已清理，main 的 CI 已回填；
- V1、V3：CR-P2-08 的拆分在模拟器与 Windows 两侧确认无损，新旧版本互通，打包含 Flutter 应用且冒烟通过；
- V2：U-N1 的更正被实测证实。后台或锁屏约半分钟回来，同一任务恢复并传完；超过 QUIC 空闲上限则连接断开，用新任务码重发会续传；进程在后台确实被冻结；
- V4：Windows 包与 Android 覆盖升级、卸载走查完成；
- V5：全部门槛通过。

复核方登记了 V-N1 至 V-N6：

- V-N1：证据表被 shell 转义破坏，已修复，并加了控制字符检查；
- V-N2（P1）：长时间后台后连接已断，界面仍显示在传输；
- V-N3：一次回前台黑屏；
- V-N4：错误显示异常原文，闲置过期的文案与原因不符；
- V-N5：Android 冷启动慢，待 profile 实测；
- V-N6：打包卫生。

[全项目审核](AUDIT_2026-11-22.zh-CN.md) 的结论：安全、路径防护、直连红线与 CI 都扎实；离 v1 发布的差距主要在所有者事项、配对码过长、局域网发现没有进图形界面、界面仍是开发态，以及流程文档成本过高。审核给出了界面设计方案与分阶段路线（W、X、Y、发布候选）。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：

- 本地 agent 执行 [批次 W](HANDOFF-BATCH-W-2026-11-22.zh-CN.md)：
  - 收尾清理；
  - 修僵尸连接行、复现黑屏、错误文案映射；
  - 应用名与模板残留、打包卫生、性能实测；
  - 加入时不选方向、连接成功后直接进入发送、传输期间屏幕常亮与前台提示；
  - 可选的文档归档；
  - 全部门槛与回归走查。
- 所有者集中决定 [审核 §6](AUDIT_2026-11-22.zh-CN.md#6-需要所有者决定的事项) 的事项：Android 后台策略、移动数据、应用名与图标、平台范围、新依赖、流程瘦身，以及签名、部署、许可证、物理双机。批次 X 的界面改造依赖其中的依赖审批与图标。

M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 V 的执行方证据见 [V 交接 §8](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与结论见 [V 交接 §9](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#9-复核方证据2026-11-22)；批次 W 的证据由执行方记在 [W 交接 §8](HANDOFF-BATCH-W-2026-11-22.zh-CN.md#8-证据表执行方填写)。批次 U 的证据见 [U 交接 §8](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#9-复核方证据2026-11-21)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [W 交接（批次 W 起点，当前）](HANDOFF-BATCH-W-2026-11-22.zh-CN.md) · [V 复核（批次 V 验收与批次 W 计划）](REVIEW_2026-11-22-batchV.zh-CN.md) · [全项目审核与界面设计方案（2026-11-22）](AUDIT_2026-11-22.zh-CN.md) · [V 交接](HANDOFF-BATCH-V-2026-11-21.zh-CN.md) · [U 复核（批次 U 验收与批次 V 计划）](REVIEW_2026-11-21-batchU.zh-CN.md) · [U 交接](HANDOFF-BATCH-U-2026-11-21.zh-CN.md) · [T 复核（批次 T 验收与批次 U 计划）](REVIEW_2026-11-21-batchT.zh-CN.md) · [T 交接](HANDOFF-BATCH-T-2026-11-20.zh-CN.md) · [S 复核（批次 S 验收与批次 T 计划）](REVIEW_2026-11-20-batchS.zh-CN.md) · [S 交接](HANDOFF-BATCH-S-2026-11-20.zh-CN.md) · [R 复核（批次 R 验收与批次 S 计划）](REVIEW_2026-11-20-batchR.zh-CN.md) · [R 交接](HANDOFF-BATCH-R-2026-11-20.zh-CN.md) · [Q 复核（批次 Q 验收与批次 R 计划）](REVIEW_2026-11-20-batchQ.zh-CN.md) · [Q 交接](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 更早：[H/I 复核及 U1 清单](REVIEW_2026-11-13-batchH.zh-CN.md) · [H 交接](HANDOFF-BATCH-H-2026-11-13.zh-CN.md) · [G 复核](REVIEW_2026-11-13-batchG.zh-CN.md) · [G 交接](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [F 复核](REVIEW_2026-11-12-batchF.zh-CN.md) · [F 交接](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [审核修复轮验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [审核修复轮交接](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [方案评审（2026-11-02）](REVIEW_2026-11-02.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
