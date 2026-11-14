# 交接文档：批次 J（2026-11-13—14，验收进行中）

> **2026-11-14 复核注（Claude）**：本文 §0、§1 已写到十轮重跑之后，但 §2.2 末条、§3.1 的 run 表、§4、§5、§6 仍是重跑之前的内容，以 §0/§1、[批次 J 复核](REVIEW_2026-11-14-batchJ.zh-CN.md) 与状态表为准（CR-P3-20）。本文"不改 main""开发分支"等说法写于合并之前；2026-11-14 起仓库只保留 main，见 [STATUS](STATUS.zh-CN.md)。正文保留为执行方原记录，未改。

> 工单来源：[批次 H/I 复核](REVIEW_2026-11-13-batchH.zh-CN.md) §8；J1 原清单及行为取舍见 [U1 交接](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md) §3/§5。
> 当前状态入口：[STATUS](STATUS.zh-CN.md)；逐项提交与验收见 [审核文档](REVIEW_2026-11-11.zh-CN.md) §7。
> 本文区分已提交代码、本地门禁、Linux 模拟验收及未完成的界面走查。J2/J5 十轮验收已达成；J1 视觉/系统SAF及 SafeArea 复拍为剩余边界，C4 Windows 抓包由所有者执行。

## 0. 一页结论

- **J0 已完成**：PR #15 内容已在远端开发分支，本地快进到 `0a17311`；CI 43439835214 为12项常规任务成功、手动 release 深测跳过。
- **J3 已完成**：`8ef4a07` 将实验变量读取置于默认关闭的 `exp-knobs` 特性之后；普通 CI 43466913061、netem 43466913049 均成功，Android 双 ABI 的新字符串检查实际执行成功，Windows 默认 DLL 本地扫描无 `AK_EXP_`。
- **J4 已完成**：`aee55e0` 让钩子显式携带尝试结果与终态标志，daemon 仅按终态打印；两次尝试回归及 CI 43444609038 通过。
- **J2/J5 已完成**：Rust `c60512b`、UI `6e576d6`、矩阵修复 `593b473` 已入库。**J5 十轮验收于 `ad975c0` 达成**——run 43538159455 十轮全部通过（10/10 轮、40/40 目标案例 pass-v4），decline-retry-40s 每轮实测重试间隔 40000–40001ms、字节一致、命名空间清理零残留。普通 CI 43538159415 同 SHA 全绿。修复前 `534f145` 的失败保留为历史时点。
- **J1 部分验收**：Windows 第1–5、10项同机行为已验证，第6项补了两个真实进程的历史生命周期检查；第7项视觉受环境阻塞。Android 有亮色窄屏进度和辅助真实8MiB发送/哈希一致证据；系统 SAF 普通流程、暗色传输状态及 SafeArea 修复后的标准首页截图仍未验证。不得把辅助 runner 的 exit1 写成全部通过。
- **J6 三者一致性已核**：STATUS → §7 每行 → 本文 §0/§5 逐行对照完成；十轮验收、修复提交与全部 run 号已回写。J7 可选缓冲优化本轮不实现。
- **C4 前置状态**：J1（同机行为层+辅助流程部分通过，视觉/系统SAF未完成）、J2（十轮验收达成）、J5（十轮验收达成）——J2/J5 已满足，J1 视觉及系统SAF普通流程为剩余项。Windows IPv4 TTL 抓包和生产部署均由所有者负责。

## 1. 提交清单

全部在 `dev/review-fixes-20261111`，不改 main、不强推、不合并 PR #11。

