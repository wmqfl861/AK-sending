# 交接文档：批次 L（2026-11-16）

> 起点：main 等于批次 K 的最终提交（父提交 `2dd63e4`，第 24 轮修复的实现提交是 `b8142cb`）。本文由复核方（Claude）在批次 K 合入后起草，是批次 L 的起点交接。执行方从 §8 证据表开始记录本批的运行编号与计数。
> 每项状态只写在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。批次 K 的结论与验收口径见 [K24 复核](REVIEW_2026-11-16-batchK24.zh-CN.md)，批次 K 的全部证据见 [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md)（第 24 轮在 §53）。

## 0. 一句话现状

批次 K 已验收并合入 main。仓库现在只差两类事情：

- **真机与上线**：生产部署 E6、物理双机跨 NAT 的 C4、签名、许可证。这些都需要所有者执行或授权。
- **可选的工程项**：拆分超大文件、UDP 缓冲调优。

批次 L 的主线是 L0 收尾清理，加上两个可选工程项。部署脚本只在出现 K24 §5 的阻断问题时才动。

## 1. 批次 K 交付总览

| 项 | 内容 | 主要位置 | 证据 |
|---|---|---|---|
| K0 | 历史分支清理：远端只剩 main 和批次分支 | — | K 交接 §1/§5 |
| K1 | J1 走查余项：Windows 主题进程内截图、Android SAF 普通流程、SafeArea 截图、辅助 runner 修复、模拟器恢复 | `flutter_app/integration_test/k1_*`、`j1_android_u1_aux_test.dart`、`flutter_app/test/k1_runner_guards_test.dart` | K 交接 §2/§6/§9（执行方走查；复核方没有重跑 GUI） |
| K2 | CR-P3-20：文档机械校验（STATUS 链接到最新交接、run 号可追溯、“已核”不能与待办措辞并存） | `scripts/check_repository.py` 及其单测 | K 交接各轮 |
| K3 | CR-P3-21：探测早退之前至少先发一轮 | `crates/ak-net/src/lib.rs`（`probe_round` 的首轮 `send_burst` 与 `interval_at`、`biased;`） | K 交接 §53（压力复测 1200 次用例执行 0 失败、NAT 十轮） |
| K-CI | 两处测试夹具时序修复 | `crates/ak-itests/tests/invite_pairing_security.rs`、`crates/ak-ffi/tests/ffi_pairing.rs` | K 交接 §6/§40 |
| 部署脚本 | 一键部署 registry-server 与 stun-server，共 7 个阶段：预检发现、上传、服务器构建、安装（含统一恢复）、systemd、防火墙、运行进程校验 | `scripts/deploy_linux.sh`；测试 `scripts/tests/test_deploy_linux_install.py`（205 项） | 23 轮复核记录；第 24 轮见 §2 |
| 跨 NAT 脚本 | Windows 发送端到公网 Linux 接收端的 QUIC 实测脚本（服务器只做协调，是最终接收端，不代转） | `scripts/cross_nat_test.sh` | 自检通过；没有对生产服务器运行 |
| `registry-id` | 新增只读 CLI 子命令 | `crates/ak-cli/src/main.rs`、`crates/ak-identity/src/lib.rs` | 见 §2.3 |

## 2. 第 24 轮改动详解（接手部署脚本前必读）

### 2.1 发现环节的四处修复

1. **`/home` 枚举的记录协议（K23-N1）**
   - 生产者 `sudo -n find -H /home -mindepth 1 -maxdepth 1 -print0` 输出的是完整路径 `/home/<名字>`。
   - 消费者先用一遍循环校验全部记录：只剥一次 `/home/` 前缀，其他形状、多级路径、含 LF/CR 的名字一律以 exit 3 拒绝。校验通过后，第二遍才逐个调用 `registry_data_probe "/home/<名字>/.ak-sending"`。
   - 改动时不要重新拼接前缀，也不要回到边校验边探测的写法。
2. **起点符号链接（K23-N2）**
   - `-H` 只跟随命令行起点 `/home`，`/home` 下面的条目不跟随。
   - 生产者之前有一道 `sudo -n test -d /home` 闸：`/home` 不是目录（包括悬空链接）时直接拒绝。
3. **字面诊断（K23-N3）**
   - `priv_exists()` 只在 `pe_out` 与 `ls: cannot access '<路径>': No such file or directory` 字符串完全相等时判为不存在，其余情况都是不可判定（rc 2，调用方拒绝）。
   - 不要把路径放进 `case` 模式里。
