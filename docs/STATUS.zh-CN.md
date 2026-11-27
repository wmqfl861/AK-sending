# 项目状态

> 更新：2026-11-27。批次 X 已由复核方验收并合入 main（`5b6ae31`）。批次 Z（Android 文件夹发送）的复核结论：不合入，继续做，并建议先做减法——删掉没有带来功能的持久读授权机制，再在最终提交上取设备证据。接手从 [Z 接手交接](HANDOFF-BATCH-Z-2026-11-27.zh-CN.md) 开始，结论与原因见 [Z 复核](REVIEW_2026-11-27-batchZ.zh-CN.md)。项目规则：所有任务的计划与结束审核依 [AGENTS.md](../AGENTS.md) 执行；复核方只做审核与计划，不改代码（所有者指示）。

## 当前状态

**批次 X——已验收，已合入 main。** codex 结束审核第十五轮批准，分支头的 CI 全绿，复核方抽查上一轮问题的落点都在；各项的结论见下。复核文档放在批次 Z 的分支上，所以 main 上的本文件暂时还是批次 X 收尾时的文字，随批次 Z 合入更新。

**批次 Z——未合入，接手工作单已排定**（分支 `dev/batch-z-20261124`；批次 X 合入后它等同于从 main 切出）。

- 接手进展（2026-11-27）：项目规则已按所有者指示修订。批次 Z 的接手：Z9 的计划与 Z14 的三项交付物已由 codex 结束审核批准，各笔提交已推送，代码提交 `662c964` 的 CI 全部通过，Ubuntu 与 Linux 上的专项后补；Android 的验证（Z10 至 Z13）待环境：Windows Hypervisor Platform 已启用，模拟器加速要重启后才能验证，还缺现有的 debug keystore。接手机有间歇性的计算错误，本机的测试结果只作参考；这一轮的可靠证据来自 CI，它不改变本机门槛的要求。逐项状态见[审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)，证据见 [Z 接手交接 §8](HANDOFF-BATCH-Z-2026-11-27.zh-CN.md#8-证据表接手方填写)；
- 做到哪里（执行方记录）：发送侧经系统选择器取得读授权，原生代码把文件夹暂存到应用私有目录，再把暂存路径交给现有发送引擎；引擎、FFI、契约、协议、权限与依赖都没有改。第四轮修复在 Android 15 与 Android 10 两台模拟器上通过；codex 结束审核四轮都判「需改」；第五轮修复已提交，**但最终提交上没有设备证据，全部门槛、安装包与第五轮审核也都没有做**。所有者 2026-11-27 指示停下并移交；
- 复核结论：不照原样硬做完。原生代码里管理持久读授权的那一整套机制没有带来功能，却是四轮审核阻断项的主要来源，也是设备探针里最重的部分；建议整套删掉（Z-N1）。这一判断里关于 Android 平台行为的部分没有在设备上验证，所以接手的第一段是保全材料、请 codex 出计划、做一次验证试验，不成立就退回原路线；
- 卡住本地工作的四件事各有原因与办法：一次探针里应用在复制途中不再输出（Z-N4，原因未查明，复核方的判断是界面被结束而进程还在）；模拟器嵌套在云虚拟机里、时快时慢（Z-N3）；证据所依赖的脚本都在仓库之外的临时目录（Z-N2）；CI 的两次失败是两个与本批无关的偶发用例（Z-N6）；
- 查 CI 失败时看到两个既有问题：配对的提交若在截止时间之后才到达会被端点拒绝（Z-N7）；配对对设备之间的时钟偏差只能容忍很短的一段（Z-N8，复核时的意见是改法待所有者决定；所有者 2026-11-27 已决定暂缓）；
- 所有者桌面上的安装包是第四轮修复之后的构建，不作为验收对象；154 测试服务器上的两个测试服务仍在运行，接手后先停掉。

批次 X 各项：

- 完成：X0、X1（含 K1 走查）、X2（含 W-N11 的复现与修复）、X5；
- 部分完成：X4（代码、widget 验收与两项设备走查都有证据；Android 上的「含文件夹」已由批次 Z 补上）；X8；X3 补验 V-N3 维持待验证；
- 未做：X6（release 构建依赖所有者 key.properties）、X7（可选，沿用 W8 的顺延理由）；
- codex 结束审核与修复波、W-N1 至 W-N15 的处置：见 [X 交接 §8](HANDOFF-BATCH-X-2026-11-22.zh-CN.md#8-证据表执行方填写)。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：这一轮的改动已推送并取得 CI 证据（所有者已接受分阶段路线），Z14 的专项后补。安卓这条线：Windows Hypervisor Platform 已启用，等所有者重启并提供现有的 debug keystore 之后做 Z10 验证试验，通过才做 Z11、Z12，再在最终提交上取设备证据（Z13）。Z14 的专项（Ubuntu 上重复 Z-N6、Linux 上的核对）待有 Ubuntu 环境。Z8 里原机材料的保全与入库、154 的停服，都待所有者；远端 X 分支已按授权删除。Z-N8 按所有者的决定暂缓，设备轮带上时间差核对后再定方向，评估见[方案评估](design/pairing-deadline-clock-skew.zh-CN.md)。所有者的手机测试在新安装包出来之后进行。其后：W-N11 长等待场景在更稳的设备环境补做；V-N3 在更稳的机器或手工进入传输的方式下补验；依赖所有者批准的界面项（二维码、拖放、分享入口、通知、`gen-l10n`）见 [审核 §5](AUDIT_2026-11-22.zh-CN.md#5-分阶段路线)。

正式完成还取决于所有者事项（O-1 至 O-7 与 E5/E6/E7/C4，见 [审核 §6](AUDIT_2026-11-22.zh-CN.md#6-需要所有者决定的事项)）。M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。


## 最新验证

批次 Z 接手之后的证据见 [Z 接手交接 §8](HANDOFF-BATCH-Z-2026-11-27.zh-CN.md#8-证据表接手方填写)；此前的执行方证据见 [原 Z 交接 §8](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [原 Z 交接 §10](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md#10-复核方证据2026-11-27)。批次 X 的执行方证据见 [X 交接 §8](HANDOFF-BATCH-X-2026-11-22.zh-CN.md#8-证据表执行方填写)，复核方的见 [X 交接 §9](HANDOFF-BATCH-X-2026-11-22.zh-CN.md#9-复核方证据2026-11-27)。批次 W 的执行方证据见 [W 交接 §8](HANDOFF-BATCH-W-2026-11-22.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [W 交接 §9](HANDOFF-BATCH-W-2026-11-22.zh-CN.md#9-复核方证据2026-11-22)。批次 V 的执行方证据见 [V 交接 §8](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [V 交接 §9](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#9-复核方证据2026-11-22)；批次 U 的证据见 [U 交接 §8](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#9-复核方证据2026-11-21)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [Z 接手交接（批次 Z，当前）](HANDOFF-BATCH-Z-2026-11-27.zh-CN.md) · [Z 复核（批次 X 验收、批次 Z 复核与接手方案）](REVIEW_2026-11-27-batchZ.zh-CN.md) · [原 Z 交接（设计、接口、此前的证据与移交说明）](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md) · [X 交接（批次 X，已合入）](HANDOFF-BATCH-X-2026-11-22.zh-CN.md) · [W 复核（批次 W 验收与批次 X 计划）](REVIEW_2026-11-22-batchW.zh-CN.md) · [W 交接](HANDOFF-BATCH-W-2026-11-22.zh-CN.md) · [V 复核（批次 V 验收与批次 W 计划）](REVIEW_2026-11-22-batchV.zh-CN.md) · [全项目审核与界面设计方案（2026-11-22）](AUDIT_2026-11-22.zh-CN.md) · [V 交接](HANDOFF-BATCH-V-2026-11-21.zh-CN.md) · [U 复核（批次 U 验收与批次 V 计划）](REVIEW_2026-11-21-batchU.zh-CN.md) · [U 交接](HANDOFF-BATCH-U-2026-11-21.zh-CN.md) · [T 复核（批次 T 验收与批次 U 计划）](REVIEW_2026-11-21-batchT.zh-CN.md) · [T 交接](HANDOFF-BATCH-T-2026-11-20.zh-CN.md) · [S 复核（批次 S 验收与批次 T 计划）](REVIEW_2026-11-20-batchS.zh-CN.md) · [S 交接](HANDOFF-BATCH-S-2026-11-20.zh-CN.md) · [R 复核（批次 R 验收与批次 S 计划）](REVIEW_2026-11-20-batchR.zh-CN.md) · [R 交接](HANDOFF-BATCH-R-2026-11-20.zh-CN.md) · [Q 复核（批次 Q 验收与批次 R 计划）](REVIEW_2026-11-20-batchQ.zh-CN.md) · [Q 交接](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 更早：[H/I 复核及 U1 清单](REVIEW_2026-11-13-batchH.zh-CN.md) · [H 交接](HANDOFF-BATCH-H-2026-11-13.zh-CN.md) · [G 复核](REVIEW_2026-11-13-batchG.zh-CN.md) · [G 交接](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [F 复核](REVIEW_2026-11-12-batchF.zh-CN.md) · [F 交接](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [审核修复轮验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [审核修复轮交接](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [方案评审（2026-11-02）](REVIEW_2026-11-02.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