| 项 | 提交 | 内容与证据 |
|---|---|---|
| J0 | `0a17311` | 已包含 PR #15；对应 CI 已取证 |
| J4 / CR-P3-17 | `aee55e0` | 尝试结果与终态标志；daemon 终态打印；两次尝试回归 |
| J3 / CR-P3-16 | `8ef4a07` | 默认关闭实验特性；矩阵/netem 显式开启；Android 双 ABI 防泄漏扫描 |
| J2 Rust / CR-P3-15 | `c60512b` | 单一配对生命周期 gate、常驻 keeper、无资源及倒序失败回归；拒绝重试矩阵案例 |
| J2 UI / CR-P3-15 | `6e576d6` | 闲置事件流与页面闩锁；真实终态行过滤的 fake 与 mutation 回归 |
| J1 布局修复 | `0e1f793` | AppShell 单层 SafeArea；10项系统 inset、亮暗与桌面布局回归 |
| J1 走查 runner | `a9bd53c` | 参数注入的真实窗口场景，以及 seed/verify 两进程历史检查 |
| J5 执行器 | `fb19a01` | 固定 SHA 顺序十轮、四目标/重试证据/清理校验、脱敏上传；51项边界测试 |
| J5 矩阵修复 | `593b473` | exec保留receiver PID、独立feeder/sampler worker、有界stop、owned namespace、TERM/INT退出；11项真实Bash进程回归 |
| J6 进度交接 | `534f145` | 首次集成CI与十轮验收针对该提交（失败，保留为历史时点） |
| **最终tip** | `ad975c0` | **修复后集成CI 43538159415 全绿、J5十轮 43538159455 全部通过（10/10、40/40）** |

历史来源：U1 `b007b16`、`d0eab21`、`ab3d6e5`；复核文档更正 `a8dc521`。这些历史提交不代替 J1 或本批 J6 的验收。

## 2. 实现与验收边界

### 2.1 J1 十项走查结果

Windows 使用 `flutter test -d windows` 驱动真实应用窗口、真实 DLL 和同机 CLI。原场景使用新 DLL/CLI `0a17311`，降级场景使用 `97fac96` DLL；重启补验使用仓外固定 DLL 副本并记录实际加载模块。仅本地 registry/STUN 和合成数据，截图及原始日志留仓外。

| # | 项目 | 结论 | 现象与证据 |
|---|---|---|---|
| 1 | Windows GUI 发送进度 | 通过（同机行为） | 200MiB 合成文件，22个样本覆盖等待接受、百分比/速度/剩余时间、等待校验与成功摘要；实际标签为局域网直连。`j1-send.log` |
| 2 | 正常收尾 | 通过（同机行为） | 已完成标记、发送结果和返回按钮保留，无误报断开；首页移除当前连接并新增成功历史。`j1-send.log` |
| 3 | Windows GUI 接收进度 | 通过（同机行为） | 18个样本覆盖横幅、首页卡片同步进度、校验、完成卡与接收历史。`j1-recv.log` |
| 4 | 目录发送 | 通过（同机行为） | 6条目、约38MiB 的合成目录，条目序号不越界，摘要与 CLI 提交/校验一致。`j1-dir.log` |
| 5 | 拒绝与中途失败 | 通过（同机行为） | 拒绝显示失败及历史记录；约4%时停止接收端后显示 connection lost，不显示已完成。`j1-reject.log`、`j1-kill.log` |
| 6 | 清空与真正重启 | 通过（分层证据） | 清空后历史节消失。另用两个真实 OS 进程、同配置/身份目录/身份摘要/应用哈希/DLL验证：首进程在 App 自有历史对象注入一条合成记录且不清空，退出后次进程历史计数0且无 history_section；两次 flutter 直接退出码均0。`restart-20261114-0051/result.json`、`seed.stdout.log`、`verify.stdout.log`。注入验证内存生命周期，不冒充新传输；原 pumpWidget 不作重启证据 |
| 7 | Windows 深色可读性 | 阻塞，未完成 | 两种渲染后端的客户区截图均为纯底色，UIA 未提供语义树。真实窗口行为断言不能替代视觉；Android 暗色也不代替本项 |
| 8 | Android 约390dp窄屏 | 亮色辅助真实进度布局通过；其余边界未验 | 780×1688、DPR2的实际进度截图显示三行分离、长名省略且不溢出。`aux-light-three-lines-live.png` 来自200MiB传输；该传输因4分钟采样预算未完成，不判200MiB通过。暗色传输未验。标准首页状态栏遮挡已修，修复后标准首页截图尚未完成 |
| 9 | Android GUI 发到 Windows | 辅助真实8MiB传输通过；系统SAF普通流程未验 | App沙箱合成源及测试 FilePickGateway 注入，其余为真实GUI/FFI/本地CLI；接收8388608字节、源/目标 SHA256 均为 `e442fcea17247bfcfac3fd499d91cee7787bc397907cbee1b4087db39e7c5bc3`，CLI exit0、final ack verified。完成页显示局域网直连、32秒。`aux8-light-result.png`、`aux8-light-byte-verification.json`。辅助 flutter 整体 exit1：漏采速度行的断言失败，暗轮未运行；共享 Download 写权限拒绝后未提权/改权限，系统 SAF 路径未验 |
| 10 | 旧 DLL 降级 | 通过（同机行为） | 新 GUI 配 `97fac96` DLL：不确定进度、无百分比、成功及收尾正常、无引擎故障横幅。`j1-old.log` |

