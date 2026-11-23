# 项目状态

> 更新：2026-11-23，批次 X 执行完毕；codex 结束审核完成十轮（均为「需改」，逐轮收窄），第十一轮两次发起均因网关不可用未完成——**没有批准裁决**。第十轮之后的处置未经 codex 复核。本批首次按项目规则走“先 codex 计划、结束 codex 审核”（gpt-6.2-astra/max）；复核方只做审核与计划，不改代码（所有者指示）。

## 当前状态

批次 X 主体执行完毕，六轮修复已全部落地；codex 结束审核十轮完成（全部「需改」：从 6 项 P1 逐轮收窄到纯文档项，第十轮无新 P0/P1/P3），第十一轮两次发起均因 codex 网关 502 未完成。**没有批准裁决，分支未合入 main。**

- 完成：X0（分支清理 + 复核方三处越权改动的验收）；X1（两层假服务对齐引擎语义、契约 role 句改写为纯接口语义；K1 设备轮顺延）；X2（P2 全部；W-N13/N14 检查脚本；**W-N11 复现成立并修复**——常亮扩展到两个等待阶段，设备验证含常亮持有/释放成对与 4 MiB 传输 SHA 相等）；X5（接收确认设备名与摘要；修复波接通正式 GUI deviceName、旧引擎缺省「对方的设备」、快照回填）；
- 部分完成（如实记录）：X4（代码与 widget 验收完成，两项设备验收顺延——codex 第四/五审不接受「记完成+双顺延」口径，完成口径待所有者裁定；「含文件夹」没有端到端证据，Android 上按现有架构不可用，是已知能力缺口；**最终代码没有跑完过任何一轮界面驱动的设备传输**——32 MiB 的 V2-a 证据跑在六轮修复之前的构建上）；X8（门槛、T1、V2-a 传输恢复均过，K1 设备轮与严格同轮常亮采样欠，升级所有者事项）；X3 补验 V-N3——两轮有效 debug 轮未复现黑屏，profile 轮未取得有效轮，五轮有效未达成，V-N3 维持待验证；K1 完整设备轮十次尝试（两种 GPU 后端）被本宿主模拟器 ANR 类阻断顺延（第七至十次均到达次要入口加入成功与系统选择器打开——应用侧到系统边界验证）；
- 未做：X6（顺延——release 构建依赖所有者 key.properties，E5 域）、X7（可选，沿用 W8 顺延理由）；
- codex 结束审核与修复波：裁决与逐项修复见 [X 交接 §8](HANDOFF-BATCH-X-2026-11-22.zh-CN.md#8-证据表执行方填写)（含 W-N9/-2 场景文案、空间不足分侧、multi 内部值等 P2，与状态表/交接/STATUS 口径更正）；
- W-N1 至 W-N15 全部处置完毕（15 项）：修复八项（W-N2/N4/N7/N9/N10/N11/N13/N14）、验收两项（W-N1/N6）、完成一项（W-N15）、顺延三项（W-N3/N5/N8）、脚本已适配而设备轮顺延一项（W-N12）。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：codex 网关恢复后补做一轮复审（复核自查更正、CI 实况与文件夹补记）；所有者对三件事的裁定——X4 完成口径（两项设备验收顺延，codex 第四审不接受「记完成加双顺延」）、「含文件夹」在 Android 上不可用如何处置（降级为桌面端能力并改文案，或立项 SAF 发送侧读取）、审核循环如何收口（每轮裁决行入账的提交本身又被审）；K1 完整设备轮与 W-N11 长等待场景在更稳的设备环境补做；V-N3 在更稳的机器或手工进入传输的方式下补验；依赖所有者批准的界面项（二维码、拖放、分享入口、通知、`gen-l10n`）见 [审核 §5](AUDIT_2026-11-22.zh-CN.md#5-分阶段路线)。

正式完成还取决于所有者事项（O-1 至 O-7 与 E5/E6/E7/C4，见 [审核 §6](AUDIT_2026-11-22.zh-CN.md#6-需要所有者决定的事项)）。M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。


## 最新验证

批次 X 的执行方证据见 [X 交接 §8](HANDOFF-BATCH-X-2026-11-22.zh-CN.md#8-证据表执行方填写)。批次 W 的执行方证据见 [W 交接 §8](HANDOFF-BATCH-W-2026-11-22.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [W 交接 §9](HANDOFF-BATCH-W-2026-11-22.zh-CN.md#9-复核方证据2026-11-22)。批次 V 的执行方证据见 [V 交接 §8](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [V 交接 §9](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#9-复核方证据2026-11-22)；批次 U 的证据见 [U 交接 §8](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#9-复核方证据2026-11-21)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [X 交接（批次 X，当前）](HANDOFF-BATCH-X-2026-11-22.zh-CN.md) · [W 复核（批次 W 验收与批次 X 计划）](REVIEW_2026-11-22-batchW.zh-CN.md) · [W 交接](HANDOFF-BATCH-W-2026-11-22.zh-CN.md) · [V 复核（批次 V 验收与批次 W 计划）](REVIEW_2026-11-22-batchV.zh-CN.md) · [全项目审核与界面设计方案（2026-11-22）](AUDIT_2026-11-22.zh-CN.md) · [V 交接](HANDOFF-BATCH-V-2026-11-21.zh-CN.md) · [U 复核（批次 U 验收与批次 V 计划）](REVIEW_2026-11-21-batchU.zh-CN.md) · [U 交接](HANDOFF-BATCH-U-2026-11-21.zh-CN.md) · [T 复核（批次 T 验收与批次 U 计划）](REVIEW_2026-11-21-batchT.zh-CN.md) · [T 交接](HANDOFF-BATCH-T-2026-11-20.zh-CN.md) · [S 复核（批次 S 验收与批次 T 计划）](REVIEW_2026-11-20-batchS.zh-CN.md) · [S 交接](HANDOFF-BATCH-S-2026-11-20.zh-CN.md) · [R 复核（批次 R 验收与批次 S 计划）](REVIEW_2026-11-20-batchR.zh-CN.md) · [R 交接](HANDOFF-BATCH-R-2026-11-20.zh-CN.md) · [Q 复核（批次 Q 验收与批次 R 计划）](REVIEW_2026-11-20-batchQ.zh-CN.md) · [Q 交接](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 更早：[H/I 复核及 U1 清单](REVIEW_2026-11-13-batchH.zh-CN.md) · [H 交接](HANDOFF-BATCH-H-2026-11-13.zh-CN.md) · [G 复核](REVIEW_2026-11-13-batchG.zh-CN.md) · [G 交接](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [F 复核](REVIEW_2026-11-12-batchF.zh-CN.md) · [F 交接](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [审核修复轮验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [审核修复轮交接](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [方案评审（2026-11-02）](REVIEW_2026-11-02.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
