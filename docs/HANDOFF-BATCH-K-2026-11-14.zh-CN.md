# 交接文档：批次 K（2026-11-14）

> 分支：`dev/batch-k-20261114`。首轮范围 `d6c7136..f9582e5`；修复轮一 `f9582e5..f150503`；修复轮二 `f150503..3db5259`；修复轮三 `3db5259..31cf22c`；修复轮四 `31cf22c..a8bb3b9`；修复轮五 `a8bb3b9..9c2d3bd`；修复轮六 `9c2d3bd..e286513`；修复轮七 `e286513..12a71c9`；修复轮八 `12a71c9..ccd8fb2`；修复轮九 `ccd8fb2..806ea6c`。§0–§24 为前序记录与执行方声明，§25 为本次独立复核证据。首轮文档随 `b6296ea`、K2 本地纠正随 `f7dece2`、K3 报告及同步随 `e8b621a`、K4 报告及同步随 `46473fc`、K5 报告及同步随 `e8f811d`、K6 报告及同步随 `bb1f673`、K7 报告及同步随 `514cb87`、K8 报告及同步随 `7e8bfb2`、K9 报告及同步随 `e03a44f` 提交；本次 K10 文档同步仅保留本地。
> 每项状态只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。运行编号、计数与结果集中在本文对应轮次的证据表；当前发现见 [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md)，历史见 [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md)、[K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md)、[K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md)、[K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md)、[K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md)、[K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md)、[K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md)、[K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md) 与 [K 首轮复核](REVIEW_2026-11-14-batchK.zh-CN.md)。

## 0. 范围与来源

以下 §0–§5 的“本批/本轮”均指 `f9582e5` 首轮快照，不是修复轮当前状态。