4. **监听参数（K23-N4）**
   - `bound_listen_arg <行> <选项>` 设置两个全局变量 `bl_val` / `bl_bad`，必须直接调用，不能放在子 shell 里。
   - `reg_args_agree()` 里 `--port` / `--bind` 先过语法门（rc 3），再按“缺省值替换后的字符串”比较（rc 1 表示不同）。

### 2.2 注册中心身份连续性守卫

**选项**

| 选项 | 含义 |
|---|---|
| 默认 | 期望值是脚本里的 `DOCUMENTED_PIN`，即 OPERATIONS §3 记载的生产 pin，尚未在服务器上核实 |
| `--expect-pin <64hex>` | 期望值改为指定值（大小写都接受，比较时统一小写） |
| `--new-identity` | 有意使用全新身份：不允许服务任何已有的 key |

`--expect-pin` 和 `--new-identity` 互斥；格式错误或二者同时出现时 exit 2。

**三道检查**

1. **停服前**（phase456，`IDENTITY_GUARD=1` 时）：
   - `identity_pin()` 用暂存的新二进制运行 `sudo -n <SRC_BIN> --id-dir <目录> registry-id`。
   - 推导哪份 key 会被服务：迁移会把来源 key 复制进身份目录时，推导来源那份；否则推导身份目录自己那份。
   - 以下情况都以 exit 5 拒绝，此时没有停任何服务：
     - pin 与期望值不同；
     - 将要服务的 key 不存在（服务会新建身份）；
     - pin 无法推导；
     - 身份目录不是绝对路径；
     - `--new-identity` 模式下却会服务一个已有的 key。
2. **迁移之后**（`do_switchover` 的步骤 2b）：对实际要被加载的 `<IDENTITY_DIR>/registry.key` 再推导一次。不一致时 `return 1`，进入 `recover_services` 统一恢复，此时还没有换二进制、也没有写 unit。
3. **第 7 阶段**：如果运行中服务在 journal 里打印的 pin 与期望不同，以 exit 6 失败，而且不会导出 `/opt/ak-sending/registry-id`。

**由谁设置**

- `real_run` 总会设置期望值：`IDENTITY_EXPECT` / `IDENTITY_NEW` / `IDENTITY_DIR`，其中 `IDENTITY_DIR` 来自最终 ExecStart 的 `--id-dir`，并按 K13-N1 的白名单校验。
- 测试里直接调用 `write_phase456`、不传这几个变量时，守卫关闭。这只用于测试；生产路径一定会经过 `real_run`。

**守卫不覆盖的范围**

- 数据库连续性：仍靠迁移后的 cmp 校验与备份。
- 原生 systemd/journald 上的行为：没有验证过。

### 2.3 `ak-cli registry-id`

- 用法：`ak-cli [--id-dir D] registry-id`。读取已有的 `D/registry.key`，打印一行 64 位小写 hex。
- 它与 `registry-server` 共用两段代码，结果不会出现偏差：
  - 加载：`DeviceKeyPair::load_existing`，校验规则与 `load_or_create` 共享 `read_existing_seed`，只是不创建；
  - 格式化：`registry_pin()`。
- 不创建身份目录：主函数里对这个子命令走 `id_dir_path`，而不是 `id_dir`。
- 测试：
  - `ak-identity`：`load_existing_reads_the_same_identity_and_never_creates`
  - `ak-cli`：`registry_id_reads_the_served_pin_and_never_creates`
  - 跨进程：`ak-itests` 的 `invite_pairing_cli_e2e::registry_id_matches_the_served_pin_and_never_creates_an_identity`

### 2.4 部署测试夹具（写新测试前必读）