**日志证据层级**：发送/接收另存完整 `j1-send-console.log`、`j1-recv-console.log`，含 All tests passed；目录/拒绝/中断/旧DLL的场景日志及工具当时测试输出存在，但没有单独持久化 flutter 退出码，不能用管道末端退出码代替。双进程补验及最终门禁均保存直接进程退出码。

**UI 修复**：闲置事件订阅、终态行移除、按钮禁用已由服务/widget测试和移除订阅的 mutation 验证；SafeArea 旧版标题位置断言失败，修复后10项通过。最终集成 analyze 零问题、Flutter208/208、Dart170/170。

**Android 版本边界**：UI 基线 `aee55e0`，复用 J1 从 `0a17311` 重建的 FFI/CLI；后续 APK 叠加 `0e1f793` 的 SafeArea 文件。该走查不作为 J3 最终发布产物验收；J3 的双 ABI 检查见对应 CI。模拟器到宿主是同机组合，不是物理双机跨 NAT。

**清理与未恢复状态**：Windows 临时渲染改动已还原，自有服务已停，旧DLL副本仓外留证。Android 模拟器及专用服务在复拍期间收到 stopped/killed 通知，来源未确定，未擅自重启；只读确认自有进程和端口无残留。测试显示覆盖780×1688/density320/自动旋转关闭未及恢复，原1080×2400/density420/自动旋转开启记于仓外 `emulator-original-state.json`。合成数据、8MiB收件、200MiB未完成部分及日志均留仓外。SafeArea标准APK构建安装exit0，但 `09-main-safearea-light.png` 仅启动页，不能作修复后视觉通过证据。

### 2.2 J2 保活生命周期

- `SenderLifetime` 以一个同步 gate 管理绝对 deadline、活跃发送 ID、终态、watch 状态与唯一 keeper；发送准入与到期决定共享该 gate，无锁内 await。
- `SendLease` 先发布 op Finished，再释放本次占用；最后一个活跃发送退出后才恢复或到期。修复前的失败 op 代次/计数推断已移除，A/B倒序失败不会相互让位而永久漏恢复。
- keeper 在无socket、plain-loopback-only或地址族过滤后零目标时仍计时；活跃发送跨期限不被中止。取消等实际 future 退出后释放，关闭/teardown不复活，弱引用避免 keeper 强持有运行时。
- ffi-v2 先登记 `idle_expired`；同引擎对端即时通知、跨设备尽力通知边界见契约，不将预算到期说成网络映射必然消失。
- CLI/FFI 共用常驻 keeper；实验重试仍在 `exp-knobs` 后。矩阵摘要由实际首拒绝、一次配对、Instant计时≥40秒及第二次字节一致共同断言。
- 定向独立复核确认两项原缺口关闭；本地全量门禁通过。Linux十轮因脚本收尾缺陷失败，J2整体验收继续保留。

### 2.3 J3、J4、J5

