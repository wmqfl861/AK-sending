# 交接文档：批次 K（2026-11-14）

> 范围：`d6c7136..f9582e5`，分支 `dev/batch-k-20261114`。本页已按独立复核纠正原交接的过时表述；本次文档改动在本地保留，未提交、未推送。
> 每项状态只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)；复核发现见 [K 复核](REVIEW_2026-11-14-batchK.zh-CN.md)。运行编号、计数与结果只集中在本文 §5 证据表。

## 0. 范围与来源

工单来源为 [批次 J 复核 §8](REVIEW_2026-11-14-batchJ.zh-CN.md#8-批次-k-工作单交给本地-agent)，J1 逐项定义见 [U1 交接](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md)。`f9582e5` 追加了 Linux 部署与公网接收端测试脚本；其静态/离线验证不代表已经部署或完成跨网实验。

## 1. 仓库与分支

K0 的历史分支清理由执行方完成；独立复核重新读取了远端引用和 worktree。批次 K 的代码未合入 main，分支继续保留用于修复。具体基线与引用记录见 §5。

## 2. J1 走查及边界

Windows 主题图使用 `RepaintBoundary.toImage`，属于进程内光栅。执行方使用真实 DLL/CLI、本地 registry/STUN 和合成文件；截图保存在仓外合成实验目录，复核抽查了深色传输和拒绝图。新 runner 的漏采断言、有效端点配置问题见复核报告。

Android 辅助 runner 的采样调整已经在 `bd61553` 入库，原文“worktree 未提交”已纠正。它注入文件选择结果，不替代系统 SAF 普通流程。本批没有用修复后的引擎重新完成 SAF、暗色传输和辅助 runner 验证。

QEMU 数据面未建立与 CR-P3-21 的因果关系尚未验证；不能因探测单测通过就认定 Android 场景已修复。SafeArea 首页图与模拟器设置恢复属于执行方报告，详见证据表。C4 前置继续遵循原工作单，不用未执行的真机实验替代 J1 缺项。

## 3. 文档机械校验

`5415cb6` 添加运行编号追溯与矛盾词检查。被审快照的检查器按文件名挑选 K 交接，即使 STATUS 未链接 K 也会通过；复核报告记录了该反例。原文“当前有红项”是交接落库前的历史结果，不作为当前门禁结论。

## 4. 探测修复与脚本

`1d995b0` 在接收轮询之前执行首轮发送，再通过 `interval_at` 和 `biased;` 管理后续发送与接收。新回归测试用预缓冲包钉住竞态前置条件；原有早退时间限制保留。该修复不构成真实对端身份、路径或 QEMU 失败根因的证明。

Linux 部署脚本与公网测试脚本存在安装、旧版本路径、Paramiko 等执行问题，具体触发和修复要求见复核报告。公网测试脚本的 Linux 进程是最终接收端，不是代转节点；该路线不替代 C4 的产品任务码路径和受限 NAT 组合验收。

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

复核方 2026-11-14 给出 REJECTED 后，执行方按 [K 复核](REVIEW_2026-11-14-batchK.zh-CN.md) §1 修复全部阻断项并重跑走查，形成本节证据。修复轮提交号见 §7；与 §5 的被审快照（`f9582e5`）区分。

| 修复项 | 对应发现 | 内容 |
|---|---|---|
| K-R1/K-R2/K-R3+STUN 标签 | K-deploy | 预编译部署改独立暂存路径+哈希/版本/可执行三重预检+失败恢复重启服务；ExecStart 仅保留参数、可执行路径改写为已验证新路径并核对运行中服务版本；两个 Paramiko helper 改阻塞+双流排空+空闲预算；`--skip-doctor` 时 STUN 门标签为 SKIPPED。离线自测 s1..s8/s1..s9 扩充，新增两个测试文件在旧代码上 9/9、5/5 失败、新代码全过 |
| K-R4/K-R5 | U1（Windows runner） | 截图覆盖改单调计数器+缺失命名断言+导出前重查文件；端点 guard 改为按应用同款两级解析（运行期 env 优先）校验**生效值**，同一份已验证配置传给应用与 CLI；+12 个单测（直接 import runner） |
| K-R6 | CR-P3-20 | 检查器新增 `status_linked_handoff`：按 STATUS 实际链接的交接锚定校验；链接缺失/过期/指向不同批次即失败。在 `f9582e5` 导出树上复现原反例并报错 |
| K-CI | K-CI | Windows workspace 失败用例定位为 `revoke_after_pairing_preserves_books_and_admits_further_sends`（redeemer 90s 窗口内 owner 未完成应答，95.8s 超时退出）；属既有慢机类（run 43312035496、43273272597 同类）。修复：测试先等 owner `listening`（身份→TLS→配对服务 adopt→receiver bind 全部就绪的信号）再启动 redeemer，窗口只覆盖配对流程本身。同代码本地 5/5 通过（45.45s） |
| 措辞 | 非阻断 | `ak-net/src/lib.rs` 探测早退注释的 "guaranteed to observe" 改为「本端先尝试发送，UDP 丢失与瞬态发送错误除外」 |
| 辅助 runner 加固 | U1（item d） | 速率行断言改为**证据级条件判定**：按进度流自身时间戳判断 transferring 相位是否跨过 1s 均值预热期，跨过则速率行必须出现，未跨过记录 `RATE_LINE_NOT_APPLICABLE` 通过（发现：宿主↔模拟器链路上全新 8MiB 发送 ~4.7s 完成，中间快照都落在首秒内，bps 合法为 null，到 verifying 时速率行按设计移除——J 批次「漏采速度行」的根因即此，非采样问题）；轮末新增 `flow_done_button` 重置配对流程视图（否则 `_flow.active` 使主页不再显示生成码按钮，暗轮无法开始）；发送前清除本运行自己的 checkpoint 目录（防上轮尝试的续传污染）；每轮重置诊断缓冲 |

### 修复轮走查证据

| 走查 | 结果与边界 |
|---|---|
| Android 辅助 runner（亮+暗） | 两轮全新 8MiB 真实发送全部完成：亮轮 1m14s/110KB/s、暗轮 1m17s/105KB/s（全程含等待接受；实际数据面 ~5s）；接收端均 8388608 字节、SHA-256 与源一致 `e442fcea…e5bc3`；暗轮主题在测试绑定内验证；整体 exit 0。速率行两轮均为 NOT_APPLICABLE（原始流时间戳在证据日志，transferring 相位未跨 1s 预热）；三行同现布局的长期传输证据仍属 J 批次 200MiB 轮，本轮不重复宣称。对端为主机 `task join-recv`（task 平台端点 blob 方向校验通过——普通 wave-21 `daemon --invite-code` 不带端点 blob 会被 P1-05 gate 拒绝，此前 0x2101 根因） |
| Windows 主题图（修复后 runner） | 亮、暗各一轮全部通过：每轮 6 张进程内 PNG（等待接受/传输中/等待校验/完成/失败/最近传输）全部导出且覆盖断言 transferring=1、verifying=1；亮暗合计 12 张；暗色传输中抽查（76.0MB/200MB、60.1MB/s 速率行可见、无裁剪）。修复轮还发现并修复 runner 缺陷：合成文件此前由外部预置、runner 不自建——被清空的目录使全部候选路径报 `storage error: path not found`，现 runner 自建确定性合成文件并记录 SYNTH_CREATED；另加 SEND_STATUS/SEND_RESULT/SEND_ERROR/DIAG 证据行 |
| Android 系统 SAF 普通流程 | 新增 `flutter_app/integration_test/k1_android_saf_test.dart`（混合驱动：Flutter 侧 widget 级输入配对码/选角色/打开发送，系统 DocumentsUI 侧由 adb/uiautomator 外部驱动——Flutter 无 uiautomator 语义树、纯 adb 盲驱已证明不可靠，尝试记录在案）。走查通过：真实系统选择器打开 → 外部在 DocumentsUI 的 Downloads roots 视图点选 `saf_pick_20261114.bin`（4MiB，SHA-256 `f488b0ee…39cc`）→ 插件真实返回 URI → 已选 1 项 → 测试驱动发送 → 发送完成（互联网直连，1m13s）→ 接收端 4194304 字节哈希一致 → 整体 exit 0。本机 registry/STUN 端点经 dart-define 注入（未配置时应用 fail-closed 显示错误卡，已截图验证） |
| SafeArea 标准首页（亮+暗） | 正式 `flutter build apk --debug` 安装的 app：亮色与系统夜间模式（`ui_night_mode=2` + 重启生效）各一张标准首页 OS 截图（`09-main-safearea-light.png`、`09-main-safearea-dark.png`，仓外证据目录）；标题与内容位于状态栏之下，生成/输入配对码两按钮均渲染 |
| 模拟器显示恢复 | 全程使用标准 1080×2400/density 420；走查后确认值并恢复：自动旋转=1、夜间模式=0、adb unroot；恢复前后值记录于仓外 `emulator-restored-state.json`。期间一次夜间模式试验需重启框架导致模拟器卡死，冷重启同 AVD 后继续（安装与设置持久） |

### 修复轮走查环境

本机 registry（127.0.0.1:17200，指纹 `4395e22a…2687`）+ 本机 STUN（127.0.0.1:17101），Windows 端 CLI/DLL 为 `git 1e59342` 构建（修复轮 Rust 代码仅注释与测试变更，产品二进制与 `f9582e5` 一致）：`ak_ffi.dll` SHA-256 `6414d48b…57ab`、`ak-cli.exe` `0675e7d2…5b36`、Android `libak_ffi.so`（x86_64）`764d940d…c54`。Android 侧文件与哈希均为本运行内生成的确定性合成数据；未触碰共享存储权限与用户文件。

## 7. 修复轮提交与最终 CI

| 提交 | 内容 |
|---|---|
| `c2e6d2c` | CR-P3-20：检查器锚定 STATUS 链接的交接（K-R6）+9 回归 |
| `11f91e9` | K1：Windows 主题图 runner 覆盖计数器+生效端点 guard+自建合成文件+诊断行（K-R4/K-R5）+12 单测 |
| `dbb46fc` | K 脚本：部署暂存/恢复、ExecStart 改写与运行中版本核对、Paramiko 阻塞读、STUN 标签（K-R1/R2/R3）+两个旧码必败测试文件 |
| `5beb10a` | CI flake：owner listening 先行再启 redeemer；ak-net 早退注释措辞收敛 |
| `c42bbf8` | K1：辅助 runner 速率行条件判定/流程重置/checkpoint 护栏；新增系统 SAF 混合驱动 runner |
| （文档提交） | 本交接 §6/§7、STATUS、审核状态表修复轮行与 K 复核文档入库 |

修复轮本地门禁（新候选）：fmt/clippy/rustdoc exit 0；Rust workspace 1200 passed、0 failed、8 ignored（与被审快照的本地复跑一致）；Flutter analyze 零问题、单测全过（含新增 12 项 runner guard）；`check_repository.py` exit 0（STATUS 链接 K 交接）；契约 5/5；Python 135/135；两脚本 `bash -n`+自检全过。最终 CI run 号推送后由复核或执行方按流程补记。

## 8. 收尾

复核方本地同步的 STATUS/状态表/K 复核文档与修复轮代码改动一并提交为新的合并候选；运行编号、计数与结果仍只集中在本页 §5/§6/§7。
