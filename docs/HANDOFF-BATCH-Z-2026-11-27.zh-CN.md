# 交接文档：批次 Z 接手（2026-11-27）

> 起点：分支 `dev/batch-z-20261124` 上复核方的文档提交（本文随它入库）。main 已快进到 `5b6ae31`，批次 X 已合入，所以这个分支现在等同于从 main 切出。本文由复核方（Claude）编写，写给接手批次 Z 的执行方。
>
> - 复核结论、原因分析与方案见 [Z 复核](REVIEW_2026-11-27-batchZ.zh-CN.md)；
> - 批次 Z 的设计、冻结接口、已有证据与执行方的移交说明见 [原 Z 交接](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md)（§3 接口、§8 证据、§9 移交）；
> - 复核方的命令与运行编号见 [X 交接 §9](HANDOFF-BATCH-X-2026-11-22.zh-CN.md#9-复核方证据2026-11-27) 与 [原 Z 交接 §10](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md#10-复核方证据2026-11-27)；
> - 接手方从本文 §8 证据表开始记录运行编号与计数；
> - 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 X 已验收并合入 main。批次 Z（Android 发送文件夹）做到第五轮修复后停下：代码已提交，但最终提交上没有设备证据、没有跑全部门槛、没有重建安装包、没有第五轮外部审核，另有一次应用在复制途中不再输出、原因不明。

复核方的结论是**不照原样硬做完，先做减法**：原生代码里管理“持久读授权”的那一整套机制没有带来功能，却是四轮审核阻断项的主要来源，也是设备探针里最重的部分（[Z 复核 §2.3](REVIEW_2026-11-27-batchZ.zh-CN.md#23-为什么四轮审核收不了口一套没有用处的机制z-n1)）。删掉它之后，剩下的验证量大约是原来的一半，而且不再需要 1 GiB 的保留文件、停靠的系统选择器和变异构建。

这个结论里有一条是复核方对 Android 平台行为的判断，**没有在设备上验证过**。所以本批的第一段是保全材料、请 codex 出计划、做一次半天的验证试验；验证不成立就退回原路线。

## 1. 路线与出口

| 路线 | 何时走 | 做什么 |
|---|---|---|
| A（默认） | Z9 的 codex 计划没有指出功能损失，且 Z10 的验证试验三项都成立 | Z11 至 Z13：删掉持久授权机制，收敛测试，取最终提交上的设备证据 |
| B | codex 认定去掉持久授权有功能损失，或 Z10 有任何一项不成立，或所有者指定 | 不做 Z10 至 Z12；改按 [原 Z 交接 §9.2](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md#92-接手的人要做的按顺序) 的六步做完；设备轮照本文 §2.4 的规矩 |
| C | 只由所有者决定 | 关掉功能开关，暂缓 Android 文件夹发送；批次 Z 改过的接收侧原生路径仍要还原或验证 |

转到路线 B 或 C 时，在 §8 写明是哪一条依据，并告诉复核方。Z8、Z14、Z15 三条路线都要做。

## 2. 接手前必读

### 2.1 角色与规则

- 你是唯一改代码、做设备验证的一方。复核方与 codex 只做计划与审核，不改代码；角色边界以 AGENTS.md「计划与审核规则」（2026-11-27 修订）为准。
- **复核方也会写错**（批次 W 的 W7.1 就是）。本文和 Z 复核里任何一条与代码、现有测试或设备上的结果对不上：先核对，在 §8 写明依据，停下那一项并报告，不要照做。本文里凡是涉及 Android 平台行为的说法，都要以设备上的结果为准。
- 本交接里的每一项任务（Z8 至 Z15，以及文档、运维与规则修改本身）动手前都要先请 codex 出计划，结束后请 codex 审核实际改动与验证证据；计划结论、裁决与理由记入 §8（项目规则，2026-11-27 修订：适用于所有任务）。

### 2.2 沿用的规矩

- 报完成、往文档里写内容、测试替身与真实引擎、改产品代码、Android 走查：沿用 [X 交接 §2.2 至 §2.6](HANDOFF-BATCH-X-2026-11-22.zh-CN.md#2-接手前必读)；
- 门槛与命令：沿用 [原 Z 交接 §5](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md#5-门槛与命令)；本批的 Z14 会改 Rust，Rust 各项必须在本机跑；
- 红线：沿用 [原 Z 交接 §7](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md#7-红线)。

### 2.3 范围冻结：本批不做什么

批次 Z 四轮审核每一轮都在加机制。从本批起，下面这些明确不做；外部审核若提出与它们有关的新要求，记为后续批次，不在本批加代码：

- 不为“界面已经没了、暂存还在继续”做任何事——Activity 结束，暂存就取消并清理；
- 不保证暂存能跨 Activity 重建或跨进程续作；
- 不支持同时进行两个暂存（现在的行为是新的暂存会取消在跑的那个，保持）；
- 不新增任何与授权有关的状态、日志、诊断通道；
- 不处理云盘等非本地 provider、完全空的文件夹、超过条目上限的文件夹（保持拒绝并提示）。

外部审核提出新的阻断项时，先回答一个问题：**引出这个阻断的机制还要不要？** 能靠删除解决的，不靠再加一层解决。

### 2.4 设备证据的规矩

- 只认最终提交、干净检出上的结果；原 Z 交接 §8 里第四轮的设备记录不是现在代码的证据；
- 一轮一台模拟器；每轮外面有墙钟上限，应用不再输出就判失败并保留现场，不等；
- 每轮都留完整的 logcat，并另抓事件缓冲区（`adb logcat -b events -d`）；失败时立刻取 `adb shell dumpsys activity activities` 与 `adb shell dumpsys activity exit-info <包名>`；
- 每轮开始前记录设备与宿主的时间差（`adb shell date +%s` 对比宿主的 `date +%s`），相差超过 5 秒先校时再跑（原因见 Z 复核 §3.4 的 Z-N8）；
- 模拟器上的结果不写成真机验证；测试输出里不出现从设备读到的名字、路径、任务码。

## 3. 未完成事项

### 3.1 所有者事项（agent 不得代办）

- **路线**：本文默认路线 A；所有者可以改为 B 或 C；
- **设备轮在哪里跑**：是否同意在 GitHub 的 CI 上增加一个手动触发的模拟器任务（占用 CI 用量），见 §4.6；
- **手机测试**：新安装包出来之后按原 Z 交接 §6.2 的步骤做；手机上若装过旧包，先卸载；
- **Z-N8（时钟偏差）**：是否改配对的计时方式，等 Z14 的方案评估出来再定；
- 其余沿用：O-1 至 O-7、E5、E6、E7、C4，见 [审核 §6](AUDIT_2026-11-22.zh-CN.md#6-需要所有者决定的事项) 与 [状态表 §8](REVIEW_2026-11-11.zh-CN.md#8-需所有者确认的事项)。

### 3.2 批次 X 留下的工程项（不在本批）

V-N3（回前台黑屏）待验证；W-N11 的长等待场景待在更稳的设备环境补做；W-N3、W-N5、W-N8 顺延；X6（CI 发布构建，等所有者的签名配置）；X7（归档）。依赖所有者批准的界面项见 [审核 §5](AUDIT_2026-11-22.zh-CN.md#5-分阶段路线)。

## 4. 接手工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| Z8 | 保全与入库，按 §4.1 | 备份位置记入 §8；脚本入库且仓库检查通过；X 分支已删；154 的测试服务已停 |
| Z9 | 请 codex 就路线 A 出计划，按 §4.2 | 计划的结论与对五个问题的回答记入 §8 |
| Z10 | 验证试验，按 §4.3 | 三项判据逐项有设备记录；结论是“进入 Z11”或“转路线 B” |
| Z11 | 实施精简，按 §4.4 | §4.4 的验收全部满足 |
| Z12 | 测试收敛，按 §4.5 | 探针、重建检查、宿主检查都已删减并通过各自的自检 |
| Z13 | 最终提交上的设备证据，按 §4.6 | 六轮各有一行证据 |
| Z14 | 既有问题（Rust），按 §4.7；可以与 Z9 至 Z13 并行 | 两处修复各有自动化测试；Z-N8 的评估成文 |
| Z15 | 收尾，按 §4.8 | 全部门槛通过；安装包已重建；codex 结束审核的裁决入 §8 |

顺序：Z8 最先（今天就做第 1 步的备份）；然后 Z9、Z10；Z10 的结论决定后面走哪条路线。Z14 与安卓这条线互不相干，可以交给另一个子代理并行。

### 4.1 Z8：保全与入库

1. **先备份**（Z-N2）。把系统临时目录下的 `ak-y/`、`ak-x/`、临时目录根下的 `ak-x-z-*.out`，以及仓库里的 `target/device-evidence/`，整份复制到临时目录与 `target/` 之外的固定位置。在 §8 记下位置（用占位符，不写真实用户目录）。这一步不依赖任何决定，先做。
2. **入库**。把宿主脚本（四个入口、`z-common.sh`、`z-common-recreate.sh`、`_selftest.sh`）、执行方的运行器、宿主 JVM 检查（`harness/` 与 `run5.sh`）、契约文本（`CONTRACT.md`）放进仓库，建议放在 `scripts/android_probe/`。入库之前去掉服务器地址、任务码、真实路径与设备名；配一份说明：依赖什么、怎么运行、每个入口对应证据表里的哪一类行。路线 A 之后要删的部分可以先原样入库，到 Z12 再删，这样历史可查。确实不能入库的，写明原因与存放位置。
3. **分支**。确认 `git merge-base --is-ancestor origin/dev/batch-x-20261122 origin/main` 成立后，删除远端的 `dev/batch-x-20261122`，清理本地旧分支与 worktree。继续使用 `dev/batch-z-20261124`，不另开分支。
4. **154 测试服务器**（Z-N9）。开发已经停下，两个对公网开放的测试服务先停掉：`systemctl --user disable --now ak-test-registry ak-test-stun`；单元文件留着，做手机测试时再启用。地址不入库。
5. 在 §8 回填两次 CI 的运行编号与结论，都用接口自己核实：本文所在提交在分支上的那一次，以及 `5b6ae31` 在 main 上的那一次（批次 X 合入时触发）。

### 4.2 Z9：请 codex 出计划

把 [Z 复核 §2.3 与 §4](REVIEW_2026-11-27-batchZ.zh-CN.md#23-为什么四轮审核收不了口一套没有用处的机制z-n1) 和本文 §2.3 的范围冻结交给它，请它专门回答：

1. 去掉发送侧的持久读授权，有没有任何功能损失？（复核方的论据：`MainActivity.onDestroy` 调 `SafSendStager.shutdown()`，取消本实例在跑的任务并让已交付的副本作废；选择与暂存在 `pickSendDirectory()` 里紧挨着发生。）
2. 系统选择器结果带来的临时授权，在 Android 7 到 15 上是否足以覆盖“选择完成到暂存结束”这段时间？有没有已知的例外？
3. 接收侧原生 `pickFolder` 恢复到批次 Z 之前的行为之后，它的授权调用放在哪条线程上？前提是不得再排在长时间的复制后面。
4. 过期清理、实例令牌、定点清理还要不要改？
5. 删减之后的文件与函数清单，具体到函数名。

codex 认定有功能损失：把它的理由原样记入 §8，转路线 B。

### 4.3 Z10：验证试验（决定路线 A 的去留）

目的是用最小的改动确认一件事：**不取持久授权，暂存照样能读完整棵目录树。**

- 改动：只让暂存跳过“取授权”和“释放授权”这两步，其余代码一行不动。这是试验用的改动，不是最终实现；
- 环境：Android 15 与 Android 10 各一台模拟器（或真机），profile 构建，指向本机的注册服务器与 STUN；
- 三项判据，每项都要有设备记录：
  1. 真实系统选择器选中测试目录之后，暂存完成，逐项核对相对路径、大小与 SHA-256 全部一致；
  2. 端到端走查通过（经真实发送页把暂存的文件夹发给宿主接收端，两端一致）；
  3. 暂存途中结束 Activity：暂存收场，暂存目录删净，应用下次启动正常；
- 贯穿全程的第四条：整个过程前后，持久授权表（`listSafPersisted`）逐项不变；
- 另记一条观察，不作判据：暂存途中把应用切到后台 60 秒再回来，暂存是否继续。

三项都成立，进入 Z11。任何一项不成立，把现象原样记入 §8，转路线 B。

### 4.4 Z11：实施精简

按 Z9 的计划删减。复核方读代码后预期的范围如下，**以 codex 的计划和编译结果为准**：

- `SafSendStager.kt`：删除取授权与释放、归属日志（`.send-grants`）的读写、了结与它的准入、接收侧选择的登记与延后启动、授权轨迹、`takeReceiveGrant`、`settleJournal`、`grantTrace`，以及头部说明里对应的段落；
- `MainActivity.kt`：`pickFolder` 恢复到 `5b6ae31` 的行为；删除 `safGrantTrace`；`getStorageInfo` 与 `listSafPersisted` 的应答不再等待复制；
- 保留：枚举、复制、长度与成员复核、取消、定点清理、过期清理、名称校验、实例令牌；第五轮里“定不了型就拒绝复用 ID”的修复（`f0c1259` 的第一部分）；测试输出的脱敏（`9ca500c`）；
- Dart：网关与页面预计不用改；授权失效时原生答 `SAF_PERMISSION_DENIED`，页面走现有的“无法发送此文件夹 / 重新选择”；
- 文档：原 Z 交接 §3（冻结的接口）由你按实现改写，删掉“读授权的归属”“放行闸门”和第五轮修复里与授权有关的两条，改动与代码在同一个提交里。

验收：

- Kotlin 编译、`flutter analyze`、`flutter test` 通过；
- 发送路径上搜不到 `takePersistableUriPermission` 与 `releasePersistableUriPermission`；
- `git diff 5b6ae31 -- flutter_app/android/app/src/main/kotlin/dev/ak/ak_sending_app/MainActivity.kt` 里，接收侧的函数逐个说明“语义不变”或“改了什么、为什么”；
- 删减前后的行数记入 §8。

### 4.5 Z12：测试收敛

- 暂存探针：保留真实选择器、暂存、逐项核对（A 段）、失败路径（D 段）、取消与清理；B、C 两段改为断言“发送前后持久授权表逐项不变”；删除 E1、E2、重取授权、变异构建相关的判定与轨迹读取；宿主脚本里 1 GiB 保留文件与停靠选择器的逻辑一并删掉；
- 重建检查：保留“销毁即取消并清理”与冷启动两个变体；
- 宿主 JVM 检查：删掉授权相关的用例，保留 ID 复用拒绝、安全删除、名称校验；
- widget 与单元测试：按 Dart 一侧的实际改动更新，被改的断言在 §8 列出；
- 各自的自检在删减之后完整跑一遍并记入 §8（原 Z 交接 §9.3 记着：宿主脚本最后三处改动之后自检没有跑完）。

### 4.6 Z13：最终提交上的设备证据

- 范围：Android 15 上探针、杀停、端到端、重建四轮；Android 10 上探针与端到端两轮。都在同一个最终提交、干净的检出上，按 §2.4 的规矩；
- 环境三选一或组合，在 §8 写明用的是哪一种：
  1. 现有的模拟器。先看一眼速度，慢的时候不要硬跑；
  2. CI 上手动触发的模拟器任务（需要所有者同意）。仓库的 CI 已经有 ubuntu 上的 Android 构建任务和只在手动触发时运行的深测任务，照同样的方式加一个；ubuntu 执行器有硬件虚拟化。新增的 action 按现有做法钉到提交号；
  3. 所有者的手机，只做端到端那一轮，记为真机证据。
- 如果仍然遇到“应用不再输出”：按 §2.4 保留现场，对照 [Z 复核 §3.1](REVIEW_2026-11-27-batchZ.zh-CN.md#31-应用在复制途中不再输出z-n4原因未查明) 的判断查事件缓冲区，结论记入 §8。

### 4.7 Z14：既有问题（Rust，可以并行）

依据见 [Z 复核 §3.4](REVIEW_2026-11-27-batchZ.zh-CN.md#34-ci-的两次失败z-n6以及由此看到的两个既有问题z-n7z-n8)。两条都先请 codex 出计划。

1. **提交帧在截止时间之后到达被拒绝**（Z-N7，也是 `sender_stun_and_probe_round_share_the_adopted_dial_socket` 偶发失败的直接原因）。位置：`crates/ak-session/src/pairing/orchestrator.rs` 里加入方与创建方的实时路径，等提交帧的上限是“截止时间加 10 秒”，校验却用原始的当前时间；同一文件的崩溃恢复路径用的是“当前时间与截止时间取小”。请 codex 确认把实时路径改成与恢复路径一致是否安全（复核方的论据：服务器只在截止之前提交；等待上限已经把接受窗口限制在截止后 10 秒之内；新鲜性由双方各自的随机数保证）。验收：一条能稳定复现“服务器在截止前提交、提交帧在截止后到达”的测试，修复前失败、修复后通过；不改线上协议；配对流程的契约里若有相关文字，同步。
2. **取消后同一端口立刻重新绑定偶发失败**（`cancelled_single_receive_port_rebinds_immediately`）。先让这个测试在超时的时候把第二个任务的结果一并打印出来，确认是不是绑定失败。位置：`crates/ak-ffi/src/core.rs` 的 `job_cancel`，中止后等待收尾有 500 毫秒的硬上限。修法倾向于“取消返回之前确认端口已经放掉（有界重试）”，不要靠放宽测试的等待。验收：修复有测试；这个用例连续运行多次不再失败，次数与结果记入 §8。
3. **时钟偏差**（Z-N8）。本批只做两件事：把“设备与宿主时间差”的核对写进走查脚本（§2.4）；写一页方案评估交给所有者——两种方向是用注册服务器的时间作基准，或者放宽截止窗口、由服务器在固定时刻给出“不可用”的应答。**不改协议，不改服务器。**

改了 Rust，全部 Rust 门槛都要在本机跑；`ak-ffi` 变了就重建 Android 的库并在 §8 记一行。

### 4.8 Z15：收尾

- 在最终提交、干净的检出上跑全部门槛（原 Z 交接 §5），缺工具写“未执行”；
- 由最终源码重建 arm64 的 profile 安装包（debug 签名，三个 `--dart-define` 都显式给出），核对 ABI、签名与 SHA-256；交所有者之前提醒先卸载旧包；
- 请 codex 做结束审核，并把本文 §2.3 的范围冻结清单一并交给它；裁决与理由记入 §8；
- 更新状态表的 Z8 至 Z15 与 Z-N1 至 Z-N10 各行、STATUS、本文 §8；
- 154 测试服务器：所有者的手机测试做完之后移除两个服务单元，记入 §8；
- 报完成的规矩同 X 交接 §2.2：最后一次推送之后核对远端分支头，等 CI 跑完，回复里写明提交号、CI 运行编号与结论、走的是哪条路线、哪些项完成、哪些没做及原因。

## 5. 门槛与命令

命令清单沿用 [原 Z 交接 §5](HANDOFF-BATCH-Z-2026-11-24.zh-CN.md#5-门槛与命令)，不在这里重复。补充三点：

- 本机跑 Kotlin 编译那一项时，`JAVA_HOME` 与 `ANDROID_HOME` 要指向本机的工具链，否则 Flutter 的构建钩子找不到 SDK（原 Z 交接 §9.2 的提醒）；
- 原批次“不改 Rust”，本批的 Z14 会改：Rust 各项、`ak_ffi_dart` 的测试都必须在本机跑，不能只依赖 CI；
- 安装包的三个 `--dart-define` 必须全部显式给出；缺工具时写“未执行”，不能写成通过。

## 6. 文档与分支规则

- 继续使用 `dev/batch-z-20261124`，只推这个分支；任何人都不强推，发现错误就追加提交；
- 每项状态只写在状态表里，提交列写真实提交号；运行编号与计数只写在本文 §8；STATUS 只写阶段结论、下一步和链接；
- Z8 至 Z15 各行由执行方更新；Z-N1 至 Z-N10 由对应的工作项原地补充结论，保留原有描述；状态表表尾的说明行由复核方写；
- 所有失败都记入 §8，不能只记最后一次；结论依赖的关键输出行原样贴进 §8（路径、指纹、任务码按红线处理）；
- 没有做的项，在 §8 写明没有做和原因。

## 7. 红线

沿用原 Z 交接 §7 的全部各条：不加 relay；不碰生产服务器；不生成签名密钥；不加第三方依赖（测试依赖也不加）；不改清单权限；密钥、任务码、真实路径、截图不入库；154 测试服务器的地址不入库。

## 8. 证据表（接手方填写）

按 Z8 至 Z15 的顺序逐行填写；每行写明环境（本机、哪台模拟器或真机、debug 或 profile 构建）、提交或运行编号、原样输出与结论。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| 接手机与工具链（2026-11-27） | 本机；无运行编号 | 接手机是 Windows 11 的 QEMU 虚拟机，64 个逻辑处理器。按 codex 的工具链计划安装：VS Build Tools 2022 17.14（MSVC 14.44.35207、Windows SDK 10.0.26100.0；安装器退出码 3010，即“成功、要求重启”——没有重启，工具链经一次最小程序的编译、链接、运行验证可用）、Rust 1.99.1-x86_64-pc-windows-msvc（rustc 58cf8f9c3 2026-10-13；另装 `x86_64-linux-android`、`aarch64-linux-android` 两个目标）、Temurin JDK 17.0.21.1+2、Flutter 3.47.9（Dart 3.13.7）、Android SDK 36 与 build-tools 36.0.0、NDK 28.2.13676358、emulator 37.3.10.0、系统镜像 `android-35` 与 `android-29` 的 `default;x86_64`，AVD `ak_api35` 与 `ak_api29`；Python 用本机已有的 3.12.10。`flutter doctor -v`：`No issues found!`。`emulator -accel-check` 退出码 6（`Android Emulator hypervisor driver is not installed on this machine`，`HypervisorPlatform` 为 `Disabled`）；单独执行 `sc query aehd` 返回 `[SC] EnumQueryServicesStatus:OpenService FAILED 1060`（服务不存在）——两条诊断指向同一个缺失的驱动。模拟器没有启动。所有者 2026-11-27 授权启用加速之后，执行方按 codex 工具链计划的判定表先做只读判定：来宾 CPU 报告 VT-x、虚拟机监控程序模式扩展与 SLAT 可用；启动项里没有 `hypervisorlaunchtype` 这一项，按计划不改启动项；`systeminfo` 只报“检测到虚拟机监控程序”（计划写明单凭这一条不足以判成败）。随后执行 `Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform -All -NoRestart`：18:46:42 至 18:46:47（本机时间），状态由 `Disabled` 变为 `Enabled`，返回 `RestartNeeded: True`，另有一行 `Restart is suppressed because NoRestart is specified`；没有重启，重启由所有者安排。启用之后、重启之前再查，`emulator -accel-check` 仍是退出码 6、同一句“驱动未安装”——加速是否真的可用，要重启之后才能验证。本机没有 debug keystore，按红线没有生成，所以没有打任何 APK。过程中的失败（都保留）：安装 Python 时安装器返回成功，但按预期位置调用 `python.exe -VV` 找不到可执行文件——本机已登记着另一个 3.12.10，同版本的安装器什么都没做；随后改用本机已有的那个，没有另装。SDK 包的安装（阶段 C）跑过两次。第一次在安装 Build-Tools 时因 TLS 记录校验失败中断（`(bad_record_mac) Tag mismatch!`，另有两条下载清单的 IO 异常警告）——这一次的日志被后一次覆盖，按执行方当时的记录。第二次的留存日志里是两次 TLS 中断（`(bad_record_mac) Tag mismatch!` 一次、`(handshake_failure) Invalid CertificateVerify signature` 一次）与一条清单 IO 异常警告，都在重试后完成。建 AVD 的脚本在 `create avd` 一步报 `create ak_api35 failed`；随后手动创建成功，两个 AVD 都已就绪。Gradle 下载依赖时的中断记在“Flutter、Kotlin 与 Android 双 ABI”一行。 |
| 接手机的计算错误（金丝雀与基线对照） | 仓库之外的独立压力程序；本机 | 用工作区锁定的 `ed25519-dalek 2.2.0`、`blake3 1.8.7`、`ring 0.17.14` 多线程反复做“签名、校验有效签名、校验被篡改的签名、带密钥的哈希对参考值”。`ed25519-dalek` 把有效签名判为无效的次数（按执行方当时的运行输出；单线程 × 40000 的后两次、都是 0 的那两次，输出留有文件）：64 线程 × 3000 两次为 71、42；32 × 4000 为 68；16 × 6000 为 0；8 × 8000 为 32；4 × 10000 为 10；单线程 × 40000 三次为 85、0、0。观察到的 400 多次错误全部是“有效签名被拒”，“篡改签名被接受”为 0；`blake3` 与 `ring` 为 0。逐个逻辑处理器钉核扫描一次只在 4、10、16 号上出错，重扫全为 0——错误随时间成段发生，不固定在处理器上。TLS 同样受影响：`sdkmanager` 与 Gradle 下载时报 `(bad_record_mac) Tag mismatch!`、`(handshake_failure) Invalid CertificateVerify signature`。未改动的基线 `05e4773` 在改动任何 Rust 文件之前先跑过一次 `cargo test --locked -p ak-session -p ak-signal -p ak-ffi --no-fail-fast`：24 段结果里 6 段有失败，563 个通过、13 个失败——`ak-ffi --lib` 68 通过 3 失败、`ffi` 28 通过 1 失败、`ffi_pairing` 22 通过 2 失败、`pairing_e2e` 11 通过 2 失败、`task_pairing` 10 通过 1 失败、`ak-signal --lib` 111 通过 4 失败。基线对照：`05e4773` 的干净检出与送第二轮审核的那一版工作区（第一轮修改之后）各编出同样的测试程序，交替运行 `ak-ffi --lib` 6 轮与 `ffi_pairing` 3 轮——基线失败 4、5、2、8、6、5 与 3、6、4（合计 43 个失败用例），工作区 1、5、5、2、2、5 与 4、3、5（合计 32 个），两边失败的用例每次都不同。结论与边界：这台机器的测试结果只作参考，一次通过只说明那一次；本机的通过不能单独作为验收依据；CI 提供可靠环境里的追加证据，不改变本机门槛的要求。 |
| Z8 核对 | 阅读基线 `05e4773`；CI 运行 45913735858、45913842435、45913739823、45399604798（接口核实）；远端分支删除（`git push origin --delete`，没有提交号） | `git merge-base --is-ancestor origin/dev/batch-x-20261122 origin/main` 退出 0。远端 X 分支：所有者 2026-11-27 授权之后已删除——删除之前刷新引用，`origin/dev/batch-x-20261122` 与 `origin/main` 同指 `5b6ae31`，祖先检查仍退出 0，以它为头的 PR 一个都没有（任何状态），分支未受保护；`git push origin --delete dev/batch-x-20261122` 的输出是 ` - [deleted]         dev/batch-x-20261122`，之后 `git ls-remote --heads origin dev/batch-x-20261122` 无输出，远端的分支只剩 `dev/batch-z-20261124` 与 `main`；本地只有 `main` 与 `dev/batch-z-20261124` 两个分支、一个 worktree，没有闲置的可清理。接手文档的提交 `cd690ad` 在分支上的运行 45913735858 为 `cancelled`（被一分钟后下一次推送取代），其后 `05e4773` 的运行 45913842435 为 `success`。`main` 上 `5b6ae31` 的运行：接手之初按分支 `main` 与提交 `5b6ae31` 查询时没有找到，当时原因没有核实（工作流对 `main` 配置了 push 触发）；第十四轮审核之后重查，原因查明——`gh run list --commit` 用短提交号 `5b6ae31` 返回空，用完整提交号 `5b6ae31a8da2da46ec0b342229487058fee4be6b` 查到两个：`main` 上的 45913739823（push，2026-11-26T23:55:27Z 创建、2026-11-27T00:07:28Z 完成，`success`，12 个任务成功、2 个手动项跳过，`attempt` 为 1）与同一个提交在 `dev/batch-x-20261122` 上的 45399604798（2026-11-24T13:04:00Z 创建，`success`）。没有做的：原机的宿主脚本、JVM 检查、契约文本与设备证据都不在接手机上，未备份、未入库；154 的两个测试服务没有登录方式，停服未核实。 |
| Z9 计划 | `b549f82`；`docs/plans/2026-11-27-batch-z-z9.zh-CN.md` | 五问的回答在该文件 B 节：冻结范围内未发现依赖发送侧持久读授权的功能；临时授权是否够用是平台行为，待 Z10 在设备上裁定；原生接收的读写授权恢复到 Activity 主线程同步取得；只从过期清理里拆掉授权工作，实例令牌、`.owner`、定点清理与 ID 复用拒绝都保留；按符号的删除、保留、改写清单在 B.5。结论：进入 Z10，Z11 等 Z10 通过。同批存档的还有 Z11、Z12、Z14 的计划（四个文件已随 `b549f82` 提交）；Z11、Z12 的计划只存档、不执行。去敏时执行方的脚本把相对链接的层级写错过一级——按执行方当时的记录，由自写的链接检查发现后改正（那一次的失败输出没有留下来）；之后仓库检查通过。 |
| 项目规则修订 | `129afc4` | `AGENTS.md`「计划与审核规则」整节替换为所有者 2026-11-27 的规定：所有任务先计划、结束必审核、`gpt-6.2-astra` 与 `max`，2026-11-22 的同名规则作废；接手交接 §2.1 的两行与 STATUS 导语同步。codex 第一轮审核：规则修订通过。 |
| Z-N7 修前红例（最终夹具） | 当时的最终夹具，对着 `05e4773` 的校验语义；夹具随 `c2bccee` 提交 | 基线里没有 `verify_live_commit`：`05e4773` 的两端直接用原始的当前时间调 `verify_commit_full`。红例是临时把工作区的 `verify_live_commit` 换回那段语义（不核对 transcript、不做宽限判断、用原始取帧时刻）跑出来的，其余不动。`cargo test --locked -p ak-session --lib live_commit -- --nocapture`：`test result: FAILED. 0 passed; 2 failed`，两条是 `taken at D+1 ms: Err(Verify(Expired))` 与 `owner accepts the commit the registry made before the deadline: Verify(Expired)`，用时 0.19 秒；压力检查前后为 88、0。按发生顺序另记三次观察。第一版夹具（真实时间加 2 秒窗口）对着旧逻辑的红例报的是 `Timeout`，那是闸门放行时又扣了一次帧的夹具失误——这一次的日志被后一次覆盖，按执行方当时的记录。修正闸门之后、改夹具之前，用当时的夹具对着旧逻辑跑了一次：`test result: FAILED. 0 passed; 2 failed`，2.28 秒，表驱动用例报 `delivered at D+1 ms`（left false、right true），流程用例报 `owner accepts the commit the registry made before the deadline: Verify(Expired)`。codex 第一轮审核不接受真实时间的做法，改成只钉两个测试实例的取帧时刻之后，才是本行开头的那一次。 |
| Z-N7 修后绿例与回归 | 当时的各版工作区（版本见行内）；交付 `c2bccee` | 第一版（真实时间）夹具下，修后的绿例是 `test result: ok. 2 passed; 0 failed`、2.44 秒，与两个恢复用例同跑的一次是 `3 passed; 0 failed`；改成最终夹具之后，同一命令是 `2 passed; 0 failed`、0.32 秒，处理第二轮意见之后、第三轮送审之前又一次是 `2 passed; 0 failed`、0.30 秒。最终夹具里两端各扣住一帧、同属本轮 pairing id、墙钟早于截止、两端未落簿；取帧时刻钉在 `D + 1 毫秒`；放行后两端都到 `Applied`、互相落簿，加入方取回创建方的 endpoint。只跑这两个定向用例、重复 30 遍：24 遍两个都通过，6 遍失败——纯函数的表驱动测试一次报 `taken at D+0 ms: Err(Verify(PublicKeyMismatch))`，流程测试一次在连接时报证书签名无效、一次两端没有都收到提交帧、三次放行后 20 秒内没有结束；与本机的计算错误相符，记作参考。第三轮审核的修改之后又跑了 9 遍：1 遍在流程测试连接注册服务器时证书校验失败（日志没有区分是哪一端），原文是 `connect to the registry: Protocol("pinned TLS dial failed: unexpected error: rejected registry certificate: certificate is not self-signed or signature invalid")`，其余 8 遍两个都通过。`-p ak-signal --test protocol_v2_codec` 54 个通过，含 `pair_commit_wire_layout_byte_exact`；这样的定向运行共三次（第一版的逐字节断言、处理第一轮意见改写样本之后、处理第二轮意见之后），每次都是 54 个通过。早期把 `ak-session --lib` 整个重复过两组（当时 `live_commit` 的夹具还是真实时间的版本）：先跑 10 遍（不含两个 `live_commit` 用例），1 遍失败，是 `cancel_running_channel_task_drops_the_engine_first` 一次；再跑 30 遍，9 遍失败、共 11 个失败结果——两个恢复用例 5 次、两个 `live_commit` 用例 4 次（流程用例 3 次：两次两端没有都收到提交帧、一次放行后 20 秒内没有结束；表驱动用例 1 次：`delivered at D-1 ms`）、`cancel_running_channel_task_drops_the_engine_first` 2 次，后者都是证书与握手的校验失败。夹具重写之后的混合回归（`-p ak-session --lib -p ak-identity -p ak-signal --test protocol_v2_codec`，当时压力检查前后 73、69，按执行方当时的运行输出）：`ak-identity` 75 个通过、编解码 54 个通过，`ak-session --lib` 137 通过 1 失败（就是下面恢复用例的那一次），`ak-signal --lib` 111 通过 4 失败（`connections_beyond_the_global_cap_are_rejected_with_server_error`、`fair_share_breaker_admits_the_twenty_first_source`、`fair_share_serving_cap_admits_a_newcomer_past_the_soft_cap`、`over_cap_flood_beyond_the_reject_budget_is_dropped_immediately`——三个是 TLS 校验失败、一个是读帧失败，与未改动基线上失败的是同样四个）。两个恢复用例在本机没有稳定通过，修后的失败与通过都记在这里：`crashed_redeemer_recovers_without_code_via_the_pinned_transcript` 失败 4 次、通过 42 次——失败是第一版修复之后的回归运行里 1 次、夹具重写之后的回归运行里 1 次、早期 30 遍重复里 2 次，其中 3 次是 `redeem to commit: Verify(PublicKeyMismatch)`、1 次是 `connect to the registry: ConnectionLost`；`recovery_without_code_or_pin_is_refused_not_degraded` 失败 3 次、通过 42 次——失败都在早期 30 遍重复里，都是 `redeem to commit: Timeout`。修前基线的第一次运行里两个用例各有一次通过，不记在修后的数字里。两个用例的可靠证据取自推送之后的 CI：Ubuntu 与 Windows 上都是 `ok`（见“推送与 CI”一行）。 |
| Z-N6 诊断 | 当时的各版工作区（诊断改过几版，见行内）；交付 `118c3e2` | 只加了诊断，产品代码未改：`cancelled_single_receive_port_rebinds_immediately` 在等不到 `listening`、或任务提前结束时，带上 `ak_job_result` 的返回码与原样 JSON（ABI 负码附 `ak_engine_last_error`）和已收事件，并解析出实际监听地址、核对端口。本机复现情况：最初版的诊断下单独运行 40 次全部通过；第一轮审核之后改过的诊断下定向通过 1 次；整个 `ffi` 测试程序限制在 4 个处理器上运行 20 遍，这个用例 0 次失败（另有 3 遍是别的用例因证书签名校验失败）；最终版诊断下定向通过 2 次（第三轮送审前一次、第四轮一次）。结论：“是不是绑定失败”仍未证实，Z-N6 保持诊断中。没有做的：Ubuntu 上定向重复 100 次——本机没有 Ubuntu，CI 也不做定向重复；`662c964`、`7e6842b`、`75e5bf2` 与 `c452a29` 的 CI 在 Ubuntu 上各随全工作区测试跑了这一用例一遍，都是 `ok`（见“推送与 CI”一行）。 |
| Z-N8 方案评估 | `b549f82`；`docs/design/pairing-deadline-clock-skew.zh-CN.md` | 评估已成文：现状各环节的规则与对偏差的含义，方向 A（以服务器时间为基准）与方向 B（放宽兑换窗口、固定时刻给统一失败）的改法、难点与对照，以及需要所有者回答的四个问题。不改协议、不改服务器。没有做的：走查脚本里的“设备与宿主时间差”核对（脚本原件不在接手机上）、它的自检、带核对的设备轮。 |
| 本机门槛（本批历次工作区的累计，按时点） | 本批各版工作区（都基于 `05e4773`，时点见行内）；本机；交付 `c2bccee`、`118c3e2` | 这一行的数字横跨几个版本的代码，不是同一版；三个时点都运行在基于 `05e4773` 的未提交工作区上，没有留存运行当时的文件快照或哈希。按五个文件的最后写入时间（最晚的是 14:30:53，早于第四轮送审的 14:35:25）与送审记录（第四轮的送审记录列着最后两处代码调整，第五至第十轮的都写明只改文档）判断，时点三的改动与 `c2bccee`、`118c3e2` 交付的内容相符。**时点一**是第一版修复送第一轮审核之前；**时点二**是处理第二轮意见之后、送第三轮审核之前；**时点三**是处理第三轮意见之后（代码只比时点二多一处注释和一个测试辅助函数）。**时点一**：`cargo fmt --all -- --check` 最初报出执行方手写代码的排版差异，经 `cargo fmt` 修整后通过；`cargo clippy --locked --workspace --all-targets -- -D warnings` 通过（16 秒）；`RUSTDOCFLAGS=-D warnings cargo doc --locked --workspace --no-deps` 通过（16 秒）；`cargo test --locked -p ak-store --features sqlite` 54 个通过。**时点二**：`cargo fmt --all -- --check` 通过；全工作区的 Clippy 通过（9 秒）；rustdoc 通过（7 秒）；`cargo test --locked -p ak-store --features sqlite` 又一次 54 个通过；T03 的 `sender_stun_and_probe_round_share_the_adopted_dial_socket -- --exact --nocapture` 两次里一次通过（`test … ok`，30.07 秒；两次都没有 `skipping:`，失败的一次是 `no line containing "receiver bound" within 10s`）；`cargo build --locked -p ak-ffi --release` 之后的 `dart test`（`ak_ffi_dart`）172 个通过。**时点三**：`cargo fmt --all -- --check` 通过；`ak-session`、`ak-signal`、`ak-ffi` 三个 crate 的定向 Clippy 通过；Z-N7 的两个定向测试 9 遍里 8 遍通过（见上面绿例一行）；Z-N6 的定向测试通过。**累计**：定向 Clippy 有四份完成记录——最早一份（时点一）是 `ak-session` 与 `ak-signal` 两个 crate，之后三份（每轮修改之后）是 `ak-session`、`ak-signal`、`ak-ffi` 三个。`python scripts/check_repository.py` 在各版工作区上都跑过、都通过，截至第八轮裁决时留存十份（更早几份的链接数是 918、919、932、933，最近几份是 113 个 Markdown、934 个本地链接、219 行状态表、errors 为空），此后每次修订又各复跑过；独立运行的 `python scripts/check_contracts.py` 留存七份（截至第八轮裁决），每次都是 5/5；`git diff --check` 每次都通过。**没有在本机通过的**：`cargo test --locked --workspace --no-fail-fast` 跑过两次——时点一那次 69 段结果（含 12 段 doctest）里 13 段有失败，1205 个通过、36 个失败，其中 `ak-cli` 的单元测试 142 个通过、5 个失败；时点二那次 11 段有失败，1213 个通过、28 个失败，其中 `ak-cli` 144 个通过、3 个失败。`cargo test --locked -p ak-signal --features server-persistence` 从时点一到时点二之间的各个版本上共十二次有效运行，一次都没有通过：最早一次没带 `--no-fail-fast`，第一个测试目标就停下（110 通过、5 失败）；随后用重试脚本跑的八次分别是 215 通过 7 失败、214 通过 8 失败、216 通过 6 失败、217 通过 5 失败、214 通过 8 失败、218 通过 4 失败、216 通过 6 失败、216 通过 6 失败；最后一组三次是 218 通过 4 失败、215 通过 7 失败、218 通过 4 失败。重试脚本最初六次调用因为执行方传参的写法有误而没有跑起来（退出码 1），记作脚本调用错误。逐个重跑三轮（时点一之后、第一轮审核之前，代码仍是时点一那一版）：第一轮全过的有 `ak-net --lib`（49 个）、`pairing_e2e`（13 个）、`attack_recovery`（6 个）、`dual_scheduler_modes`（3 个）、`invite_pairing_security`（5 个）与默认特性下的 `ak-signal --lib`（50 个，只覆盖默认特性下的用例）；`send_admission_cli` 三轮依次是 4 通过 2 失败、5 通过 1 失败、6 通过 0 失败；三轮之后仍不稳定的有 `ak-ffi --lib`（66 通过 5 失败、62 通过 9 失败、67 通过 4 失败）、`ffi_pairing`（20/4、20/4、21/3）、`invite_pairing_cli_e2e`（3/2、4/1、4/1）与 `t03_ipv6_loopback`（9/3、11/1、8/4）。`ak-signal --test server_v2` 的三次观察要连起来看：时点一那次全工作区运行里它是 24 通过、2 失败（`prepared_with_a_bad_signature_is_rejected_then_retried`、`commit_wins_against_a_late_abort`，已计入那一次的 36 个失败）；逐个重跑的脚本没有启用 `server-persistence`，而这个测试文件整个受该特性控制，所以那次重跑实际执行了 0 个用例、退出码 0，脚本据此把它移出了后面的轮次——它并没有覆盖原先失败的两个用例；时点二那次全工作区运行里它是 26 通过、0 失败。那一轮重跑的脚本对 `ak-cli` 用错了目标（`--lib`，连续三次都以退出码 101 结束），所以没有重跑它——用正确的 `--bins` 目标单独补跑过一次，145 通过、2 失败，当时的压力检查出错 79 与 33 次（这两个数按执行方当时的运行输出，没有写进 Cargo 日志）。首次失败与各次结果都保留在本机日志里；失败的报错多数是签名与证书校验失败及其连带结果（证书签名无效、公钥不匹配、“配对目标不可用”、握手失败），另有连接丢失、等不到监听事件与子进程启动超时。**本机执行过但没有通过的**：`python -m unittest discover -s scripts/tests`，382 个里 242 个失败，全部落在三个 Linux shell 脚本的自测模块（`scripts/` 没有改动）；原因都是当时自测进程在 PATH 上找不到名为 `bash` 的命令（本机的 Git 里只有 `sh.exe`），报错有三种同类文字——205 个是 `Bash is required for these regressions`，11 个是 `Bash is required for lifecycle regressions`，26 个是 `Bash is required for runner contract tests`——这是 Windows 上的结果，不算有效运行；Ubuntu 上的有效运行取自推送之后的 CI（见“推送与 CI”一行）。另记：时点一那次的门槛脚本的汇总计数器出过错，把已经跑过的工作区测试、第一遍 `server-persistence` 与那一次的 `ak-store --features sqlite` 记成了零，解析空的 fmt 输出时还抛过一次异常（fmt 自身退出 0）；那个脚本用 Windows PowerShell 把 cargo 写到标准错误流的进度行包成了 `NativeCommandError` 记录，不是失败。上述 Rust 测试的通过数与失败数取自逐项的 Cargo 日志。 |
| 提交之后的本机门槛 | `ff9dc0a`（五个提交之后的干净树）；本机 | 五个提交落地之后、工作区干净时，重跑了直接覆盖 `c2bccee`、`118c3e2` 的几项：`cargo fmt --all -- --check` 通过（3.4 秒）；`cargo clippy --locked --workspace --all-targets -- -D warnings` 一次通过（Cargo 报 6.87 秒）；`RUSTDOCFLAGS=-D warnings cargo doc --locked --workspace --no-deps` 一次通过（Cargo 报 6.42 秒）；Z-N7 的两个定向用例（`-p ak-session --lib live_commit`）5 遍，每遍 `2 passed; 0 failed`（0.29 至 0.35 秒）；`-p ak-signal --test protocol_v2_codec` 5 遍，每遍 `54 passed; 0 failed`；Z-N6 的定向用例（`-p ak-ffi --test ffi cancelled_single_receive_port_rebinds_immediately -- --exact --nocapture`）5 遍，每遍 `1 passed; 0 failed`（0.41 至 0.45 秒，另有 28 个被过滤）。门槛跑完后没有已跟踪文件被改动。边界：同一时段的压力检查（16 线程 × 5000，每项前后各一次）36 次里 29 次不为 0（5 至 128），计算错误当时仍在发生，这些通过只说明那几遍，仍只作参考；全工作区测试、`server-persistence`、T03 与 Dart 这一次没有重跑，它们的记录见上一行（时点一至时点三）。 |
| Flutter、Kotlin 与 Android 双 ABI | 从 `05e4773` 建立的检出（Flutter、Dart、Kotlin 的源码）；送第三轮审核的那一版工作区（后次重建的宿主 DLL、Android 库；交付 `c2bccee`） | 从 `05e4773` 建立的检出上（Flutter、Dart、Kotlin 的文件在本轮没有改动；运行结束时它有 6 个已跟踪文件显示为改动，核对后只是换行符差异）跑过两组，包装脚本逐次覆盖同名的日志，所以两组的证据来源不同。留存的那一组摘要里：`flutter analyze` 为 `No issues found!`；`flutter test` 为 `506: All tests passed!`；`dart test`（`ak_ffi_dart`）首遍退出 1、第二遍通过（`172: All tests passed!`）。更早的一组按执行方当时的记录，`flutter analyze`、`flutter test` 与 `dart test` 都是首遍通过，这一组的逐项日志没有留下来。这两组的 Dart 测试加载的宿主 `ak_ffi.dll` 都是从当时的工作区（当时未提交）复制过去的，不是 `05e4773` 构建的；后来在送第三轮审核的那一版工作区（第二轮修改之后）重建宿主 DLL 之后的 172 个通过另有日志（见“本机门槛”一行）。`gradle :app:compileDebugKotlin` 首组三次、第二组前六次都因下载依赖时 TLS 校验失败而中断（据 Gradle 守护进程的日志，九次里 `bad_record_mac` 依次是 1、1、4、1、1、3、6、1、1 处；后两次表现为具体任务失败——`:android_file_picker:compileDebugKotlin`、`:app:mergeDebugAssets`——根因相同），第二组第七次通过（112 秒，0 处），Gradle 模型报 `compileSdk=36 buildTools=36.0.0 ndk=28.2.13676358 minSdk=24 targetSdk=36`；过程里没有生成 debug keystore。送第三轮审核的那一版工作区（第二轮修改之后；此后对 Rust 的改动只有一处注释和测试代码）上 `scripts/build_android_ffi.sh --api 24`：最初三次失败在执行方启动 shell 的方式上：没有把 MSYS 的工具目录放进 PATH（`dirname: command not found`、`grep: command not found`），脚本定位仓库根的那一步随之算错，cargo 报 `error: could not find Cargo.toml`，退出码 101；在 shell 里先进入仓库、再跑脚本之后三次都是两个 ABI 编出、脚本自带的导出检查通过，原文是 `==> symbols verified: ak_engine_new ak_identity_info ak_jobs_list ak_pairing_start ak_sched_listen_start ak_task_start ak_task_create_code ak_task_join ak_task_send ak_task_list ak_task_disconnect ak_task_set_save_dir ak_task_consent ak_task_set_event_notify present in both .so`，随后在哈希一步以退出码 127 结束——`scripts/build_android_ffi.sh: line 231: sha256sum: command not found`（本机的 Git 里没有这个工具）。大小与 SHA-256 改用系统工具计算：`x86_64-linux-android` 9707552 字节、`729d92f05e4fbfbdd739383f64388ca3a224d74c15355f3bd749d4091fe61de3`；`aarch64-linux-android` 9735712 字节、`c8db39ff6c393948ca38fd15159e1897ee2fac635a099045d53302b92dd4fb28`；`jniLibs` 里的两份副本与之逐字节相同。执行方的脚本另有三次失误（Flutter 的路径少写一级、函数的返回值被日志行污染、Gradle 属性文件里转义的冒号没有去掉），都在当时改正后重跑，不属于门槛本身。边界：这两份库包含本轮的 Z-N7 修复，但没有装进任何 APK，也没有在设备上运行过。 |
| codex 结束审核 | 审核对象：第一至十轮是当时的工作区（基于 `05e4773`，未提交），第十一轮起是各提交的实际差异；本机 codex 会话，`gpt-6.2-astra`、`max`、只读 | 第一轮：需改。规则修订通过；Z-N7 的流程夹具不稳，真实时间的做法不接受，超时优先级、线格式样本、时间保证的表述要改；Z-N6 的诊断有竞态；Z-N8 五处表述；归档说明与重复的运行号。第二轮：需改。前述各项已落实；剩 Z-N6 诊断的两个竞态（超时分支读结果之后没有再排空、ABI 负码的错误文本会被后续调用清空）、`verify_live_commit` 的注释把受理采样写成了实际提交时刻、契约与评估里的两处措辞；并裁定“复审后直接推送、靠 CI 收口”不满足前提，做不了的专项要逐项记未执行，分阶段路线要所有者明确接受。第三轮：需改。三个技术阻断已处理；登记里的事实错误与过宽表述要改（`5b6ae31` 没有 CI 运行的原因未证实、红例的基线归属、Python 自测的状态与链接数、导出检查的原文、Z-N7 的“稳定”措辞、STATUS 的重复、计划补注与推送前提的冲突），另有一处 `recover` 的旧注释要收窄。第四轮：需改。两处 Rust 问题（`recover` 的旧注释、`joiner_state` 会丢掉外层原因）已关闭；要改的是登记——不同时点的运行混在一起、一次 TLS 失败被写成了“加入方”、Dart 产物的来源与各次重试、状态表的提交号归属，以及几处阶段用语。第五轮：需改。`server-persistence` 的运行次数记少了（漏了更早的八次重试与最早的一次）；一个恢复用例的多次失败被写成了“报过一次、其余通过”；Z-N2 行提前写了“已去敏入库”；早期平台运行的证据来源要加限定。提交划分与回填映射可以采用。第六轮：需改。首个恢复用例的修后通过数少算了一次（修后应为 42 通过 4 失败；第二个原为 42 通过 3 失败）；还漏记了几组失败——未改动基线的第一次运行、早期 `ak-session` 整个重复时的失败、夹具重写后混合回归里 `ak-signal` 的四个失败；另要补上红例先后顺序里的一步，并注明早期门槛脚本的计数器出过错。第七轮：需改。`server_v2` 的一次零用例重跑没有与前后两次连起来写，工具链一行的两次操作失败漏记；另补全门槛脚本误零的范围、错误目标的调用次数、Z-N7 的几次独立绿例与编解码的运行次数。第八轮：需改。两次 Z-N7 的成功运行被归到了较晚的代码版本，SDK 中断的次数超出了留存日志能证实的范围，几次数得清的成功运行被合写成了一次“通过”，两处引文要限定证据范围。第九轮：需改。“本机门槛”一行把跨几个代码版本的 Rust 运行归进了两个时点，独立契约检查的复跑次数与部分数字的来源写得过宽，“本机没有 bash”的说法不准。第十轮：批准。第九轮点名的四处与自查的三处版本标注都已落实；codex 复跑了仓库检查（113 个 Markdown、934 个本地链接、219 行状态表、errors 为空）、契约检查（5/5）与 `git diff --check`，都通过，没有重新运行 Rust 门槛，没有发现新的仓库内容问题；裁决是可以在本地提交，在所有者对分阶段路线表态之前，批准只覆盖本地提交。另记（审核环境，不计为仓库缺陷）：codex 报告它的一次计划调用因找不到 home 目录没有启动，只读审核照常完成。第十轮批准后按五笔在本地提交：`c2bccee`（Z-N7）、`118c3e2`（Z-N6 诊断）、`129afc4`（项目规则）、`b549f82`（Z-N8 评估与四份计划），第五笔是本表的登记。第十一轮（对象是第五笔 `ff9dc0a` 的实际差异与前四笔的划分）：需改。五笔的划分、状态表的提交号和三项复跑都核对通过；要改的是“本机门槛”一行对历史源码版本的断言超出了证据（时点三运行在未提交的工作区，没有留存当时的文件快照或哈希，“代码就是这两个提交里的代码”写得过满），另有三处措辞——“Flutter、Kotlin 与 Android 双 ABI”一行提交栏的宿主 DLL 应限定为后次重建的那一个，状态表 Z14 行的 Z-N6 应限定为 Ubuntu 用例，Z15 行应直接指向本表、并把 154 的处置写明是手机测试之后移除。第十二轮（对象是追加提交 `56e994a` 的实际差异）：批准。第十一轮的一项 P2 与三项 P3 都已落实；提交之后那几项 Rust 门槛的版本、时点和数字与留存材料相符；codex 复跑了仓库检查（113 个 Markdown、934 个本地链接、219 行状态表、errors 为空）、契约检查（5/5）、`git diff --check` 与 `git diff --check ff9dc0a 56e994a`，都通过；没有新的仓库内容问题。另记（审核环境，不计为仓库缺陷）：codex 报告它额外启动的一个计划子进程先报找不到 home，重试时 app-server 初始化返回“拒绝访问（os error 5）”，复审与四项检查由当时的只读会话完成。第十二轮复审时，分支上是六笔本地提交：前五笔之外，第六笔 `56e994a` 登记第十一轮的修订与提交之后的本机门槛。第十三轮（对象是登记第十二轮裁决的追加提交 `662c964` 的实际差异）：批准。第十二轮的裁决与理由登记准确，对 `56e994a` 的复审已从待办移出；codex 复跑了仓库检查（113 个 Markdown、934 个本地链接、219 行状态表、errors 为空）、契约检查（5/5）、`git diff --check` 与 `git diff --check 56e994a 662c964`，都通过；没有新的仓库内容问题。另记两点裁定：在所有者没有作答时不推送，符合“推送要等所有者对分阶段路线表态”的要求；“本轮裁决在给所有者的回复里报告、随下一笔提交入库”的收口方式可以接受，不必为登记裁决另起一笔。第十四轮（这一轮接手工作在分阶段路线下的最终结束审核；对象是登记提交 `7e6842b` 的实际差异与 CI 运行 45984768383 的证据）：需改。CI 的每个数字都与原始日志相符；45984768383 可以作为 Z-N7 的两个回归用例、两个恢复用例、两个平台的 `server-persistence` 与全工作区测试、Ubuntu 上 Python 自测（带 62 个跳过）的可靠证据，本机没有全部跑通的事实保留充分；Z14 保持进行中、Z15 保持未开始恰当，可以写“安卓这条线等所有者重启并提供现有的 debug keystore”；接受“先在回复里报告、再随下一笔提交入库”的收口方式，但每次推送的 CI 记录都要入库。要改的都在登记文字：Z8 明确要求回填的 `main` 上 `5b6ae31` 的运行漏了——执行方当时用短提交号查询，返回空（P2）；STATUS 把“Windows 功能已启用”写成了“模拟器加速已启用”，Z14、Z15 与 STATUS 的“推送之后的 CI”没有点明是 `662c964`，Z-CI 行里“带上第五轮修复的那次推送的运行要在 GitHub 上读”是过时的待办，待办里 T03 的专项没有写明是“Linux 上”（P3）。另记（不计为仓库缺陷）：codex 那边 `git ls-remote` 遇到 `SEC_E_NO_CREDENTIALS`、`gh pr list` 返回 401，改用只读的 REST 接口核实；执行方留存的两个 macOS 可移植性日志，文件名把架构写反了（已改名）；送审清单里列的 `virt-check.txt` 不存在，启用加速之前的状态有 `install-stage-D.log` 与 `whpx-diagnose.txt` 为证。codex 的第一次调用没有跑起来——模型提供方连续返回 `502 Bad Gateway: Upstream access forbidden`，重连 5 次都失败；用同一配置重试后完成。第十五轮（对象是记录第十四轮裁决的追加提交 `75e5bf2` 的实际差异）：需改。第十四轮点名的事项都已处理，四项检查通过；要改的是新加文字里的三处——T03 的 `ok` 少记了一次、把只改文档的 `662c964` 称作“代码提交”（应写“推送分支头”）、“两次 CI 逐项相同”说宽了（两次上传的制品大小不同）（P3）；另把推送之后 Windows 上的那次 CI 失败列为待归因的新事项（P2），要求登记两次尝试并保留第一次的失败。另记（审核环境）：codex 那边直接下载日志的接口返回 HTTP 403，它核对的是执行方留存的原始日志。第十六轮（对象是记录第十五轮裁决的追加提交 `c452a29` 的实际差异）：需改。两次 CI 尝试与失败现场的登记和留存材料一致，只重跑失败的任务处理恰当；要改的是——执行方对 CI 历史的扫描跳过了最终结论为成功的运行，漏掉了“第一次尝试失败、重跑后转绿”的那些，据此写下的“此前只失败过两次”“这两个用例没有失败过”“再出现才立项”是错的：批次 Q 的运行 44691268201 第 1 次尝试里，`probe_round_observes_the_real_advertised_candidates` 已以同一句超时失败过，即状态表的 Q-N3，这次是复发，应按 Q-N3 的约定查因（P2）；对 `0x2101` 的源码解释过宽——码有效而所有者离线时服务器仍受理兑换，到期未完成就中止并通知双方，已提交的事务收到中止请求返回的是 `REVISION_CONFLICT`，两个用例同因只能留作假设；Z14、Z-N6 少算一次 Windows 执行（所列三个提交下该用例 Ubuntu 3 次、Windows 4 次 `ok`），状态表只写两个平台都有通过记录，分平台的次数放 §8；STATUS 的“最终全部通过”要点明提交范围（P3）。另记（审核环境）：codex 那边 `git ls-remote` 报 `SEC_E_NO_CREDENTIALS`，末次直接查询遇到 GitHub API 限额 HTTP 403，`c452a29` 自身的 CI 终态与计数是从执行方留存的任务清单核实的。 |
| 推送与 CI | 分支头 `662c964`、`7e6842b`、`75e5bf2` 与 `c452a29`；CI 运行 45984768383、45987622997、45992565932（两次尝试）、45997650618（接口与各任务日志核实） | 所有者 2026-11-27 接受分阶段路线（先推送取得 CI 证据、后补专项证据）。推送前核对：分支是 `dev/batch-z-20261124`、工作区干净、远端分支头仍在 `05e4773`、可以快进。`git push origin dev/batch-z-20261124` 的输出是 `05e4773..662c964  dev/batch-z-20261124 -> dev/batch-z-20261124`；推送后 `git ls-remote` 的分支头是 `662c964c3df5d4a74e7a8b3b6469067ad2ff2765`。CI 运行 45984768383（push；10:45:24Z 创建、10:57:09Z 完成，都是 UTC）的 `headSha` 就是这个提交，结论 `success`：12 个任务成功，两个只能手动触发的深度测试按设计跳过，没有失败，没有重试。各任务里该跑的步骤都是 success（Windows 上多文件接收那一步按条件跳过）；`cargo deny` 的日志里另有一行 rustup 的报错，见本行末。逐项，数字取自各任务的日志。**Rust（ubuntu-latest 与 windows-latest）**：格式、Clippy、rustdoc 在两个平台上都通过；全工作区测试在两个平台上都是 69 段结果、1241 个通过、0 个失败、11 个忽略（只在 release 下跑的用例），其中 Z-N7 的 `live_commit_enforces_grace_and_expected_transcript` 与 `live_commit_delivered_after_deadline_applies_on_both_sides`、`crashed_redeemer_recovers_without_code_via_the_pinned_transcript` 与 `recovery_without_code_or_pin_is_refused_not_degraded`、`cancelled_single_receive_port_rebinds_immediately`、`sender_stun_and_probe_round_share_the_adopted_dial_socket` 都是 `ok`；`server-persistence` 在两个平台上都是 5 段、222 个通过、0 个失败（含本机失败过的 `prepared_with_a_bad_signature_is_rejected_then_retried` 与 `commit_wins_against_a_late_abort`）；`ak-store --features sqlite` 是 Ubuntu 52 个、Windows 54 个通过；256 个文件描述符上限下的多文件接收只在 Linux 上跑，1 个通过。**Repository checks**：仓库检查通过（113 个 Markdown、934 个本地链接、219 行状态表、errors 为空）；`python -m unittest discover -s scripts/tests -v` 是 `Ran 382 tests in 87.793s`、`OK (skipped=62)`——跳过的 62 个都是与旧提交对照的用例（`test_deploy_linux_install` 的 BaselineContrastTests 与 `test_ssh_helper_exec` 的 OldHelperFromGitTests），原因同为 `git baseline <提交号> not available locally`，因为 CI 的检出只有一层（`--depth=1`）；本机因找不到 bash 而失败的 `test_nat_matrix_lifecycle`、`test_netem_benchmark` 在这里的结果行全是 `ok`，`test_deploy_linux_install` 里不依赖旧提交的 DeployInstallTests 也都是 `ok`。**Flutter**：`flutter analyze` 是 `No issues found!`，`flutter test` 是 506 个通过，`dart test`（`ak_ffi_dart`）是 172 个通过。**Android**：双 ABI 的 `libak_ffi.so` 编出，导出检查的原文与本机一致；`x86_64` 9704472 字节、SHA-256 `3d5ac2bb26e0d152ee47b8e506ee2e25457da2c95d8a3694eb58df2bc1df9fb8`，`arm64-v8a` 9735472 字节、SHA-256 `0d45116914a76d7a607aed6bb14af73a21817ca92d4bc609e4f18e416238aa03`（与本机构建的大小、哈希都不同，原因执行方没有查）；两个 ABI 都没有 `AK_EXP_` 字符串；`source commit` 是 `662c964c3df5d4a74e7a8b3b6469067ad2ff2765`；`flutter build apk --debug` 生成了 `app-debug.apk`，作为构建产物上传（产物 75032612 字节，保留 14 天）。**其余**：旧明文信号（`legacy-plaintext-signal`）的 `cargo check` 通过、`cargo test` 是 5 段 205 个通过；NAT 矩阵 8 个用例都是 PASS（含“对称 NAT 对对称 NAT 必须干净地失败”的一项），汇总是 `all runnable matrix cases PASS`；`cargo audit` 载入 1571 条通报、扫描 227 个依赖，没有报出漏洞；`cargo deny` 是 `advisories ok, bans ok, licenses ok`；`x86_64-unknown-linux-gnu`、`aarch64-apple-darwin`、`x86_64-apple-darwin` 三个目标的 `cargo check` 通过。另记：`cargo deny` 任务的日志里有一行 `error: override toolchain '1.99.1-x86_64-unknown-linux-musl' is not installed`，它出现在容器里 rustup 自动安装工具链的时候，随后三项检查都是 ok；基线 `05e4773` 的运行 45913842435 里，同一任务有同一行。**登记提交自己的运行**：登记提交 `7e6842b`（只改三份文档）推送之后，CI 运行 45987622997（push；11:05:57Z 创建、11:17:14Z 完成，UTC）的 `headSha` 是 `7e6842b0ddd764b3b26c858fae5160fa95386ba4`，结论 `success`，12 个任务成功、2 个手动项跳过，没有失败，没有重试。从它的日志重新汇总，下面列出的计数与两份 `libak_ffi.so` 的大小和哈希都与上面这一次相同：Rust 两个平台的全工作区、`server-persistence`、sqlite 与多文件接收，旧明文信号的 5 段 205 个，仓库检查的 113、934、219，Python 的 382 个与 62 个跳过（这次是 `Ran 382 tests in 111.114s`），Flutter 的 506 与 172，NAT 矩阵的 8 个 PASS，`cargo audit` 的 1571 与 227，两份 `libak_ffi.so` 的大小与哈希；点名的几个用例在两个平台上也都是 `ok`，Android 的 `source commit` 是 `7e6842b0ddd764b3b26c858fae5160fa95386ba4`，`cargo deny` 的日志里同样有那一行 rustup 的报错；两次上传的 `app-debug-apk` 制品，最终大小分别是 75032612 与 75032614 字节（取自上传那一步的输出）。**记录第十四轮裁决的提交 `75e5bf2`（同样只改三份文档）的运行，两次尝试**：CI 运行 45992565932（push；11:41:43Z 创建，UTC）的 `headSha` 是 `75e5bf273973b23277b4ca5f59bbfc8c20cae787`。第 1 次尝试在 11:50:27Z 以 `failure` 结束——11 个任务成功、1 个失败、2 个手动项跳过，失败的是 Rust（windows-latest）的全工作区测试一步，失败的用例见下一行；同一次尝试里 Rust（ubuntu-latest）成功，全工作区 69 段、1241 个通过、0 个失败、11 个忽略，`t03_ipv6_loopback` 12 个全过（95.97 秒），多文件接收 1 个、sqlite 52 个、`server-persistence` 5 段 222 个通过；其余 10 个任务成功，这一次的计数执行方没有逐项重新汇总。执行方随后只重跑了失败的任务（`gh run rerun 45992565932 --failed`，是执行方的操作，不是自动重试）：第 2 次尝试里 Rust（windows-latest）在 12:08:25Z 成功——全工作区 69 段、1241 个通过、0 个失败、11 个忽略，`t03_ipv6_loopback` 12 个全过（97.55 秒），sqlite 54 个、`server-persistence` 5 段 222 个通过，rustdoc 通过；整个运行在 12:08:26Z 以 `success` 结束（第 2 次尝试，12 个任务成功、2 个手动项跳过）。GitHub 上这次运行现在显示的是第 2 次尝试的结论，第 1 次的失败只在尝试历史里。**记录第十五轮裁决的提交 `c452a29`（只改三份文档）的运行**：CI 运行 45997650618（push；12:19:19Z 创建、12:31:14Z 完成，UTC，第 1 次尝试）的 `headSha` 是 `c452a29080f219d81f53afb43bec15257b638f0a`，结论 `success`，12 个任务成功、2 个手动项跳过，没有失败，没有重试。执行方只重新汇总了两个 Rust 任务：Windows 是全工作区 69 段、1241 个通过、0 个失败、11 个忽略，`t03_ipv6_loopback` 12 个全过（98.72 秒），sqlite 54 个、`server-persistence` 5 段 222 个通过；Ubuntu 是全工作区 69 段、1241 个通过、0 个失败、11 个忽略，`t03_ipv6_loopback` 12 个全过（95.68 秒），多文件接收 1 个、sqlite 52 个、`server-persistence` 5 段 222 个通过；其余任务这一次的计数没有逐项重新汇总。边界：这些是提交 `662c964`、`7e6842b`、`75e5bf2` 与 `c452a29` 上的 CI 运行。点名的几个用例按平台计——Z-N6 的 `cancelled_single_receive_port_rebinds_immediately` 在 Ubuntu 上 4 次 `ok`、Windows 上 5 次 `ok`（`75e5bf2` 的两次尝试各一次）；Z-N7 的两个回归用例与两个恢复用例在 Ubuntu 上各 4 次 `ok`、Windows 上各 4 次 `ok`（`75e5bf2` 的第 1 次尝试在全工作区测试停下之前没有轮到它们）；`sender_stun_and_probe_round_share_the_adopted_dial_socket` 在 Ubuntu 上 4 次、Windows 上 5 次 `ok`。专项仍然没有做：Z-N6 的 100 次定向重复；T03 在 Linux 上的 `--exact --nocapture` 与“没有 `skipping:`”的核对——Linux 上只有全工作区测试的 `ok`，而这个用例在回环 UDP 绑定失败时会打印 `skipping:` 并正常返回（`crates/ak-itests/tests/t03_ipv6_loopback.rs` 的 `sender_stun_and_probe_round_share_the_adopted_dial_socket`），一个 `ok` 完不成这项核对；Z-N6 保持诊断中。CI 的 debug 包是没有打试验补丁的应用的 debug 构建，不是 Z10 的试验包，也不是设备轮的 profile 包。 |
| 推送之后 Windows 上的一次 CI 失败（用例②是 Q-N3 的复发；用例①待归因） | CI 运行 45992565932 的第 1 次尝试（提交 `75e5bf2`） | 失败的是 `crates/ak-itests/tests/t03_ipv6_loopback.rs` 这个测试程序，原文是 `test result: FAILED. 10 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 138.19s`。CI 的全工作区测试没有带 `--no-fail-fast`，cargo 在第一个失败的测试程序处停下，那一步只留下 26 段结果（559 个通过、2 个失败、8 个忽略），排在后面的测试程序这一次没有跑。**用例②：`probe_round_observes_the_real_advertised_candidates`——Q-N3 的复发。** 日志里先有 `has been running for over 60 seconds`，最后在 `t03_ipv6_loopback.rs` 第 346 行超时，原文 `no line containing "the peer wants to send" within 120s`；加入方（发送端）的标准错误是 `Error: unknown task code (or expired, revoked, owner offline)` 与 `Caused by:` 下面的 `pairing target unavailable (0x2101)`；注册服务器的日志里是 11:48:05Z 启动、11:48:06Z 监听并有两个设备上线、11:48:23Z 一个设备断开，没有任何“拒绝”的记录。批次 Q 的运行 44691268201 第 1 次尝试（提交 `29532ee`，Windows）里，同一个用例以同一句超时失败过——当时报在第 324 行，报错里只有接收端的输出——重跑后通过，即状态表的 Q-N3；`5245ea3` 之后约定“再出现时按新的报错查因”，这次多出来的就是上面发送端与注册服务器的输出。处置：按 Q-N3 的约定查因——执行方在本笔成文之前已请 codex 出只读的查因计划（成文时还没有出具），出具之后另行登记并送审；根因在计划与证据到位之前仍未查明，与批次 Q 那次是否同因也待查。**用例①：`receiver_punch_reaches_the_sender_advertised_reflexive_port`——独立的观察，待归因。** 在 `t03_ipv6_loopback.rs` 第 1669 行断言失败，原文 `the receiver's punch must arrive from its bound QUIC port 62085`；这条断言只证明 30 秒内没有收到来自接收方 QUIC 端口的打洞包。在下面那 80 次运行的第一次尝试日志里，这个用例没有失败记录。它与用例②是否同因是假设——用例②的日志只能定位到加入方没有完成配对。**对照。** 同一个测试程序里的 `sender_stun_and_probe_round_share_the_adopted_dial_socket` 这一次是 `ok`。`7e6842b`、`75e5bf2` 与 `c452a29` 只差文档，同一份代码在 Windows 上另外三次运行（45984768383、45987622997、45997650618）与这次的第 2 次尝试里，这个测试程序都是 12 个全过（97.70、96.58、98.72、97.55 秒）；45984768383、45987622997、45997650618 与这次第 1 次尝试的 Ubuntu 任务里也都是 12 个全过（96.44、94.55、95.68、95.97 秒）。改动之后，Windows 上这个任务共跑了 5 次，1 次失败。**历史（更正）。** 执行方第一次扫描时跳过了最终结论为成功的运行，漏掉了“第一次尝试失败、重跑后转绿”的那些，据此写下的“Rust 任务此前只失败过两次”“这两个用例没有失败过”是错的，已由第十六轮审核指出。按逐次尝试重扫到 45992565932 为止的 80 次运行（最早一条 44689759020，2026-11-20T06:43:00Z）：Rust 任务的第一次尝试失败共 7 次，Windows 6 次——44691268201（批次 Q，`29532ee`，就是用例②）、44713033916（R，`13acefc`，`pairing::cancel_tests::sched_cancel_stops_file_transfer_growth`）、44743443447（S，`67fc851`，`pairing_e2e` 的四个用例）、44776125791（T，main 上的 `095f7d8`，`mid_transfer_port_takeover_by_another_key_is_refused_by_pinning`）、45561759000（Z，`ebc80df`，即 Z-N7 的 `sender_stun_and_probe_round_share_the_adopted_dial_socket`）与这一次；Ubuntu 1 次——45688067005（Z，`1eadb2f`，即 Z-N6 的 `cancelled_single_receive_port_rebinds_immediately`）。这 6 次 Windows 失败里，45561759000 的结论本就是 failure，早先的另外 4 次都是重跑后转绿。另有 X 批次三次运行（45373375565、45204899838、45194550651）结论是 failure 而任务里没有标为失败的，执行方没有再查。**源码上的解释（收窄；读源码得出，没有运行验证）。** 按摘要查不到有效的码（未登记、过期或已撤销）时，注册服务器先记为“幽灵”、到截止时刻统一回 `PAIR_UNAVAILABLE`（`crates/ak-signal/src/server_v2/engine/pairing.rs` 的 `handle_redeem`）。码有效而所有者离线时，服务器照常受理兑换、留下一笔待完成的配对等所有者重连，到期仍未完成就中止它，并向双方发同一个 `PAIR_UNAVAILABLE`（`engine/pairing.rs` 第 174 行附近的注释、`store.rs` 约第 1242 行、`engine.rs` 的 `sweep`）。发起方在收到转发的报价之后中止这笔配对，对方也会收到“不可用”（`engine/pairing.rs` 的 `handle_abort`）；发起方循环的出口包括：码在本地找不到、请求 MAC 不符、加入方的签名校验不过、等加入方的证明超过截止时间加 5 秒、证明校验不过、等提交帧超时或校验不过。对已经提交的事务，中止请求得到的是 `REVISION_CONFLICT`。这次的输出只能定位到加入方没有完成配对，走的是哪一条路径没有证据。这一轮的改动（`c2bccee`）只动了最后一个出口里对提交帧的校验，前面的都没有动；Windows 上 Rust 任务的第一次尝试失败在这一轮之前就反复出现过，但这不足以说明这一次与这一轮的改动无关。**留存**：`%TEMP%\ak-zcode\ci\c\`（第 1 次尝试的任务结论与各任务日志，`rust-windows.log` 含两个用例的失败输出）、`ci\c-failures.txt`（两个用例的输出）、`ci\tally-c-rust-windows.txt` 与 `ci\tally-c-rust-ubuntu.txt`、`ci\45992565932-jobs.txt`（第 1 次尝试的任务清单）、`ci\c2\`（第 2 次尝试的任务清单与 Windows 日志）、`ci\d\`（45997650618 的任务清单与两个 Rust 任务的日志）、`ci\history2-scan.txt` 与 `ci\history2-names.txt`（逐次尝试的重扫结果）、`ci\history2\`（各次第一次尝试的日志）、`ci\history2-q-failure.txt`（批次 Q 那次的失败输出）。 |
| 没有做的与待所有者的 | — | 没有做的：Ubuntu 上定向重复 Z-N6 100 次——`662c964`、`7e6842b`、`75e5bf2` 与 `c452a29` 的 CI 只随 Ubuntu 的全工作区测试各跑了一遍；Linux 上 T03 的 `--exact --nocapture` 与“没有 `skipping:`”的核对——Windows 上本机跑过两次（见“本机门槛”一行），`662c964`、`7e6842b`、`75e5bf2` 与 `c452a29` 的 Ubuntu 全工作区测试各报告一次 `ok`，四次都没有用 `--exact --nocapture` 核对 `skipping:`；推送之后 Windows 上 `t03_ipv6_loopback` 两个用例的一次 CI 失败（见“推送之后 Windows 上的一次 CI 失败”一行）——`probe_round_observes_the_real_advertised_candidates` 是 Q-N3 的复发，按 Q-N3 的约定查因，codex 的查因计划已申请（本笔成文时还没有出具），根因待查；`receiver_punch_reaches_the_sender_advertised_reflexive_port` 待归因；走查脚本里的“设备与宿主时间差”核对与它的自检（脚本原件不在接手机上），以及带着这条核对的设备轮；Z10 至 Z13（Windows Hypervisor Platform 已启用、等所有者重启，重启后才能验证加速；试验包与设备轮的 profile 包都要现有的 debug keystore）——安卓这条线等所有者重启并提供现有的 debug keystore。记录第十六轮裁决的这一笔追加提交，它自己的实际差异还要做一次只读复审，它推送之后的 CI 运行也还没有入库（两者都在给所有者的回复里报告，并随下一笔提交入库，本行不引用自己的提交号）。待所有者：重启（启用加速之后需要；与 VS 安装器标记的重启是同一个维护窗口）并提供现有的 debug keystore；154 的两个测试服务的停服（接手机没有登录方式）；原机材料的保全与去敏入库。所有者 2026-11-27 已决定、已执行的：接受分阶段路线并推送（见“推送与 CI”一行）；删除已合入的远端 X 分支（见“Z8 核对”一行）；授权启用加速、重启自行安排（见“接手机与工具链”一行）。所有者 2026-11-27 已决定、还没到实施时候的：Z-N8 暂缓——维持现状，设备轮带上时间差核对、有了真实偏差数据再定方向。 |
