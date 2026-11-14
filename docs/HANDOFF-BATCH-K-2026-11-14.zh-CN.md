# 交接文档：批次 K（2026-11-14）

> 工单来源：[批次 J 复核](REVIEW_2026-11-14-batchJ.zh-CN.md) §8；J1 原清单见 [U1 交接](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md) §5。
> 分支 `dev/batch-k-20261114`（自 main `d6c7136` 切出）。本文按 CR-P3-20 新规则写：run 号只在此处，状态只在 §7。

## 0. 一页结论

- **K0 仓库清理完成**：7 个远端分支全部删除（均验证为 main 祖先后删），远端只剩 `origin/main`。worktree 清理、本地旧分支 `-d` 删除。main NAT 十轮 run [43558234150](https://github.com/wmqfl861/AK-sending/actions/runs/43558234150) **成功**。
- **K1 J1 走查余项**：Windows 深色可读性**通过**（12 张进程内截图，RepaintBoundary.toImage 方案）；Android SAF **不通过**（推送成功但 QUIC 数据面在模拟器 QEMU NAT 下未建立——根因与 CR-P3-21 一致）；SafeArea 修复后标准首页截图**通过**（亮暗各一张）；暗色传输未完成（同一数据面阻塞）；8MiB runner 已修复但未执行（同一原因）；模拟器显示设置**已恢复**。
- **K2 CR-P3-20**：check_repository.py 新增 run 号溯源 + 矛盾词两条机械校验，配 8 项单测（Python 113/113）。
- **K3 CR-P3-21**：探测早退修复——前置 burst 保证早退前至少发过一轮（`biased;` 单独不够，tokio 1.54 实证 `interval` 首 tick 非立即）。确定性新测试旧代码 20/20 失败→修复后 500/500 + 50/50 并行通过。200ms 上限不放宽。
- **C4 前置**：J2/J5 已在批次 J 达成；J1 行为层完成但 SAF 和暗色传输在模拟器数据面阻塞（建议 K3 修复合入后用修复后的 runner 复验，或直接进入 C4 由真机覆盖）。

## 1. K0 仓库清理

| 操作 | 结果 |
|---|---|
| 7 个远端分支 | ✅ 全部删除（`dev/review-fixes-20261111` 等，均为 main 祖先） |
| 远端分支列表 | ✅ 只剩 `origin/main` |
| 本地切 main | ✅ 快进到 `d6c7136` |
| worktree 清理 | ✅ 2 个已移除（aux runner 仓外保留） |
| 本地旧分支 | ✅ 均用 `-d` 成功 |
| 新分支 | ✅ `dev/batch-k-20261114` |
| main NAT 十轮 | [43558234150](https://github.com/wmqfl861/AK-sending/actions/runs/43558234150) **成功** |

## 2. K1 走查结果

### a. Windows 深色可读性 — 通过

进程内截图（RepaintBoundary.toImage），非 OS 截图。真实 DLL/CLI/本地 registry-STUN/合成 200MiB+5MiB。12 张 PNG（浅/暗 × 6 状态），每主题独立进程，flutter 退出码均 0。

| 状态 | 浅色 | 暗色 |
|---|---|---|
| 等待接受 | win-light-01 | win-dark-01 |
| 发送进度（速度/剩余） | win-light-02 (35%, 65.0MB/s) | win-dark-02 (32%, 60.3MB/s) |
| 等待校验 | win-light-03 | win-dark-03 |
| 已完成（绿色+用时+局域网直连） | win-light-04 | win-dark-04 |
| 失败（红底白字 DECLINED） | win-light-05 | win-dark-05 |
| 最近传输（成功+失败各一） | win-light-06 | win-dark-06 |

截图仓外：`C:/Users/Administrator/tmp/ak-batch-k-k1-20261114/screenshots/`

### b. Android SAF — 不通过（阻塞于传输数据面）

**完成**：`MSYS_NO_PATHCONV=1` 后 adb push 成功（上一批"Permission denied"根因查明：Git Bash MSYS 路径改写）；APK 构建/安装/启动成功；配对码生成并读出；CLI join-recv 经 registry 配对成功。

**阻塞**：QUIC 数据面在 QEMU user networking 下始终未建立。CLI 侧只观察 self-echo 探测，应用侧探测从未到达 CLI。约 10 分钟后接收端 1000s 超时退出。现象与 CR-P3-21（探测早退可能在本端发出第一个探测包之前返回）一致，K3 修复后可复验。

### c. SafeArea + 暗色 — 部分完成

- SafeArea 修复后标准首页：**通过**（`home-safearea-light.png` / `home-safearea-dark.png`，标题完整显示在状态栏下方）
- 暗色实际传输截图：**未完成**（同一数据面阻塞）

### d. 8MiB runner — 修复完成、重跑未执行

修复内容（150ms 采样 + 累计 `sawRate`/`sawFile` 判定）保留在 worktree 未提交。未执行原因：同一数据面阻塞。

### e. 模拟器显示设置 — 已恢复

恢复前：Override 780×1688 / density 320 / 自动旋转 1 → 恢复后：Physical 1080×2400 / density 420 / 自动旋转 1，与原始记录一致。

## 3. K2/CR-P3-20 机械校验

- `check_repository.py` 新增：`handoff_run_number_issues`（STATUS "最新验证" 的每个 run 号必须在最新交接中出现）+ `handoff_contradiction_issues`（最新交接不得同时含"已核"与"尚待/待补/仍待"）。
- 配 8 项单测：正例、run 缺失、矛盾词、引号豁免、交接定位、fail-closed。
- Python 113/113 全过。
- 当前文档在新校验下有 5 条红项（预期行为：K 交接落库后转绿）。

## 4. K3/CR-P3-21 探测早退修复

- 方案：前置 burst（循环前先发第一轮探测）+ `biased;` select + `interval_at`。`biased;` 单独不够——tokio 1.54 实证 `interval` 首 tick 约 5ms 后才就绪，biased 下旧 bug 仍复现。
- 新确定性测试 `early_exit_still_sends_at_least_one_probe_when_confirmation_is_pre_buffered`：旧代码 20/20 失败（确定性检出），修复后通过。
- 验收：500/500 单独 + 50/50 并行（--test-threads=8），200ms 上限不放宽。ak-net 48/48，fmt/clippy 干净。

## 5. CI 证据

| 项 | 提交 | run | 结果 |
|---|---|---|---|
| main NAT 十轮 | `dc8a91b` | [43558234150](https://github.com/wmqfl861/AK-sending/actions/runs/43558234150) | 成功 |
| main CI | `dc8a91b` | [43558234099](https://github.com/wmqfl861/AK-sending/actions/runs/43558234099) | 12项成功 |
| main netem | `dc8a91b` | [43558234106](https://github.com/wmqfl861/AK-sending/actions/runs/43558234106) | 成功 |
| dev分支最后CI | `4f604bc` | [43544631036](https://github.com/wmqfl861/AK-sending/actions/runs/43544631036) | 12项成功 |
| 批次J十轮 | `ad975c0` | [43538159455](https://github.com/wmqfl861/AK-sending/actions/runs/43538159455) | 10/10, 40/40 |
| netem A/B | `8ef4a07` | [43466913049](https://github.com/wmqfl861/AK-sending/actions/runs/43466913049) | 8MiB 23.0 vs 5.7 MiB/s |

本地门禁：fmt/clippy/rustdoc/workspace tests 全 exit 0；Python 113/113；契约 5/5。

## 6. C4 前置状态

- J2/J5：✅ 已在批次 J 达成（十轮 10/10、40/40）
- J1：行为层完成（Windows 1-6/10 同机行为 + 12 张进程内主题截图 + SafeArea 修复后首页截图）；SAF 和暗色传输在模拟器数据面阻塞（根因 CR-P3-21，K3 已修复，修复合入后可复验或由 C4 真机覆盖）
- Windows IPv4 TTL 抓包：所有者 C4 步骤

## 7. 红线

- 未引入 TURN/relay；未访问生产 122.51.121.24；未生成签名密钥；合成文件 only；截图仓外；模拟器测试未写成真机验证；不直接推 main，不强推。
