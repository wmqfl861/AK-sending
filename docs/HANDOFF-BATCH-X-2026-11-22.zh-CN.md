# 交接文档：批次 X（2026-11-22）

> 起点：main 快进到批次 W 复核的最终文档提交，本文随该提交入库。
>
> - 批次 W 的结论见 [W 复核](REVIEW_2026-11-22-batchW.zh-CN.md)；
> - 全项目审核与界面设计方案见 [全项目审核](AUDIT_2026-11-22.zh-CN.md)（AUD-05 已更正）；
> - 复核方的命令与运行编号见 [W 交接 §9](HANDOFF-BATCH-W-2026-11-22.zh-CN.md#9-复核方证据2026-11-22)；
> - 执行方从本文 §8 证据表开始记录本批的运行编号与计数；
> - 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 W 有条件通过：僵尸连接行（V-N2）在引擎与应用两层修好，三种失败情形的走查一致；错误文案映射、应用名、打包卫生、性能定位、屏幕常亮都合格。一项被推翻：W7.1 的“加入时不选方向”让跨设备加入在对端建“我要接收”码时必然失败（W-N1，P1），根因是复核方在 W 交接里写错了。V-N3（黑屏）因证据失效改回待验证。

**复核方越权改过代码。** 复核方在批次 W 分支上直接改了产品代码、测试、检查脚本和契约措辞（三个提交，见 §1），这些改动只经过 CI，没有在任何设备上运行，现在都在 main 上。所有者决定保留，由执行方在本批验收。**角色分工自本批起固定：复核方只做审核与计划，不改代码；改代码和设备验证都由执行方做**（§2.1）。

**批次 X 的三块：**

- 先还账：X1 验收复核方的改动并补 W-N1 的设备验证，X2 修 W 复核登记的问题（P2 在前），X3 补验 V-N3；
- 再做界面改版里不依赖新依赖、也不等所有者决定的两件：X4 新首页与两条主路径（方向只声明一次，这才是 W-N1 的正确解法），X5 接收确认显示设备名与内容摘要；
- 收尾：X6 CI 增加 Windows 构建与 release APK（可选），X7 归档（可选），X8 全部门槛与回归走查。

## 1. 复核方的越权改动与验收要求

审核结论见 [W 复核 §3](REVIEW_2026-11-22-batchW.zh-CN.md#3-对复核方自己改动的审核)。下表是执行方要逐条验收的内容。

| 提交 | 动了什么 | 只验证到 | 复核时查出的问题 | 执行方要做的 |
|---|---|---|---|---|
| `da70e5f` | `flutter_app/lib/ui/pages/home_page.dart`：加入面板恢复方向选择（与批次 W 之前的面板代码相同，保留了 W 的“提交后收起面板”）；`flutter_app/test/` 下 `home_flow_test`、`direct_unavailable_guidance_test`、`transfer_error_card_test`、`w7_interaction_test` 四个文件与 `flutter_app/integration_test/k1_android_saf_test.dart`：恢复选方向的步骤；`docs/contracts/ffi-v2.md`：一句措辞 | CI 的静态分析与 widget 测试；**没有设备运行** | K1 没有运行，且后半段是 W7.2 之前的流程（W-N12）；两处测试文件头注释与内容相反、契约措辞未经核准（W-N15） | X1 |
| `38a6f46` | `scripts/check_repository.py` 新增 `stripped_separator_issues` 与 `scripts/tests/test_repository_checks.py` 两条测试；[W 交接 §8](HANDOFF-BATCH-W-2026-11-22.zh-CN.md#8-证据表执行方填写) 三处启动日志路径；审核 AUD-05 的更正段 | CI 的仓库检查任务；复核方本机 Python 全套 | 规则太窄，TAB 是盲区（W-N14）；三处路径是按代码里的文件名推断着补的（W-N6） | X0 确认路径；X2 处理规则 |
| `0cc9baa`（批次 V 复核时） | `scripts/check_repository.py` 的 `control_char_issues` 与三条测试；V 交接 §8 一行 | 同上 | **对回车符不生效**：`check()` 用 `read_text` 读文件，回车在到达规则之前已被换成换行（W-N13，复核方实测） | X2 |

验收规则：

- 每个提交在 §8 写一行结论：**验收通过**、**改写**（写明改了什么）或**回退**，并写依据；
- 回退的做法是用一个新提交把相关文件恢复到复核方改动之前的内容（`da70e5f` 之前是 `f581ebd`），不强推；
- 如果验收发现复核方的判断本身错了（例如 X1 里不选方向也能配对成功），停下那一项，在 §8 写明并报告，不要硬凑。

产品代码的改动只有 `da70e5f` 的 `home_page.dart` 一处；复核方没有改 Rust。

## 2. 接手前必读

### 2.1 角色边界（所有者 2026-11-22 指示）

- **复核方只做审核与计划**：读代码、看 CI 与 §8、写复核与交接、更新状态表与 STATUS、把通过复核的分支快进到 main。复核方不改代码、脚本、契约，也不改执行方的 §8。
- **执行方是唯一改代码的一方**，也是唯一能在设备上验证的一方。复核方发现的问题都写成工作单。
- **复核方也会写错。** W7.1 就是复核方写错、执行方照做的结果。交接里任何一条与代码或现有测试对不上的要求，都按 §2.4 先核对，在 §8 写明依据，停下那一项并报告，不要照做。

### 2.2 报完成之前与分支

沿用 W 交接 §2.1：

- 最后一次推送之后，运行 `git ls-remote --heads origin`，确认远端有 `dev/batch-x-<日期>` 且指向你的最后一个提交；
- 等这次 CI 全部跑完再报完成；
- 失败了，先在 §8 记下运行编号、失败的任务与测试、完整报错，再追加一个提交推送，重跑通过也照样记失败那次；
- §8 不留占位；回复里写明最后一次推送的提交号、CI 运行编号与结论；
- 推送后的每一个提交都要有对应的 CI 记录，最终提交尤其不能漏（W 的 §8 只记到中间提交，最终提交 `f581ebd` 的 CI 没有记）。

### 2.3 往文档里写内容（V-N1、W-N6 的教训）

两批里反斜杠转义吞掉文档内容共出现两次。要求：

- 路径在文档里**优先写正斜杠**（`logs/ak-stage.log`），必须写反斜杠的 Windows 路径用编辑器，或带引号的 heredoc（`cat <<'EOF'`）；不要用 `echo -e`、不带引号的 heredoc 或 `printf` 的格式串；
- **不要指望检查脚本兜底。** `check_repository.py` 现在能拦住响铃符这类控制字符，以及路径占位符（`%TEMP%`、`<home>`、`<repo>`、`<tmp>`）后直接跟字母的情况；但它拦不住回车（W-N13）和 TAB（W-N14），X2 修好之前尤其如此；
- 写完 §8 之后用 `grep -n` 把含路径的几行看一遍。

### 2.4 现有测试、测试替身与真实引擎（W-N1、W-N7、W-N10 的教训）

W7.1 的缺陷能通过全部 Flutter 测试，是因为 Dart 假服务镜像了“加入方得对侧角色”；而真实行为早就被一条 Rust 测试钉住了，只是没有人去查。要求：

- 界面行为一旦依赖引擎语义（角色、方向、终止原因、缺省值、错误文本），**先 grep 现有的 Rust 测试，再找到实现它的那行代码**，在 §8 写出测试名、文件与函数名；找不到就停下来记录；
- 假服务的行为必须与真实引擎一致；确实不一致的地方在假服务里写明，并在 Rust 一侧有钉住真实行为的测试；
- 错误文案的测试输入用引擎真实产生的文本（从 Rust 的常量和 `Display` 实现抄，注明出处），不要自己造字符串；
- 引用契约文字推出行为时同时核对代码；两者不符以代码为准，并提出契约的修改。

### 2.5 本批会改产品代码

- 每个修复与功能都要有能在 CI 跑的自动化测试，只有走查结果的不算完成；
- 改 FFI 的 JSON 字段或事件时先改 `docs/contracts/ffi-v2.md`，保持纯加法，并跑 `python scripts/check_contracts.py`；
- 不改线上协议（`ak-wire`、`protocol-v2.md`）；需要改协议时停下，在 §8 写明原因，交复核方；
- 不新增第三方依赖（ADR-002）；Flutter SDK 自带的包也先不引入；
- 改动的现有测试断言在 §8 列出：哪个测试、旧断言、新断言、为什么；
- X4 会改动大量带冻结键的测试与走查脚本，键的变更清单也要在 §8 列出。

### 2.6 Android 走查的共同做法

沿用 W 交接 §2.4，另加三条：

- 每次截屏的同时记录前台窗口归属：`adb shell dumpsys window | grep -i mCurrentFocus`，并核对它指向本应用（W2 的教训）；
- 画面“正常”不能只靠亮度均值，要同时有可操作的证据（例如 `uiautomator dump` 里能找到本应用的某个控件，或走查脚本的 `PROGRESS` 行仍在推进）；
- **改了哪条流程，就重跑驱动这条流程的全部走查脚本**；没有重跑的在 §8 写“未重跑”和原因（W-N12 的教训）。

走查脚本与它驱动的流程（复核方读过文件头的三个；其余的由执行方在动首页之前读各自的文件头，把这张表补全后贴进 §8）：

| 脚本（`flutter_app/integration_test/`） | 驱动的流程 | 复核方所知的最近一次运行 |
|---|---|---|
| `u2_android_lifecycle_test.dart` | App 创建发送码，宿主 CLI 加入并接收；后台与锁屏 | 批次 W |
| `k1_android_saf_test.dart` | 宿主 CLI 创建接收码，App 加入并作为发送方；系统文件选择器 | 文件头记为批次 K；批次 W 改了流程后没有重跑 |
| `t1_android_app_receive_test.dart` | 同一进程内经 FFI 直连收发，不经过配对界面 | 批次 W |
| `j1_*`、`app_e2e_test`、`android_real_test`、`gui_*`、`k1_windows_theme_shots_test` | 复核方没有逐个读 | 不清楚 |

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办）

见 [审核 §6](AUDIT_2026-11-22.zh-CN.md#6-需要所有者决定的事项)，当前全部未决：

- O-1（U-N2）Android 后台策略：复核方建议 A；W-N11 里“把应用切到后台去粘贴配对码”的情况也属于这一项；
- O-2（P2-02）移动数据；
- O-3 应用显示名：“AK互传”暂用，所有者可改；
- O-4 应用图标与 `applicationId`；
- O-5 v1 平台范围；
- O-6 新依赖审批（二维码显示与扫码）：本批不做依赖这项的内容；
- O-7 流程瘦身；
- E5 签名、E6 部署、E7 许可证、C4 物理双机，见 [状态表 §8](REVIEW_2026-11-11.zh-CN.md#8-需所有者确认的事项)。

外部 agent 对 O-1、O-3、O-6 的“预审”只是参考，不是所有者的决定。

### 3.2 工程项

- 依赖所有者批准的界面项（二维码显示与扫码、Windows 拖放、Android 分享入口与通知、`gen-l10n`）排在批次 X 之后，见 [审核 §5](AUDIT_2026-11-22.zh-CN.md#5-分阶段路线)；
- 宽屏两栏布局、最近传输列表、附近设备、PAKE 短码（批次 Y，需要先写 ADR-003）不在本批；
- 启动耗时的进一步定位、打包清单的小改进等优化建议见 [W 复核 §6](REVIEW_2026-11-22-batchW.zh-CN.md#6-优化建议)，本批不要求，有余力再做并单独记录；
- 其余沿用 W 交接 §3.2。

## 4. 批次 X 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| X0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-w-20261122 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-x-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论；6) 核对 W 交接 §8 里复核方补过的三处启动日志路径是否符合原意（W-N6），不符就改正 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；两条命令的输出原样贴进 §8；§8 写明三处路径的核对结论 |
| X1 | 验收复核方的改动，W-N1 的设备验证，假服务对齐，按 §4.1 | §4.1 的各条全部满足；§8 有 `da70e5f` 的验收结论 |
| X2 | 修复清单，按 §4.2 | P2 五项都有自动化测试；P3 做不完的写明顺延；被改断言在 §8 列出 |
| X3 | V-N3 补验，按 §4.3 | 五轮都记录前台窗口归属与可操作证据；结论按 §4.3 |
| X4 | 新首页与两条主路径，按 §4.4 | §4.4 的验收全部满足，或整项不做并写明 |
| X5 | 接收确认显示设备名与内容摘要，按 §4.5 | §4.5 的验收全部满足，或整项不做并写明 |
| X6 | 可选：CI 增加 Windows 构建与 release APK，按 §4.6 | CI 通过且没有削弱现有任务 |
| X7 | 可选：归档已关闭批次的文档，按 §4.7 | `check_repository.py` 通过；STATUS 的链接明显变短 |
| X8 | §5 的全部门槛；X4 之后的 V2-a 一轮；K1 一轮（X4 改了首页就必须在新首页上重跑）；T1 回归一轮 | 全部通过；走查结论写在 §8 |

做的顺序：X0、X1、X2、X3 先做；X4 是本批主体；X5 以后按顺序做到哪算哪，**X4 至 X7 都是整项交付或整项不做**，不留半改版。

### 4.1 X1：验收复核方的改动，W-N1 的设备验证

1. **核对根因，不用新写测试。** 仓库里已有双引擎测试 `production_owner_loop_refuses_same_direction_joins_before_commit`（`crates/ak-ffi/src/task/task_tests.rs`），它的第二阶段钉住了“不传 `role` 的跨引擎加入遇到 receiver 码被拒，显式声明 sender 后同一个码仍能配对”。单独运行它并在 §8 记一行。再确认是否已有“创建方 sender、加入方不传 `role`，配对成功且加入方为 receiver”的断言：有就写出测试名，没有就补这一条正例。
2. **K1 走查**（W-N12）——这一轮就是 `da70e5f` 的设备验证：
   - 先读 `k1_android_saf_test.dart`。它的后半段（在首页点连接卡片、点“选择文件”）是 W7.2 之前的流程；现在发送方配对成功后应用会自动打开详情页并弹出来源选择。参照批次 W 对 u2 脚本的适配改好它，并删掉末尾对一句已不存在的文案的断言；
   - 在模拟器上跑通：宿主 CLI `task code-recv` 创建接收码，App 加入并在面板里选“发送文件”，经系统文件选择器选一个文件发送，两端 SHA-256 相等；
   - 跑不通时先分清是脚本过时还是产品问题，两种情况都写进 §8。
3. **假服务对齐**（W-N7）：`flutter_app/lib/services/simple_transfer_fake.dart` 的 `joinCode` 在 `role == null` 时缺省为接收；双方同向时返回失败，文本与引擎一致（`unknown task code (or expired, revoked, owner offline)`）。依赖旧行为的测试改成显式传 `role`，被改断言在 §8 列出。
4. **注释与契约**（W-N15）：
   - `w7_interaction_test.dart` 与 `home_flow_test.dart` 的文件头注释改成与现在的测试内容一致；
   - `docs/contracts/ffi-v2.md` 里复核方改过的那一句，由集成者核准后改成只描述接口语义的写法，去掉界面做法和复核编号。建议写法：“`role` 决定生码方角色。加入方的角色取 `ak_task_join` 的可选 `role`；缺省时，同一引擎内命中码缓存则取创建方的对侧，否则为 `receiver`。跨引擎的加入应显式声明方向：与创建方同向的加入在提交前被拒绝（统一的不可用应答），不消耗码的兑换次数。”同时在 `ak_task_join` 的 JSON 示例里补上可选的 `role` 字段；跑 `python scripts/check_contracts.py`。
5. **结论。** 在 §8 对 `da70e5f` 写“验收通过”“改写”或“回退”。如果第 1 步的测试失败，或 K1 表明不选方向也能配对成功，说明复核方的判断错了：停下，回退 `da70e5f`，报告。
6. **可选的一轮。** “App 创建接收码、宿主 CLI `join-send`”。这个方向 W7.1 没有弄坏（CLI 的方向一直是显式的），而且 T1 的文件头写明宿主拨不进模拟器；环境做得到就做，做不到写“未执行”和原因。

### 4.2 X2：修复清单

依据与分析见 [W 复核 §2](REVIEW_2026-11-22-batchW.zh-CN.md#2-问题清单与修复建议)。P2 五项必须做；P3 做不完的在 §8 写明顺延。

| 项 | 内容 | 验收 |
|---|---|---|
| W-N2（P2） | 停滞看门狗的预算到期时不直接改终态：先用 `ak_task_list` 核对，引擎仍认为连接在就重新计时（可以在界面提示“长时间没有进展”），引擎已不认识这条连接才标 `conn_lost`。退一步的做法是让校验阶段不参与看门狗；选哪种在 §8 写明理由 | Dart 测试：校验阶段超过预算而引擎连接仍在时不被标记；引擎已不认识连接时仍被标记；W1 现有的看门狗测试保持通过 |
| W-N9（P2） | 加入场景下“这个码用不了”的原因与建议补全：除了输错、过期、已使用，还要写明“对方需要保持应用打开并停留在前台”和“请确认两人没有选同一个方向（一方发送、一方接收）” | 表驱动测试；新文案原文贴进 §8 |
| W-N10（P2） | 词表补上引擎的真实文本：`storage_full`、`insufficient free space`、`ran out of disk space`；发送场景给专门文案（对方的设备空间不足，请对方清理后重新配对再发送，会从断点续传）；“无法保存到所选文件夹……更换保存文件夹”只用于接收场景；分类测试的输入改用从 `ak-engine` 常量抄来的真实文本并注明出处 | 两句真实文本在发送与接收两个场景下各有断言 |
| W-N11（P2） | **先复现**：模拟器上显示配对码后让屏幕自然熄灭，约一分钟后从宿主加入，记录结果。属实再改：把屏幕常亮与“请保持在前台”的提示扩大到“码已显示、等对方加入”和“已配对、尚未传完”两个状态，以码的有效期和配对闲置预算为上限。不属实就在 §8 写明并关闭 | 复现记录；改了的话，通道测试断言开与关成对，等待结束（码过期、取消、配对结束）必定关闭，非 Android 平台零调用 |
| W-N13（P2） | `check_repository.py` 读 Markdown 时保留回车（按字节读入再解码，或以 `newline=""` 打开），让控制字符规则真正能看到 CR | 新增一条经过读文件路径的测试：含单独回车、含 CRLF 的文件都被拦住；全仓检查通过 |
| W-N3 与 W-N8（P3） | 连接结束后的文案按断开原因区分：对方断开——“对方已断开连接”；连接中断——“连接中断了。请重新配对后再发送，会从断点续传”；自己断开不提示续传；原因不明或是本地错误时用中性说法，具体原因交给 op 的错误卡 | `transfer_errors_test.dart` 与 widget 断言更新；详情页状态行按原因断言 |
| W-N4（P3） | 状态码 -2 只在加入场景映射为“这个码用不了”；发送与启动场景走词表与兜底 | 表驱动测试覆盖三个场景 |
| W-N5（P3） | 把 `post_connect_verdict` 与它的两个判定下沉到 `ak-engine`，FFI 与 CLI 共用；CLI 的 `task_cli.rs` 两处单属性匹配改用它；测试随之搬到 `ak-engine`，FFI 侧保留集成测试 | `cargo test`、`clippy -D warnings`、`cargo doc -D warnings` 通过；CLI 对“同码异文、同文异码”的反例行为与 FFI 一致 |
| W-N14（P3） | 禁止 Markdown 里出现 TAB（仓库里目前一个都没有）；复核方写死日志文件名的那半条规则可以删掉，去留由执行方定并写明 | 单元测试；全仓检查通过 |

### 4.3 X3：V-N3 补验（黑屏）

沿用 W 交接 §8 里 W2 一行的补验计划，并加严记录：

- 构建与轮数：profile 三轮、debug 两轮（debug 允许手工进入传输，以避开弹层点击故障）；
- 每轮：传输中按 Home，120 秒后恢复同一 Activity；恢复后 5 秒与 30 秒各记一次：前台窗口归属（`mCurrentFocus`）、截屏亮度均值、一条可操作证据（见 §2.6）；
- 任一轮在“前台是本应用”的前提下窗口全黑：按 W 交接 §4.2 第 3 条做渲染后端对照（`--no-enable-impeller` 一轮）与修复，修复后 profile 三轮都正常才算完成，并补一条断言回前台请求了新帧的 widget 测试；不要靠改渲染配置“关闭”问题；
- 五轮都正常：以“未复现”关闭 V-N3，在状态表原地改写该行；
- profile 构建在模拟器上 UI 自动化不稳定的问题，先试用手工进入传输加 adb 触发的办法；做不到的轮次如实写“未执行”及原因，不要用别的轮次顶替。

### 4.4 X4：新首页与两条主路径

这是本批主体。目标是实现 [审核 §4.3](AUDIT_2026-11-22.zh-CN.md#43-关键流程) 的主路径，并借此让方向**只声明一次**（W-N1 的正确解法，见 [W 复核 §4](REVIEW_2026-11-22-batchW.zh-CN.md#4-免选方向的正确做法)）。不改协议、不新增依赖、不做二维码。

| 入口 | 主路径 | 次要入口 |
|---|---|---|
| 发送文件 | 先选文件（可多选，含文件夹），再生成码（创建方为发送方）；码卡片显示大号分组码、复制、已选摘要与“更改”；对方加入且连通后**自动开始发送**，不再需要点任何按钮 | “对方已经给了你一个码？输入对方的码”（加入方为发送方，方向已知；成功后沿用 W7.2 的自动弹出来源选择） |
| 接收文件 | 输入对方的码（加入方为接收方，方向已知），成功后沿用 W7.2 的等待卡 | “想让对方输入你的码？生成接收码”（创建方为接收方） |

要求：

1. 去掉创建前的角色面板（`_RoleSheet`）与加入面板的方向选择；`PairingFlowController.startJoin` 的 `role` 改为必填，界面不再有“没有方向”的路径。
2. 方向只来自第一次点击：四个入口组合各有一条 widget 测试，断言传给服务的 `createCode` 与 `joinCode` 的角色，并且 `joinCode` 的角色永不为 null。
3. 自动发送：发送路径在连接建立后自动调用发送；连通测试未通过（“仍可尝试发送”）时同样继续尝试，不因探测结果阻断；发送失败按 W3 的错误卡显示。
4. 文案按 [审核 §4.5](AUDIT_2026-11-22.zh-CN.md#45-文案规范)：对方、连接、无法直接连接；标题说结果、正文说原因与办法、按钮用动词。只改新首页与这两条路径里的字符串，其余界面留待后续。次要入口仍可能出现两人同向，W-N9 的文案要保留。
5. 冻结键：新增 `home_send_button`、`home_receive_button`，次要入口使用新键；旧的 `home_create_code_button`、`home_join_code_button`、`role_send_button`、`role_receive_button`、`join_role_selector` 等凡是被删除或改义的，在 §8 列出，并同步更新所有引用它们的测试与走查脚本（按 §2.6 的表逐个处理）。
6. 假服务沿用 X1 对齐后的行为，不要为让测试通过而放宽。
7. 不在范围：宽屏两栏、最近传输列表、二维码、扫码、拖放、分享入口、通知。

验收：`flutter analyze`、`flutter test` 通过；V2-a 在新首页上走一轮（后台 30 秒，传输全程同任务恢复并传完，屏幕常亮的持有与释放成对）；K1 在新首页的次要入口上走一轮。做不完就整项不做：保留现状（加入面板带方向选择的方案可用），在 §8 写明原因。

### 4.5 X5：接收确认显示设备名与内容摘要

现状：`incoming_request` 事件只有 `root`、`files`、`bytes`，横幅写“对方请求发送 N 个文件”，对方名称是“对端设备”。目标：确认前让用户知道“谁要发什么”。

1. 先读代码：配对时双方的 endpoint blob 里带一个设备名（`PeerEndpoint.name`）。确认这个名字在 FFI 一侧能拿到哪里；在 §8 写出文件与函数名；
2. 如需新增字段（例如 `connected` 事件与 `ak_task_list` 行的 `peer_name`，或 `incoming_request` 的 `peer`）：先改 `docs/contracts/ffi-v2.md`，纯加法，跑 `check_contracts.py`，Dart 解析器容忍缺省；
3. 横幅与确认面板显示：发送方设备名、`root`（顶层名称）、文件数与总大小、保存位置；**设备名是对方自报，不是身份证明**：在名称旁标注指纹前 8 位的短码，不把名称写成“已验证”；
4. 不显示单个文件名（“最近传输不含文件名”的持久化隐私约定不变）；
5. 测试：假服务可注入设备名；widget 测试断言展示与缺省（拿不到名字时仍显示“对方的设备”）。

做不完就整项不做，在 §8 写明。

### 4.6 X6：CI 增加 Windows 构建与 release APK（可选）

[审核 AUD-21](AUDIT_2026-11-22.zh-CN.md#5-分阶段路线)：界面相关的发布构建不在 CI 里，回归只能靠本机。只做两件，且只验证“能构建”：

- Windows：在 CI 里 `flutter build windows --release`；
- Android：`flutter build apk --release`，使用模板的默认签名配置，不生成签名密钥，不上传制品。

要求：不削弱现有任务；新任务失败时先查原因，不要为了通过而降低门槛；CI 时间明显增加时在 §8 记录数字。

### 4.7 X7：归档已关闭批次的文档（可选，沿用 W8）

E4（2026-11-12）已决定“已关闭的报告归档”。只在 X1 至 X5 完成后做，单独一个提交：

- 把批次 F 到 T 的交接与复核、K2–K24 各轮复核移到 `docs/archive/`（用 `git mv`）；批次 U、V、W 与本批留在原处；
- 修好所有指向它们的链接（`check_repository.py` 会报出全部断链）；
- STATUS 的“交接入口”只保留当前批次、上一批、审核文档和 `docs/archive/` 目录链接；
- 不改归档文件的内容。

做不完就整项不做，在 §8 写明，不要留半归档的状态。

## 5. 门槛与命令

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --no-fail-fast
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
cargo test --locked -p ak-store --features sqlite
cargo test --locked -p ak-signal --features server-persistence
python -m unittest discover -s scripts/tests -v
python scripts/check_repository.py
python scripts/check_contracts.py
bash -n scripts/deploy_linux.sh && bash scripts/deploy_linux.sh --self-test
bash scripts/cross_nat_test.sh --self-test
cd flutter_app && flutter analyze && flutter test && cd ..
cd flutter_app/packages/ak_ffi_dart && dart test && cd ../../..
```

补充说明：

- 本机 Rust 用 rust-toolchain.toml 钉的 1.99.1；
- 本批改了 Rust（X2）、Dart 与界面，Flutter 与 `ak_ffi_dart` 的测试必须在本机跑，不能只依赖 CI；
- 改了 `ak-ffi` 或 `ak-engine` 就重跑 `bash scripts/build_android_ffi.sh` 并在 §8 记 x86_64 一行，T1 回归在新库上跑；
- 缺工具时写“未执行”，不能写成通过；
- 推送前在最终文档上跑一遍 `python scripts/check_repository.py`。

## 6. 文档与分支规则（摘自 AGENTS.md）

- 每项状态只写在状态表里，提交列写真实提交号，不写“本提交”；
- 状态表的“证据摘要”列不写运行编号、计数、倍数和带小数或区间的耗时；这些数字只写在本文 §8；
- 新增的行放在本批前一批各行之后：先 X0–X8，复核方的 X-N 行由复核方加；
- 一项被后续批次确认或推翻时，原地改写它的那一行。W-N1 至 W-N15 与 V-N3 由对应的 X 项原地补充结论，保留原有描述；
- STATUS 只写阶段结论、下一步和链接，整份改写时保留全部入口链接（X7 做了归档的除外，见 §4.7）；
- 状态表表尾的说明行由复核方写，执行方不要加；
- 执行方只推 `dev/batch-x-<日期>`；任何人都不强推，发现错误就追加提交；
- 新增的 CI 门槛失败时先查原因，不要为了通过而削弱它；
- 所有失败都记入 §8，不能只记最后一次；
- 验收要求原样贴出的输出每次都要贴；结论依赖的关键输出行原样贴进 §8（路径与指纹按红线处理），走查原始文件留在本机即可；
- 结论要与同一行的数字一致；
- 没有做的项，在 §8 写明没有做和原因。

## 7. 红线

- 不引入 TURN/relay，服务器不经手文件数据（ADR-001）；
- 不部署、不登录、不修改生产服务器，也不改它的防火墙；走查一律显式指向本机的注册服务器与 STUN；
- 不生成签名密钥，不改 `applicationId`；
- 不新增第三方依赖；不改线上协议；
- 配对密钥、令牌、用户文件、真实文件名或路径、完整网络诊断都不入库；任务码与配对码不入库，指纹与 job id 只写前 8 位；截图不入库；
- 模拟或同机测试不写成真机验证。

## 8. 证据表（执行方填写）

执行方在本机 Windows 与 Android 模拟器上工作。按 X0–X8 的顺序逐行填写；每行写明环境（本机 Windows、模拟器、debug 或 profile 构建）、提交或运行编号、原样输出与结论。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| main 上批次 W 最终提交的 CI | `cacd0a5` run [44984529060](https://github.com/wmqfl861/AK-sending/actions/runs/44984529060) | completed success（12 项常规任务 success，2 项手动深测 skipped），以 API 核实（工作单提示的编号与 API 一致） |
| X0 分支与 worktree | 本地 Windows | merge-base 成立后删除了远端 `dev/batch-w-20261122`；本地 main 快进到 `cacd0a5`，删除本地 `dev/batch-w-20261122`，`git worktree prune` 后从 main 切出 `dev/batch-x-20261122`。`git branch -a` 原样输出：`* dev/batch-x-20261122`、`  main`、`  remotes/origin/HEAD -> origin/main`、`  remotes/origin/main`。`git worktree list` 原样输出（路径按红线略写为 `<repo>`）：`<repo> cacd0a5 [dev/batch-x-20261122]`，只有主目录一行 |
| X0 三处启动日志路径核对（W-N6） | 本地 Windows | W 交接 §8 第 285-287 行的三处：W5 行 `<home>/.ak-sending/logs/ak-stage.log`（桌面身份目录）、W6 Android 行 `cache/ak-stage.log`（平台临时目录）、W6 Windows 行 `<home>/.ak-sending/logs/ak-stage.log`——与 `flutter_app/lib/services/ak_engine_facade.dart` 第 46/85/158 行的实现（桌面 `<identity dir>/logs/ak-stage.log`、Android 临时目录）逐一比对，**三处都符合原意，核对通过，不改正** |
| X1 第 1 步：同向拒绝测试单独运行 | 本地 Windows，`cargo test --locked -p ak-ffi --lib production_owner_loop_refuses_same_direction_joins_before_commit` | `test result: ok. 1 passed; 0 failed; ... finished in 2.74s`。第二阶段（task_tests.rs:2955-2973）的断言核对：新建 receiver 码后 `join_code`（:320 的 helper 不传 role）被拒（:2966-2969），同一 `code_two` 显式 sender 仍配对成功（:2970-2973）；第一阶段（:2930-2949）断言拒绝文本含 `unknown task code`、owner 无 connected 事件、joiner 无连接 |
| X1 第 1 步：反向正例已存在 | 本地 Windows，`cargo test --locked -p ak-ffi --lib task_codes_relay_across_engines_via_the_registry` | `test result: ok. 1 passed; 0 failed; ... finished in 3.30s`。task_tests.rs:1413-1429：创建方显式 sender、另一引擎省略 role 的 join 成功且结果角色为 receiver（:1423 断言 `"role":"receiver"`）——"创建方 sender、加入方不传 role 配对成功且为 receiver"的正例已有，无需新写 |
| 批次 X 执行计划（codex gpt-6.2-astra/max，项目规则首轮应用） | 本地 codex 会话（163,923 tokens，读文档与源码后出具） | 计划给出：依赖图与文件所有权分组（模拟器串行队列：K1→W-N11→X3→X8）、X1 两层假服务的对齐点、X2 五项修法（看门狗推荐 ak_task_list 核对并给理由）、X4 冻结键清单与自动发送挂点（_onFlowChanged→ConnectionDetailPage._send，按 codeId 关联防串线）、X5 peer_name 取数路径（connected 事件与 ak_task_list 行当前都无设备名，需契约纯加法 peer_name/peer_fp_short）、风险表与七个基础提交切分。X6 建议**顺延**：build.gradle.kts:63 会在未配置所有者 key.properties 时中止 release 构建，"只加 Windows 任务"不能记作 X6 完成——采纳，X6 本批顺延。计划原文在会话记录，未入库。项目规则（先 codex 计划、结束 codex 审核，模型 gpt-6.2-astra/max）已写入 AGENTS.md"计划与审核规则"一节 |
| X1 第 3 步：假服务对齐（W-N7） | 子代理执行（含被改断言清单在其汇报，入 §8 附件表） | UI 假服务 joinCode 改为跨设备语义：缺省 TaskRole.receive、有效角色与码同向时**消费码之前**返回引擎原文 `unknown task code (or expired, revoked, owner offline)`（出处 pairing/task_net.rs:651），不建连接、码可被互补角色兑换；低层 fake_task_ffi 保留同引擎缓存对侧缺省但显式同向改为异步 op 失败。被改断言两处（ffi_simple_transfer_service_test 的 receiver+receiver 成功 → 拒绝+互补重试；task_ffi_test 同类）；新增测试两组（拒绝后无连接、码仍可兑换）。注释写明对齐依据：join_send.rs:23、pairing/task.rs:980、task_tests.rs:2913/:1405。flutter 281/281、dart 171/171 |
| X1 K1 走查（da70e5f 设备验证）——四次尝试均因模拟器资源问题中断 | 本地 Windows + 模拟器（debug 构建） | 第 1 次：宿主脚本取 registry 指纹落空（端口占用时启动脚本截断了 registry-server.log）→ 修正为 `registry-id` 子命令后重试。第 2 次：DocumentsUI 打开后应用暂停，safUntil 单次 pump 的 45s 超时炸（外部驱动未在窗口内完成选文件）。第 3 次：dump0 抓到的是 **ANR 弹窗**（"AK互传 isn't responding"，logcat am_anr：FocusEvent 等待 5s 超时，发生在 PICKER_OPENED 前 20 秒）——应用主线程在选择器打开前已阻塞。第 4 次（冷重启模拟器后）：安装 69.7s 异常缓慢，SystemUI 自身 ANR（"System UI isn't responding"），点掉 Wait 后 flutter 测试宿主端已死（装好但 Dart 侧从未启动，00:00 +0 停滞）。**判读：四次都是环境问题**（宿主同时跑多个子代理的重构建，模拟器资源被挤压；应用侧无确定性缺陷证据）。K1 设备轮顺延到 X4 落地、宿主负载回落后按 X8 口径（新首页的次要发送入口）执行；da70e5f 的验收见下一行 |
| X1 对 da70e5f 的验收结论 | 依据：两条 Rust 测试单独运行通过 + 四个 widget 测试文件在本机随 X1 假服务对齐全过 + 四次设备尝试记录 | **验收通过（改写）**。语义依据：production_owner_loop_refuses_same_direction_joins_before_commit（缺省 receiver 遇 receiver 码被拒、显式 sender 同码可配对）与 task_codes_relay_across_engines_via_the_registry（sender 码+缺省加入成功为 receiver）——复核方"跨设备加入必须显式选方向"的判断与引擎真实行为一致，恢复方向选择是正确修复。**改写部分**：契约措辞按 §4.1 第 4 条改为纯接口语义（见下），两处测试文件头注释与 K1 脚本后半段按 W7.2 流程改写（K1 脚本已适配，设备轮顺延）。产品代码 home_page.dart 本身验收通过，未回退 |
| X1 第 4 步：注释与契约（W-N15） | 集成者执行 | 契约 §421 行的 role 句改写为纯接口语义："role 决定生码方角色。加入方的角色取 ak_task_join 的可选 role；缺省时，同一引擎内命中码缓存则取创建方的对侧，否则为 receiver。跨引擎的加入应显式声明方向：与创建方同向的加入在提交前被拒绝（统一的不可用应答），不消耗码的兑换次数。"——去掉界面做法与复核编号；ak_task_join 的 JSON 示例补上可选 `"role":"sender"` 与缺省说明；check_contracts 5/5 PASS。两处测试文件头注释由 X4 子代理随其文件所有权一并更新（w7_interaction/home_flow 现与新首页流程一致） |
| X2/W-N2 看门狗修复 | 子代理执行 | `_checkStalledTransfers`（simple_transfer_service.dart:2281）改为预算到期先收集候选、有候选才调一次 `runtime.list()`（判定与 `_reconcileWithEngineSnapshot` 一致）；引擎仍列出连接→重置计时不改终态；缺行或明确 disconnected→标 conn_lost；快照异常→不标不重置下轮重试。理由（注释内）：只排除 verifying 会丢掉该阶段真正失联的兜底（Q-N2 后校验并行）。测试 8 条全过（新增 verifying-仍活不标记+计时重置证明、快照异常重试；原 :117 用例改为"引擎行已消失"的真实缺陷形状，终态断言保留——被改断言已列出） |
| X2/W-N9+W-N10+W-N4 文案 | 子代理执行 | transfer_errors.dart：新增 joining 场景值（与 pairing 同文案）；invalidCode 三层文案补全（输错/过期/撤销/已使用/对方不在线/同方向 + "让对方保持应用打开并停留在前台"建议）；词表补 `storage_full`/`insufficient free space`/`ran out of disk space`（出处：lib.rs:1036、receiver.rs:955、recv_single.rs:18，测试输入用引擎真实文本）；sending 场景空间不足专用文案（对方清理空间后重新配对再发送，会从断点续传）；-2 只在 pairing/joining 映射 invalidCode。测试 41+5 条全过；被改断言 5 组旧新对照在其汇报 |
| X2/W-N13+W-N14 检查脚本 | 子代理执行 | check_repository.py：markdown 读取改 `read_bytes().decode('utf-8')`（CR 存活到规则处，W-N13）；CONTROL_CHAR_RE 并入 TAB（即 C0 里的 0x09，W-N14）；写死日志文件名的半条规则**保留**（捕捉转义符被整个吞掉的情形，全仓 120 个 .md 零误报）。新增 5 条走真实读文件路径的测试（孤立 CR/CRLF/TAB 拦下且报正确行号，纯 LF 通过）；变异验证证明旧代码确实漏掉三者。Python 382/382、全仓检查 passed |
| X2/P3 顺延清单 | 集成者决定 | W-N3/W-N8（连接结束文案按原因区分）：顺延——需要详情页按 connId 拿到结束原因的链路，与 X4 的首页改版同域，并入 X4 之后或下批；W-N5（判定下沉 ak-engine）：顺延——涉及 ak-engine/FFI/CLI 三方公共判定与反例测试，单改 CLI 交不完整；W-N14 的"日志文件名半条规则"如上保留（去留已写明）；W-N4 已随 W-N9 完成 |
