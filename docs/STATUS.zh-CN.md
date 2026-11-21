# 项目状态

> 更新：2026-11-21，批次 V（拆分确认、U2 聚焦复测、安装升级卸载走查）执行完毕，待复核。

## 当前状态

批次 V 由本地 agent 在 Windows 与 Android 模拟器上执行完毕，等待复核：

- V0：上一批的分支已清理，main 的 CI 已回填；
- V1：拆分后重建的 Android 库哈希按预期改变，应用域接收回归通过——CR-P2-08 的拆分在模拟器侧无损；
- V2：U2 聚焦复测（观察窗放宽后）给出完整答案：三十秒级后台/锁屏中断（进程被冻结，dumpsys 摄到 isFrozen=true）回前台后同一任务恢复并传完；两分钟级中断（超过 QUIC 空闲上限）连接在后台断开，界面一次黑屏加僵尸行、一次明确报错；断开后用新任务码加重发为续传并传完，两端 SHA-256 相等——U-N1 的更正得到实测确认，黑屏与僵尸行交复核方；
- V3：三行规模测试的阶段耗时与拆分前同量级，新旧版本互通八项全部通过，打包含 Flutter 应用且冒烟通过——拆分在 Windows 侧无损；
- V4：Windows 包桌面应用正常启动关闭、包外仅一处启动日志；Android 同签名覆盖升级数据保留、无崩溃、卸载干净；
- V5：全量门槛通过。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：复核方复核批次 V（按所有者对 U-N2 的决定处理 Android 后台策略、写发布说明、刷新文档），见 [V 交接 §3.2](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#32-工程项)。

正式完成还取决于所有者事项（U-N2 后台策略、P2-02、E6 部署、C4 物理双机、E5 签名、E7 许可证等）。M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。

## 最新验证

批次 V 的执行方证据见 [V 交接 §8](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#8-证据表执行方填写)。批次 U 的执行方证据见 [U 交接 §8](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [U 交接 §9](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#9-复核方证据2026-11-21)；批次 T 的证据见 [T 交接 §8](HANDOFF-BATCH-T-2026-11-20.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-T-2026-11-20.zh-CN.md#9-复核方证据2026-11-21)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [V 交接（批次 V，当前）](HANDOFF-BATCH-V-2026-11-21.zh-CN.md) · [U 复核（批次 U 验收与批次 V 计划）](REVIEW_2026-11-21-batchU.zh-CN.md) · [U 交接](HANDOFF-BATCH-U-2026-11-21.zh-CN.md) · [T 复核（批次 T 验收与批次 U 计划）](REVIEW_2026-11-21-batchT.zh-CN.md) · [T 交接](HANDOFF-BATCH-T-2026-11-20.zh-CN.md) · [S 复核（批次 S 验收与批次 T 计划）](REVIEW_2026-11-20-batchS.zh-CN.md) · [S 交接](HANDOFF-BATCH-S-2026-11-20.zh-CN.md) · [R 复核（批次 R 验收与批次 S 计划）](REVIEW_2026-11-20-batchR.zh-CN.md) · [R 交接](HANDOFF-BATCH-R-2026-11-20.zh-CN.md) · [Q 复核（批次 Q 验收与批次 R 计划）](REVIEW_2026-11-20-batchQ.zh-CN.md) · [Q 交接](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 更早：[H/I 复核及 U1 清单](REVIEW_2026-11-13-batchH.zh-CN.md) · [H 交接](HANDOFF-BATCH-H-2026-11-13.zh-CN.md) · [G 复核](REVIEW_2026-11-13-batchG.zh-CN.md) · [G 交接](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [F 复核](REVIEW_2026-11-12-batchF.zh-CN.md) · [F 交接](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [审核修复轮验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [审核修复轮交接](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [方案评审（2026-11-02）](REVIEW_2026-11-02.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