| 夹具 / 旋钮 | 行为 |
|---|---|
| `FIND_STUB` | 对 `/home` 执行**真实 find**，只把沙箱前缀 `$PROBE_DIR` 映射回 `/home`；原始与映射后的记录分别存为 `find-home-real.bin` 和 `find-home-logical.bin`。`FIND_HOME_FILE` 只用于“消费者形状”测试，可以注入真实文件系统产生不了的记录 |
| `setUp` | 每个探针根目录都会预建 `home/`（模型服务器根文件系统） |
| `TEST_STUB` | 新增对裸 `/home` 的映射 |
| `_run_p1_homes(script, out, records)` | `records` 是夹具实际建出的 `/home` 子目录名。生产者运行过时，会断言记录集合与之完全相等 |
| `_ntfs_backing()` | 只在原生 Windows Python 上映射 MSYS 私用区名（0x01–0x1f 和 `"*:<>?|`）；POSIX 上原样返回 |
| `AK_CLI_STUB` | `registry-id` 返回**模型 pin**，即 key 文件字节的 sha256（Python 侧用 `model_pin()` 计算）。没有 key 文件时失败；`REGISTRY_ID_FAIL=1` 时强制失败 |
| `JOURNALCTL_STUB` | `JOURNAL_PIN` 用来指定运行中服务打印的 pin |
| `_k23_chain(name, records, text=None, rev=None)` | 完整链：phase1（真实 find）、真实决策块、真实安装。传 `text` / `rev` 时执行某个基线版本自己的代码 |
| `_run_guard_install(...)` | 以指定的 `IDENTITY_*` 生成并运行 phase456 |
| `_k23_reg_args(text, cases)` | 对生成脚本里的 `reg_args_agree` 做合同测试 |

基线对照测试依赖 git 历史。CI 是浅克隆，这些测试会被跳过（CI 上 362 项里有 62 项跳过）；本机完整克隆时它们会实际运行。

### 2.5 部署脚本验收口径（K24 §5 摘要）

- **阻断**：身份被换掉或在没有确认的情况下新建身份；停服后没有恢复，又没有报告 RECOVERY INCOMPLETE，或者失败了却报告成功；来源被改写；违反红线；CI 有红灯。
- **不阻断**：停服前保守拒绝、主机状态没有任何变化的特殊输入；措辞问题。这类记入 `docs/BACKLOG.zh-CN.md` 的“部署脚本非阻断项”。

## 3. 全项目未完成事项

### 3.1 所有者事项（agent 不得代办）

| 项 | 建议做法 |
|---|---|
| E6 生产部署 | 1) 在开发机运行 `bash scripts/deploy_linux.sh --check-only --ssh-user <u> --ssh-host <ip>`，只看报告，不做任何改动；2) 正式运行时带 `--expect-pin`，默认值就是文档记载的 pin；如果脚本在停服前拒绝，并打印出“将服务的 pin”，先确认那是现网身份，再用 `--expect-pin <该值>` 重跑；3) 部署后从外网运行 `ak-cli doctor --stun <ip>:7101` 和 `scripts/cross_nat_test.sh`；4) 关闭公网 TCP 7100（OPERATIONS §6） |
| C4 物理双机跨 NAT | 两台真实设备分别在不同的真实 NAT 后面，跑任务码完整传输。内容见批次 J 工作单。目前所有跨 NAT 结论都来自 Linux 网络命名空间模拟 |
| E5 签名 | Android 上传密钥由所有者离线生成和保管，不入库 |
| E7 许可证 | 决定是否加 LICENSE 文件 |
| CR-P3-03 | 注册中心地址改用域名，客户端支持“当前 + 下一把”双 pin（在此之前，身份轮换等于全量客户端重发） |
| CR-P3-05 | 服务器 SSH 只允许密钥登录，并收紧来源 IP |

### 3.2 工程项

- K4 / CR-P2-08：拆分超大文件（批次 L 的 L2，可选）。
- K5 / CR-P3-19：UDP 缓冲（L3，可选）。
- release 深测里历史上的 chunk sweep / multistream 超时：一直没解决，也不能称已解决。
- Linux GUI 还没构建；Android arm64 只做了编译验证；macOS 和 iOS 由所有者排除。
- BACKLOG 的 P3/P4：五端适配、大文件与长时间恢复、签名升级与发布材料、外部安全审查、Android targetSdk 37 之前的本地网络权限流程。

## 4. 批次 L 工作单

