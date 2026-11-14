# 交接文档：批次 K（2026-11-14）

> 分支：`dev/batch-k-20261114`。首轮范围 `d6c7136..f9582e5`；修复轮范围 `f9582e5..f150503`。§0–§5 保留首轮历史，§6–§7 为执行方修复轮记录，§9 为本次独立复核证据。首轮文档已随 `b6296ea` 提交，本次 K2 文档同步仅保留本地。
> 每项状态只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)；复核发现见 [K 复核](REVIEW_2026-11-14-batchK.zh-CN.md)。运行编号、计数与结果集中在本文对应轮次的证据表；本轮发现见 [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md)。

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

执行方已将首轮复核文档与修复代码提交为 `f150503` 候选。修复轮独立判断见 [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md)，逐项状态只在审核状态表；本轮状态与证据纠正保留本地，未提交或推送。

## 9. 修复轮独立复核证据（2026-11-15）

被审代码固定为 `f150503`，修复差异为 `f9582e5..f150503`；本节区分本次实际执行、GitHub API 查询、对既有走查产物的复核与未执行事项。判断与修复要求见 [K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md)，每项状态只在审核状态表。本节及同步文档是本地未提交记录。

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

[K2 复核](REVIEW_2026-11-15-batchK2.zh-CN.md)给出 REJECTED 后，执行方按其 §1 修复全部阻断项；复核方本地同步的 §9/STATUS/状态表/首轮复核注记与本节代码改动一并提交。

| K2 发现 | 修复内容 |
|---|---|
| K2-N1（P1） | 整个服务切换窗口（停服→legacy 退位→二进制切换→unit 写入→daemon-reload→enable --now）纳入统一失败恢复：切换前备份旧二进制/unit/启用与运行状态（任一备份失败在停服前中止）；`recover_services` 恢复旧配置并按先前状态重启，输出 `RECOVERY OK` 或显式 `RECOVERY INCOMPLETE`。对 unit 写入/reload/enable-start/恢复本身失败均有行为回归；旧候选（`git show f150503`/`f9582e5` 真实执行同 stub）复现为无恢复停服退出 |
| K2-R2（P1） | 停服失败不再吞掉（`|| return 1` 进入统一恢复）；阶段 7 保留 ExecStart 配置检查后，以每 unit `MainPID`+`sudo -n readlink /proc/<PID>/exe` 与部署二进制严格相等为决定性核验，显式拒绝 `(deleted)` inode。回归保留「旧进程仍在运行、磁盘路径已替换」前置：旧候选安装与核验双双假成功，新代码停服失败即中止并恢复、旧进程映射不匹配报 running-process mismatch（exit 6） |
| K2-R3（P1） | phase1 占用探测、pid→unit/cmdline、unit 文件读取、身份目录枚举与存在性判断全部经 `sudo -n` 只读执行；无法确定占用/身份/数据库时在切换前 exit 3 明确失败。离线权限模型（非 root SSH 用户、他人持有端口、私有身份目录）对照：`f9582e5` 输出 `pid unknown`/not-a-systemd-unit 且漏私有身份；新代码发现 pid/unit/ExecStart/私有 key+db。均为离线模型，不称已验证 Linux 权限场景 |
| K2-R4（P2） | 检查器改为从 STATUS 所在目录解析链接真实目标：去 fragment、拒绝外部 URL/绝对路径/越界 `..`、词法规范化后必须与最新 HANDOFF-BATCH-* 的规范路径相等；run 号/矛盾词校验锚定到该真实目标。反例翻转：同名异目录链接、外部同名 URL、缺失子目录目标在 `f150503` 检查器下通过、新代码报错；`f9582e5` 的同名 basename 旧反例保持失败 |
| K2-R5（P2） | `cross_nat_test.sh` 摘要按实际执行区分：doctor 跳过时写 `direct QUIC transfer sha256-verified over the observed path (no NAT observation)`，不再无条件宣称 real-NAT；s8 断言两种形态。SSH 套件移除恒真假自检，换为预修模式（timeout=0 立即读）真实负控；`TextIOWrapper` 修 mock，旧码结果为 7 失败 0 错误。部署测试改为执行基线自身生成的安装/核验脚本（`git show` 提取，缺标记则 skip），旧码对照为行为级未缓解失败演示；`OldHelperFromGitTests` 直接执行 `f9582e5` 两个脚本内嵌的真实 helper（延迟/空输出均 exit 255） |
| 文档 | 采纳并提交复核方 §9 及全部本地纠正（含暗色首页 ANR 遮挡边界、速率行时间跨度≠相位时长、owner 冷启动为推断非日志直接证明、`+修复轮` 占位换真实提交号、K-CI 行更新为新候选已验证）；本节为执行方补记，来源区别保留 |

### 修复轮二门禁与提交

本地：两脚本 `bash -n`+自检全过；Python 156/156（含部署 23、SSH 7）；`check_repository.py` exit 0（本机 STATUS→K 交接链接解析通过）；契约 5/5；Rust 树与已验证的 `5beb10a..f150503` 链一致（本轮未改 Rust）。修复轮二提交清单与最终 CI run 见本表下方追加行。

| 提交 | 内容 |
|---|---|
| `f7dece2` | K2-N1/K2-R2/K2-R3 部署脚本与回归；K2-R4 检查器链接解析与回归；K2-R5 摘要/负控/旧码执行化；复核方文档纠正与执行方补记入库 |
