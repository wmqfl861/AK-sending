# 项目状态

> 更新：2026-11-26。批次 X 已执行完毕并经 codex 结束审核**批准**，分支交复核方合入 main。批次 Z（Android 文件夹发送）叠在批次 X 的分支之上执行：实现与两台模拟器（Android 15、Android 10）上的走查已完成，并经过四轮修复；codex 结束审核四轮都判「需改」。第三轮留下的阻断（Android 7 到 10 上接收侧的授权可能被发送侧的撤销削掉）已在第四轮修复，并在 Android 10 的模拟器上取得了修复之前失败、修复之后通过的同一个故障次序的证据，codex 第四轮据此关闭了它；第四轮留下三项合入阻断——同一 ID 复用在目录第一次判型短暂失败时会误删已交付副本、同一 URI 的发送任务接棒可能撤掉对方需要的读授权、测试输出仍可能带出实际名称或路径——第五轮修复进行中。安装包已交所有者做真机测试（第四轮修复之后的构建，第五轮之后更换）。详见 [X 交接](HANDOFF-BATCH-X-2026-11-22.zh-CN.md) 与 [Z 交接](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md)。项目规则：先 codex 计划、结束 codex 审核（gpt-6.2-astra/max）；复核方只做审核与计划，不改代码（所有者指示）。

## 当前状态

**批次 X——已批准，待合入。** 各项的结论见下；原阻断项 K1 设备轮已按所有者的答复补做（模拟器上的 profile 构建），codex 据此批准。