| 项 | 步骤 | 验收 |
|---|---|---|
| L0 | 1) `git fetch --prune`；2) 确认 `git merge-base --is-ancestor origin/dev/batch-k-20261114 origin/main`，然后删除远端分支 `dev/batch-k-20261114`；3) 切到 main 并 `git pull --ff-only`，删除本地旧分支，执行 `git worktree prune`；4) 从 main 切出 `dev/batch-l-<日期>`；5) 在本文 §8 回填 main 上批次 K 最终提交的 CI 运行编号与结论 | `git branch -a` 只剩 main 和本批分支；`git worktree list` 只剩主目录；§8 有回填行 |
| L1 | 部署脚本只处理 K24 §5 阻断口径内的问题；口径外的发现写进 BACKLOG 的“部署脚本非阻断项” | 身份守卫与已有 205 项部署回归全部保持通过 |
| L2（可选） | K4：每次只拆一个模块，只移动代码。从 `crates/ak-engine/src/lib.rs` 或 `crates/ak-ffi/src/task.rs` 中挑结构最清晰的一块开始 | 测试数不变；clippy/rustdoc 通过；公开 API 不变（可以用 `cargo doc` 输出或 `pub use` 清单对照） |
| L3（可选） | CR-P3-19：调大 QUIC socket 的收发缓冲 | 按 REVIEW_2026-11-11 §4.5 的 CR-P3-19；用 netem 工作流做前后对照 |

并行建议：L2 与 L3 改的是不同的 crate，可以分给两个执行者，文件互斥；L1 只有出现阻断问题时才需要。

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
- 改了 Flutter 或 Dart，加跑 `flutter analyze` 和 `flutter test`。
- 改了平台代码，要做对应平台的构建。
- 缺工具时写“未执行”，不能写成通过。

## 6. 文档与分支规则（摘自 AGENTS.md）

- 每项状态只写在状态表里，提交列写真实提交号，不写“本提交”。
- run 号和计数只写在本文 §8；STATUS 只写阶段结论、下一步和链接，并链接到本文。
- 本文不要同时出现“已核”与“尚待 / 待补 / 仍待 / 待执行”（`check_repository.py` 会拦截）。
- 执行方只推 `dev/batch-l-<日期>`；复核通过后由复核方快进合入 main，再由执行方删除分支；任何人都不强推。

## 7. 红线

- 不引入 TURN/relay，服务器不经手文件数据（ADR-001）。
- 不部署、不登录、不修改生产服务器，也不改它的防火墙。
- 不生成签名密钥。
- 配对密钥、令牌、用户文件、真实文件名或路径、完整网络诊断都不入库。
- 模拟或同机测试不写成真机验证。

## 8. 证据表（执行方填写）

| 证据 | 提交 / 运行 | 结果与边界 |
|---|---|---|
| dev 分支批次 K 最终提交的 CI | `9e8c36a` run [43898974384](https://github.com/wmqfl861/AK-sending/actions/runs/43898974384) | 第 1 次运行 aarch64-apple-darwin 因 macOS arm64 托管 runner 容量不足从未开始而被取消；第 2 次只重跑该任务并 success，整体 success。其余 11 项常规任务两次均 success，1 项手动深测 skipped |
| main 上批次 K 最终提交的 CI | `9e8c36a` run [43902374141](https://github.com/wmqfl861/AK-sending/actions/runs/43902374141) | 第 1 次运行 aarch64-apple-darwin 因 macOS arm64 托管 runner 容量不足被取消（其余 11 项常规任务 success，1 项手动深测 skipped），整体 failure；第 2 次重跑仅 aarch64 任务 success，整体 success。与 dev 分支 run 43898974384 相同的容量问题 |
| L1 身份守卫与部署回归 | 本地（本提交） | `bash scripts/deploy_linux.sh --self-test` PASS；部署套件 205/205 OK；全量 Python 363/363 OK；`check_repository.py` exit 0；`check_contracts.py` 5/5。无 K24 §5 阻断口径内的新发现问题 |
| L2 K4/CR-P2-08 ak-engine 拆分 | 本地（本提交） | `lib.rs` 10,082→4,866 行，`#[cfg(test)] mod tests`（5,215 行）移至 `tests.rs`（纯移动，零可见性/API 变更，`use super::*` 不变）；测试数不变（78/0/1 前后一致）；fmt/clippy（ak-engine -D warnings）/rustdoc（-D warnings）通过。admission 岛因耦合密度不满足"纯移动"标准而未选（报告见子代理） |
| L3 K5/CR-P3-19 UDP 缓冲 | 本地（本提交） | `ak-net/src/lib.rs` `bind_inner` 前插 `enlarge_udp_socket_buffers`（8 MiB 目标，socket2 SockRef，尽力设+读回记录，失败不阻断 bind）；netem 工作流+脚本在整形前升 `rmem_max/wmem_max` 至 16 MiB（namespace 内+主机级）；`ak-net` 49/0 含新单测；netem 脚本+测试 25/25。**netem A/B 对照须 CI 提供**（本机 Windows 无特权 Linux）|