工单来源为 [批次 J 复核 §8](REVIEW_2026-11-14-batchJ.zh-CN.md#8-批次-k-工作单交给本地-agent)，J1 逐项定义见 [U1 交接](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)。`f9582e5` 追加了 Linux 部署与公网接收端测试脚本；其静态/离线验证不代表已经部署或完成跨网实验。

## 1. 仓库与分支

K0 的历史分支清理由执行方完成；独立复核重新读取了远端引用和 worktree。批次 K 的代码未合入 main，分支继续保留用于修复。具体基线与引用记录见 §5。

## 2. J1 走查及边界

Windows 主题图使用 `RepaintBoundary.toImage`，属于进程内光栅。执行方使用真实 DLL/CLI、本地 registry/STUN 和合成文件；截图保存在仓外合成实验目录，复核抽查了深色传输和拒绝图。新 runner 的漏采断言、有效端点配置问题见复核报告。

Android 辅助 runner 的采样调整已经在 `bd61553` 入库，原文“worktree 未提交”已纠正。它注入文件选择结果，不替代系统 SAF 普通流程。首轮复核时没有使用修复后的引擎重跑 SAF、暗色传输和辅助 runner；修复轮执行方报告及本次产物复核见 §6、§9。

首轮未证明 QEMU 数据面问题与 CR-P3-21 有因果关系；修复轮关于 task endpoint gate 的新证据单独列于 §6、§9，不能用探测单测替代。SafeArea 首页图与模拟器设置恢复属于执行方报告，详见证据表。C4 前置继续遵循原工作单，不用未执行的真机实验替代 J1 缺项。

## 3. 文档机械校验

`5415cb6` 添加运行编号追溯与矛盾词检查。被审快照的检查器按文件名挑选 K 交接，即使 STATUS 未链接 K 也会通过；复核报告记录了该反例。原文“当前有红项”是交接落库前的历史结果，不作为当前门禁结论。

## 4. 探测修复与脚本

`1d995b0` 在接收轮询之前执行首轮发送，再通过 `interval_at` 和 `biased;` 管理后续发送与接收。新回归测试用预缓冲包钉住竞态前置条件；原有早退时间限制保留。该修复不构成真实对端身份、路径或 QEMU 失败根因的证明。

首轮 Linux 部署脚本与公网测试脚本存在安装、旧版本路径、Paramiko 等执行问题，具体触发见首轮复核报告；修复轮剩余问题见 K2 复核。公网测试脚本的 Linux 进程是最终接收端，不是代转节点；该路线不替代 C4 的产品任务码路径和受限 NAT 组合验收。

## 5. 证据表

以下区分执行方报告、复核实际执行、GitHub API 查询和未执行项，不相互替代。

| 执行方原证据 | 提交/来源 | 结果与边界 |
|---|---|---|
| K0 清理 | `d6c7136` 之后 | 执行方报告删除 7 个已被 main 包含的远端分支及多余本地分支/worktree |
| Windows 主题图 | `bd61553`；仓外合成实验截图 | 12 张（浅/暗各 6 状态），执行方报告两主题 Flutter exit 0；复核确认文件存在，抽查 2 张深色图可读；未独立重跑完整走查 |
| Windows 走查合成负载 | `bd61553` | 执行方使用 200 MiB 与 5 MiB 文件；非用户数据 |
| Android SAF | 执行方原走查 | adb push、构建/安装/启动及 registry 配对有报告；QUIC 数据面未建立；未完成 SAF 传输验收 |
| SafeArea 首页 | 执行方原走查 | 亮/暗各 1 张标准首页图；不等于暗色实际传输 |
| Android 辅助 runner | `bd61553` | 150 ms 采样及累计 sawRate/sawFile 已提交；8 MiB 辅助流程本批未重跑 |
| 模拟器显示恢复 | 执行方原走查 | 从 override 780×1688 / density 320 恢复到 physical 1080×2400 / density 420；自动旋转 1 |
| CR-P3-21 原验收 | `1d995b0`；执行方报告 | 旧代码 20/20 失败；修复后 500/500 单独、50/50 并行通过；200 ms 上限保留；复核没有重复该完整次数 |

| CI/历史证据 | 提交 | run | 结果与来源 |
|---|---|---|---|
| main NAT 十轮 | `dc8a91b` | [43558234150](https://github.com/wmqfl861/AK-sending/actions/runs/43558234150) | 执行方 K0 报告成功 |
| main CI | `dc8a91b` | [43558234099](https://github.com/wmqfl861/AK-sending/actions/runs/43558234099) | 原交接：12 项常规任务成功 |
| main netem | `dc8a91b` | [43558234106](https://github.com/wmqfl861/AK-sending/actions/runs/43558234106) | 原交接：成功 |
| 批次 J 开发分支 CI | `4f604bc` | [43544631036](https://github.com/wmqfl861/AK-sending/actions/runs/43544631036) | 原交接：12 项常规任务成功 |
| 批次 J NAT 十轮 | `ad975c0` | [43538159455](https://github.com/wmqfl861/AK-sending/actions/runs/43538159455) | 原交接：10/10 轮、40/40 目标案例；网络命名空间模拟 |
| netem A/B | `8ef4a07` | [43466913049](https://github.com/wmqfl861/AK-sending/actions/runs/43466913049) | 原交接：8 MiB 窗口 23.0 vs 5.7 MiB/s；人工时延 |
| 所有者指定的 K CI | `579310a` | [43577126893](https://github.com/wmqfl861/AK-sending/actions/runs/43577126893) | 复核经 GitHub API 查询：12 项常规任务 success，1 项手动 release 深测 skipped；早于最终脚本提交 |
| K 最终提交 CI | `f9582e5` | [43601899282](https://github.com/wmqfl861/AK-sending/actions/runs/43601899282) | 复核经 GitHub API 查询：最终 failure；11 项常规任务 success，Windows Rust workspace 步骤 failure，1 项手动深测 skipped |
| K 最终提交 CI 重跑 | `f9582e5` | 同 run（rerun-failed-jobs，run_attempt=2） | 执行方用 git 凭据读取 job 日志定位失败用例（见 §6 K-CI 行）后触发重跑：Windows `Tests (workspace)` 步骤 completed success，整体 success；与 579310a 同代码通过、根因修复、重跑通过三者共同构成 flake 证据链 |

| 复核实际执行 | 对象/命令 | 结果与边界 |
|---|---|---|
| 仓库检查 | `python scripts/check_repository.py`；被审 `f9582e5` | exit 0；同时重现 STATUS 未链接 K、检查器却选择 K 并放行 |
| Python 单测 | `python -m unittest discover -s scripts/tests -q` | 113 passed，exit 0 |
| 契约检查 | `python scripts/check_contracts.py` | 5/5 PASS |
| ak-net | `cargo test --locked -p ak-net` | 48 passed，0 failed |
| 早退专项复跑 | 现有测试二进制过滤 `holepunch_probe_early_exit --test-threads=8` | 连续 20 轮、共 60 次用例通过，含原早退约束与预缓冲回归；Windows 本机环回 |
| Rust workspace | `cargo test --locked --workspace` | 1200 passed、0 failed、8 ignored，exit 0；Windows 本机执行 |
| Rust 格式与差异 | `cargo fmt --all -- --check`；`git diff --check main..dev/batch-k-20261114` | exit 0 |
| Rust 静态检查 | `cargo clippy --locked --workspace --all-targets -- -D warnings` | exit 0 |
| rustdoc | `RUSTDOCFLAGS=-D warnings cargo doc --locked --workspace --no-deps` | exit 0 |
| Flutter analyze | Flutter 3.47.9 / Dart 3.13.7；`flutter analyze --no-pub` | exit 0，无问题 |
| Flutter 单测 | `flutter test --no-pub --reporter expanded` | 208 passed，exit 0 |
| Shell 语法 | 两个新增脚本分别执行 `bash -n` | 均 exit 0 |
| 部署脚本离线门禁 | `bash scripts/deploy_linux.sh --self-test` | PASS；包括生成的远端脚本语法，不等于执行部署 |
| 公网测试脚本离线门禁 | `bash scripts/cross_nat_test.sh --self-test` | PASS；使用既有 release CLI（git `1e59342`），没有连接远端 |
| 同文件安装反例 | GNU install，对合成 fixture 执行源目标相同的安装 | 报 are the same file，exit 1；对应 `--binary` 的停服后路径 |
| Paramiko 反例 | 提取两个脚本真实 helper，离线替换 SSH 连接，使用实际 Channel | `timeout=0` 下两个 helper 均 exit 255；无网络连接 |
| CI 失败详情读取 | `GET /repos/wmqfl861/AK-sending/actions/jobs/132468237206/logs` | HTTP 403：`Must have admin rights to Repository.`；公开 job 元数据仅定位到 Tests (workspace)，注释为 exit code 1。未换身份或绕过权限，具体失败用例尚未定位 |
| 文档同步后检查 | `python scripts/check_repository.py`；`git diff --check` | 均 exit 0；仅文档更改，不重复宣称代码测试 |
| 本地/远端引用 | `git fetch --prune origin`、`git rev-parse`、`git worktree list` | main/origin/main 均 `d6c71360784e1570720e56028a39f2a80777bb21`；批次分支均 `f9582e5ace7fc35104b0e6fad9786972b1d7a76a`；仅 1 个 worktree |

| 未执行项 | 原因/边界 |
|---|---|
| shellcheck | 本机无该工具 |
| 本地 Linux 原生构建与 systemd 部署 | 本机为 Windows；本轮只执行静态和离线脚本验证，未连接生产 |
| Windows/Android 集成 runner、SAF、修复后模拟器传输 | 本轮未重跑；Flutter 单测与既有截图不替代这些验证 |
| 物理双机 C4、真实抓包、release 深载荷 | 本轮未执行；所报常规 CI 的手动深测为 skipped |

## 6. 修复轮（K 复核后的新候选）

复核方 2026-11-14 给出 REJECTED 后，执行方按 [K 复核](REVIEW_2026-11-14-batchK.zh-CN.md) §1 提交修复并重跑走查，形成本节执行方证据。下列内容已按独立复核修正证据层级，结果仍须与 §9 对照；修复轮提交号见 §7，与首轮被审快照（`f9582e5`）区分。

| 修复项 | 对应发现 | 内容 |
|---|---|---|
| K-R1/K-R2/K-R3+STUN 标签 | K-deploy | 预编译部署改独立暂存路径与哈希/版本/可执行预检，增加二进制切换失败恢复；ExecStart 保留参数并改写可执行路径，但所谓运行中版本仍只查配置/磁盘路径。两个 Paramiko helper 改阻塞+双流排空+空闲预算；`--skip-doctor` 的 STUN 标签为 SKIPPED。新测试当前全过；执行方原称旧码 9/9、5/5 失败，独立复核发现其中含准备阶段/mock 错误，不能作行为证明，详见 §9 |
| K-R4/K-R5 | U1（Windows runner） | 截图覆盖改单调计数器+缺失命名断言+导出前重查文件；端点 guard 改为按应用同款两级解析（运行期 env 优先）校验**生效值**，同一份已验证配置传给应用与 CLI；+12 个单测（直接 import runner） |
| K-R6 | CR-P3-20 | 检查器新增 `status_linked_handoff`，能拒绝缺失/过期/不同批次文件名链接，并使 `f9582e5` 原反例报错；但只比较 basename，实际同名异路径链接仍会核对错误文档，反例见 §9 |
| K-CI | K-CI | Windows workspace 失败用例定位为 `revoke_after_pairing_preserves_books_and_admits_further_sends`（redeemer 90s 窗口内 owner 未完成应答，95.8s 超时退出）；执行方将其解释为既有慢机类（run 43312035496、43273272597 为同用例等待超时；原日志不直接证明 owner 冷启动耗尽窗口）。修复：测试先等 owner `listening`（身份→TLS→配对服务 adopt→receiver bind 全部就绪的信号）再启动 redeemer，窗口只覆盖配对流程本身。同代码本地 5/5 通过（45.45s） |
| 措辞 | 非阻断 | `ak-net/src/lib.rs` 探测早退注释的 "guaranteed to observe" 改为「本端先尝试发送，UDP 丢失与瞬态发送错误除外」 |
| 辅助 runner 加固 | U1（item d） | 速率行断言按进度监听回调的接收时间计算 transferring 快照跨度：跨度跨过 1s 则要求看到速率行，否则记录 `RATE_LINE_NOT_APPLICABLE`。这些时间不是模型内部 transferStartedAt，不能据此证明整个 transferring 相位不足 1s，也不能断言 J 批次失败的唯一根因；本轮全部已观测 transferring 快照 bps=null，应用条件与不显示速率行一致，具体时间对照见 §9；轮末新增 `flow_done_button` 重置配对流程视图（否则 `_flow.active` 使主页不再显示生成码按钮，暗轮无法开始）；发送前清除本运行自己的 checkpoint 目录（防上轮尝试的续传污染）；每轮重置诊断缓冲 |

### 修复轮走查证据

| 走查 | 结果与边界 |
|---|---|
| Android 辅助 runner（亮+暗） | 两轮全新 8MiB 真实发送全部完成：亮轮 1m14s/110KB/s、暗轮 1m17s/105KB/s（全程含等待接受；实际数据面 ~5s）；接收端均 8388608 字节、SHA-256 与源一致 `e442fcea…e5bc3`；暗轮主题在测试绑定内验证；整体 exit 0。速率行两轮均为 NOT_APPLICABLE（已观测 transferring 快照跨度未跨 1s，不能等同整个相位时长；复核见 §9）；三行同现布局的长期传输证据仍属 J 批次 200MiB 轮，本轮不重复宣称。对端为主机 `task join-recv`（task 平台端点 blob 方向校验通过——普通 wave-21 `daemon --invite-code` 不带端点 blob 会被 P1-05 gate 拒绝，此前 0x2101 根因） |
| Windows 主题图（修复后 runner） | 亮、暗各一轮全部通过：每轮 6 张进程内 PNG（等待接受/传输中/等待校验/完成/失败/最近传输）全部导出且覆盖断言 transferring=1、verifying=1；亮暗合计 12 张；暗色传输中抽查（76.0MB/200MB、60.1MB/s 速率行可见、无裁剪）。修复轮还发现并修复 runner 缺陷：合成文件此前由外部预置、runner 不自建——被清空的目录使全部候选路径报 `storage error: path not found`，现 runner 自建确定性合成文件并记录 SYNTH_CREATED；另加 SEND_STATUS/SEND_RESULT/SEND_ERROR/DIAG 证据行 |
| Android 系统 SAF 普通流程 | 新增 `flutter_app/integration_test/k1_android_saf_test.dart`（混合驱动：Flutter 侧 widget 级输入配对码/选角色/打开发送，系统 DocumentsUI 侧由 adb/uiautomator 外部驱动——Flutter 无 uiautomator 语义树、纯 adb 盲驱已证明不可靠，尝试记录在案）。走查通过：真实系统选择器打开 → 外部在 DocumentsUI 的 Downloads roots 视图点选 `saf_pick_20261114.bin`（4MiB，SHA-256 `f488b0ee…39cc`）→ 插件真实返回 URI → 已选 1 项 → 测试驱动发送 → 发送完成（互联网直连，1m13s）→ 接收端 4194304 字节哈希一致 → 整体 exit 0。本机 registry/STUN 端点经 dart-define 注入（未配置时应用 fail-closed 显示错误卡，已截图验证） |
| SafeArea 标准首页（亮+暗） | 正式 `flutter build apk --debug` 安装的 app：亮色与系统夜间模式（`ui_night_mode=2` + 重启生效）各保留一张首页 OS 截图（`09-main-safearea-light.png`、`09-main-safearea-dark.png`，仓外证据目录）；亮图为干净标准首页，暗图有 System UI ANR 模态框遮挡/变暗，不能称无遮挡暗色标准首页。两图标题与生成/输入配对码按钮仍在状态栏之下；暗图边界见 §9 |
| 模拟器显示恢复 | 全程使用标准 1080×2400/density 420；走查后确认值并恢复：自动旋转=1、夜间模式=0、adb unroot；恢复前后值记录于仓外 `emulator-restored-state.json`。期间一次夜间模式试验需重启框架导致模拟器卡死，冷重启同 AVD 后继续（安装与设置持久） |

### 修复轮走查环境

本机 registry（127.0.0.1:17200，指纹 `4395e22a…2687`）+ 本机 STUN（127.0.0.1:17101），Windows 端 CLI/DLL 为 `git 1e59342` 构建（修复轮 Rust 代码仅注释与测试变更，产品二进制与 `f9582e5` 一致）：`ak_ffi.dll` SHA-256 `6414d48b…57ab`、`ak-cli.exe` `0675e7d2…5b36`、Android `libak_ffi.so`（x86_64）`764d940d…c54`。Android 侧文件与哈希均为本运行内生成的确定性合成数据；未触碰共享存储权限与用户文件。

## 7. 修复轮提交与最终 CI

| 提交 | 内容 |
|---|---|
| `c2e6d2c` | CR-P3-20：新增 STATUS 链接文件名校验（K-R6）+8 回归；真实路径问题见 §9 |
| `11f91e9` | K1：Windows 主题图 runner 覆盖计数器+生效端点 guard+自建合成文件+诊断行（K-R4/K-R5）+12 单测 |
| `dbb46fc` | K 脚本：部署暂存/恢复、ExecStart 改写与配置路径版本检查、Paramiko 阻塞读、STUN 标签；新增两个脚本测试文件，剩余行为与旧码对照边界见 §9 |
| `5beb10a` | CI flake：owner listening 先行再启 redeemer；ak-net 早退注释措辞收敛 |
| `c42bbf8` | K1：辅助 runner 速率行条件判定/流程重置/checkpoint 护栏；新增系统 SAF 混合驱动 runner |
| `b6296ea` / `9797c1b` / `f150503` | 本交接 §6/§7、STATUS、审核状态表修复轮行与 K 首轮复核文档入库，随后记录 CI 结果 |

| 执行方修复轮门禁记录 | 提交/证据 | 结果与边界 |
|---|---|---|
| 代码候选 CI | `b6296ea`；[43627058493](https://github.com/wmqfl861/AK-sending/actions/runs/43627058493) | 12 项常规任务 success，1 项手动 release 深测 skipped；API 独立对照见 §9 |
| 文档记录提交 CI | `9797c1b`；[43629054632](https://github.com/wmqfl861/AK-sending/actions/runs/43629054632) | 12 项常规任务 success，1 项手动深测 skipped；`f150503` 为其后的纯文档记录提交，tip 自身 CI 见 §9 |
| 本地门禁（执行方报告） | 修复轮候选 | fmt/clippy/rustdoc exit 0；Rust 1200 passed、0 failed、8 ignored；Flutter analyze 无问题、单测含新增 12 项全过；Python 135/135；契约 5/5；check_repository exit 0；两脚本 bash 语法和自检通过。独立执行结果见 §9，不将原报告替代本轮实测 |

## 8. 收尾

执行方将首轮复核文档与修复代码提交为 `f150503` 候选。修复轮一独立判断见 [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md)，逐项状态只在审核状态表；该轮纠正出具时保留本地，随后随 `f7dece2` 提交。本节为历史收尾记录。

## 9. 修复轮独立复核证据（2026-11-15）

被审代码固定为 `f150503`，修复差异为 `f9582e5..f150503`；本节区分该轮实际执行、GitHub API 查询、对既有走查产物的复核与未执行事项。判断与修复要求见 [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md)，每项状态只在审核状态表。本节及同步文档出具时为本地未提交记录，随后已在 `f7dece2` 入库；以下“本次/本轮”均指该历史复核。

| 本次实际执行 | 命令/对象 | 结果与边界 |
|---|---|---|
| Python 全集 | `python -m unittest discover -s scripts/tests -q` | 135 passed，0 failed，exit 0；121.298s |
| 检查器专项 | `python -m unittest discover -s scripts/tests -p test_repository_checks.py -v` | 35 passed，exit 0；新增实际为 8 项（原 27），不是执行方所称 9 项 |
| 仓库与契约 | `python scripts/check_repository.py`；`python scripts/check_contracts.py` | 原候选仓库检查 exit 0、契约 5/5 PASS；仓库检查通过不能排除下列错误链接目标反例 |
| Rust workspace | `cargo test --locked --workspace` | 1200 passed，0 failed，8 ignored，exit 0；本机 Windows，含修复后的配对安全用例 |
| 配对顺序专项复跑 | 从上述 workspace 日志定位本轮生成的 invite_pairing_security 测试可执行文件，过滤 revoke_after_pairing_preserves_books_and_admits_further_sends | 连续 5/5 通过，95.05s；只使用本机合成测试环境，不以此证明 CI 原失败的唯一根因 |
| Rust 静态门禁 | `cargo fmt --all -- --check`；`cargo clippy --locked --workspace --all-targets -- -D warnings`；`RUSTDOCFLAGS=-D warnings cargo doc --locked --workspace --no-deps` | 全部 exit 0；rustdoc 为本地环境变量方式传入 |
| Flutter/Dart | `flutter analyze --no-pub`；`flutter test --no-pub --reporter expanded`；包内 `dart test` | analyze 无问题；Flutter 220 passed（含直接 import runner 的 12 项）；Dart 包 170 passed；全部 exit 0 |
| Windows 构建 | `flutter build windows --debug --no-pub` | exit 0，实际产出桌面可执行文件；不等于重跑集成走查 |
| Android 构建 | `flutter build apk --debug --no-pub` | 首次默认环境因找不到 Android SDK 退出 1；配置已有 SDK/JDK 环境后 exit 0，产出 debug APK；没有签名操作，也没有重新交叉编译 Rust SO |
| 脚本新测试 | `python -m unittest scripts.tests.test_deploy_linux_install scripts.tests.test_ssh_helper_exec -v` | 14 passed，exit 0；当前测试通过不证明旧版本执行到了故障点 |
| Shell 离线门禁 | 两脚本分别 `bash -n` 和 `--self-test` | 全部通过；cross_nat 自检使用既有 `git 1e59342` release CLI，仅本地解析/keygen，无远端连接 |
| 修复差异 | `git diff --check f9582e5..f150503`；对照 `579310a..f9582e5` 的 crates/Cargo 文件 | 差异格式检查 exit 0；后者 Rust 源码与 Cargo 文件无差异，支持“同 Rust 代码已有绿运行”的范围限定 |
| 本地文档同步 | `python scripts/check_repository.py`；`git diff --check` | 同步后的仓库检查与差异格式均 exit 0；只修改文档，不重复宣称代码门禁 |
| 最终引用与工作区 | `git fetch --prune origin`、`git rev-parse`、`git branch -avv`、`git worktree list` | HEAD/批次分支及 origin 对应分支均 f150503cc77536bb5a5ac10e41cd86d9e377edb1；main/origin/main 均 d6c71360784e1570720e56028a39f2a80777bb21；仅 1 个 worktree、main 与当前批次两个本地/远端分支。5 份文档本地改动（含新 K2 报告），未提交、未推送、未合并或删除分支 |

| 代码反例与新旧对照 | 实验 | 实际结果与边界 |
|---|---|---|
| unit 写入失败 | 执行真实生成的 phase456，合成 fixture；只将首个 unit tee 设为失败 | binary 安装成功后 exit 1；调用日志已有两个 stop，没有任何恢复 start。离线故障注入，未运行 Linux systemd |
| stop 失败却核验成功 | 执行 phase456→phase7，stop 返回失败，旧 unit 仍 active，磁盘路径已更新 | 两阶段均 exit 0，并输出两服务运行目标版本；未检查 MainPID/进程映像。离线模型证明脚本接受条件不足，不冒充真实服务升级 |
| 非 root 发现 | 执行真实 phase1，以模型隐藏其他用户 PID；另建私有 synthetic identity 访问模型 | ss 均未走 sudo；输出 pid unknown/not-a-systemd-unit，仍 exit 0。私有 key 存在且 sudo test 成功，未提权探测却不输出 registry-data；不是 Linux 权限实机验证 |
| 旧部署测试的准备失败 | 将新测试 SCRIPT 指向 `git show f9582e5:scripts/deploy_linux.sh` 导出文件 | 9 个用例均在提取不存在的 compose_registry_exec 时 ValueError，尚未执行旧安装行为；不能记为旧安装逻辑 9 个行为反例 |
| 旧 SSH 测试的实际结果 | 对旧脚本执行新测试文件 | 5 个测试方法包含 1 个始终通过的 fake 自检；其余结果含 2 个 subtest assertion failures、5 个 subtest/errors，涉及 mock stderr.write 与缺失 run_command；不能记为 5/5 行为证明 |
| Paramiko 独立行为证明 | 真实 `paramiko.Channel`，仅替换网络连接，分别投递延迟 stdout/stderr 和空 EOF | 两旧 helper 在两类场景各返回 255；两新 helper 的延迟场景返回远端 7 且双流完整、空 EOF 场景返回 0。无 SSH 网络连接 |
| STUN/最终标签 | 提取并执行真实 print_summary，设置 SKIP_DOCTOR=1 | STUN 确实标 SKIPPED；最终句仍无条件出现 real-NAT，即使脚本允许 LAN debugging。应将 direct-transfer/hash 事实与 NAT 观测分开 |
| 原 STATUS 未链接 K | `git archive f9582e5`；分别运行旧/新 checker | 旧检查器 passed=true 并选未链接 K；新检查器 passed=false，选已链接 J 并报最新 K 未链接、run 与措辞问题 |
| 同名不同目标 | `git archive f150503`；STATUS 所有 K 链接改指独立 archive 同名文件，其中加入直接矛盾 | 完整 checker 仍 passed=true，核对的是未链接 docs/K；对真实 archive 目标单独调用矛盾检查正确失败 |
| 外部同名目标 | STATUS 所有 K 链接改为 example.invalid 下同名 URL | 完整 checker 仍 passed=true，并检查本地 docs/K；全程无外部 URL 请求 |

| GitHub API 独立查询 | 提交/run | 实际结果与限制 |
|---|---|---|
| 原已有绿运行 | `579310a` / [43577126893](https://github.com/wmqfl861/AK-sending/actions/runs/43577126893) | completed success；12 常规任务 success，1 手动 release 深测 skipped |
| 原失败与重跑 | `f9582e5` / [43601899282](https://github.com/wmqfl861/AK-sending/actions/runs/43601899282) | attempt 1 的 Windows Tests (workspace) failure；attempt 2 completed success、12 常规 success、1 深测 skipped |
| 原失败日志 | job 132468237206 | 使用配置好的同一 git 凭据成功下载；确认 revoke_after_pairing_preserves_books_and_admits_further_sends 在 95.80s 套件结束时失败，redeemer 输出 starting/redeem 后以 pairing flow timeout 退出。日志不含足以量化 owner 冷启动的时间线。初次客户端将 GitHub Authorization 头转发到签名下载域，下载端返回 401 InvalidAuthenticationInfo；将该头限制为原 GitHub 域后下载成功，未更换身份或凭据 |
| 同用例历史 | [43312035496](https://github.com/wmqfl861/AK-sending/actions/runs/43312035496) / [43273272597](https://github.com/wmqfl861/AK-sending/actions/runs/43273272597) | API 和日志均确认同测试失败：前者 Ubuntu 等 listening 180s，后者 Windows 等 60s；说明同用例存在历史等待问题，不能据此证明相同唯一根因 |
| 修复候选 | `b6296ea` / [43627058493](https://github.com/wmqfl861/AK-sending/actions/runs/43627058493) | completed success；12 常规 success、1 手动深测 skipped，包含两平台 Rust、Flutter、Android 构建、仓库检查和 NAT 模拟 |
| 后续文档候选 | `9797c1b` / [43629054632](https://github.com/wmqfl861/AK-sending/actions/runs/43629054632) | completed success；12 常规 success、1 手动深测 skipped |
| 文档 tip 自身 CI | `f150503` / [43630743658](https://github.com/wmqfl861/AK-sending/actions/runs/43630743658) | 最后 API 查询 completed success；12 常规 success、1 手动深测 skipped。完整 head SHA 与被审 tip 一致 |

| 对执行方既有走查产物的复核 | 证据 | 结果与边界 |
|---|---|---|
| Windows 主题图 | win-light.log、win-dark.log 与 shots-light/dark | 两日志都有 COVERAGE transferring=1/verifying=1、ROUND_DONE、All tests passed；每主题 6 张有效 1264×681 PNG。抽查暗色传输图显示 76MB/200MB、60.1MB/s，可读无裁剪；属于进程内光栅 |
| Android 辅助 runner | aux-run7.log 与两轮接收文件 | 亮暗均完成且整体 All tests passed；重算两接收文件均为 8,388,608 字节，SHA-256 与源摘要一致（e442fcea…e5bc3）；不是系统 SAF 路径 |
| 速率条件边界 | aux-run7.log 的 transferring/verifying 样本 | 亮轮 transferring 样本跨度 706ms、暗轮仅 1 个样本跨度 0；首个 transferring 到 verifying 分别相隔 5384ms/4566ms。全部已观测 transferring 的 bps=null，因此本轮无速率行符合模型；不得称整个 transferring 相位不足 1s，亦不证明 J 失败的唯一根因 |
| 系统 SAF | saf-run5.log、DocumentsUI screen-22.png、合成源与接收文件 | 日志包含真实 picker 打开、选择 1 项、发送完成、All tests passed；截图为系统 Downloads 页面；重算两文件均 4,194,304 字节且 SHA-256 相等（f488b0ee…39cc）。本次核对既有产物，没有重跑完整流程 |
| SafeArea 首页 | 09-main-safearea-light/dark.png | 亮图干净完整，支持原工作单的标准首页 SafeArea 证据；暗图有 System UI isn't responding 对话框遮挡并变暗，标题/生成码/输入码按钮仍位于状态栏下。原工作单未要求两主题各一张干净首页，本点作为证据边界，不另加 Flutter 阻断 |
| 模拟器恢复 | emulator-restored-state.json | 记录 1080×2400、density 420、自动旋转 1、夜间模式 0、adb unroot；只核对已有记录，没有在本轮再次修改设备 |
| 产物版本 | 当前 CLI/DLL/x86_64 SO 与 §6 摘要 | SHA-256 前后对应；Rust 修复轮只有测试和注释变更，现有产品产物的证据不扩为新引擎部署 |

| 未执行项 | 边界 |
|---|---|
| Linux 原生 systemd/非 root 部署 | 本机 Windows；仅离线脚本行为及权限模型，未连接生产或远端 SSH |
| 完整 Windows/Android 集成走查重跑 | 本轮运行 analyze、单测与实际平台构建，并复核已有走查产物；不冒充新设备走查 |
| Android Rust FFI 双 ABI 重建 | 本轮本地 APK 构建使用现有 SO；候选 CI 的 Android job 包含实际双 ABI 源码构建 |
| 物理双机、抓包、release ignored 深载荷 | 本轮未执行；常规 CI 的手动深测均 skipped，不以常规成功替代 |
| shellcheck | 本机命令不可用；已运行 bash 语法、自检与离线执行模型 |

仓外证据：`ak-batchk2-review-20261115` 保存本次 Rust 日志、CI metadata/job logs 和配对专项结果；`ak-k2-script-review-3viokub9` 保存真实生成脚本、工具 stub、调用轨迹及 replay 脚本；`ak-k2-checker-review-cdqtxo09` 保存导出树与反例 JSON；Flutter 构建/测试日志使用 `k2-flutter-*`、`k2-dart-package.log`；执行方既有走查产物为 `ak-k1r-walkthrough`。上述均在临时目录、仓库外，未提交日志、身份或用户文件。

## 10. 修复轮二（K2 复核后的新候选，执行方记录）

[K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md)给出 REJECTED 后，执行方提交以下修复声明；复核方本地同步的 §9/STATUS/状态表/首轮复核注记一并入库。下表保留执行方提交时的声明，不能替代 [K3 独立复核](REVIEW_2026-11-15-batchK3.zh-CN.md) 对剩余反例及声明边界的判断；实际证据见 §11。

| K2 发现 | 修复内容 |
|---|---|
| K2-N1（P1） | 整个服务切换窗口（停服→legacy 退位→二进制切换→unit 写入→daemon-reload→enable --now）纳入统一失败恢复：切换前备份旧二进制/unit/启用与运行状态（任一备份失败在停服前中止）；`recover_services` 恢复旧配置并按先前状态重启，输出 `RECOVERY OK` 或显式 `RECOVERY INCOMPLETE`。对 unit 写入/reload/enable-start/恢复本身失败均有行为回归；旧候选（`git show f150503`/`f9582e5` 真实执行同 stub）复现为无恢复停服退出 |
| K2-R2（P1） | 停服失败不再吞掉（`|| return 1` 进入统一恢复）；阶段 7 保留 ExecStart 配置检查后，以每 unit `MainPID`+`sudo -n readlink /proc/<PID>/exe` 与部署二进制严格相等为决定性核验，显式拒绝 `(deleted)` inode。回归保留「旧进程仍在运行、磁盘路径已替换」前置：旧候选安装与核验双双假成功，新代码停服失败即中止并恢复、旧进程映射不匹配报 running-process mismatch（exit 6） |
| K2-R3（P1） | phase1 占用探测、pid→unit/cmdline、unit 文件读取、身份目录枚举与存在性判断全部经 `sudo -n` 只读执行；无法确定占用/身份/数据库时在切换前 exit 3 明确失败。离线权限模型（非 root SSH 用户、他人持有端口、私有身份目录）对照：`f9582e5` 输出 `pid unknown`/not-a-systemd-unit 且漏私有身份；新代码发现 pid/unit/ExecStart/私有 key+db。均为离线模型，不称已验证 Linux 权限场景 |
| K2-R4（P2） | 检查器改为从 STATUS 所在目录解析链接真实目标：去 fragment、拒绝外部 URL/绝对路径/越界 `..`、词法规范化后必须与最新 HANDOFF-BATCH-* 的规范路径相等；run 号/矛盾词校验锚定到该真实目标。反例翻转：同名异目录链接、外部同名 URL、缺失子目录目标在 `f150503` 检查器下通过、新代码报错；`f9582e5` 的同名 basename 旧反例保持失败 |
| K2-R5（P2） | `cross_nat_test.sh` 摘要按实际执行区分：doctor 跳过时写 `direct QUIC transfer sha256-verified over the observed path (no NAT observation)`，不再无条件宣称 real-NAT；s8 断言两种形态。SSH 套件移除恒真假自检，换为预修模式（timeout=0 立即读）真实负控；`TextIOWrapper` 修 mock，旧码结果为 7 失败 0 错误。部署测试改为执行基线自身生成的安装/核验脚本（`git show` 提取，缺标记则 skip），旧码对照为行为级未缓解失败演示；`OldHelperFromGitTests` 直接执行 `f9582e5` 两个脚本内嵌的真实 helper（延迟/空输出均 exit 255） |
| 文档 | 采纳并提交复核方 §9 及全部本地纠正（含暗色首页 ANR 遮挡边界、速率行时间跨度≠相位时长、owner 冷启动为推断非日志直接证明、`+修复轮` 占位换真实提交号、K-CI 行更新为新候选已验证）；本节为执行方补记，来源区别保留 |

### 修复轮二门禁与提交

执行方门禁与提交如下；本次复核实际重跑及 tip CI 的 API 查询统一记录在 §11，未要求执行方为已完成 CI 再追加文档提交。

| 执行方修复轮二门禁报告 | 对象 | 结果与边界 |
|---|---|---|
| Shell/Python/文档契约 | `f7dece2` 修复内容 | 两脚本 `bash -n` 与自检通过；Python 156/156（部署 23、SSH 7）；check_repository exit 0；契约 5/5。此行为执行方报告，独立执行见 §11 |
| Rust 变动范围 | `5beb10a..f150503` 至本轮 | 执行方报告本轮未改 Rust，沿用已验证代码链；独立 diff 与 tip CI 门槛见 §11 |

| 提交 | 内容 |
|---|---|
| `f7dece2` | K2-N1/K2-R2/K2-R3 部署脚本与回归；K2-R4 检查器链接解析与回归；K2-R5 摘要/负控/旧码执行化；复核方文档纠正与执行方补记入库 |
| `3db5259` | 回填实际修复提交号；本次独立复核的固定 tip |

## 11. 修复轮二独立复核证据（2026-11-15）

被审差异固定为 `f150503..3db5259`，判断与修复要求见 [K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md)，每项状态只在审核状态表。本节、K3 报告及该轮入口同步出具时为本地未提交记录，随后随 `e8b621a` 入库；本节保留当时的证据。离线 Bash/systemctl/权限模型、CI runner 实际执行和物理设备验收分别记录。

| 本次复核实际执行 | 命令/对象 | 结果与边界 |
|---|---|---|
| Python 全集 | `python -m unittest discover -s scripts/tests -v`；`3db5259` | 156 passed，0 failures/errors/skipped，exit 0；131.519s。覆盖现有回归，不覆盖下面新增的状态性反例 |
| 仓库/契约/差异 | `python scripts/check_repository.py`；`python scripts/check_contracts.py`；`git diff --check f150503..3db5259` | 仓库检查 exit 0、契约 5/5 PASS、差异格式 exit 0；另用完整导出树复现 checker 的混合目标漏检 |
| Shell 语法与离线自检 | 两脚本分别 `bash -n`、`--self-test` | 全部 exit 0；cross_nat 自检使用已有 `git 1e59342` release CLI，仅本地合成数据/解析，不连接 SSH 或执行跨网传输 |
| Rust/Cargo 变动范围 | `git diff --name-only 5beb10a..3db5259 -- crates Cargo.toml Cargo.lock rust-toolchain.toml` | 无差异，exit 0；未把既有本机 Rust 结果写成本轮重跑，tip CI 两平台 Rust 门槛见下表 |
| 旧部署代码行为 | `BaselineContrastTests` 及全量套件 | 独立专项 5/5，0 errors/skipped；真实提取 `f150503`、`f9582e5` 自身生成阶段，重现无恢复 unit-write 失败、旧进程假成功及非 root 发现盲区 |
| 旧 SSH helper | 将当前 `SshHelperDrainTests` 指向 `git show f9582e5` 导出的两脚本；另执行 `OldHelperFromGitTests` | 旧码 5 个方法产生 7 failures（含 subtest）、0 errors/skipped；失败包含行为及结构断言。真实旧 helper 的两脚本×延迟/空输出均 exit 255；当前套件全部通过 |
| 摘要对照 | 执行 `f150503` 与 `3db5259` 的真实 print_summary | skip-doctor：旧版仍称 real-NAT，新版使用 observed path/no NAT observation；未跳过时保留相应 STUN/NAT 文案 |
| 声明边界 | 静态核对旧码提取与缺失链接回归 | 指定旧基线阶段均实际执行；缺 marker 仍可能 ValueError，只有不可用 revision 会 skip。缺失 subdir 在旧专项函数中放行，但旧完整检查器已报断链，不能扩成完整检查器全部放行 |
| 本地文档同步门槛 | `python scripts/check_repository.py`；`git diff --check` | 同步后全部 exit 0；工作区仅 K3 新报告及 STATUS/状态表/交接/K2 历史归属注记的本地文档更改，未提交或推送 |

| 独立反例 | 实际执行与结果 | 证据层级 |
|---|---|---|
| K2-N1 部分 enable/start 成功 | 真实 phase456；两个旧服务 active/enabled，先启动新 registry，再让 STUN 启动失败。RUN_RC=5，输出 RECOVERY OK；DISK_OLD=True，但 registry 仍 git deadbee、STUN 为 git olddead | 维护服务状态及启动时版本的离线 systemctl 模型，复核集成者已重放 |
| K2-N1 disabled/inactive | 查询输出 disabled/exit 1、inactive/exit 3 被写成 unknown；部分启动失败后 RUN_RC=5、RECOVERY OK，两个服务仍 enabled、registry active | 同上；证明原启用/运行状态未恢复 |
| K2-N1 旧 binary rename 失败 | 旧文件 rename 返回失败，随后新文件切换成功、首次 unit tee 失败。RUN_RC=5、RECOVERY OK、BACKUP_OLD=True、DISK_OLD=False，两服务重启新版本 | 真实生成阶段、一次 rename 失败与 unit 写失败注入，仓外合成文件 |
| K3-N1 首次安装新旧对照 | 同一无 binary/unit 初始状态、stop 缺失 unit 返回 5；`f150503` RUN_RC=0 且完成写入启动，`3db5259` RUN_RC=5、unit-not-loaded、未安装 binary/unit | 执行两版自身 generator 的离线对照；不是 Linux systemd 真机结果 |
| K2-R3 关键读取被拒绝 | 保持 sudo true 成功，分别只拒绝 home ls、私有 key/db test、registry cgroup cat；真实 phase1 均 RUN_RC=0，实际后续决策均 POST_PHASE1_ACCEPTED。前两者漏已有合成 key/db，后者接受未知 registry 占用者 | 离线逐命令权限模型；既执行发现阶段，也执行实际 real_run 的预检后决策，未绕过真实权限 |
| K2-R3 正向发现解析 | 使用带 NUL、无结尾换行的 cmdline fixture，无失败注入。old-registry-unit 拼入 cmdline 行，实际行首 parser 输出空；RUN_RC=0、POST_PHASE1_ACCEPTED old_registry= | 真实 parser 链；现有正向测试只检查子串，未覆盖该遗漏 |
| K2-R4 全部链接改 archive/外部 URL | 在 `git archive 3db5259` 树执行旧/新完整 checker；两类均为旧通过、新拒绝 | 仓外文档树、完整检查器实际执行，不访问外部 URL |
| K2-R4 混合当前批次目标 | 第一处 K 链接规范，后续最新验证/交接入口指 archive 同名矛盾文件；新 checker exit 0、passed=true、errors=[]。交换顺序后拒绝，直接查 archive 也报矛盾 | 已保留被误接受的完整导出树，复核集成者直接运行 CLI 重现 |

| GitHub API / 引用 | 提交/来源 | 实际结果与边界 |
|---|---|---|
| tip CI | `3db5259a3a56704e56709c0d615287081d17c055`；[43636744785](https://github.com/wmqfl861/AK-sending/actions/runs/43636744785) | run_attempt=1，push，completed/success；13 jobs 中 12 常规 success、1 release-deep-tests skipped。run/jobs API 通过当前 git 凭据读取，head_branch 与完整 head SHA 均匹配 |
| CI 实际门槛 | 同上 jobs/steps metadata 与对应 workflow | Linux/Windows Rust fmt、clippy、workspace/特性测试、rustdoc；Flutter analyze/test、纯 Dart、Android 双 ABI 与 debug APK、Linux/macOS portability、依赖审计、仓库检查和 NAT netns 模拟均 success；不等于生产 systemd 部署或物理双机验收 |
| 本地及远端引用 | `git fetch --prune origin`、rev-parse、branch/worktree 与 branches API | main/origin/main 均 `d6c71360784e1570720e56028a39f2a80777bb21`；HEAD/开发分支及 origin 对应分支均 `3db5259a3a56704e56709c0d615287081d17c055`。仅 main 与当前批次两个本地/远端分支，1 个 worktree；未合并、未推送、未删除分支 |

| 未执行项 | 原因/边界 |
|---|---|
| 本轮本机 Rust/Flutter/Dart 全量与平台重建 | 本轮只改脚本/检查器/证据文档；Rust 树无变化，使用已核实 tip CI 的实际门槛，不冒充本机重跑 |
| Linux 原生权限/systemd 部署、远端 SSH | 本机 Windows；本轮只执行真实生成脚本的隔离状态/权限模型，未连接生产 |
| 物理双机 C4、抓包、release ignored 深载荷 | 本轮未执行；tip CI 手动深测为 skipped |
| shellcheck | 本机命令不可用；已执行 bash 语法、自检与离线行为模型 |

仓外材料：`AK-review-K3-20261115` 保存脱敏后的 run/jobs/branches API 元数据；`ak-k3-recovery-repro.py` 与 `ak-k3-fresh-baseline-repro.py` 及同名前缀 `.out.txt` 保存状态性恢复和首次安装对照；`batchK3-privilege-repro.py` 保存权限及发现/parser 链重放；`ak-k3-checker-evidence-27iv0s4y` 保存 replay、JSON 与误接受导出树。均在本机临时目录，不提交凭据、日志、身份数据或用户文件。重放命令见 K3 报告。

## 12. 修复轮三（K3 复核后的新候选，执行方记录）

[K3 复核](REVIEW_2026-11-15-batchK3.zh-CN.md) 后，执行方提交下列修复声明；复核方本地同步（§11、STATUS、状态表、K2 报告历史注记）与修复代码和本节一并入库。以下保留执行方提交时的记录，新的独立行为证据见 §13，逐项判断只在审核状态表。

| K3 发现 | 修复内容 |
|---|---|
| K3-N1（P1） | 新增 `probe_unit`/`stop_unit`：经 `systemctl cat` 三态判定存在/确认缺失/不可判定；确认缺失的 unit 记录「nothing to stop」跳过，存在的 unit 保持 K2-R2 严格停服，不可判定即拒绝。全新安装与 legacy-only 迁移回归：真实生成脚本在缺失 unit 夹具上完成安装+enable+核验（对照 `f7dece2` 同夹具盲停服 exit 5、`f150503` 可完成） |
| K2-N1 余项（P1） | (a) 部分启动恢复：恢复时对捕获为 active 的 unit 检查现时状态——active 则 `restart`（吃到恢复后的旧文件）、inactive 则 `start`，再以 MainPID→`/proc/<PID>/exe` 核对运行镜像，不匹配报 `RECOVERY INCOMPLETE`；(b) 捕获保留 `is-enabled`/`is-active` 原始输出（不再置空），无法判定即切换前拒绝，恢复按布尔对恢复 enable/disable 与 start/stop；(c) 旧二进制 move-aside 不再 `|| true`：失败保留停服前备份为恢复源，恢复先试 `ak-cli.old-<ts>` 再回退 `cp -a` 备份，两者皆败报 INCOMPLETE，全新安装场景整体回滚新二进制。systemctl stub 升级为有状态（enabled/active/MainPID/running.bin=启动时镜像字节），断言对象为运行镜像而非调用日志；`f7dece2` 基线以同夹具复现三种缺陷各自的行为 |
| K2-R3 余项（P1） | 每个特权发现探测（cgroup 读取、home 枚举、`priv_exists` 身份/库存在性）不可判定即 exit 3；real_run 决策块对 pid-unknown/非 systemd 占用显式拒绝并单独回归；`/proc/<pid>/cmdline` 输出显式补行尾，无换行夹具经真实 p1+决策块验证 unit 归属正确（`f7dece2` 同夹具字段粘连得出 none-detected） |
| K2-R4 余项（P2） | 检查器逐链接校验：STATUS 中每个交接形态链接各自解析（去 fragment、拒外部/绝对/越界），当前批次链接必须全部解析到最新交接的规范路径（旧批次链接为历史，仅要求可解析存在）；逐条带行号报错，无首链接短路。反例翻转：规范首链+后续 archive 链（复核复现）旧检查器 exit 0、新代码 exit 1 列出两处行号；顺序颠倒同样失败；通用断链 walk 已拒缺失子目录的目标，专项错误为改进点。“仅 git 无法提供基线时 skip”的限定适用于 SSH helper；部署 baseline 缺 marker 仍可能 SkipTest，实际核对见 §13 |

### 修复轮三门禁与提交

| 执行方本地门禁 | 对象 | 提交时报告 |
|---|---|---|
| Shell | 两脚本 `bash -n` 与自检 | 全过 |
| Python | 完整套件，含真实 git 基线对照 | 177/177；其中部署套件 40 |
| 仓库与契约 | `check_repository.py`；`check_contracts.py` | exit 0；5/5 |
| Rust 变动范围 | Rust 树与已验证链比较 | 无变化；未声称本轮重跑 |

| 提交 | 内容 |
|---|---|
| `e8b621a` | K3-N1/K2-N1/K2-R3/K2-R4 修复与回归；复核方文档纠正与执行方本节记录入库 |
| `31cf22c` | 仅回填实际修复提交号；本次独立复核固定 tip |

## 13. 修复轮三独立复核证据（2026-11-15）

被审差异固定为 `3db5259..31cf22c`。发现、触发与修复要求见 [K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md)，每项状态只在审核状态表。本节、K4 报告及入口同步出具时仅保留本地，随后随 `46473fc` 入库。下表为该轮实际执行时的历史证据；离线 systemctl/权限模型不代表原生 Linux 或生产验收。

| 本次复核实际执行 | 命令/对象 | 结果与边界 |
|---|---|---|
| Python 全集 | `python -m unittest discover -s scripts/tests -v`；`31cf22c` | 177 passed，0 failures/errors/skipped，exit 0；311.264s。指定旧 git 基线均实际执行；绿套件不覆盖下表额外反例 |
| 并行专项 | 部署套件；K2-R3 筛选；checker 套件 | 分别 40/40（265.904s）、10/10（35.058s）、44/44，通过且无 skip；与全集为不同执行，不累计为独立案例数 |
| 仓库/契约/差异 | `python scripts/check_repository.py`；`python scripts/check_contracts.py`；`git diff --check 3db5259..31cf22c` | 仓库 exit 0、契约 5/5 PASS、差异格式 exit 0；完整 checker 额外反例见下表 |
| Shell 语法与自检 | 两脚本分别 `bash -n` 与 `--self-test` | 全部 exit 0；cross_nat 使用既有 `git 1e59342` release CLI，仅本地合成数据/解析，无 SSH 或跨网传输 |
| Rust/Cargo 变动范围 | `git diff --name-only 5beb10a..31cf22c -- crates Cargo.toml Cargo.lock rust-toolchain.toml` | 无差异，exit 0；本机未重复 Rust/Flutter/Dart 全量，tip CI 实际门槛见下表 |
| 基线提取声明 | 对可读但缺 marker 的合成 baseline 调用部署 `phase_defs` 与 SSH `extract_helper_text` | 部署抛 SkipTest，SSH 抛 AssertionError；不能把后者的限定泛化到全部部署基线。指定实际旧基线均可提取，本轮行为对照有效 |
| 本地文档同步门槛 | `python scripts/check_repository.py`；`git diff --check` | 同步后均 exit 0；仅新增 K4 报告并同步 STATUS、状态表、交接与 K3 历史归属，代码未修改，文档未提交/推送 |

| 独立追加反例 | 实际执行与结果 | 证据层级 |
|---|---|---|
| K3-N1 标准缺失 unit 文案 | 真实 phase456：仓内文案 INSTALL_RC=0；只换成 `No files found for ...` 后候选 INSTALL_RC=5、切换未开始、binary/unit 未安装；同夹具真实 `f150503` INSTALL_RC=0 | 集成者重放；systemd v255 上游源码核对＋离线模型，不是本机原生 systemd |
| K2-N1 恢复查询无法判断 | disabled/inactive 原态，捕获及切换前查询正常；先启动新 registry 再令 STUN 失败，只让恢复阶段 registry is-active 无输出/exit 1。INSTALL_RC=5、日志 RECOVERY OK；最终 registry disabled 但 active，running.bin 仍 git deadbee 新镜像 | 集成者重放真实 phase456、有状态 systemctl 模型，最终状态与镜像直接读取 |
| K2-N1 非空未知捕获值 | 已有 stub 输入 SYSTEMCTL_IS_ACTIVE=unknown，真实 phase456 捕获 enabled:unknown 并 INSTALL_RC=0，执行 stop/安装 | 合成边界实验；不声称某真实 systemd 前置必然返回 unknown |
| K2-R3 标准权限诊断 | 选择性令已有合成身份目录，或其 key/db 的 ls -d 返回标准 cannot access + Permission denied。两变体均 PHASE1_RC=0、decision exit 0，existing registry data dir 为 none-found | 集成者重放真实 phase1＋real_run 实际决策块；工具为离线权限 stub |
| K2-R3 旧 unit 未进入迁移集合 | 分别令已知旧 unit 的 test -f 权限失败，或 unit 只在 vendor 目录。两变体 phase1 均识别 oldregistry.service 占端口，但 decision exit 0、old registry 为 none-detected、ExecStart 为 none | 集成者重放；机械执行至真实预检决策，不称已执行后续部署 |
| K2-R4 原混合目标反例 | 两个 git archive 完整树：规范首链/后续 archive，3db5259 exit 0；31cf22c exit 1，点名 STATUS 第 9/13 行；反序候选 exit 1，点名第 5/13 行；历史缺失目标也拒绝 | 集成者重放完整检查器；原顺序问题已有有效修复 |
| K2-R4 跨行链接回归 | 首个 archive 链接 label 跨行、其他规范链接保留：3db5259 完整 checker exit 1；31cf22c exit 0、passed=true、errors=[]。带 title 的 archive 链接两版均 exit 0，属既有限制 | 完整导出树＋存在的合成 archive 文件；未修改仓内 STATUS 为反例 |

| GitHub API / 引用 | 提交/来源 | 实际结果与边界 |
|---|---|---|
| tip CI | `31cf22c0d0583b7794e896aca6564ffd4d5ab0a7`；[43645127728](https://github.com/wmqfl861/AK-sending/actions/runs/43645127728) | run_attempt=1，push，completed/success；13 jobs 中 12 常规 success、1 release-deep-tests skipped。当前 git 凭据读取 run/jobs API，完整 head SHA、head_branch 均匹配 |
| CI 实际门槛 | 同上 jobs/steps metadata 与固定 tip workflow | Linux/Windows Rust fmt、clippy、workspace/特性测试、rustdoc；Flutter analyze/test、纯 Dart；Android 双 ABI/符号/实验变量泄漏检查及 debug APK；Linux/macOS portability、依赖审计、仓库检查与 NAT netns 模拟均 success。不是生产部署或物理双机证据 |
| 本地及远端引用 | fetch --prune、ls-remote、branch/worktree、branches API | main/origin/main 均 `d6c71360784e1570720e56028a39f2a80777bb21`；HEAD/开发分支及 origin 对应分支均 `31cf22c0d0583b7794e896aca6564ffd4d5ab0a7`。仅 main 与当前批次两个本地/远端分支，1 个 worktree；本轮未合并、未提交、未推送、未删除分支 |

| 未执行项 | 原因/边界 |
|---|---|
| 本机 Rust/Flutter/Dart 全量与平台重建 | 本轮脚本/检查器/文档变更，Rust/Cargo 树无变化；使用已核实 tip CI 的实际门槛，不冒充本机重跑 |
| Linux 原生 systemd/权限部署、远端 SSH | 本机 Windows，无可用 WSL 发行版；未安装环境或连接生产，使用真实生成脚本的隔离模型 |
| 物理双机 C4、抓包、release ignored 深载荷 | 本轮未执行；tip CI 手动深测 skipped |
| shellcheck | 本机命令不可用；已执行 bash 语法、自检和离线行为模型 |

| 仓外材料（均在本机 TEMP） | 内容 |
|---|---|
| `AK-review-K4-20261115` | 脱敏 run/jobs/branches JSON、Python 完整测试日志、集成者 recovery/privilege/checker 重放日志；无凭据 |
| `ak-k4-recovery-repro.py`；`ak-k4-recovery-evidence-lj0gt6pq` | 恢复与首次安装重放脚本、各场景 generator/状态文件/输出、summary.json、上游 systemd v255 源码；全部合成数据 |
| `ak-k4-privilege-repro.py`；`ak-k4-privilege-evidence-rx1eyxh_` | 真实 phase1/决策块重放、选择性权限与 unit 路径模型、逐案 JSON、summary.json |
| `ak-k4-checker-replay.py`；`ak-k4-checker-evidence-ec0q812_` | 两版完整导出树及逐场景 JSON；脚本结束时导出树 STATUS 已还原，反例应按 replay 重放，不能以还原后的树代替反例 |

## 14. 修复轮四（K4 复核后的新候选，执行方记录）

[K4 复核](REVIEW_2026-11-15-batchK4.zh-CN.md)（K2-R2 维持通过、K2-R5 行为证据接受）后，执行方修复其四项余留；复核方本地同步（§13、STATUS、状态表、K3 报告历史注记）与本节代码改动一并提交。

| K4 发现 | 修复内容 |
|---|---|
| K3-N1 余留（P1） | `probe_unit` 缺失判定补入 systemd v255 标准诊断 `No files found for <unit>.`（引 upstream `systemctl-util.c` finish 路径；测试 stub 默认诊断同步改为该串）。复核精确夹具（该诊断串的全新安装）在轮四代码 exit 5、新代码完成安装；未识别诊断仍拒绝且零触碰；`f150503` 基线同夹具可完成（复核锚点） |
| K2-N1 余留（P1） | 新 `qstate` 三态化恢复期查询（active/inactive/不可判定，非零 rc 保留 stdout）；active 分支查询失败走 restart+镜像核验；inactive/absent 分支遇 active-or-不可判定保守 stop 后确认，不可确认即 `RECOVERY INCOMPLETE`——复核序列（disabled/inactive 起步、新 registry 已启动、仅恢复期 registry is-active 查询失败）从「RECOVERY OK+新镜像在跑」翻转为 INCOMPLETE+保守停机；捕获端不支持的值（unknown 等）切换前拒绝 |
| K2-R3 余留（P1） | `priv_exists` locale 固定（`sudo -n env LC_ALL=C ls -d`）且仅纯 ENOENT 判缺失：`cannot access … Permission denied` 等一律不可判定 exit 3（目录级与 key/db 级夹具从「none-found 放行」翻转为拒绝）；已识别占用 unit 的配置探测改为在 `/etc`、`/run/systemd`、`/run/systemd/transient`、`/usr/lib`、`/lib` 五处 systemd 目录经 `priv_exists` 搜索——探测失败拒绝、找到即入迁移集、识别却无处定位显式 exit 3（占用不再静默脱靶；vendor 目录夹具经真实 p1+决策块绑定 oldregistry） |
| K2-R4 回归（P2） | 检查器改全文匹配（`finditer` 全文 + 偏移计算行号）：跨行标签与带 CommonMark 标题的链接均被捕获并在起始行报错；复核多行标签 archive 夹具在 `31cf22c` exit 0、新代码 exit 1；带标题 archive 链接同翻；既有单行两序/外部 URL/全规范回归保持 |

### 修复轮四门禁与提交

以下为执行方提交时的本地报告；独立复核另见 §15。

| 执行方本地门禁 | 对象 | 提交时报告 |
|---|---|---|
| Shell | 两脚本 `bash -n` 与自检 | 全部通过 |
| Python | 完整套件，含真实 git 基线对照 | 190/190；部署套件 49，含 9 项新增部署回归与 `f9582e5`/`f150503`/`f7dece2` 基线 |
| 仓库与契约 | `check_repository.py`；`check_contracts.py` | exit 0；5/5 |
| Rust 变动范围 | Rust 树与已验证链比较 | 无变化；未声称本轮重跑 |

| 提交 | 内容 |
|---|---|
| `46473fc` | K3-N1/K2-N1/K2-R3 余留与 K2-R4 回归修复；复核方文档纠正与执行方本节记录入库 |
| `a8bb3b9` | 仅回填修复提交号；修复轮四独立复核固定 tip |

## 15. 修复轮四独立复核证据（2026-11-15）

被审差异固定为 `31cf22c..a8bb3b9`。发现、触发与修复要求见 [K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md)，逐项判断只在审核状态表。本节、K5 报告及入口同步出具时仅保留本地，随后由执行方在 `e8f811d` 提交。旧反例的有效修复与追加边界分别记录；离线模型不作原生 systemd、Linux 权限或生产验收证明。

| 本次复核实际执行 | 命令/对象 | 结果与边界 |
|---|---|---|
| Python 全集 | `python -m unittest discover -s scripts/tests -v`；`a8bb3b9` | 190 passed，0 failures/errors/skipped，exit 0；387.244s。指定 `f9582e5`/`f150503`/`f7dece2` 基线均实际执行；日志保存仓外 |
| 并行专项 | 部署套件；checker 套件 | 部署两次独立运行均 49/49、无 skip；checker 48/48、无 skip。与全集不累计为新增案例数 |
| 仓库/契约/差异 | `python scripts/check_repository.py`；`python scripts/check_contracts.py`；`git diff --check 31cf22c..a8bb3b9` | 仓库 exit 0、契约 5/5 PASS、差异格式 exit 0；完整 checker 追加反例见下表 |
| Shell 语法与自检 | deploy_linux/cross_nat 分别 `bash -n` 与 `--self-test` | 全部 exit 0；cross_nat 使用既有 `git 1e59342` release CLI，只执行本地合成数据/解析，无 SSH 或跨网传输 |
| Rust/Cargo 变动范围 | `git diff --name-only 5beb10a..a8bb3b9 -- crates Cargo.toml Cargo.lock rust-toolchain.toml` | 无差异；本机未重复 Rust/Flutter/Dart 全量，固定 tip CI 实际门槛见下表 |
| 哈希回填 | `git diff 46473fc a8bb3b9` | 仅交接、状态表三行替换为 `46473fc`，无代码变化 |
| 本地文档同步门槛 | `python scripts/check_repository.py`；`git diff --check` | 同步后均 exit 0；仅新增 K5 报告并同步 STATUS、状态表、交接与 K4 历史归属，代码未修改，文档未提交/推送 |

| K4 输入与追加边界 | 实际执行与结果 | 证据层级 |
|---|---|---|
| K3-N1 v255 精确缺失文案 | 从 git 提取 `31cf22c` 与 `a8bb3b9` 各自真实 generator，同一 `No files found for ak-registry-server.service.`：旧 phase456 exit 5、binary 未安装；新 exit 0、新 binary 已安装；全集中的 `f150503` 同夹具可完成 | 专项旧新对照；当前全集另执行 f150503 锚点，无真实 systemd |
| K3-N1 标准 D-Bus ENOENT | 集成者重放：所有 systemctl 持续返回 `Failed to connect to bus: No such file or directory` 时，捕获 absent:: 仍安装 binary/写 units，reload 后才失败并 INCOMPLETE；只让 cat 失败的变体 phase456 exit 0、无 stop、磁盘新而运行旧 | 真实 phase456、离线模型＋v255 上游调用链核对；持续故障夹具中旧 binary/unit 成功恢复，未观察到删除 |
| K2-N1 K4 查询失败与未知捕获 | 精确 afterN 故障注入回归通过：原 disabled/inactive，新 registry 已启动后恢复查询失败，最终 INCOMPLETE、inactive/disabled、无新镜像；两个 unknown 捕获值均停服前拒绝 | 全集及部署专项实际执行，保留原反例前置 |
| K2-N1 active/absent 查询失败 | 集成者追加：原 active 查询失败走 restart，旧镜像+OK；原 absent 查询失败保守 stop，最终 inactive/disabled、binary/unit 已移除，因查询不可确认报 INCOMPLETE | 最终重放补充 rm 路径映射与越界拒绝；不采用早期未映射 rm 的 absent 文件证据 |
| K2-N1 deactivating 余项 | 集成者重放真实 phase456：旧 disabled/inactive、新 registry 启动、STUN 失败时 registry 异步停止；候选 RECOVERY OK，但 deactivating、PID 5001、新镜像仍在；只有切换前一次 stop。仅仓外移除 qstate 的 deactivating 终态分类后，同夹具执行第二次 stop，最终 inactive、无 PID/新镜像 | 有状态模型＋v255 service 状态映射；单变量对照定位原因，不是仓内修复或真机部署 |
| K2-R3 K4 权限/旧名 unit 输入 | 目录及 key 的标准 Permission denied 均 exit 3；旧名 vendor unit 经真实 phase1+decision 绑定 oldregistry；配置探测失败、识别但无处定位均拒绝 | 全集与部署专项实际执行；sudo 工具为离线模型 |
| K2-R3 标准名 vendor unit | 集成者重放：cgroup=ak-registry-server.service、unit 只在 /usr/lib、合成旧 key/db 与完整 cmdline 存在；phase1/真实决策均 exit 0，却记录 unit absent、ExecStart none、data none-found，实际 compose 选择默认 REMOTE_DATA，DATA_SOURCE 空 | 执行至真实 phase4–6 配置选择；没有执行后续部署，不声称真实身份已轮换 |
| K2-R4 K4 精确输入 | 两版完整 git archive 树：跨行 label 与双引号 title 的 archive 链接在旧版 exit 0；候选 exit 1，分别报起始行 5/9；单行两序及缺失历史目标仍拒绝 | 集成者重放完整检查器，archive 文件真实存在 |
| K2-R4 合法标题/空白余项 | 圆括号 title、destination 前空格/换行三种 archive 输入，候选完整 checker 都 exit 0、passed=true、errors=[]，只核对规范文档；CommonMark 0.31.2 示例 505/510 已读取 | 完整导出树＋合成矛盾 archive 文件＋官方规范，未向仓内增加 archive 文件 |
| K2-R2/K2-R5 保留证据 | 严格 stop、phase7 旧路径/deleted executable；skip-doctor 摘要；真实旧 SSH helper 和部署 git 阶段对照 | 全集及两脚本自检实际执行；基线缺 marker 的部署/SSH 不同处理已准确限定 |

| GitHub API / 引用 | 提交/来源 | 实际结果与边界 |
|---|---|---|
| tip CI | `a8bb3b9fa103329b5c4bd410fab45d689da5d8b1`；[43651202803](https://github.com/wmqfl861/AK-sending/actions/runs/43651202803) | run_attempt=1，push，completed/success；13 jobs 中 12 常规 success、1 release-deep-tests skipped。当前 git 凭据读取 run/jobs API，head_branch 与完整 head SHA 匹配 |
| CI 实际门槛 | 同上 jobs/steps metadata 与固定 tip workflow | Linux/Windows fmt、clippy、workspace/特性测试及 rustdoc；Flutter analyze/test 与纯 Dart；Android 双 ABI/符号/实验变量泄漏检查、debug APK；Linux/macOS portability、依赖审计、仓库检查、legacy signal 与 NAT netns 模拟均 success。不是生产部署或物理双机证据 |
| 本地及远端引用 | fetch --prune、rev-parse、ls-remote、branch/worktree、branches API | main/origin/main 均 `d6c71360784e1570720e56028a39f2a80777bb21`；HEAD/开发分支与 origin 对应分支均为固定 tip。仅 main 与当前批次两个本地/远端分支、1 个 worktree；本轮未合并、未提交、未推送、未删除分支 |

| 未执行项 | 原因/边界 |
|---|---|
| 本机 Rust/Flutter/Dart 全量及平台重建 | 本轮脚本/检查器/文档变更，Rust/Cargo 树无变化；使用已核实固定 tip CI 的实际门槛，不冒充本机重跑 |
| Linux 原生 systemd/权限部署、远端 SSH | Windows 本机只执行生成脚本的隔离模型和上游源码核对，未连接生产 |
| 物理双机 C4、抓包、release ignored 深载荷 | 本轮未执行；tip CI 手动深测 skipped |
| shellcheck | 本机命令不可用；已执行 bash 语法、自检和离线行为模型 |

| 仓外材料（本机 TEMP） | 内容 |
|---|---|
| `AK-review-K5-20261115` | 脱敏 run/jobs/branches JSON、Python 全集日志、集成者 checker/probes/vendor/recovery 重放日志；无凭据 |
| `ak-k5-v255-contrast.py`；`ak-k5-v255-contrast-o827z_ol` | git 旧新 generator 的精确 v255 输入对照与 summary.json |
| `ak-k5-probes-repro.py`；`ak-k5-probes-evidence-tdoqk1bb`；`ak-k5-systemd-upstream` | 集成者 D-Bus ENOENT 重放、各阶段输出/状态、summary.json、上游 systemd v255 调用链源码 |
| `ak-k5-vendor-repro.py`；`ak-k5-vendor-evidence-i_7ur_8i` | 标准名 vendor unit 的真实 phase1/决策/compose 链；result.json 与合成身份文件 |
| `ak-k5-recovery-boundary-repro.py`；`ak-k5-recovery-review-vnp45bct` | 集成者 active/absent/deactivating 三场景；results.json、隔离删除 stub、状态与运行镜像 |
| `ak-k5-recovery-confirm.py`；`ak-k5-deactivating-confirm-g713dwtk` | 集成者候选与仓外单变量修正对照；上游 v255 service.c/systemctl-is-active.c |
| `ak-k5-checker-replay.py`；`ak-k5-checker-evidence-dbfxuxqr`；`ak-k5-commonmark-0.31.2-spec.json` | 集成者两版完整树及各输入 JSON，官方 Markdown 示例；脚本结束已还原导出树 STATUS，反例按脚本重放 |

## 16. 修复轮五（K5 复核后的新候选，执行方记录）

[K5 复核](REVIEW_2026-11-15-batchK5.zh-CN.md)（K2-R2 维持通过、K2-R5 行为证据继续接受）后，执行方修复其四项余留；复核方本地同步（§15、STATUS、状态表、K4 报告历史注记）与本节代码改动一并提交。

| K5 发现 | 修复内容 |
|---|---|
| K3-N1 余留（P1） | `probe_unit` 缺失判定锚定 unit 名（locale 固定）：仅 `No files found for <unit>.`、`Unit <unit> could not be found`、`<unit> is not loaded`、`Unit <unit> not loaded` 判缺失；`Failed to connect to bus: No such file or directory` 等不含 unit 名的连接/守护错误一律不可判定、捕获期拒绝。复核三种变体（仅 cat 失败/全部调用失败/cat+reload 双失败）从轮五的「跳过全部停服 exit 0」或「装到 reload 才恢复」翻转为切换前拒绝或响亮失败；`46473fc` 基线同夹具复现静默成功缺陷；v255 全新安装回归保持 |
| K2-N1 余留（P1） | `qstate` 仅映射终态（active→0，inactive/failed→1），activating/reloading/deactivating/空值一律不可判定走保守路径。复核 deactivating 场景（部分启动失败后 RECOVERY OK+新镜像+PID 存活）翻转为保守停机+确认后 OK 或 INCOMPLETE；`46473fc` 基线同夹具复现缺陷 |
| K2-R3 余留（P1） | 新 `find_unit_file` 五目录搜索；占用块不再排除标准名 `ak-registry-server.service`——识别到的标准名占用读取其 ExecStart/`--id-dir` 入迁移配置（`old-registry-unit` 字段仍限非标准名，标准 unit 已在托管停备集合）。复核夹具（/usr/lib 下标准名 unit + `--id-dir /var/lib/ak-legacy`）从轮五的「unit 判缺席→默认身份目录→迁移源空」翻转为真实决策+配置链保留 `/var/lib/ak-legacy`；`46473fc` 基线复现丢失；标准名缺席时全新安装不变 |
| K2-R4 余留（P2） | 共享 `MARKDOWN_LINK_RE` 扩至 CommonMark inline 链接目标+标题语法：`(` 后可选空白/换行、标题支持双引号/单引号/圆括号三种（括号内不再跨括号，范围注释注明）。复核三种形态（圆括号标题、换行前置空白、空格前置空白）指向 archive 的入口从「passed=true errors=[]」翻转为行号报错；真实仓库两代正则链接计数一致（231=231）；规范变体全部通过 |

### 修复轮五门禁与提交

以下为执行方提交时的本地报告；独立复核另见 §17。

| 执行方本地门禁 | 对象 | 提交时报告 |
|---|---|---|
| Shell | 两脚本 `bash -n` 与自检 | 全部通过 |
| Python | 完整套件及真实 git 基线 | 203/203；部署套件 58，含 9 项轮六回归与 `f9582e5`/`f150503`/`f7dece2`/`46473fc` 基线对照 |
| 仓库与契约 | `check_repository.py`；`check_contracts.py` | exit 0；5/5 |
| Rust 变动范围 | Rust 树与已验证链比较 | 无变化；未声称本轮重跑 |

| 提交 | 内容 |
|---|---|
| `e8f811d` | K3-N1/K2-N1/K2-R3 余留与 K2-R4 链接语法修复；复核方文档纠正与执行方本节记录入库 |
| `9c2d3bd` | 仅回填修复提交号；修复轮五独立复核固定 tip |

## 17. 修复轮五独立复核证据（2026-11-15）

被审差异固定为 `a8bb3b9..9c2d3bd`。发现、触发与修复要求见 [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md)，逐项判断只在审核状态表。本节、K6 报告及入口同步出具时仅保留本地，随后由执行方在 `bb1f673` 提交。下列“集成者”指该轮主复核者，“专项”指按模块并行的只读复核；离线模型和上游源码核对不作原生 systemd、Linux 权限或生产验收证明。

| 本次复核实际执行 | 命令/对象 | 结果与边界 |
|---|---|---|
| Python 全集 | `python -m unittest discover -s scripts/tests -v`；固定候选代码 | 203 passed，0 failures/errors/skipped，exit 0；unittest 计时 468.110s。部署套件 58、checker 52；指定 `f9582e5`/`f150503`/`f7dece2`/`46473fc` 基线均实际执行，日志保存仓外 |
| 并行专项 | checker 全集；部署 `-k k2r3` | 52/52、18/18，均无 skip；与全集不累计为新增案例数。K2-R3 最初手选类名有一次 unittest loader error，纠正后专项完成，不是产品失败 |
| 仓库/契约/差异 | `python scripts/check_repository.py`；`python scripts/check_contracts.py`；`git diff --check a8bb3b9..9c2d3bd` | 仓库 exit 0、契约 5/5 PASS、差异格式 exit 0；完整 checker 追加反例见下表 |
| Shell 语法与自检 | deploy_linux/cross_nat 分别 `bash -n` 与 `--self-test` | 全部 exit 0；cross_nat 使用既有 `git 1e59342` release CLI，只执行本地合成数据/解析，无 SSH 或跨网传输 |
| Rust/Cargo 变动范围 | `git diff --name-only 5beb10a..9c2d3bd -- crates Cargo.toml Cargo.lock rust-toolchain.toml` | 无差异；本机未重复 Rust/Flutter/Dart 全量，固定 tip CI 实际门槛见下表 |
| 哈希回填与前轮入库 | `git diff e8f811d 9c2d3bd`；`git show e8f811d` | 回填仅交接 1 行、状态表 2 行，无代码变化；K5 报告、§15、STATUS、状态表与 K4 历史注记已入库 |
| 本地文档同步门禁 | `python scripts/check_repository.py`；`git diff --check`；最终引用核对 | 同步后均 exit 0，checker errors=[]、local_links_checked=240。只新增 K6 报告并同步 STATUS、状态表、交接和 K5 历史归属，共 5 个文档；代码未修改、文档未提交/推送，最终 ls-remote 再次确认 main 与候选引用保持原值 |

| K5 输入与追加边界 | 实际执行与结果 | 证据层级 |
|---|---|---|
| K3-N1 D-Bus ENOENT | 集成者/专项各从 git 提取 `46473fc`、`9c2d3bd` 的真实 generator：仅 cat、全部调用、cat+reload 三变体，候选均 phase456 exit 5、未进入 switchover、零 stop、旧 binary/进程保持。基线仅 cat 时 exit 0、磁盘新/运行旧；另外两例在切换后进入恢复 INCOMPLETE | 两版同夹具离线执行；拒绝发生在捕获期，没有把后续失败算成提前拒绝 |
| K3-N1 v255 首次安装 | 同一 `No files found for <unit>.`、真正缺席/inactive 输入，两版 phase456 均 exit 0、新 binary 与新进程 | 真实生成脚本，非原生 systemd |
| K2-N1 deactivating | 基线 RECOVERY OK+PID 5001+新镜像；候选 phase456 exit 5，但恢复执行第二次 registry stop，最终 inactive、无 PID/新版、RECOVERY OK | 集成者/专项各独立重放；失败部署与成功回退分开记录 |
| K2-N1 追加过渡态与停止失败 | 专项重放 activating/reloading 各进入保守 stop+确认，最终 inactive/无 PID/新版；deactivating 的恢复 stop 故障则明确 INCOMPLETE、没有 OK | 追加 3 案状态模型，结果 JSON 已读取；不把未能停止的进程写成恢复完成 |
| K3-N1 缺文件但 active 余项 | 集成者/专项重放：移除合成 unit 文件、保留运行态，无 force-cat；两版捕获 `absent:not-found:active`，phase456 exit 0、零 stop、磁盘新版、PID 4242 仍持旧镜像。专项仓外单变量捕获拒绝对照翻为 exit 5、未切换、旧磁盘/进程 | 针对成功 phase1 后到 phase456 前的配置变化窗口；只证明安装阶段放行，phase7 旧镜像检查仍有效。v255 缺文件/active 语义另经本地上游源码核对 |
| K2-R3 标准名 vendor 与 fresh | 集成者/专项各执行真实 phase1+decision+compose：基线标准名 vendor 输入选择默认目录，候选保留 `/var/lib/ak-legacy` 的 ExecStart/RW；无 unit/occupant 的两版均 plain fresh | 两版同夹具；全新安装、标准名已有配置分开核对 |
| K2-R3 旧名 drop-in 余项 | 主 fragment 指 `/var/lib/ak-default`，合法 drop-in 与实际 NUL cmdline 指 `/var/lib/ak-legacy`，合成 key/db 仅在后者。两版真实 phase1/decision 均 exit 0，CHOSEN_EXEC/RW 选 default、DATA_SOURCE 空、OLD_REGISTRY=oldregistry | 集成者/专项各重放，执行至真实配置选择；未安装、未声称真实身份已轮换。v255 fragment/drop-in 覆盖语义有本地源码支持 |
| K2-R4 K5 精确输入 | 集成者/专项各导出完整 `a8bb3b9`、`9c2d3bd` 树：圆括号标题、destination 前空格/换行的 archive 输入从旧 exit 0 翻为候选 exit 1，均报 STATUS 起始行 9；canonical 对应输入保持 exit 0 | 完整检查器，archive 文件真实存在且含矛盾声明 |
| K2-R4 多行标题回归 | 双引号/单引号标题含一个真实换行，同一 archive 输入旧 exit 1、行 9 错误；候选 exit 0、passed=true、errors=[]，实际仍检查 canonical。括号多行标题也被候选忽略 | 集成者/专项各重放完整树；CommonMark 0.31.2 spec.txt 第 7510 行明确允许非空行的多行标题 |
| K2-R4 固定 tip 链接计数 | 两代 regex 对同一 `9c2d3bd` 导出树均 233 个本地链接、314 个全部匹配；候选原树 checker local_links_checked=233 | 执行方的 231=231 不是最终 tip 的计数；“两代匹配数相同”成立。另见双版本均遗漏 nested-label 的已有边界，本轮不单列新阻断 |
| K2-R2/K2-R5 保留证据 | 严格 stop、phase7 旧路径/deleted executable、skip-doctor 摘要、真实旧 SSH helper 与部署 git 阶段对照 | 全集及两脚本自检实际执行；baseline 缺 marker 的部署/SSH 不同处理边界保持 |

| GitHub API / 引用 | 提交/来源 | 实际结果与边界 |
|---|---|---|
| tip CI | `9c2d3bdbc8d8118f0c0af4d005aa9346db4c05a1`；[43658502260](https://github.com/wmqfl861/AK-sending/actions/runs/43658502260) | run_attempt=1，push，completed/success；13 jobs 中 12 常规 success、1 release-deep-tests skipped。当前 git 凭据读取 run/jobs API，head_branch 与完整 head SHA 匹配 |
| CI 实际门槛 | 同上 jobs/steps metadata 与固定 tip workflow | Linux/Windows fmt、clippy、workspace/特性测试、`RUSTDOCFLAGS=-D warnings` rustdoc；Flutter analyze/test、纯 Dart；Android 双 ABI/符号/实验变量泄漏检查、debug APK；Linux/macOS portability、依赖审计、仓库检查、legacy signal 与 NAT netns 模拟均 success。不是生产部署或物理双机证据 |
| 本地及远端引用 | fetch --prune、rev-parse、ls-remote、branch/worktree、branches API | main/origin/main 均 `d6c71360784e1570720e56028a39f2a80777bb21`；HEAD/开发分支与 origin 对应分支均为固定 tip。仅 main 与当前批次两个本地/远端分支、1 个 worktree；本轮未合并、未提交、未推送、未删除分支 |

| 未执行项 / 接口限制 | 原因与处理 |
|---|---|
| 本机 Rust/Flutter/Dart 全量及平台重建 | 本轮脚本/检查器/文档变更，Rust/Cargo 树无变化；使用已核实固定 tip CI 的实际门槛，不冒充本机重跑 |
| Linux 原生 systemd/权限部署、远端 SSH | Windows 本机仅运行生成脚本的隔离模型和上游源码核对，未连接生产 |
| 物理双机 C4、抓包、release ignored 深载荷 | 本轮未执行；tip CI 手动深测 skipped |
| shellcheck | 本机命令不可用；已执行 bash 语法、自检和离线行为模型 |
| 专项上游 man XML 请求 | 一次 `functions.exec → exec_command` 原拟写 TEMP Python 并只读请求 systemd v255 man XML，被自动审核以 `CreateProcess ... Rejected(... rejected: blocked by policy)` 拒绝，未给进一步原因。该动作未执行，未更换接口/身份重试；已用本机现有 v255 源码完成相同语义核对 |

| 仓外材料（本机 TEMP） | 内容 |
|---|---|
| `AK-review-K6-20261115` | 脱敏 run/jobs/branches JSON、Python 全集与门禁日志、集成者 checker/migration/recovery 重放日志；无凭据 |
| `ak-k6-recovery-review.py`；`ak-k6-recovery-review-ruedgudt` | 集成者双版本六场景；真实 generator、状态、输出和 results.json；专项原始结果在 `ak-k6-recovery-review-sexifkw8` |
| `ak-k6-absent-conflict-control.py`；`ak-k6-absent-conflict-control-2pq_9vl_` | 专项仓外捕获条件单变量原因定位对照，未修改被审代码 |
| `ak-k6-transitions-review.py`；`ak-k6-transitions-review-mjqthgcr` | 专项 activating/reloading 与恢复 stop 失败的状态与输出 |
| `ak-k6-migration-repro.py`；`ak-k6-migration-evidence-do26yw7i` | 集成者两版 standard_vendor/legacy_dropin/fresh 的真实 phase1/决策/compose、合成文件及 summary.json；专项原始结果在 `ak-k6-migration-evidence-ai_11vm7` |
| `ak-k6-checker-replay.py`；`ak-k6-checker-evidence-xch62ltx`；`ak-k6-commonmark-spec.txt` | 集成者两版完整树、逐案 STATUS/JSON 与 summary.json、官方 Markdown 规范；专项原始结果在 `ak-k6-checker-evidence-55uzc_4c`。导出树 STATUS 结束已还原，反例应按保存输入/脚本重放 |
| `ak-k6-systemd-upstream`；既有 `ak-k5-systemd-upstream`、`ak-k5-deactivating-confirm-g713dwtk/service.c` | v255 fragment/drop-in、LoadState/ActiveState、reload/deserialize/coldplug 的本地源码核对材料 |

## 18. 修复轮六（K6 复核后的新候选，执行方记录）

[K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md) 后，执行方提交其修复候选；复核方本地同步（§17、STATUS、状态表、K5 报告历史注记、旧 tip 链接计数纠正）与本节代码改动一并提交。以下保留执行方声明，后续独立证据见 §19，逐项判断只在审核状态表。

| K6 发现 | 修复内容 |
|---|---|
| K3-N1 余项（P1） | 捕获与 stop 决策交叉核对存在性与运行状态：`absent` 仅在 `inactive/failed` 时放行；缺 unit 文件却 active/过渡态/不可判定即以「无恢复前态」在切换前拒绝，stop 分支对运行中缺文件 unit 拒绝跳过。复核夹具（自然产生 v255 缺文件诊断的删除模型，进程/镜像保留）从轮六「2×nothing-to-stop+装新版退出 0」翻转为捕获期 exit 5、旧盘旧进程不动；真实 absent+inactive 全新安装回归保持；`e8f811d` 基线同夹具复现静默放行 |
| K2-R3 余项（P1） | 新 `effective_execstart` 从 `systemctl cat` 合并视图按 systemd 语义（空赋值清列表、后项追加）推导有效 ExecStart，cat 失败/零条目拒绝；`reg_args_agree` 将所选 `--port/--id-dir` 与运行 cmdline 交叉核对，多条目按 cmdline 唯一消解（0 或 >1 匹配拒绝）、实质分歧拒绝。复核夹具（vendor 主文件 ak-default + `/etc` drop-in 清空重设 ak-legacy + cmdline=ak-legacy）从轮六「选 ak-default/迁移源空」翻转为真实决策+配置链保留 `CHOSEN_RW=/var/lib/ak-legacy`；主文件唯一、追加歧义、分歧拒绝及 `e8f811d` 基线丢失均有回归 |
| K2-R4 回归（P2） | 共享正则标题支持跨行（首行+换行+非空行，仅空行禁止，三种引法一致）；新增 `HANDOFF_LINK_PREFIX_RE` 兜底：无法完整解析的交接形态条目显式拒绝而非静默忽略；尖括号目标统一解包供两个消费方使用。复核三种多行标题 archive 入口从 `9c2d3bd` 的「passed=true errors=[]」翻转为行号报错，空行标题显式拒绝，规范变体保持；执行方此处原有的当前 tip 链接计数仍属旧树口径，纠正值见 §19 证据表 |

### 修复轮六门禁与提交

本地：两脚本 `bash -n`+自检全过；Python 218/218（部署套件 66，含 8 项轮七回归与 `f9582e5`/`f150503`/`f7dece2`/`46473fc`/`e8f811d` 基线对照；检查器套件 59）；`check_repository.py` exit 0；契约 5/5；Rust 树与已验证链一致（本轮未改 Rust）。

| 提交 | 内容 |
|---|---|
| `bb1f673` | K3-N1/K2-R3 余留与 K2-R4 多行标题回归修复；复核方文档纠正与执行方本节记录入库 |

## 19. 修复轮六独立复核证据（2026-11-15）

被审差异固定为 `9c2d3bd..e286513`。发现、输入与修复要求见 [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md)，逐项状态只在审核状态表。本节、K7 报告及入口同步出具时仅保留本地，随后由执行方在 `514cb87` 提交；没有改动被审代码。下列“专项”为并行只读复核，“集成者”为主复核者；实际执行、源码旁证与未执行验收分别记录。

| 本地门禁 | 命令/来源 | 实际结果与边界 |
|---|---|---|
| Shell 语法与自检 | `bash -n scripts/deploy_linux.sh`、`bash -n scripts/cross_nat_test.sh`；两脚本分别 `--self-test` | 集成者全部 exit 0；自检只执行隔离工具和本地合成数据，没有 SSH 或部署 |
| Python 全集首次执行 | `python -m unittest discover -s scripts/tests -v` | 集成者实际运行 218 项，217 通过、1 ERROR，exit 1；`test_k2n1_round3_disabled_inactive_not_restored` 的旧 `f7dece2` Bash driver 触发 60 秒 `TimeoutExpired`，不是断言失败；原日志完整保留，不能写成单次 218/218 全绿 |
| 超时用例独立重跑 | `python -m unittest scripts.tests.test_deploy_linux_install.BaselineContrastTests.test_k2n1_round3_disabled_inactive_not_restored -v` | 1/1 通过，10.167 秒，exit 0。没有改代码、超时阈值或夹具；本轮未证明首次超时的唯一根因，不因重跑通过抹去首次错误 |
| 部署专项完整套件 | `python -m unittest scripts.tests.test_deploy_linux_install -v` | 专项 66/66，615.263 秒，exit 0，无 skip；包括 8 项本轮新增回归/对照，真实 `f9582e5`、`f150503`、`f7dece2`、`46473fc`、`e8f811d` git 阶段均执行 |
| 检查器专项套件 | `python -m unittest scripts.tests.test_repository_checks -v` | 专项 59/59，exit 0；集成者全集中同套件也通过 |
| K2-R3 指定输入 | 原 drop-in、主文件、唯一追加消解、歧义/分歧拒绝及 `e8f811d` 对照测试 | 专项 6/6，exit 0；集成者全集中也通过。K6 精确 drop-in 反例确实从选 default 翻转为保留 legacy |
| 仓库检查与契约 | `python scripts/check_repository.py`；`python scripts/check_contracts.py` | 原候选检查器及本地文档同步检查均 exit 0；契约实际结果 5/5 PASS（未仅依据其 advisory exit code） |
| Rust 本机门禁 | Rust 1.99.1；`cargo fmt --all -- --check`；`cargo clippy --locked --workspace --all-targets -- -D warnings`；`cargo test --locked --workspace`；`RUSTDOCFLAGS=-D warnings` 下 `cargo doc --locked --workspace --no-deps` | Windows 本机全部 exit 0。测试日志累计 1200 passed、0 failed、8 ignored，含文档测试；ignored 未被当作执行。Rust/Cargo 树在本轮差异中无变化 |
| 文档收尾 | `git diff --check`；完整仓库检查 | 本地同步后均 exit 0；最终工作树检查 250 个本地链接，区别于原始 tip 计数。结果与日志留在本节列出的集成者门禁目录。仅新增 K7 报告并修改交接、状态表、STATUS 与 K6 历史归属，未提交/推送 |

| 行为重放/源码旁证 | 实际输入和结果 | 证据层级与边界 |
|---|---|---|
| K3-N1 精确删除模型 | 自然 `No files found for <unit>.`、明确 `is-enabled=not-found`、active、旧 MainPID/旧镜像：候选 phase456 exit 5，switchover 未开始、zero stop、旧盘旧进程保留；`e8f811d` exit 0，打印 nothing-to-stop，磁盘新而进程旧 | 专项执行真实 generator/phase456；补齐仓内 fixture 保留 enabled marker、实际会返回 enabled 的细节，未把注释当执行结果 |
| K3-N1 状态与 stop 边界 | active/activating/reloading/deactivating/unknown/运行查询失败的捕获均 exit 5；真实生成 stop 函数对运行中/不确定状态返回 1，inactive 返回 0；真正首次安装通过 | 8 项捕获状态模型与 7 项 stop 模型。代码也允许 absent+failed；无进程的 failed 输入安装成功，不等于验证所有 failed 残留进程场景 |
| K2-N1/K2-R2 保留行为 | 恢复失败显式 INCOMPLETE，严格 stop，旧/deleted executable 拒绝，实际 MainPID 映像验证，首次安装与 git 旧版对照 | 完整部署套件及集成者全集的相应用例实际执行；没有新增经证实的状态恢复阻断 |
| K2-R3 cmdline 读取失败余项 | NUL cmdline=legacy，配置=default，旧身份/数据库存在；只注入 `CAT_FAIL_MATCH=/proc/4242/cmdline`，候选 phase1=0、decision=0，CHOSEN_RW=default、DATA_SOURCE 空；去掉注入则 phase1=3、decision=1，报 material disagreement | 专项与集成者各重放真实 phase1/preflight/phase456 decisions/compose；保留 cmdline 文件和占用者，不是简单删掉运行证据夹具；`e8f811d` 同失败输入也放行 |
| K2-R3 数据库冲突余项 | 端口与 id-dir 相同，运行 `old.db`、配置 `new.db`，旧库存在新库不存在；候选与 `e8f811d` 均 phase1=0、decision=0，CHOSEN_EXEC 指向 new.db；一致对照保留 old.db | 专项/集成者各重放；证明配置选择错误，未继续安装或声称真实资料丢失 |
| K2-R3 默认身份补充输入 | 运行 cmdline 无 id-dir，旧 `User=otheruser`、`HOME=/home/otheruser`、默认目录有合成 key/db，配置新增 default 目录；候选 phase1/decision=0，RW 指 default，复制源却为 home 默认目录 | 7 案确认套件中的补充输入；默认目录依据 `crates/ak-cli/src/main.rs:709`，假定未设置 USERPROFILE，不单独命名新阻断 |
| K2-R3 systemd 文档旁证 | v255 `man/systemctl.xml:349` 起说明 cat 输出磁盘 fragment/drop-in，可能与 manager 理解不同 | 只读上游文档，支持运行绑定要求；不冒充本机原生 systemd 运行 |
| K2-R4 K6 精确输入 | 双引号、单引号、括号多行标题的 archive 输入由 `9c2d3bd` exit 0 翻转为候选 exit 1、STATUS 起始行 9；各 canonical 变体 exit 0；空行标题和隐藏入口显式拒绝 | 专项与集成者分别导出 `a8bb3b9`、`9c2d3bd`、`e286513` 完整树，各执行 48 次完整 CLI，逐案输入/JSON 保留，结束还原 STATUS |
| K2-R4 含空格尖括号目标余项 | 真实存在的 `docs/archive space/` 同名交接带矛盾声明；STATUS 最新验证使用尖括号目标，候选 exit 0、passed=true/errors=[]，仍检查 canonical；加多行标题也放行 | 专项与集成者独立复现；旧版同样放行，为余项。完整 regex 忽略，prefix 仅捕获 `archive`；内存中单变量目标捕获对照可翻成第 9 行拒绝，仓内代码未改 |
| K2-R4 原始 tip 链接计数纠正 | 三代 regex 对同一原始 `e286513` 文档树均 242 个本地链接、329 个全部匹配；原始候选完整 checker 同为 242。233 属 `9c2d3bd`，保留于 §17 | “三代计数相同”成立；执行方 §18 的“当前 tip 233”不成立。本轮新增审核文档后的工作树计数不冒充原始 tip 计数；纠正不另列阻断 |
| K2-R5 保留证据 | skip-doctor 的摘要区分、旧 SSH helper 双流排空与部署 git 基线对照 | 两脚本自检与全集执行；部署/SSH baseline marker 缺失的不同处理边界保持 |

| GitHub API / 引用 | 提交/来源 | 实际结果与边界 |
|---|---|---|
| tip CI | `e28651322f2d2c9eac94366da965da494d78c4bd`；[43664451004](https://github.com/wmqfl861/AK-sending/actions/runs/43664451004) | 当前 git 凭据读取 GitHub run/jobs API；首次 in_progress，后续轮询 completed/success。run_attempt=1，head_branch=dev/batch-k-20261114；13 jobs 中 12 常规 success、1 release-deep-tests skipped；完成更新时间 2026-11-15T01:54:59Z |
| CI 实际门槛 | 同上 jobs/steps metadata 与固定 tip workflow | Linux/Windows fmt、clippy、workspace/特性测试、严格 rustdoc；Flutter analyze/test 与纯 Dart；Android FFI 双 ABI/符号检查、debug APK；Linux/macOS portability、cargo audit/deny、仓库检查、legacy signal 与 NAT netns 模拟均 success，未发现 failure step |
| 本地及远端引用 | `git fetch --prune origin`、rev-parse、ls-remote、branch/worktree、branches API | main/origin/main 均 `d6c71360784e1570720e56028a39f2a80777bb21`；HEAD/开发分支及 origin 对应分支均为固定 tip。仅 main 与当前批次两个本地/远端分支、1 个 worktree；本轮未合并、未提交、未推送、未删除分支 |

| 未执行项 / 限定 | 原因与处理 |
|---|---|
| Linux 原生 systemd/权限部署、远端 SSH、物理双机 C4、生产抓包 | 本机是 Windows；只执行生成脚本的隔离模型和只读上游资料核对，没有连接生产服务 |
| 本机 Flutter/Dart、Android/Linux/macOS 平台重建 | 本轮未改这些实现，相关实际构建/测试采用已匹配固定 tip 的 CI 结果；不冒充本机执行。本机 PATH 未发现 flutter/dart 命令 |
| release ignored 深载荷 | 本轮未执行；tip CI 手动深测任务 skipped，Rust 测试中的 ignored 仍未执行 |
| shellcheck | 本机 PATH 未发现命令；已执行 Bash 语法、自检和离线行为模型，不声称跑过 shellcheck |

| 仓外材料（本机 TEMP） | 内容 |
|---|---|
| `ak-k7-ci-review.py`；`ak-k7-ci-evidence` | 可重复只读 API 查询；完成后的 run/jobs/branches JSON，无凭据写入材料 |
| `ak-k7-gates-4h4a9li1`；`ak-k7-rust-gates-ln1xqdrw` | 集成者 Bash/Python/仓库/契约/Rust 全部日志和 summary；首次 Python 超时、单测重跑分别保留 |
| `ak-k7-state-replay.py`；`ak-k7-state-evidence-fo0f1qld` | 专项捕获状态矩阵，生成脚本、工具日志、最终合成状态与 results.json |
| `ak-k7-state-stop.py`；`ak-k7-state-stop-35fekhek` | 真实生成 probe/stop 函数的独立行为重放 |
| `ak-k7-state-baseline.py`；`ak-k7-state-baseline-_tf0momu` | 明确 not-found + active 的 `e8f811d` 同输入静默安装对照 |
| `ak-k7-migration-confirm.py`；`ak-k7-migration-confirm-5bci3xeu` | 集成者运行参数失败/可读、数据库冲突/一致、默认身份及旧版对照，真实阶段/选择输出、NUL cmdline 和 summary.json；专项原证据在 `ak-k7-migration-confirm` |
| `ak-k7-checker-replay.py`；`ak-k7-integrator-checker.py`；`ak-k7-integrator-checker-4in33w_u` | 集成者完整导出树、逐案 STATUS/JSON、三代 regex 同树计数与 summary；专项原证据在 `ak-k7-checker-evidence-fdf9wu3t`，含 angle-space-control.json |
| `ak-k7-commonmark-spec.txt`；`ak-k7-migration-upstream` | 官方 CommonMark 尖括号空格语义、systemd v255 cat 文档旁证；没有原生 systemd 实验 |

## 20. 修复轮七（K7 复核后的新候选，执行方记录）

[K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md) 后，执行方提交以下修复声明；复核方本地同步（§19、STATUS、状态表、K6 历史注记、链接计数口径）与本节代码改动一并提交。后续独立证据见 §21，逐项判断只在审核状态表。

| K7 发现 | 修复内容 |
|---|---|
| K2-R3 余项 1（P1，fail-open） | 已识别占用的运行 cmdline 不可读/为空即硬拒绝（exit 3「无证据，切换前拒绝」；注释说明 argv[0] 保证非空、`cat|tr` 管道无法经退出码传达读失败，以空为失败信号）；末次交叉核对去掉 `[ -n ]` 旁路改为无条件。复核单变量注入夹具（仅 cmdline 读取失败）从轮七「双阶段 exit 0 选错 ak-default」翻转为 phase1 exit 3 且不再输出 ExecStart；可读分歧拒绝与匹配通过保持；`bb1f673` 基线同夹具复现 fail-open |
| K2-R3 余项 2（P1，比对集） | `reg_args_agree` 比对面按 `ak-cli registry-server` 实际 CLI 定义取全：`--id-dir`、`--db-path`、`--port`、`--bind`；cmdline 有值而配置缺值/异值即实质分歧。复核夹具（port/id-dir 匹配、`--db-path` old.db vs new.db）从轮七「通过并选 new.db」翻转为拒绝；一致对照经真实决策+配置链保留 old.db |
| K2-R4 余项（P2） | 尖括号目标语法入共享正则目标位，兜底 `HANDOFF_LINK_PREFIX_RE` 采用相同的尖括号目标分支（拒绝信息携带未截断目标）；`_link_destination` 先解包再去 fragment（修正 `<name#anchor>` 边界）。复核夹具（label 为 K、目标为尖括号包裹且含空格的 archive space 路径）从「exit 0 完全不可见」翻转为行号+解析路径报错；带合法多行标题同翻；空行标题兜底显式拒绝。执行方原文称目标“可含空格/换行、仅禁空行”；CommonMark 对 destination 禁止所有换行，非空多行规则属于 title，规范纠正与按树计数见 §21 |

### 修复轮七门禁与提交

本地：两脚本 `bash -n`+自检全过；Python 226/226（部署套件 70，含 4 项轮八回归与 `bb1f673` 基线对照；检查器套件 63）；`check_repository.py` exit 0；契约 5/5；Rust 树与已验证链一致（本轮未改 Rust）。

| 提交 | 内容 |
|---|---|
| `514cb87` | K2-R3 fail-closed/比对集与 K2-R4 尖括号空格目标修复；复核方文档纠正与执行方本节记录入库 |

## 21. 修复轮七独立复核证据（2026-11-15）

被审差异固定为 `e286513..12a71c9`。发现、输入与修复要求见 [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md)，逐项状态只在审核状态表。本节、K8 报告及入口同步出具时仅保留本地，随后由执行方在 `7e8bfb2` 提交；当次复核未修改被审代码。下列“专项”为并行只读复核，“集成者”为主复核者；实际执行、源码旁证与未执行验收分别记录。

| 本地门禁 | 命令/来源 | 实际结果与边界 |
|---|---|---|
| Shell 语法与自检 | `bash -n scripts/deploy_linux.sh`、`bash -n scripts/cross_nat_test.sh`；两脚本分别 `--self-test` | 专项均 exit 0；本地隔离工具与合成数据，没有 SSH 或部署；cross_nat 使用既有 release 工具，不代表本轮源码构建或跨网验证 |
| Python 全集 | `python -m unittest discover -s scripts/tests -v` | 集成者 226/226，582.388 秒，exit 0；无 failure/error/skip，包含部署 70 与 checker 63 项 |
| 部署专项完整套件 | `python -m unittest scripts.tests.test_deploy_linux_install -v` | 专项 70/70，539.121 秒，exit 0，无 skip；本轮新增 4 项回归/旧 git 对照及既有恢复、stop、实际镜像验证均执行 |
| 检查器专项套件 | `python -m unittest discover -s scripts/tests -p test_repository_checks.py -v` | 专项 63/63，exit 0 |
| 仓库检查与契约 | `python scripts/check_repository.py`；`python scripts/check_contracts.py` | 集成者均 exit 0；契约实际 5/5 PASS，未仅依据 advisory exit code；最终文档同步后的检查另见收尾行 |
| Rust 本机门禁 | Rust 1.99.1；`cargo fmt --all -- --check`；`cargo clippy --locked --workspace --all-targets -- -D warnings`；`cargo test --locked --workspace`；`RUSTDOCFLAGS=-D warnings` 下 `cargo doc --locked --workspace --no-deps` | Windows 本机均 exit 0；测试日志累计 1200 passed、0 failed、8 ignored，含文档测试，测试命令耗时 545.72 秒；ignored 未当作执行。Rust/Cargo/Flutter/workflow 树在本轮差异中无变化 |
| 文档收尾 | `git diff --check`；完整仓库检查 | 本地同步后均 exit 0；当前工作树 258 个本地链接，区别于原始 tip 的计数。仅新增 K8 报告并修改交接、状态表、STATUS、K7 历史归属，不提交或推送 |

| 行为重放/源码旁证 | 实际输入和结果 | 证据层级与边界 |
|---|---|---|
| K2-R3 K7 cmdline 单变量输入 | NUL argv、占用者、cgroup 与旧数据完整，仅 `CAT_FAIL_MATCH=/proc/4242/cmdline`：候选 phase1=3、无 ExecStart、decision=1；`bb1f673` phase1/decision=0/0、选 ak-default；两版可读分歧均拒绝、匹配均保留旧数据 | 专项 6 案真实阶段重放；没有通过删掉整个占用者模型制造反例 |
| K2-R3 K7 显式数据库输入 | 运行 old.db、配置 new.db：候选拒绝；一致 old.db 经真实 decision/compose 保留。将默认库显式写为 registry.db 的冲突输入同样由 `bb1f673` 的通过翻为候选拒绝 | 精确旧输入回归与独立配置链对照；该翻转不覆盖下一行默认值输入 |
| K2-R3 默认数据库余项 | 明确 id-dir、运行无 db-path、配置新增 new.db，旧 registry.key/registry.db 存在且 new.db 不存在：候选与 `bb1f673` 均 phase1/decision=0/0，CHOSEN_EXEC 指 new.db；仅把运行默认值显式化后候选=3/1。配置等价默认、双方显式相同、双方省略的对照均保留旧库语义 | 专项与集成者各独立重放真实 phase1/preflight/phase456 decisions/compose；主 runner 每方 8 案，双方默认的补充 runner 每方 2 案。CLI `main.rs:3777`–`:3778`、`:3792` 确定数据库默认值，不依赖 HOME 假定；未安装或打开真实数据库 |
| K2-R4 合法尖括号目标 | archive-space、三种合法多行 title、空行 title backstop：基线 exit 0 翻为候选 exit 1，STATUS:9 报完整目标；canonical 与 canonical#fragment 候选均通过 | 专项与集成者各导出两份完整 git 树，各执行 16 次完整 checker CLI，逐案 STATUS/JSON 保留，结束还原 STATUS；其中包含下一行的规范边界输入 |
| K2-R4 规范边界 | CommonMark 0.31.2 的 angle destination 禁 line endings，title 才可跨非空行；候选接受非法 angle fragment 换行输入，不能保证拒绝全部含空行非法形态 | 官方 spec.txt 只读核对与离线输入；未证明合法 archive 入口逃逸，不另立阻断，纠正执行方规范归因 |
| 链接计数的树归属 | `e286513` 完整导出树：242；`12a71c9` 完整导出树：251。各树用旧/新 regex 计数均相同；执行方 §20 原称工作树 250，不是固定 tip 值 | 专项与集成者分别独立计算；原始树与本轮文档同步后的工作树分开记录，不再在状态表复述计数 |
| 保留行为 | K3-N1 的 absent+running 拒绝与首次安装、K2-N1 恢复、K2-R2 stop/实际镜像核验、K2-R5 摘要与 git 旧阶段行为 | 部署/全集回归及两脚本 self-test；实际 absent 放行状态为 inactive 或 failed，不扩展为所有 failed 残留进程情况均验证 |
| 文档与 hash backfill | `514cb87` 纳入 K7 报告、§19/§20、STATUS/状态表/K6 归属；`12a71c9` 仅交接 1 行、状态表 2 行提交号回填 | `git show`、`git diff --numstat` 实查；候选 K7“仅本地”和旧入口残留在本轮纠正，不改前轮历史结论 |

| GitHub API / 引用 | 提交/来源 | 实际结果与边界 |
|---|---|---|
| tip CI | `12a71c9d6c2ff11be63aa48ed009699c691e1ebe`；[43670857567](https://github.com/wmqfl861/AK-sending/actions/runs/43670857567) | 当前 git 凭据读取 run/jobs API；首次 in_progress，轮询至 completed/success。run_attempt=1，head_branch=dev/batch-k-20261114；13 jobs 中 12 常规 success、1 release-deep-tests skipped；完成更新时间 2026-11-15T03:21:28Z |
| CI 实际门槛 | 同上 jobs/steps metadata 与固定 tip workflow | Linux/Windows fmt、clippy、workspace/特性测试、严格 rustdoc；Flutter analyze/test 与纯 Dart；Android FFI 双 ABI/符号检查、debug APK；Linux/macOS portability、cargo audit/deny、仓库检查、legacy signal 与 NAT netns 模拟均 success；不计作物理双机或生产验证 |
| 本地及远端引用 | `git fetch --prune origin`、rev-parse、ls-remote、branch/worktree | main/origin/main 均 `d6c71360784e1570720e56028a39f2a80777bb21`；HEAD/开发分支及 origin 对应分支均为固定 tip。仅 main 与当前批次两个本地/远端分支、1 个 worktree；未合并、未提交、未推送、未删除分支 |

| 未执行项 / 限定 | 原因与处理 |
|---|---|
| Linux 原生 systemd/权限部署、远端 SSH、物理双机 C4、生产抓包 | 本机是 Windows；只执行生成脚本的离线模型与只读资料核对，没有连接生产服务 |
| 本机 Flutter/Dart、Android/Linux/macOS 平台重建 | 本轮未改这些实现；相关平台门槛采用固定 tip CI 的最终结果，不冒充本机执行；本机 PATH 未发现 flutter/dart |
| release ignored 深载荷 | 本轮未执行；tip CI 手动深测任务 skipped，Rust ignored 不当作执行 |
| shellcheck | 本机 PATH 未发现命令；Bash 语法、自检、离线模型不冒充 shellcheck |

| 仓外材料（本机 TEMP） | 内容 |
|---|---|
| `ak-k8-ci-review.py`；`ak-k8-ci-evidence` | 可重复只读 API 查询及完成后的 run/jobs JSON、逐 job/step 核验与最终 refs JSON，不写入凭据 |
| `ak-k8-review-gates-aow1baff` | 集成者 Python/仓库/契约/Rust 全部日志与 summary.json |
| `ak-k8-deploy-review.py`；`ak-k8-deploy-review-0trru8bq` | 集成者默认/显式数据库输入、旧 git 对照与完整生成脚本/输出；专项结果在 `ak-k8-deploy-review-_qnj7opx`，亦含 Shell 自检日志 |
| `ak-k8-deploy-default-controls.py`；`ak-k8-deploy-default-controls-3yih8119` | 集成者双方省略数据库旗标的对照；专项证据在 `ak-k8-deploy-default-controls-w4o6_zw7` |
| `ak-k8-deploy-cmdline.py`；`ak-k8-deploy-cmdline-181kq02d` | 专项 NUL cmdline 的失败/可读分歧/匹配输入，两版完整阶段输出 |
| `ak-k8-report-repro.py` / `ak-k8-report-repro.log`；`ak-k8-doc-repro-ktakin0n` | 集成者与文档专项分别从 K8 报告原样提取最小重放并实际执行；默认冲突通过选 new.db，显式等价运行值冲突拒绝，与正文一致 |
| `ak-k8-checker-review.py`；`ak-k8-checker-qwlmsf8e` | 集成者完整树、逐案输入/JSON、计数与汇总；专项证据在 `ak-k8-checker-1v4at40t` |
| `ak-k8-commonmark-spec.txt` | 官方规范的 destination 与 title 定义，只读旁证 |

## 22. 修复轮八（K8 复核后的新候选，执行方记录）

[K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md)（仅剩 K2-R3 默认值不等价一项阻断；另两条非阻断纠正）后，执行方完成修复；复核方本地同步（§21、STATUS、状态表）与本节代码改动一并提交。

| K8 发现 | 修复内容 |
|---|---|
| K2-R3 余项（P1，默认值不等价） | `reg_args_agree` 改为**有效值比较含默认**：`--db-path` 缺省侧代入 `<有效 id-dir>/registry.db`（引 `registry_db_path()` main.rs:3777/3792）；`--id-dir` 默认依赖环境（HOME），单侧缺省无法自 argv 推导 → rc 2 拒绝，双侧缺省共享默认放行；`--port`（7200）/`--bind`（0.0.0.0）常量默认代入。返回契约 0/1/2，占位块区分「无法建立」与「实质分歧」均 exit 3。复核夹具（运行侧缺省 db=registry.db、配置显式 new.db）从轮八「双阶段 exit 0 选 new.db」翻转为 phase1/决策双拒绝；显式缺省、双侧缺省、等价拼写真、`bb1f673` 基线选错五组回归齐备 |
| 非阻断纠正 1（尖括号目标换行） | 共享正则角括号分支改为**完全禁止换行**（CommonMark 0.31.2 目标定义）；含换行的角括号交接形态由容忍式兜底整体捕获并显式拒绝（携带未截断目标）；空行角括号从「静默放行」补为显式拒绝；范围注释改为精确表述（目标：空格可、换行不可；标题：换行可、空行不可） |
| 非阻断纠正 2（链接计数） | 执行方当时记录的口径链：233=`9c2d3bd`、242=`e286513`、251=`12a71c9`、K8 本地同步时工作树=258，两代正则同树计数一致。该工作树时点不代表本轮固定候选，K9 的固定树复测见 §23 |

### 修复轮八门禁与提交

本地：两脚本 `bash -n`+自检全过；Python 232/232（部署套件 75，含 5 项轮九回归与 `bb1f673` 基线对照；检查器套件 64）；`check_repository.py` exit 0；契约 5/5；Rust 树与已验证链一致（本轮未改 Rust）。

| 提交 | 内容 |
|---|---|
| `7e8bfb2` | K2-R3 有效值默认比较与 K8 两条非阻断纠正；复核方文档同步与执行方本节记录入库 |

## 23. 修复轮八独立复核证据（2026-11-15）

被审差异固定为 `12a71c9..ccd8fb2`。发现、可复现输入与修复要求见 [K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md)，逐项状态只在审核状态表。本节、K9 报告及入口同步出具时仅保留本地，随后由执行方在 `e03a44f` 提交；复核未修改被审代码。“专项”为并行只读复核，“集成者”为主复核者；静态核对、离线模型与未执行的真实环境验收分别记录。

| 本机验证 | 实际命令 / 范围 | 结果与边界 |
|---|---|---|
| Shell 语法与自检 | `bash -n scripts/deploy_linux.sh`、`bash -n scripts/cross_nat_test.sh`；两脚本分别 `--self-test` | 集成者全部 exit 0；隔离工具与本地合成数据，没有 SSH 或部署；cross_nat 使用既有 release 工具，不代表本轮源码重建或真实跨网 |
| Python 全集 | `python -m unittest discover -s scripts/tests -v` | 集成者 232/232，unittest 计时 699.340 秒，exit 0；无 failure/error/skip，含部署 75 项（含本轮新增回归及真实 git 基线）与 checker 64 项 |
| 检查器专项 | `python -m unittest discover -s scripts/tests -p test_repository_checks.py` | 专项 64/64，0.363 秒，exit 0，无 skip；不与全集重复计为新增案例 |
| 仓库与契约 | `python scripts/check_repository.py`；`python scripts/check_contracts.py` | 固定候选均 exit 0，契约 5/5；完整 checker 记录本地链接 260。契约工具自身是 advisory exit 0，已另核所有项目实际 PASS |
| Rust 本机门禁 | `cargo fmt --all -- --check`；`cargo clippy --locked --workspace --all-targets -- -D warnings`；`cargo test --locked --workspace`；`RUSTDOCFLAGS=-D warnings` 下 `cargo doc --locked --workspace --no-deps` | Windows 本机全部 exit 0；测试日志累计 1200 passed、0 failed、8 ignored，含文档测试，测试命令耗时 575.922 秒；ignored 未被算作执行 |
| 实现变动范围 | `git diff --name-only 12a71c9..ccd8fb2 -- crates Cargo.toml Cargo.lock rust-toolchain.toml flutter_app ak_ffi_dart .github` | 无差异；平台验证使用本行固定候选的 CI，不用旧构建冒充新构建 |
| 报告原文最小复现 | 从 K9 报告 PowerShell 代码块原样提取 Python 并执行 | exit 0；两个输入均 phase1/decision=0/0，增添无关 root 数据使真实迁移源由 otheruser home 改为 root，与正文一致 |
| 文档收尾 | `git diff --check`；完整仓库检查 | 本地同步后均 exit 0；当前工作树 267 个本地链接，区别于原始 tip。仅新增 K9 报告并修改交接、状态表、STATUS、K8 历史归属，不提交或推送 |

| 行为 / 源码核对 | 实际输入和结果 | 证据层级 |
|---|---|---|
| K8 精确数据库输入 | 显式 id-dir 相同，运行省略 db，配置 new.db：候选 phase1/decision=3/1，无 CHOSEN 输出、报 materially disagrees；`12a71c9` 与 `bb1f673` 均 0/0、选择 new.db | 集成者独立重放真实 phase1/preflight/phase456 decisions/compose；完整 runner 共 7 案，含下面的对照，不安装或打开数据库 |
| 默认数据库对照 | 仅将运行默认库显式化的冲突仍拒绝；双方隐式、运行隐式/配置显式等价默认、相反方向等价默认均 0/0 并保留原库语义 | 专项每版 11 案、两版共 22 案，另涵盖身份单侧默认、显式 db、bind；真实 NUL 参数逐项写入，不只是 substring 断言 |
| 返回契约和常量默认 | 实际生成函数的 10 组直接调用：全隐式与显式常量默认双向 rc 0；有效 port/bind 不同 rc 1；单侧身份隐式双向及身份未知时单侧 db 显式 rc 2；最终占用者 caller 对 1/2 都 exit 3、消息不同 | 专项提取真实生成函数执行；CLI `main.rs:709`、`:638`、`:3777`、`:3792` 与 server_v2/config.rs:166 作为默认值源码依据 |
| K9-N1 新迁移来源反例 | 旧 unit 的 User/HOME 及运行 environ 明确为 otheruser、无 USERPROFILE，双方 argv 均无 id-dir/db。仅增加无关 root 合成 key/db，候选 0/0、CHOSEN_DATA_SOURCE 从 `/home/otheruser/.ak-sending` 变为 `/root/.ak-sending`，两案新 ExecStart 都补 REMOTE_DATA | 集成者候选/`12a71c9` 共 4 案；专项候选/`12a71c9`/`bb1f673` 共 6 案，各自独立执行真实配置选择。运行默认值相同，不以环境差异猜测制造反例；只证明选错来源，未执行安装 |
| 检查器完整 CLI | 从 git 导出 `12a71c9`、`ccd8fb2`，每树 16 案共 32 次完整 checker；候选 angle LF/CR/CRLF/空行/仅空白空行及 fragment 换行均 exit 1，定位 STATUS:9 并携带完整目标；合法 canonical/fragment/三种多行 title 均 exit 0，archive-space 仍拒绝 | 专项独立构造输入与完整树执行。旧版 LF-fragment/blank-fragment/blank-path 的静默通过确已翻转。CR/CRLF 经 Path.read_text 通用换行归一化，无孤立 CR 逃逸 |
| 固定树链接计数 | 四代 regex 对同树均相等：`9c2d3bd` local/all=233/314；`e286513`=242/329；`12a71c9`=251/341；`ccd8fb2`=260/352。原始完整 checker 也确认前一候选 local=251、当前候选 local=260 | 专项四树×四代计数。§22 的工作树=258 仅属于 §21 的 K8 本地同步时点，不是本轮固定 tip；本轮文档收尾后的工作树另记 |
| 非阻断说明 | deploy_linux.sh:987–988 残留“不比较缺省值”；checker docstring:368/371 残留“所有 handoff 形态均不忽略/同语法”泛称；完整 bind 端点的等价拼写可能保守误拒绝 | 静态核对＋实际函数调用；未据此另列代码阻断。CommonMark 0.31.2 官方 spec.txt 已只读核对，目标禁 line endings、title 禁 blank line |
| 既有行为 | K3-N1 首次安装与 absent+active 拒绝；K2-N1 恢复；K2-R2 stop/实际映像；K2-R5 诚实摘要与 git 旧阶段对照 | 全集和 Shell 自检覆盖；不扩大成所有真实 systemd/权限场景均验证 |
| 提交和同步归属 | `7e8bfb2` 纳入 K8 报告、§21/§22、STATUS/状态表/K7 归属；`ccd8fb2` 仅交接 1 行、状态表 2 行哈希回填 | `git show` 与 numstat 实查；候选残留“K8 仅本地”及旧入口已在本轮本地同步纠正，不改历史判决 |

| GitHub / Git | 固定对象 | API / 引用结果 |
|---|---|---|
| tip CI | `ccd8fb246dc96aaa3fd21233946327fa04171170`；[43676620488](https://github.com/wmqfl861/AK-sending/actions/runs/43676620488) | 使用当前 git 凭据读取 run/jobs API，首次 in_progress，轮询至 completed/success。run_attempt=1，head_branch=dev/batch-k-20261114；13 jobs 中 12 常规 success、1 release-deep-tests skipped；完成更新时间 2026-11-15T04:37:17Z |
| CI 实际门槛 | 同上 jobs/steps metadata 与固定 tip workflow | Linux/Windows fmt、clippy、workspace/特性测试、严格 rustdoc；Flutter analyze/test 与纯 Dart；Android FFI 双 ABI/符号检查、debug APK；Linux/macOS portability、cargo audit/deny、仓库检查、legacy signal、NAT netns 模拟均 success；未见 failure/cancelled step，不计作物理双机或生产部署 |
| 远端引用 | `git fetch --prune origin`；`git ls-remote --heads origin` | 只有 main=`d6c71360784e1570720e56028a39f2a80777bb21`、dev/batch-k-20261114=`ccd8fb246dc96aaa3fd21233946327fa04171170`；收尾再次读取一致。REJECTED 分支不删除 |
| 本地引用与工作树 | `git branch -avv`；`git worktree list --porcelain`；`git status --short --branch` | 当前批次分支在固定 tip，main 留在 d6c7136，只有主工作树；仅本地 K9 报告、交接、状态表、STATUS、K8 归属文档修改，没有提交、合并、推送或代码改动 |

| 未执行 / 限制 | 本轮事实 |
|---|---|
| Linux 原生 systemd/权限部署、远端 SSH、物理双机 C4、生产抓包 | Windows 本机仅运行真实生成脚本的离线模型，没有连接生产服务；合成环境/文件不等于原生 Linux 验证 |
| 本机 Flutter/Dart、Android/Linux/macOS 平台重建 | 本轮未改这些实现；实际平台门槛采用完整 SHA 匹配的最终 CI；本机 PATH 未发现 flutter/dart |
| 手动 release deep tests | push workflow 的该 job skipped，本机本轮未重跑 ignored release suite；不将未执行项记作通过 |
| shellcheck | 本机 PATH 未发现命令；Bash 语法、自检和离线模型不冒充 shellcheck |

| 仓外材料（本机 TEMP，无凭据） | 内容 |
|---|---|
| `ak-k9-review-gqhi1ztk/ci_query.py`、`run-43676620488.json`、`jobs-43676620488.json` | 当前 git 凭据只读 API 查询与最终 run/jobs；凭据只留进程内，不写材料 |
| `ak-k9-review-gqhi1ztk/run_gates.py` 及 `*-results.json`、`*.log` | 集成者 Python、仓库、契约、Bash 与 Rust 门禁日志 |
| `ak-k9-review-gqhi1ztk/replay_defaults.py`、`replay_identity.py` 及各 summary/阶段文件 | 集成者默认数据库原输入与等价对照、双隐式身份迁移来源反例和 git 基线 |
| `ak-k9-review-gqhi1ztk/report_repro.py`、`report-repro.log` | 从 K9 报告原文提取、实际执行的最小复现 |
| `ak-review-k9-defaults` | 专项三个 runner、默认参数 matrix.json、implicit-id-root-matrix.json、直接函数结果与完整阶段输出 |
| `ak-k9-checker-independent.py`、`ak-k9-checker-independent-vmnb7zod`、`ak-k9-commonmark-spec.txt` | 专项完整导出树、逐案输入/JSON、跨树/正则计数、官方规范只读旁证 |

## 24. 修复轮九（K9 复核后的新候选，执行方记录）

[K9 复核](REVIEW_2026-11-15-batchK9.zh-CN.md)（K8 默认值反例判通过；新发现 K9-N1）后，执行方完成修复；复核方本地同步（§23、STATUS、状态表、K8 历史归属、链接计数 ccd8fb2=260）与本节代码改动一并提交。

| K9 发现 | 修复内容 |
|---|---|
| K9-N1（P1） | 迁移源绑定运行实例身份：phase1 在占位者配置交叉核对后推导并输出 `runtime-id-dir`——有效 ExecStart 显式 `--id-dir` 优先；否则按 CLI 默认链（USERPROFILE→HOME，引 main.rs `id_dir()`）取自运行 `/proc/<PID>/environ` 并与 unit 合并 `Environment=`/`UnsetEnvironment=` 交叉核对（矛盾/不可读/双缺失即 exit 3，`.` 回退会依赖不可知工作目录故拒绝）；决策块迁移源选择改为 runtime-id-dir（有占位者）→ 唯一枚举候选（无占位者）→ 多候选无占位者拒绝，`head -1` 与枚举顺序彻底移出选择路径。复核翻转夹具（无关 `/root/.ak-sending` 存在与否）两次均选 `/home/otheruser/.ak-sending`；显式 id-dir 抗诱饵、无占位者单候选可选/多候选拒绝、环境证据缺失拒绝、`12a71c9` 基线同夹具选 `/root` 共七组回归；K8 已通过的等价/冲突回归全保 |

### 修复轮九门禁与提交

执行方提交时声明：`bash -n`+自检全过；Python 239/239（部署套件 82，含 7 项 K9-N1 新回归与跨六版基线对照）；`check_repository.py` exit 0；契约 5/5；Rust 树与已验证链一致（本轮未改 Rust）。独立复核对上述实现、fixture 和门禁声明的核验见 §25，不以本节声明替代。

| 提交 | 内容 |
|---|---|
| `e03a44f` | K9-N1 运行身份绑定迁移源；复核方文档同步与执行方本节记录入库 |

## 25. 修复轮九独立复核证据（2026-11-15）

被审差异固定为 `ccd8fb2..806ea6c`。发现与可复现输入见 [K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md)，逐项状态只在审核状态表。本节、K10 报告及入口同步出具时仅保留本地，随后由执行方在 `a50bb1a` 提交；复核未修改被审代码。“专项”为并行只读复核，“集成者”为主复核者；本机编译、离线模型和平台 CI 分别记录。

| 本机验证 | 实际命令 / 范围 | 结果与边界 |
|---|---|---|
| Shell 语法与自检 | `bash -n scripts/deploy_linux.sh`；`bash scripts/deploy_linux.sh --self-test` | 集成者均 exit 0；隔离工具与合成数据，没有 SSH 或部署 |
| Python 全集 | `python -m unittest discover -s scripts/tests -v` | 集成者 239/239，786.636 秒，exit 0，无 failure/error/skip；含部署 82 项及真实 git 基线 |
| 新增回归专项 | 新增当前树测试及 `12a71c9` 对照逐名执行 | 专项 7/7，72.035 秒，exit 0；不与全集重复计为新增案例 |
| 仓库与契约 | `python scripts/check_repository.py`；`python scripts/check_contracts.py` | 固定 tip 完整导出树及集成者工作树检查均 exit 0；契约 5 项实际 PASS，未只根据 advisory exit 0 判定 |
| Rust 本机门禁 | `cargo fmt --all -- --check`；`cargo test --locked --workspace`；`cargo clippy --locked --workspace --all-targets -- -D warnings`；`RUSTDOCFLAGS=-D warnings` 下 `cargo doc --locked --workspace --no-deps`；`cargo build --locked --workspace` | Windows / rustc 1.99.1 本机全部 exit 0；测试日志累计 1200 passed、0 failed、8 ignored，含文档测试；ignored 未计作执行；workspace 构建为本轮实际构建 |
| 实现变动范围 | `git diff --name-only ccd8fb2..806ea6c -- crates Cargo.toml Cargo.lock rust-toolchain.toml .github/workflows` | 无差异；本轮修复只改部署脚本、其 Python 测试与文档；平台证据采用当前完整 SHA 的 CI |
| 报告原文复现 | 从 K10 报告 PowerShell 代码块原样提取 Python 并执行 | 集成者 exit 0；4 案均 phase1/decision=0/0，空 USERPROFILE 错选 HOME，值内 LF 错选 root；目标对照安装均 0，但有 decoy 时保留无关 key/db，新 unit 指向目标 |
| 文档收尾 | `git diff --check`；完整仓库检查 | 本地同步后均 exit 0；工作树 local links=274，区别于固定 tip。仅新增 K10 报告并同步交接、状态表、STATUS、K9 历史归属；不提交或推送 |

| 行为 / 源码核对 | 实际输入和结果 | 证据层级 |
|---|---|---|
| K9 精确来源翻转 | 逐参数 NUL argv，User/HOME 明确、UnsetEnvironment=USERPROFILE；current 无/有 root decoy 均选择 home；`12a71c9` 同 decoy 仍选 root | 集成者及专项各自重放真实 phase1/preflight/phase456 decisions/compose；普通 HOME 修复与新增缺陷分开判断 |
| K10-N1 空值 | USERPROFILE 存在但为空、HOME 已知、WorkingDirectory=/var/lib/oldregistry：脚本 phase1/decision=0/0 并选 HOME；CLI 实际默认依赖 cwd | 专项与集成者各自重放；Rust 默认链逐字摘录以 rustc 编译，absent/empty/nonempty 3 案均 exit 0，empty 输出 Some("") 与相对 .ak-sending |
| K10-N1 NUL 边界 | environ 的 EXTRA 值内含 LF+USERPROFILE=/root，实际无 USERPROFILE 记录：脚本 0/0 并选 root | 专项真实链与集成者重跑，报告原文再验证；NUL 原始记录与 unit 输入保留在仓外材料 |
| K10-N2 实际目标数据 | 正确 home 来源不变，仅增减 REMOTE_DATA decoy：真实配置选择均正确，实际生成安装脚本均成功；无 decoy 复制 private-key/db，有 decoy 保留 unrelated-target-key/db；新 unit 均指向 REMOTE_DATA | 专项与集成者独立执行 actual phase456；专项追加 enabled+active/PID/合成 unit 的一致状态模型后结果不变，报告原文也用该状态。未运行真实 registry/SQLite |
| 目标冲突历史归属 | `ccd8fb2` 目标 decoy 的 phase1/decision 也 0/0，旧枚举本来就选目标，CHOSEN_DATA_SOURCE 为空 | 专项真实旧 git 脚本对照；属于既有缺口，不归为本轮引入 |
| fixture 精度 | 新 helper 原始 argv 为 `[ak-cli, registry-server, "--port 17200"]`，独立复核覆盖为逐参数 NUL 后重放 | 静态定位测试 :940–942/:974/:980 加原始 bytes 实查；纠正“argv-precise”声明，不据此撤回精确重放的通过结果 |
| unit 环境交叉检查边界 | 多赋值 Environment 行中的 HOME 与 runtime 不同，仍 0/0；行首匹配遗漏后续赋值 | 专项已执行；runtime 来源本身仍正确，不单列 P1；不能宣称完整 merged 指令语义均覆盖 |
| 固定树链接与契约 | 完整导出树：ccd8fb2 64 md/local 260，806ea6c 65 md/local 268，checker 均 exit 0、契约各 5 PASS；五版 regex 对同树计数一致：233/314、242/329、251/341、260/352、268/361（local/all） | 专项 5 树×5 regex；树依次 9c2d3bd/e286513/12a71c9/ccd8fb2/806ea6c。§23 的 267 是当时本地同步工作树，不是本轮固定 tip |
| 提交与注释 | e03a44f 纳入 K9 同步；806ea6c 仅交接/状态表各 1 行哈希回填；reg_args_agree 头注释已准确更新 | git diff/show 与源码机械核对；K9“仅本地”残留、本轮入口和下一步在本地同步纠正 |

| GitHub / Git | 固定对象 | API / 引用结果 |
|---|---|---|
| tip CI | `806ea6cc1a23db7b0dd56b38e8857483063c69a5`；[43681408648](https://github.com/wmqfl861/AK-sending/actions/runs/43681408648) | 当前 git 凭据直接读取 run/jobs API，从 in_progress 等待至 completed/success；head_branch=dev/batch-k-20261114，run_attempt=1；13 jobs 中 12 常规 success、1 手动 release-deep-tests skipped；完成更新时间 2026-11-15T05:45:49Z |
| CI 实际门槛 | 同上 jobs/steps metadata 与固定 tip workflow | Linux/Windows fmt、clippy、workspace/特性测试、严格 rustdoc；Flutter analyze/test 与纯 Dart；Android FFI 双 ABI/符号检查及 debug APK；Linux/macOS portability、cargo audit/deny、仓库检查、legacy signal、NAT netns 模拟均 success；逐项核对无 failure/cancelled/未完成步骤。不等于物理双机或生产部署 |
| 远端引用 | `git fetch --prune origin`；收尾 `git ls-remote --heads origin` | 前后均只有 main=d6c71360784e1570720e56028a39f2a80777bb21、dev/batch-k-20261114=806ea6cc1a23db7b0dd56b38e8857483063c69a5；REJECTED 分支保留，未调用合并、推送或删除 |
| 本地引用与工作树 | `git branch -avv`；`git worktree list --porcelain`；`git status --short --branch` | 批次分支在固定 tip，main 留在 d6c7136，只有主工作树；K10 报告、交接、状态表、STATUS 与 K9 历史归属本地同步，没有提交、合并、推送或代码改动 |

| 未执行 / 限制 | 本轮事实 |
|---|---|
| Linux 原生 systemd/权限部署、远端 SSH、物理双机 C4、生产抓包 | Windows 本机只执行真实生成脚本的离线模型，没有连接生产；合成 key/db 及进程状态不等于原生 Linux 验证 |
| 本机 Flutter/Dart、Android/Linux/macOS 平台重建 | 本轮未改这些实现；实际平台门槛采用完整 SHA 匹配的最终 CI，不拿旧产物冒充当前构建 |
| 手动 release deep tests | push workflow 的该 job skipped，本机未重跑 ignored release suite；不计作执行或通过 |
| shellcheck | 本机 PATH 未发现命令，未执行；同次 PATH 核对也未发现 flutter/dart。Bash 语法、自检和离线模型不等于 shellcheck |

| 仓外材料（本机 TEMP，无凭据） | 内容 |
|---|---|
| `ak-review-k10-806ea6c` | 集成者 run/jobs JSON、脚本/Rust 门禁日志与 JSON、root 来源/目标对照、报告原文 `report-repro.py`/log；凭据只在进程内，不写材料 |
| `ak-review-k10-runtime-806ea6c` | 专项运行环境原始 fixture、真实阶段输出及 Rust 默认链摘录编译 runner/log |
| `ak-k10-fixture-audit.py`/log、`ak-k10-target-active-audit.py`/log | 专项精确 root-decoy/git 基线、实际安装有/无目标 decoy、active 状态补齐与前一候选对照 |
| `ak-k10-doc-audit-uzqwmg_0` | 完整导出树、checker 输出 JSON 与固定树计数材料 |

## 26. 修复轮十（K10 复核后的新候选，执行方记录）

[K10 复核](REVIEW_2026-11-15-batchK10.zh-CN.md)（K9 翻转反例判修复、K8 维持通过）后，执行方修复其两项新发现；复核方本地同步（§25、STATUS、状态表、K9 历史归属）与本节代码改动一并提交。

| K10 发现 | 修复内容 |
|---|---|
| K10-N1（P1） | 环境解析改 NUL 记录级（`grep -za '^VAR='`，`^` 按记录锚定）：值内嵌换行不再伪造变量（复核 EXTRA 夹具中 `/root` 不再出现）；空值语义按 `id_dir()` 源码精确镜像——`Some("")` 不回退 HOME，空/相对基目录产出相对 `.ak-sending` 并由 unit `WorkingDirectory=`（去 `-` 前缀）解析，无绝对 WorkingDirectory 即拒绝。复核夹具（USERPROFILE 置空+HOME+WorkingDirectory）从误选 HOME 翻转为 `/var/lib/oldregistry/.ak-sending`；同输入缺 WorkingDirectory 拒绝；内嵌换行夹具不再选出 `/root` |
| K10-N2（P1） | 目标身份/库冲突在切换前处理：有迁移源时目标已存文件须与源 `cmp -s` 字节一致——一致保留（记录）、不一致 exit 5 拒绝（「绝不静默启用无关身份字节」）、缺失则拷贝并 cmp 校验（拷贝损坏拒绝）；无源预期时真全新安装遇目标已有 key/db 拒绝，`REMOTE_DATA_PREOWNED`（占位者运行 id-dir 即目标）原位升级放行自有数据。复核夹具（K9 源+目标无关字节）从 `INSTALL_RC=0` 无关字节上线翻转为 exit 5 全不动；一致保留、拷贝校验、损坏拒绝、全新脏目标拒绝、原位升级、`806ea6c` 基线放行共十组回归 |

### 修复轮十门禁与提交

本地：`bash -n`+自检全过；Python 249/249（部署套件 92，含 10 项轮十一回归与跨八版基线对照）；`check_repository.py` exit 0；契约 5/5；Rust 树与已验证链一致（本轮未改 Rust）。

| 提交 | 内容 |
|---|---|
| `a50bb1a` | K10-N1/N2 修复；复核方文档同步与执行方本节记录入库 |

## 27. 修复轮十独立复核证据（2026-11-15）

被审差异固定为 `806ea6c..09fceb6`。发现与完整可复现输入见 [K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md)，逐项状态只在审核状态表。本节、K11 报告及入口同步仅保留本地；复核未修改被审代码。“专项”为并行只读复核，“集成者”为主复核者；本机编译、隔离模型、SQLite 事务和平台 CI 分别记录。

| 本机验证 | 实际命令 / 范围 | 结果与边界 |
|---|---|---|
| Shell 语法与自检 | `bash -n scripts/deploy_linux.sh`、`bash -n scripts/cross_nat_test.sh`；两脚本各 `--self-test` | 均 exit 0；隔离工具与合成数据，没有 SSH 或部署。cross-NAT 自检使用已有本地 release 工具，不代表本轮 release 重建或公网传输 |
| Python 全集 | `python -m unittest discover -s scripts/tests -v` | 249/249，864.496 秒，exit 0，无 failure/error/skip；部署套件 92，含本轮 K10 回归/对照 10 项，真实 git BaselineContrastTests 20 项均实际执行；repository checker 64 项 |
| 仓库与契约 | `python scripts/check_repository.py`；`python scripts/check_contracts.py`；集成者在 `git archive 09fceb6` 完整导出树执行 checker | 均 exit 0；契约 5/5 实际 PASS，未只凭 advisory 退出码。固定导出树 66 md/local links=276；同步前工作树 81 md/local links=276（含本机忽略目录文档），两者区分记录 |
| Rust 本机门禁 | `cargo fmt --all -- --check`；`cargo test --locked --workspace`；`cargo clippy --locked --workspace --all-targets -- -D warnings`；`RUSTDOCFLAGS=-D warnings` 下 `cargo doc --locked --workspace --no-deps`；`cargo build --locked --workspace` | Windows / rustc 1.99.1 均 exit 0；测试 1200 passed、0 failed、8 ignored，含 doctest，549.297 秒。ignored 未计作执行；本轮确实完成 Windows workspace 构建，Rust 门禁日志未见 warning/error 编译诊断 |
| 实现变动范围 | `git diff --name-only 806ea6c..09fceb6 -- crates Cargo.toml Cargo.lock rust-toolchain.toml .github/workflows` | 无差异；本轮实现变动只在部署脚本、其 Python 测试与文档 |
| 报告原文复现 | 从 K11 报告 PowerShell 代码块原样提取 Python 并运行 | 集成者 exit 0；5 案完整执行：LF 变量和 cwd 差异均错选并成功安装；schema clean 保持 active/1；当前与旧候选 late-commit 均来源 revoked/2、目标 active/1。runner 成功不代表这些部署行为通过 |
| 文档收尾 | `git diff --check`；同步后 `python scripts/check_repository.py` | 本地同步后均 exit 0；最终工作树 82 md/local links=281，区别于固定 tip。本轮仅新增 K11 报告，同步交接、状态表、STATUS 和 K10 历史归属，不提交或推送 |

| 行为 / 源码核对 | 实际输入和结果 | 证据层级 |
|---|---|---|
| K10 报告原输入对照 | 原报告代码分别以 806ea6c / 09fceb6 重放，输入不变：空 USERPROFILE 从 HOME 翻转至 WorkingDirectory；EXTRA 内 LF 从错误 root 翻转至 HOME；目标 decoy 从安装 0 翻转为 5，字节保留且新 unit 不存在；空目标正确复制 | 集成者执行真实 phase1、decision 和由决策值生成的 phase456；只为预期拒绝补充“新 unit 不存在”的输出处理 |
| K10-N1 原新增回归 | 三项提交内环境回归逐名执行 | 专项 3/3，32.374 秒，exit 0；包含缺 WorkingDirectory 拒绝。不与全集重复计为新增案例 |
| 实际变量含 LF | USERPROFILE=/home/otheruser 加值内 LF+suffix 或值尾 LF；EnvironmentFile 注入 | 专项和集成者各自执行真实完整链，phase1/decision/install 均 0，错误 prefix 来源被复制。Rust 默认链摘录以 rustc --edition 2024 编译，LF 保留、空值得到相对 .ak-sending。Windows 无 LF 文件名落盘，正确 Linux 路径依 CLI 字节语义判定 |
| cwd 配置/运行不一致 | unit WorkingDirectory=/home/configured-new，运行 cwd 模型=/home/runtime-old，USERPROFILE 为空；两目录放不同合成数据 | 专项和集成者各自完整链均 0，目标为 configured-decoy；readlink stub 单独控制 exit 0 返回 runtime-old，实际部署未查询 cwd。未运行原生 systemd |
| EnvironmentFile 语义旁证 | 官方 systemd.exec 文档；两个合成单引号多行配置；Bash source 后逐字输出 NUL 记录 | 官方允许单引号值跨多行；模型原始 environ 与加载控制精确相同。Bash 仅作相同单引号语义旁证，不冒充 systemd 启动 |
| K10-N2 专项 | differing / identical / clean-copy / corrupt-copy / fresh-dirty / in-place 与真实 806ea6c baseline | 当前六项和正确类名下 baseline 均通过。首次专项命令把 baseline 方法错列在 DeployInstallTests，产生 loader error；改用 BaselineContrastTests 后通过，属于复核命令定位错误，不是候选产品失败 |
| 复制后、停服前提交 | 真实 SQLite 普通表先作交错控制；进一步使用 store.rs 的五条真实 DDL、schema v3 和合成邀请记录，在 systemctl stop 入口且旧服务仍 active 时提交撤销字段更新 | 专项与集成者各自执行完整链。当前与 806ea6c 的 clean 均 source/target=active/1；late 均 source=revoked/2、target=active/1，安装均 0。当前输出 cmp-verified 后仍丢失最终状态；未启动真实 registry 或协议 revoke |
| 提交与历史归属 | `git show/diff a50bb1a`、`git show 09fceb6`；fixture argv 检查 | a50bb1a 纳入 K10 文档同步；09fceb6 为交接/状态表各一行哈希回填。fixture 已逐参数分隔；本轮也显式写 NUL argv。残留“K10 仅本地”与旧下一步本地纠正 |

| GitHub / Git | 固定对象 | API / 引用结果 |
|---|---|---|
| tip CI | `09fceb64a2fb729fc8dbf0d0c20c64abcec67045`；[43687011227](https://github.com/wmqfl861/AK-sending/actions/runs/43687011227) | 当前 git 凭据直接读取 run/jobs API，从 in_progress 等待至 completed/success；head_branch=dev/batch-k-20261114，run_attempt=1；13 jobs 中 12 常规 success、1 手动 release-deep-tests skipped；完成更新时间 2026-11-15T07:00:10Z |
| CI 实际门槛 | 同上 jobs/steps metadata 与固定 tip workflow | 所有 job 的完整 head SHA 匹配；全部常规 steps success，无 failure/cancelled/未完成步骤。Linux/Windows Rust、特性测试、fmt/clippy/rustdoc，Flutter analyze/test、纯 Dart，Android 双 ABI FFI/符号验证/debug APK，Linux/macOS portability，audit/deny、仓库检查、legacy signal、NAT netns 模拟均 success；不等于生产或物理双机 |
| 远端引用 | 初始 `git fetch --prune origin`；收尾 `git ls-remote --heads origin` | 只有 main=d6c71360784e1570720e56028a39f2a80777bb21、dev/batch-k-20261114=09fceb64a2fb729fc8dbf0d0c20c64abcec67045；未调用合并、推送或删除 |
| 本地引用与工作树 | `git branch -avv`；`git worktree list --porcelain`；`git status --short --branch` | main 留在 d6c7136，批次分支仍在固定 tip，只有主工作树；仅五个文档变化（含新增 K11），没有提交、合并、推送或被审代码改动 |

| 未执行 / 限制 | 本轮事实 |
|---|---|
| Linux 原生 systemd/权限部署、远端 SSH、物理双机 C4、生产抓包 | 本机只执行真实生成脚本的隔离模型，没有连接生产；真实 SQLite 文件/事务加模型服务状态不等于原生 Linux 或真实协议验证 |
| 本机 Flutter/Dart、Android/Linux/macOS 平台重建 | 本轮未改相关实现；实际平台门槛采用完整 SHA 匹配的最终 CI，不以旧产物冒充当前构建 |
| 手动 release deep tests | push workflow 的该 job skipped；本机未重跑 ignored release suite，不计作已执行或通过 |
| shellcheck / Flutter / Dart 本机命令 | PATH 未发现 shellcheck、flutter、dart；对应本机检查未执行。Bash 语法、自检及模型不等于 shellcheck |

| 仓外材料（本机 TEMP，无凭据） | 内容 |
|---|---|
| `ak-review-k11-09fceb6` | 集成者 query_ci.py、run/jobs JSON、固定导出树、K10 原报告双版本重放、专项脚本独立重跑日志、K11 原文重放脚本/日志和收尾检查；凭据只在查询进程内存 |
| `ak-k11-review-gates-t828lwck` | 本机全量门禁日志、summary.json、suite_counts.json、metadata.json |
| `ak-k11-environ-review.py`；`ak-k11-environ-review-ffbxwrf3` / `ak-k11-environ-review-drhx6qtj` | 专项 / 集成者完整运行目录模型、真实生成脚本、阶段输出、unit 和 stub 日志 |
| `ak-k11-environ-support.py`；`ak-k11-id-dir.rs` / `.exe`；`ak-k11-systemd-exec.html` | EnvironmentFile 与 cwd readlink 控制、Rust 默认链摘录编译、官方语义留档 |
| `ak-k11-destination-runner.py`；`ak-k11-destination-review-o2aj8t95` / `ak-k11-destination-review-5np0yg57` | 专项 / 集成者 clean、decoy、普通 SQLite 晚提交控制及所有实际产物 |
| `ak-k11-registry-revoke.py`；`ak-k11-registry-revoke-09fceb6-57ofrnle` / `ak-k11-registry-revoke-806ea6c-nxercc5y` | 专项真实 schema 的当前/基线 clean 与晚提交对照 |
| `ak-k11-registry-revoke-09fceb6-q42pdfyk` / `ak-k11-registry-revoke-806ea6c-3wkd5l36` | 集成者独立重跑真实 schema 当前/基线对照，含 SQLite、unit、脚本、日志与 summary.json |

## 28. 修复轮十一（K11 复核后的新候选，执行方记录）

[K11 复核](REVIEW_2026-11-15-batchK11.zh-CN.md)（K10-N1/N2 双双判通过）后，执行方修复其唯一新发现；复核方本地同步（§27、STATUS、状态表、K10 历史归属）与本节代码改动一并提交。

| K11 发现 | 修复内容 |
|---|---|
| K11-N1（P1，数据库 TOCTOU） | 切换顺序重排：停服（既有捕获/备份/stop 语义不变）→**静默源的最终身份/库迁移**（K10-N2 规则不变：一致保留、不一致拒绝、拷贝后 cmp 校验并记录 `cmp-verified post-stop`、全新脏目标拒绝、原位放行；一切拒绝经 `recover_services` 恢复服务而非裸退）→二进制替换→unit 写入→reload+enable，全程统一恢复窗口。复核场景以真实 schema-v3 SQLite（DDL 取自 store.rs）重建：停服钩子在旧服务仍 active 时执行复核的真实提交（revoked/revision 2），断言调用顺序 `stop → cp → cmp` 且目标库内容达 `("revoked", 2)`；`806ea6c` 基线同夹具目标停留 `("active", 1)`——原缺陷。停服后拷贝损坏/目标不一致等拒绝路径均断言服务已恢复运行 |

### 修复轮十一门禁与提交

本地：`bash -n`+自检全过；Python 251/251（部署套件 94，含 2 新增+3 重构回归与跨八版基线对照）；`check_repository.py` exit 0；契约 5/5；Rust 树与已验证链一致（本轮未改 Rust）。

| 提交 | 内容 |
|---|---|
| `2d77eed` | K11-N1 停服后静默迁移；复核方文档同步与执行方本节记录入库 |