**批次 Z——进行中**（叠在批次 X 的分支之上，分支 `dev/batch-z-20261124`）。Android 文件夹发送按 codex 推荐的方法实现：发送侧经系统树形选择器取得读授权，原生枚举并把文件夹暂存到应用私有目录，再把暂存根目录交给现有发送引擎；引擎、FFI、契约、协议、权限与依赖都没有改。实现与本机门槛已通过；两台模拟器（Android 15、Android 10）上的暂存探针与杀停检查通过，端到端走查与重建检查的三个变体在 Android 15 上通过。codex 结束审核四轮都判「需改」：第一轮的四项 P1 已关闭；第二轮剩两项，第三轮关闭了「详情页发送中删源」，并把「读授权误撤」留为唯一的阻断——在 Android 7 到 10 上，发送侧的一次撤销会让接收侧随后的取授权失败（现行生产代码没有调用方，但它是本批承诺并测试的原生接口，且最低支持版本是 Android 7）。执行方据此做了第四轮修复：接收侧的选择先登记再启动、请求的身份留到取授权跑完（不分 API 版本的同一条规则），令牌按暂存器实例并保护所有活着的实例，归属读不出来时不删，只有普通文件会被打开。新写的检查先在修复之前的代码上取得了预期失败（两台模拟器上都有记录），修复之后全部通过；其中 Android 10 上同一个原故障次序修复前结束时接收目录没有持久授权、修复后变成读写两位都在。另一个只读审核者复审了这一轮修复：没有 P0 与 P1。codex 第四轮关闭了「读授权误撤」，仍判「需改」：没有 P0、没有 P1，合入阻断剩三项——同一 ID 复用时，已交付目录的第一次判型若短暂失败，后面会把它删掉；同一 URI 的发送任务接棒时，前一个任务的了结可能撤掉后一个任务需要的读授权；测试的输出仍可能带出界面上的实际名称或任务码的一部分。另有 T7 的严格交错、E2 的 journal 行归属与邻居变体的应答顺序，需要让检查本身能判错。执行方正在按 codex 的计划做第五轮修复。安装包目前是第四轮修复之后的构建，第五轮之后更换，**真机证据待所有者提供**。设备验证中有几次应用启动后测试迟迟没有开始，原因没有查明，记录见 Z 交接 §8。证据与未验证项见 [Z 交接 §8](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md#8-证据表执行方填写)。

批次 X 各项：

- 完成：X0、X1（含 K1 走查）、X2（含 W-N11 的复现与修复）、X5；
- 部分完成：X4（代码、widget 验收与两项设备走查都有证据；Android 上的「含文件夹」已由批次 Z 补上）；X8；X3 补验 V-N3 维持待验证；
- 未做：X6（release 构建依赖所有者 key.properties）、X7（可选，沿用 W8 的顺延理由）；
- codex 结束审核与修复波、W-N1 至 W-N15 的处置：见 [X 交接 §8](HANDOFF-BATCH-X-2026-11-22.zh-CN.md#8-证据表执行方填写)。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

下一步：批次 Z——第五轮修复（codex 的计划已出具）：同 ID 复用、同 URI 接棒、测试输出、T7 与重建检查的判据；Android 15 的变异构建与两台模拟器上的复验；安装包重建；codex 第五轮结束审核；所有者在手机上测试（真机证据待所有者提供）；合入前移除 154 测试服务器上的测试服务。批次 X——复核方把分支合入 main；合入后审阅并合入批次 Z。其后：W-N11 长等待场景在更稳的设备环境补做；V-N3 在更稳的机器或手工进入传输的方式下补验；依赖所有者批准的界面项（二维码、拖放、分享入口、通知、`gen-l10n`）见 [审核 §5](AUDIT_2026-11-22.zh-CN.md#5-分阶段路线)。

正式完成还取决于所有者事项（O-1 至 O-7 与 E5/E6/E7/C4，见 [审核 §6](AUDIT_2026-11-22.zh-CN.md#6-需要所有者决定的事项)）。M2 与之后各轮的改动要等 E6 部署后才进入生产服务器。

生产部署、物理双机、签名和其他所有者事项继续按 [状态表](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)、[OPERATIONS](OPERATIONS.zh-CN.md) 与 [支持矩阵](SUPPORT_MATRIX.zh-CN.md) 执行。本轮没有生产访问。


## 最新验证

批次 Z 的执行方证据见 [Z 交接 §8](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md#8-证据表执行方填写)。批次 X 的执行方证据见 [X 交接 §8](HANDOFF-BATCH-X-2026-11-22.zh-CN.md#8-证据表执行方填写)。批次 W 的执行方证据见 [W 交接 §8](HANDOFF-BATCH-W-2026-11-22.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [W 交接 §9](HANDOFF-BATCH-W-2026-11-22.zh-CN.md#9-复核方证据2026-11-22)。批次 V 的执行方证据见 [V 交接 §8](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#8-证据表执行方填写)，复核方的命令、运行编号与计数见 [V 交接 §9](HANDOFF-BATCH-V-2026-11-21.zh-CN.md#9-复核方证据2026-11-22)；批次 U 的证据见 [U 交接 §8](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#8-证据表执行方填写) 与 [§9](HANDOFF-BATCH-U-2026-11-21.zh-CN.md#9-复核方证据2026-11-21)。本机门槛、离线行为模型和未执行范围分开记录，不混作真机证据。

## 交接入口

- [Z 交接（批次 Z，当前）](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md) · [X 交接（批次 X，已批准、待合入）](HANDOFF-BATCH-X-2026-11-22.zh-CN.md) · [W 复核（批次 W 验收与批次 X 计划）](REVIEW_2026-11-22-batchW.zh-CN.md) · [W 交接](HANDOFF-BATCH-W-2026-11-22.zh-CN.md) · [V 复核（批次 V 验收与批次 W 计划）](REVIEW_2026-11-22-batchV.zh-CN.md) · [全项目审核与界面设计方案（2026-11-22）](AUDIT_2026-11-22.zh-CN.md) · [V 交接](HANDOFF-BATCH-V-2026-11-21.zh-CN.md) · [U 复核（批次 U 验收与批次 V 计划）](REVIEW_2026-11-21-batchU.zh-CN.md) · [U 交接](HANDOFF-BATCH-U-2026-11-21.zh-CN.md) · [T 复核（批次 T 验收与批次 U 计划）](REVIEW_2026-11-21-batchT.zh-CN.md) · [T 交接](HANDOFF-BATCH-T-2026-11-20.zh-CN.md) · [S 复核（批次 S 验收与批次 T 计划）](REVIEW_2026-11-20-batchS.zh-CN.md) · [S 交接](HANDOFF-BATCH-S-2026-11-20.zh-CN.md) · [R 复核（批次 R 验收与批次 S 计划）](REVIEW_2026-11-20-batchR.zh-CN.md) · [R 交接](HANDOFF-BATCH-R-2026-11-20.zh-CN.md) · [Q 复核（批次 Q 验收与批次 R 计划）](REVIEW_2026-11-20-batchQ.zh-CN.md) · [Q 交接](HANDOFF-BATCH-Q-2026-11-19.zh-CN.md) · [P 复核（批次 P 验收与批次 Q 计划）](REVIEW_2026-11-19-batchP.zh-CN.md) · [P 交接](HANDOFF-BATCH-P-2026-11-19.zh-CN.md) · [O 复核（批次 O 验收与批次 P 计划）](REVIEW_2026-11-19-batchO.zh-CN.md) · [O 交接](HANDOFF-BATCH-O-2026-11-18.zh-CN.md) · [N 复核（批次 N 验收与批次 O 计划）](REVIEW_2026-11-18-batchN.zh-CN.md) · [N 交接](HANDOFF-BATCH-N-2026-11-17.zh-CN.md) · [M 记录（批次 M，复核方执行）](HANDOFF-BATCH-M-2026-11-17.zh-CN.md) · [L 复核（验收与批次 M 计划）](REVIEW_2026-11-17-batchL.zh-CN.md) · [L 交接](HANDOFF-BATCH-L-2026-11-16.zh-CN.md) · [K24 复核（验收与批次 L 工作单）](REVIEW_2026-11-16-batchK24.zh-CN.md) · [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · [K23 复核](REVIEW_2026-11-16-batchK23.zh-CN.md) · [K22 复核](REVIEW_2026-11-16-batchK22.zh-CN.md) · [K21 复核](REVIEW_2026-11-16-batchK21.zh-CN.md) · [K20 复核](REVIEW_2026-11-16-batchK20.zh-CN.md) · [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md) · [K18 复核](REVIEW_2026-11-16-batchK18.zh-CN.md) · [K17 复核](REVIEW_2026-11-16-batchK17.zh-CN.md) · [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md) · [K15 复核](REVIEW_2026-11-15-batchK15.zh-CN.md) · [K14 复核](REVIEW_2026-11-15-batchK14.zh-CN.md) · [K13 复核](REVIEW_2026-11-15-batchK13.zh-CN.md) · [K12 复核](REVIEW_2026-11-15-batchK12.zh-CN.md) · [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md) · [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md) · [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md) · [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md) · [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) · [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) · [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md) · [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md) · [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) · [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) · [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)
- [审核状态表及所有者事项](REVIEW_2026-11-11.zh-CN.md) · [分支与实施规则](../AGENTS.md)
- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · [J 复核及 K 工作单](REVIEW_2026-11-14-batchJ.zh-CN.md) · [U1/J1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)
- 更早：[H/I 复核及 U1 清单](REVIEW_2026-11-13-batchH.zh-CN.md) · [H 交接](HANDOFF-BATCH-H-2026-11-13.zh-CN.md) · [G 复核](REVIEW_2026-11-13-batchG.zh-CN.md) · [G 交接](HANDOFF-BATCH-G-2026-11-13.zh-CN.md) · [F 复核](REVIEW_2026-11-12-batchF.zh-CN.md) · [F 交接](HANDOFF-BATCH-F-2026-11-12.zh-CN.md) · [审核修复轮验收](REVIEW_2026-11-12-acceptance.zh-CN.md) · [审核修复轮交接](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md) · [方案评审（2026-11-02）](REVIEW_2026-11-02.zh-CN.md)
- 历史与项目入口：[HANDOFF](HANDOFF.zh-CN.md) · [ROADMAP](ROADMAP.zh-CN.md) · [BACKLOG](BACKLOG.zh-CN.md) · [CHANGELOG](../CHANGELOG.md)