- J3：默认构建不读取实验变量，矩阵/netem显式启用特性。Android扫描先完整取得 strings 输出，区分 grep无匹配和命令错误，避免 `grep -q` 引起SIGPIPE后的假阴性。
- J4：钩子每次尝试携带 `FinishOutcome` 和 `terminal`；daemon仅终态撤回注册项并打印。`max_attempts=2`验证第一次失败、第二次成功以及终态通知早于槽位释放。
- J5：固定checkout和CLI/Cargo.lock哈希，普通runner用户构建一次，顺序跑完整矩阵。每轮严格核对四目标唯一pass-v4、真实重试摘要、最终RESULT、退出码及七个命名空间名称无残留；失败停止、不拼历史。上传仅白名单脱敏结果，原始日志不入库或artifact。
- J5的51项单测是解析/编排边界证据，不能替代真实Linux进程与网络验收。首轮失败暴露该边界，修复补真实Bash进程测试。

## 3. 验证证据

### 3.1 CI逐任务结果

| 项 | 提交 | CI run | 结果 |
|---|---|---|---|
| J0 | `0a17311` | [43439835214](https://github.com/wmqfl861/AK-sending/actions/runs/43439835214) | 12常规任务success；手动深测skipped |
| J4 | `aee55e0` | [43444609038](https://github.com/wmqfl861/AK-sending/actions/runs/43444609038) | 12常规任务success；手动深测skipped |
| J3 | `8ef4a07` | [43466913061](https://github.com/wmqfl861/AK-sending/actions/runs/43466913061) | 12常规任务success；Android双ABI Release knob leak check步骤success；手动深测skipped |
| J3 netem | `8ef4a07` | [43466913049](https://github.com/wmqfl861/AK-sending/actions/runs/43466913049) | 特权Linux netem任务success，A/B继续可运行 |
| 集成首跑 | `534f145` | [43470613069](https://github.com/wmqfl861/AK-sending/actions/runs/43470613069) | 整体cancelled：11常规任务success，NAT超时cancelled，手动深测skipped |
| 十轮首跑 | `534f145` | [43470613025](https://github.com/wmqfl861/AK-sending/actions/runs/43470613025) | failure；首轮exit124，完成0/10、目标成功0/40，后续轮未运行 |

`534f145` 成功的11项：Rust Ubuntu、Rust Windows、Flutter（analyze/app/Dart）、Android（双ABI/泄漏检查/APK）、Repository checks、audit、deny、legacy-signal、portability x86_64 Linux、x86_64 macOS、aarch64 macOS。NAT单独失败，不将11项成功写成CI全绿；skipped不记为执行通过。

### 3.2 本地最终代码门禁

推送 `534f145` 前，对冻结隔离快照执行用户要求的全部门禁，逐命令保存日志和真实退出码：

- fmt、workspace clippy `-D warnings`、workspace tests、rustdoc `-D warnings`：exit0；Rust1199通过、0失败、8忽略（release深测的忽略不算执行）。
- release `ak_ffi.dll`重建exit0，无 `AK_EXP_`；SHA256为 `fc8cb1470eddee44527541a8901c8a50181332febcb700587ea56d6fb85017cc`。
- Flutter analyze零提示、app208/208、Dart170/170，均用最终重建DLL复验，exit0。
- 仓库检查、契约5/5、Python94/94，exit0。
- J3独立快照的Windows无开关DLL、Android双ABI本地重建扫描及对应CI另有证据；模拟器走查使用的旧FFI不混作此项证据。
- 曾遇到 Windows link.exe `0xc0000142`，该次构建失败保留，旧DLL哈希未作当次构建成功证据；后续隔离重建成功。曾用Bash Flutter入口在启动锁处exit127，改用Windows原生入口后门禁成功，未删除SDK锁。

### 3.3 首轮NAT失败与修复方向

- 十轮run 43470613025的构建exit0，第一轮在15分钟外层预算后exit124；脱敏 `results.json` 首个诊断为symm-symm，其后为后续案例拓扑失败；完成0/10、0/40，命名空间**名称**无残留。该检查不能单独证明没有持有已移除命名空间的孤儿进程。
- 普通CI原始日志确认 pcone-pcone、pcone-fullcone、v6-stateful分别通过；17:13:59进入symm-symm后无下一条矩阵输出，17:38:32任务超时取消。这不证明发送方超时，具体阻塞由进程实验进一步定位。
- 无网络真实Bash对照：直接pipeline和带exec的wrapper中，跟踪PID等于子进程，TERM后退出；新增无exec函数wrapper中，PID属于包装shell，TERM后真实子进程继续存活，wait等待整个pipeline。DEVNULL和外层pipe两种stdin下结论一致。本地为Git Bash，Linux/netns效果仍须修复后CI验证。
- 另复现TERM trap仅调用cleanup却不exit，导致超时清理后继续执行后续case；这可解释十轮首跑的拓扑级联失败。修复范围限定真实receiver PID、feeder/sampler跟踪、有界退出与TERM/INT明确退出，预算不放宽。
- Rust只读追踪未发现失败退出等待10分钟keeper的路径，未因此改Rust。失败日志、实验数据保留仓外，修复前结果不计入下一次十轮。

### 3.4 历史验证

H/I `97fac96` CI43421861248、U1 `ab3d6e5` CI43433201562为各自版本证据。I1 netem run43420288043在RTT约200ms、256MiB下记录8MiB窗口21.1MiB/s、1.25MiB窗口5.6MiB/s，H/I复核已据此关闭CR-P2-04；J3特性更改后的证据为43466913049。均为回环/netem或同机测试，不代表真实广域网。

## 4. J6 一致性检查

按 **STATUS当前状态及最新验证 → 审核§7每一行 → 本文§0/§5** 检查。旧F/G/H时点与当前结论分开，E1–E4不重新列为待决定；CR-P2-06补真实提交 `99b8498`，E4删除证据保留 `951b44e`。旧H交接的全完成措辞、重复清单与I批在途说法不作为当前结论。

目前失败证据已开始回写，修复后run与对应提交尚待补齐。最终逐行核对完成后再写“三者一致性已核”及更新编号清单；本文不提前宣称本批验收完成。

## 5. 未完成项与责任

| 项 | 下一步 | 当前边界 |
|---|---|---|
| J1 | 补Windows深色视觉、系统SAF普通流程、Android暗色传输及SafeArea标准首页复拍 | 第1–6/10有对应行为证据；第8/9仅已执行部分通过，详见十项表 |
| J2 | 完成拒绝后重试的Linux连续十轮验收 | 代码与本地门禁已通过，首轮因矩阵脚本生命周期缺陷受阻 |
| J3/J4 | 已完成；最终集成版本继续受CI检查 | 不因J5脚本失败撤销各自已有对应提交的证据 |
| J5 | 修复脚本生命周期并完成全套推送前门禁，在新固定提交重新顺序10轮 | 首跑0/10、0/40，不能沿用或拼接历史次数 |
| J6 | 回填修复提交/各job与逐轮结果，完成三文档核对并推送 | 当前未声明最终一致性完成 |
| J7 | 可选UDP缓冲优化留后续 | 本轮不做，不新增为C4阻塞 |
| **C4** | **J1/J2/J5对应验收完成后，由所有者做物理双机跨NAT、WindowsTTL抓包、迟发送/拒绝重试及界面观察** | 未就绪；TTL抓包不转给J1，也不设成J5前置 |
| E5/E6/E7、域名、SSH | 所有者负责签名密钥、部署防火墙/LICENSE、域名钉扎与SSH密钥；正式命名/版本/签名验证见CR-P2-02 | 未取得完成证据，不由本次开发代做 |
| 其他工程 | CR-P2-08超大文件拆分；旧Rust在一个发布周期后评估删除 | 旧模型隔离/旧Dart删除已完成，不把E6当自动删码授权 |

E6按 [OPERATIONS §0](OPERATIONS.zh-CN.md) 在维护窗口切换独立STUN、外部验证及失败回滚；CR-P2-13代码 `2ceb37e` 不等于生产部署完成。生产部署与外部验证由所有者负责，本轮未登录或改动生产主机。

## 6. 红线与交付

- 服务器只做发现、信令及NAT协调，不处理文件数据；未引入TURN/relay。
- 未生成签名密钥；原始配对码、凭据、用户文件、真实用户路径与完整网络诊断不入库。
- 同机/模拟器/网络命名空间证据分别标注，不冒充C4物理设备验证。
- 代码与进度交接已推至开发分支 `534f145`，后续修复与最新证据待完成后推送；不改main、不强推、不合PR #11。
