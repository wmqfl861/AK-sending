# 交接文档：批次 S（2026-11-20）

> 起点：main 快进到批次 R 复核的最终文档提交（本文随该提交入库）。批次 R 的结论见 [R 复核](REVIEW_2026-11-20-batchR.zh-CN.md)；复核方的命令、运行编号与计数见 [R 交接 §9](HANDOFF-BATCH-R-2026-11-20.zh-CN.md#9-复核方证据2026-11-20)。执行方从本文 §8 证据表开始记录本批的运行编号与计数。
> 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。

## 0. 一句话现状

批次 R 通过：Q-N2 在本地 Windows 上确认，两个规模测试的校验阶段都大幅缩短。R3 的基准复测显示，本机 8 线程下分批落盘换不来明显收益，复核方决定暂不做，BACKLOG 的“海量小文件的校验阶段”一项以 Q-N2 关闭。最终推送的 CI 上一项 Windows 测试间歇失败（R-N1），复核方重跑通过并修好了这项测试。

批次 S 做两项发布前的检查，都不需要等复核方改代码：

- S0：收尾清理；
- S1：Android 模拟器上的真引擎回归；
- S2：Windows 打包冒烟；
- S3：全部门槛。

复核方下一轮实现发送端源文件复核期间的活动信号与接收端磁盘写满的报错（[R 复核 §5](REVIEW_2026-11-20-batchR.zh-CN.md#5-余下的工作与批次-s)），由批次 T 在 Windows 上确认。

## 1. 复核方在批次 R 分支上的改动

| 项 | 内容 | 主要位置 |
|---|---|---|
| R-N1 | 取消测试 `sched_cancel_stops_file_transfer_growth` 把接收端的检查点目录移到被测目录之外 | `crates/ak-ffi/src/pairing.rs` |
| 文档 | R 复核、R 交接 §9、状态表（Q-N2、R-N1、main 合并、表尾说明）、STATUS、BACKLOG（校验阶段一项关闭；源文件复核一项写明改法） | `docs/` |

## 2. 接手前必读

### 2.1 推送之后等 CI 跑完（R-N1）

批次 R 的最终推送只改了文档，它的 CI 却有一项 Windows 测试失败；执行方报完成时这次运行还没跑完，§8 里没有它。本批起：

- 最后一次推送之后，等这次 CI 全部跑完再报完成；
- 失败了，先在 §8 记下运行编号、失败的任务与测试、完整报错，再追加一个提交推送；重跑通过也照样记失败那次；
- 回复里写明最后一次推送的 CI 结论。

### 2.2 Android 模拟器回归（S1）

- `scripts/smoke_android_host.sh --reverse` 在 Android 一侧跑 `ak-cli` 接收端，宿主跑发送端，传完后把收到的文件拉回宿主逐字节比较。脚本开头的注释写了全部前提：在线的模拟器、PATH 上有 python、设备上有 `/data/local/tmp/ak-cli`。
- `ak-cli` 要交叉编译到 `x86_64-linux-android`，做法见 [BACKLOG](BACKLOG.zh-CN.md) “首个跨设备传输组合”一条：ring/rustls 的 C 构建沿用 `scripts/build_android_ffi.sh` 的 `CC_x86_64_linux_android`、`AR_x86_64_linux_android` 设置，再 `adb push` 到 `/data/local/tmp/ak-cli`。
- 脚本在宿主上起一个 UDP 转发小程序，原因见脚本里的 TRANSPORT NOTE；数据始终是端到端的 UDP 报文，不是中继，也不经过服务器。
- 这是模拟器与宿主同机的测试，§8 里写成“模拟器”，不能写成真机验证。
- 不要把配对码贴进 §8；指纹与 job id 只写前 8 位。

### 2.3 Windows 打包（S2）

- `scripts/package_windows.ps1` 只在本机打包，不上传，产物在 `target\dist\` 下（已被 .gitignore 覆盖）。
- 打包后用包里的 `ak-cli.exe` 跑 `scripts/smoke_loopback.sh --binary <包里的 ak-cli.exe> --skip-probe`；`--skip-probe` 跳过只有旧明文信令特性构建才有的探测一节。
- §8 里写包内文件名与大小、脚本打印的版本与提交号、冒烟的结论；本机路径按红线略写。

### 2.4 仓库检查

`python scripts/check_repository.py` 会拦下：STATUS 少链接 docs 下任何一份交接或复核文档，或 AGENTS、CHANGELOG、ROADMAP、BACKLOG、OPERATIONS、SUPPORT_MATRIX；状态表批次行（编号形如 S0、S1、S-N1）的“状态”“证据摘要”两列和 STATUS“最新验证”以外的部分出现运行编号、通过比、测试计数、倍数或带小数、区间的耗时。这些数字只写在本文 §8。详见 [R 交接 §2.3](HANDOFF-BATCH-R-2026-11-20.zh-CN.md#23-新增的仓库检查q-n1)。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办，决定正式完成的时间）

E6 生产部署、C4 物理双机跨 NAT、E5 签名、E7 许可证、CR-P3-03、CR-P3-05，以及 P4-02 的外部安全审查，见 [状态表 §8](REVIEW_2026-11-11.zh-CN.md#8-需所有者确认的事项)。批次 M 的 M2 与之后各轮的改动都要等 E6 部署后才进入生产服务器；CR-P1-02/05/06 的真机验收、P0-05 的网络样本与 P0-07 的抓包都依赖 C4。

### 3.2 工程项

- 复核方下一轮：发送端源文件复核期间的活动信号（新增能力位）、接收端磁盘中途写满时报 `StorageFull`；批次 T 在 Windows 上确认。
- 之后：CR-P2-08 生产代码拆分（复核方）；P2-02 的移动数据授权（先请所有者决定 v1 是否需要）；发布前整理（安装卸载、升级兼容、ROADMAP 与支持矩阵刷新、发布说明）。
- [BACKLOG](BACKLOG.zh-CN.md) 的 P3-02 之下仍开着：接收端磁盘中途写满、发送端的源文件稳定性复核、块阶段的检查点保存（默认时限下不构成问题，暂不排）。
- CR-P3-19 的剩余差距、Linux GUI、Android arm64 真机、BACKLOG 的 P3/P4：同 N 交接 §3.2。

## 4. 批次 S 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| S0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-r-20261120 origin/main` 成立，然后删除这个远端分支；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-s-<日期>`；5) 在 §8 回填 main 上本文所在提交的 CI 运行编号与结论 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；两条命令的输出原样贴进 §8；§8 有回填行 |
| S1 | 按 §2.2 交叉编译 `ak-cli`（`x86_64-linux-android`，release）并推到模拟器，先在设备上跑一次 `ak-cli --version`；再运行 `bash scripts/smoke_android_host.sh --reverse --release`，然后加 `--mb 64` 再跑一次 | 两次都 PASS：Android 接收端报 final ack verified=true，拉回的文件逐字节一致。§8 原样贴出 `--version` 的输出、每次的结论行与比较结果。环境不具备（没有 NDK、模拟器起不来）时写“未执行”和原因，不要改脚本绕过 |
| S2 | 运行 `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package_windows.ps1`（release），再用包里的 `ak-cli.exe` 运行 `bash scripts/smoke_loopback.sh --binary <包里的 ak-cli.exe> --skip-probe` | 打包成功；冒烟 PASS。§8 写包内文件清单（名称与大小）、版本与提交号、冒烟的结论行 |
| S3 | §5 的全部门槛 | 全部通过；失败按 §6 记录 |

并行建议：S1 与 S2 互不依赖，可以分给两个执行者；同一台机器上就先 S2 后 S1（S1 要模拟器，占内存多）。

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
```

补充说明：

- 本机 Rust 应使用 rust-toolchain.toml 钉的 1.99.1，与 CI 一致；改用其他版本时如实记录。
- 测试与脚本产生的日志不入库，只在 §8 写结论、耗时和失败时的报错行。
- 缺工具时写“未执行”，不能写成通过。
- 推送前在最终文档上跑一遍 `python scripts/check_repository.py`（§2.4）。

## 6. 文档与分支规则（摘自 AGENTS.md）

- 每项状态只写在状态表里，提交列写真实提交号，不写“本提交”；`check_repository.py` 会检查状态表的每一行，也会拦下被空行隔到表外的行。
- 状态表的“证据摘要”列不写运行编号、计数、倍数和耗时（§2.4 的检查会拦下大部分）。
- 一项被后续批次确认或推翻时，原地改写它的那一行，不要只在表尾追加新行。改写时保留原有的缺陷描述、修复描述和修复提交号，只补充新的结论。
- run 号和计数只写在本文 §8；STATUS 只写阶段结论、下一步和链接，并链接到本文。改 STATUS 时整份改写，保留全部入口链接，只在最前面加上新的交接与复核。
- 状态表表尾的说明行由复核方在复核时写，执行方不要加。
- 本文不要同时出现“已核”与“尚待 / 待补 / 仍待 / 待执行”（`check_repository.py` 会拦截）。
- 执行方只推 `dev/batch-s-<日期>`；复核通过后由复核方快进合入 main，再由执行方删除分支。任何人都不强推，包括改写自己刚推上去的提交：发现错误就追加一个新提交。
- 新增的 CI 门槛如果失败，先查清原因；不要为了让它通过而削弱它。
- 失败都要记入 §8，不能只记最后一次：
  - CI 运行失败，记运行编号与失败原因；
  - 测试失败后重跑通过，记失败那次的完整输出与原因分析。
- 最后一次推送的 CI 跑完再报完成（§2.1）。
- 验收要求原样贴出的输出，每次都要贴，不能写成“与上一次一致”。
- 没有做的项，在 §8 写明没有做和原因。

## 7. 红线

- 不引入 TURN/relay，服务器不经手文件数据（ADR-001）。
- 不部署、不登录、不修改生产服务器，也不改它的防火墙。
- 不生成签名密钥。
- 配对密钥、令牌、用户文件、真实文件名或路径、完整网络诊断都不入库。
- 模拟或同机测试不写成真机验证。

## 8. 证据表（执行方填写）

执行方在本机 Windows 上工作（rustc 1.99.1，与 `rust-toolchain.toml` 一致）。S1 的模拟器是本机 AVD `ak_test`（Android 15，API 35，userdebug，SELinux Enforcing，x86_64），宿主与模拟器同机；以下 S1 各条都是模拟器上的记录，不是真机验证。配对码不入库；指纹与 job id 只写前 8 位。

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| main 上批次 R 最终提交的 CI | `db51363` run [44718924740](https://github.com/wmqfl861/AK-sending/actions/runs/44718924740) | completed success（12 项常规任务 success，2 项手动深测 skipped） |
| S0 分支与 worktree | 本地 Windows | `git merge-base --is-ancestor origin/dev/batch-r-20261120 origin/main` 成立后删除了该远端分支；本地 main 快进到 `db51363`，删除本地 `dev/batch-r-20261120`，`git worktree prune` 后从 main 切出 `dev/batch-s-20261120`。`git branch -a` 原样输出：`* dev/batch-s-20261120`、`  main`、`  remotes/origin/HEAD -> origin/main`、`  remotes/origin/main`。`git worktree list` 原样输出（本机仓库根路径按红线略写为 `<repo>`）：`<repo> db51363 [dev/batch-s-20261120]`，只有主目录一行 |
| S2 打包 | 本地 Windows（release） | 成功。包目录 `target\dist\ak-sending-0.1.0-windows-x64`（.gitignore 覆盖），包内 7 个文件共 24,190,209 字节：`ak-cli.exe`（14,819,328）、`ak_ffi.dll`（9,358,336）、`README.md`（9,567）、`MANIFEST.txt`（1,032）、`ak_sending_registry.db`（0，空 SQLite 模板）、`ak_sending_registry.db.README.txt`（1,403）、`start-registry-server.bat`（543）；另有 zip 9,691,856 字节；未上传。包里 `ak-cli.exe --version` 原样输出：`ak-cli 0.1.0 (wire v1, caps 0x0007 [resume|pause-resume|multistream], release build, git db51363)` |
| S2 冒烟 | 本地 Windows，包里的 `ak-cli.exe` | `bash scripts/smoke_loopback.sh --binary <包里的 ak-cli.exe> --skip-probe` 结论行原样：`[smoke] 4/4 files identical (cmp), empty dir present, 4 files on disk`、`[smoke] pair books symmetric (A explicit, B TOFU); both checkpoints completed`、`[smoke] PASS: real two-process loopback smoke completed (port 17777)`（退出码 0；探测一节按 `--skip-probe` 跳过） |
| S1 交叉编译与设备上 `--version` | 本地 Windows + 模拟器 | `cargo build --locked --release -p ak-cli --target x86_64-linux-android`（NDK 28.2.13676358，环境变量同 `build_android_ffi.sh`：`CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER`/`CC_x86_64_linux_android` 指向 `x86_64-linux-android24-clang.cmd`，`AR_x86_64_linux_android` 指向 `llvm-ar.exe`），成功，产物 17,625,568 字节；`adb push` 到 `/data/local/tmp/ak-cli` 并 `chmod 755` 后，设备上 `--version` 原样输出：`ak-cli 0.1.0 (wire v1, caps 0x0007 [resume|pause-resume|multistream], release build, git db51363)` |
| S1 第 1 次冒烟（默认 8 MiB） | 本地 Windows + 模拟器，`bash scripts/smoke_android_host.sh --reverse --release` | **失败**。宿主发送端：`[send] connected; peer 41dc73cd…`、`[send] offered 3 entries, 8.00 MiB`、`[send] accepted 3 entries, 8.00 MiB` 之后 `Error: stream failed: connection lost: connection lost`（rc=1）；Android 接收端（`/data/local/tmp/rin`）：`[recv] manifest received`、`[recv] payload.bin (8.00 MiB)`、`[recv] 说明-中文文件名.txt (85 B)` 之后 `Error: io error: Permission denied (os error 13)`，没有到 final ack，也没有可拉回比较的文件。脚本判定 `FAIL: host sender exited rc=1` |
| S1 第 2 次冒烟（--mb 64） | 同上，`--mb 64` | **失败，同样位置**。发送端：`offered/accepted 3 entries, 64.00 MiB`，`file 1: 64.00 MiB acknowledged` 全部块确认后 `Error: stream failed: connection lost`；接收端同样在两个文件行之后 `Error: io error: Permission denied (os error 13)`（接收端指纹 e1203e28）。两次冒烟均未走到“拉回逐字节比较”一步 |
| S1 根因定位 | 模拟器上的设备内回环复现（不经宿主、不经中继与 redir） | 在设备上用同一二进制自发自收（127.0.0.1，1 MiB 载荷）复现同一报错，排除网络路径。失败后检查目标目录：两个 `.akpart` 都已写满（1 MiB 载荷完整）、mtime 已恢复成发送端的、检查点 `.akjob` 已写入、`emptydir` 已建——传输、整文件摘要校验、时间戳恢复全部完成，死在最后“建立最终名”一步。直接验证文件系统权限：shell 用户在 `/data/local/tmp` 里 `ln payload.bin.akpart payload.bin` 报 `Permission denied`（硬链接被系统策略拒绝），而 `mv`（改名）与 `ln -s`（符号链接）都成功。即接收端提交步骤的 `hard_link + remove_file` 在 Android 15（API 35，SELinux Enforcing）的 shell 域里不可用，改名可用。这是产品代码在“adb 部署 CLI”形态下的缺陷（Flutter 应用自身的应用数据目录是否受影响没有测），修复（如硬链接被拒时退回改名）由复核方决定；执行方没有为此改代码，也没有改脚本绕过 |
| S1 附带发现（诊断口径） | 本地复现过程中 | `receive` 的 `--out` 目录不存在时，引擎的真实错误（NotFound）被 CLI 包成 `receiver failed to bind / Caused by: channel closed`——引擎先开目录再绑定，绑定前失败时真实原因不显示。冒烟脚本会预建目录所以不受影响；这是报错口径问题，不是本批失败的原因（已在上行查明真正根因） |
| S 全量门槛 | 本地 Windows（rustc 1.99.1） | fmt/clippy/rustdoc/workspace（69 组，1223/0/10）/sqlite（54/0）/server-persistence（221/0）/Python（372 OK）/repo/contracts（5/5 PASS）/部署自检（PASS）/跨NAT自检（PASS）全部通过；`check_repository.py` 在回填本表提交号后通过（回填前只报状态表提交列占位） |
| S1 复测尝试与复核方的并行改动 | 复核方在本批执行期间直接向 `dev/batch-s-20261120` 推送了 S-N1 至 S-N4（源文件复核期间的活动信号与 StorageFull、混合版本回环检查、S1 缺陷的修复、接收端绑定前错误透出） | 执行方按 S1 的修复（f5ae8f6）尝试在模拟器上复测：交叉编译 `x86_64-linux-android` 失败，`ak-storage` 报 4 个 E0277（`&Path: Arg`、`io::Error: From<Errno>` 不满足）——rustix 声明缺 `std` feature；本机临时加 `std` 后 `cargo check --target x86_64-linux-android` 通过（改动已还原，未提交）。复核方随后推了 `21ba370`（同款修复），其前一次运行 44739416350 按并发规则被取消。按 f5ae8f6 提交信息，Android 路径的复测属批次 T；本表 S1 各行都是 db51363 代码上的记录 |
| S 批次最终 CI | `1b57ec2` run [44737651024](https://github.com/wmqfl861/AK-sending/actions/runs/44737651024) | **completed success**（12 项常规任务 success，2 项手动深测 skipped） |
