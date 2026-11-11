# 协调服务与运维边界

本文件的部署设计边界写于 2026-11-03（当时未部署任何服务）；2026-11-05 起所有者已部署一台生产协调服务器（registry 7200 TLS ＋旧 signal 7100/7101，见 [STATUS](STATUS.zh-CN.md)），本文件新增"生产 registry 服务器运维手册"一节承载其 runbook（审核 [REVIEW_2026-11-11](REVIEW_2026-11-11.zh-CN.md) CR-P2-09；线上细节未核实处均带"未核实"标记）。设计边界本身不变：**仅部署设备发现辅助、在线状态、信令、必要的 STUN/穿透协调。禁止数据 relay、文件上传下载、对象存储投递与 CDN 文件分发路径。**
服务可保留最少设备身份索引、短期连接会话及候选；文件清单/路径/内容摘要不入服务数据库。

主要指标：连接请求量、协商延迟、纯直连成功/失败比例及粗粒度原因、鉴权拒绝、过期会话、服务错误与资源占用。
不要把用户文件传输量写入控制面计费接口。性能诊断若需要文件规模，优先本机导出经用户审查的报告。
控制服务仍存在带宽、计算、日志和防滥用成本；取消的是文件中继带宽预算，不是所有服务器费用。
未知客户端输入严格 schema、TTL、限频、并发与总资源上限，避免成为开放扫描器/代理。

既有直连不应持续依赖服务端续租才能发文件；短期网络变化可能需要重新协调，失败就暂停。
服务提供故障状态，不假装网络一定可连。任何安全问题不能通过“临时开启中继”解决。

## 生产 registry 服务器运维手册（CR-P2-09，2026-11-11）

**现状与授权边界**：生产协调服务器＝122.51.121.24（腾讯云 Ubuntu 24.04，[SUPPORT_MATRIX](SUPPORT_MATRIX.zh-CN.md)），2026-11-05 由所有者部署：registry 7200（TLS）＋旧 signal 7100/7101，systemd 常驻（[HANDOFF](HANDOFF.zh-CN.md) 第二十五波；unit 名、二进制与数据路径均未核实——仓库无部署脚本与 unit 文件）。**线上二进制版本未核实**（CR-P2-09 的直接动因）：本轮修复只改代码与文档，未访问服务器、未部署、未改防火墙；以下所有服务器侧步骤由所有者执行。服务器只做协调，绝不承载文件数据（[ADR-001](adr/001-direct-only.zh-CN.md)）。跨设备回归用 [scripts/cross_device_test.sh](../scripts/cross_device_test.sh)（6/6 为历史基线，2026-11-05；该脚本当前用 `--ssh-pass`，按 CR-P3-05 应改密钥登录，勿把密码写进工单）。

### 1. 版本可观测性（本轮已落地的代码部分）

- **构建期注入**：`crates/ak-cli/build.rs` 在编译时把源码修订号写进二进制——`git rev-parse --short HEAD`，工作树相对 HEAD 有已跟踪改动（含暂存）加 `-dirty` 后缀；环境变量 `AK_GIT_SHA` 非空则原样优先（CI 无 git 也可注入）；git 不可用/无 `.git`（源码导出）回退 `unknown`，绝不因取不到版本而构建失败。重打包提示：若发布产物显示 `git unknown`，说明构建机没有 git 或不在仓库内，按 `AK_GIT_SHA=<commit>` 显式注入重建。
- **CLI 查询**：`ak-cli --version`（长格式）输出 `ak-cli 0.1.0 (wire v1, caps 0x0007 [resume|pause-resume|multistream], release build, git <sha>[-dirty])`；顶层与**所有子命令**同答（`ak-cli registry-server --version`、`ak-cli signal-server --version`……），`-V` 短格式仍是裸版本号。格式化函数按注入 sha 纯函数化并有单测（`crates/ak-cli/src/main.rs` 的 `version_line_with`）。
- **服务器启动日志**：`ak-cli registry-server` 启动即初始化 env_logger（默认 Info，`RUST_LOG` 环境变量可覆盖）并打出一行版本日志 `registry-server version: <同上载荷>`；engine 原有的 `v2 registry server listening on tls … (registry …)` 行随之可见（此前 CLI 从未初始化 logger，这行在线上一直不可见）。systemd 默认 stdout/stderr 进 journal，见 §3 验证步骤。
- **核实线上版本的操作**（所有者执行）：`/opt/ak-sending/ak-cli registry-server --version`（路径未核实，以现网为准）；或 `journalctl -u <unit> | grep 'registry-server version:'`。把输出（含 git sha）记录进部署工单，与预期提交对齐即完成"线上=某个提交"的核实。
- **客户端远程查询**：当前**不可行**——v2 `Welcome` 帧不带版本，详见 §7 设计提案（未实施）。

### 2. 构建（干净检出、release）

```sh
git clone <repo> && cd AK-sending && git checkout <commit>   # 或 git fetch && git checkout
cargo build --locked --release -p ak-cli                     # registry-server 即 ak-cli 子命令，server-persistence 由 ak-cli 依赖默认开启
target/release/ak-cli registry-server --version              # 记录输出进工单；git <sha> 必须等于 checkout 的提交
```

- 服务器为 Linux amd64：需在 Linux 主机（或交叉工具链）构建；仓库现有发布脚本只覆盖 Windows（`scripts/package_windows.ps1`），**线上二进制由哪台机构建未核实**——建议固定为干净 Linux 检出构建，避免 debug build（版本行会如实显示 `debug build`）。
- 工作树不干净时版本行带 `-dirty`，部署工单应解释来源或重检出；`unknown` 一律视为构建流程缺陷，不得部署。

### 3. 部署（所有者执行；现网 unit 名/路径/运行用户未核实，以下为模板）

已知坐标（均出自仓库记载，未在服务器上核实）：二进制默认路径 `/opt/ak-sending/ak-cli`（cross_device_test.sh `--remote-cli` 默认值）；SSH 用户示例 `ubuntu`；生产 registry pin（公钥 hex，非机密）`8f012017f57cfef1a3e74e90fa1e13a5f6ffbed3a9ddeecb98f819e002f94f36`（同脚本注释，未核实是否仍为现网值）。

1. **备份**（§5）：`registry.key` 与 `registry.db`（位置未核实；默认为运行用户 `$HOME/.ak-sending/` 下，即 `registry-server` 首次启动自动生成的布局——若 unit 显式传过 `--id-dir`/`--db-path` 以其为准）。
2. 上传：`scp target/release/ak-cli <user>@122.51.121.24:/opt/ak-sending/ak-cli.new`（chmod 755，属主与现网一致）。
3. 预检：`/opt/ak-sending/ak-cli.new registry-server --version` 记录进工单。
4. 切换：`systemctl stop <unit>` → `mv ak-cli ak-cli.old-<日期>` → `mv ak-cli.new ak-cli` → `systemctl start <unit>`。旧 signal 7100/7101 是否同一 unit/进程未核实（HANDOFF 记载同机常驻）；若需同时升级，一并重启并记录。
5. 验证：`journalctl -u <unit> -n 50` 应见 `registry-server version: …` 与 `v2 registry server listening on tls 0.0.0.0:7200 (registry …)`；客户端侧跑 `scripts/cross_device_test.sh`（期望 6/6）或 GUI 配对一次冒烟。
6. unit 文件模板（**草稿，非现存文件**；加固项按所有者安全基线取舍）：

```ini
[Unit]
Description=AK-sending v2 pairing registry (coordination only, ADR-001)
After=network-online.target

[Service]
User=akreg                      # 专用非 root 账号；现网实际用户未核实
ExecStart=/opt/ak-sending/ak-cli registry-server --port 7200 --id-dir /opt/ak-sending/data
Environment=RUST_LOG=info
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/opt/ak-sending/data
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

   注意：模板用 `--id-dir /opt/ak-sending/data` 求路径确定性；若现网实际用默认 `$HOME/.ak-sending`，切换前先把 `registry.key`/`registry.db` 迁到新路径（或保持原路径并在 unit 中显式指回），**不要**在换路径时丢库丢身份（§5）。

### 4. 回滚（含 schema 兼容告诫）

- **未跨 schema 的二进制回滚**：`systemctl stop` → 恢复 `ak-cli.old-<日期>` → `start`。数据格式向前兼容问题不存在于同一 schema 内。
- **跨 schema 迁移的回滚（关键告诫）**：server_v2 的 SQLite 库在打开时自动单向迁移——v1→v2（wave-28：`invite_codes.owner_endpoint`/`max_redeems`、`pairing_transactions.*_endpoint`）→v3（R36-P2-05：`claim_request_body`/`claim_response`），均加列、原子、可重入（`crates/ak-signal/src/server_v2/store.rs`，契约见 [storage-schema](contracts/storage-schema.md)）。**旧二进制打开新库会以 `SchemaVersion` 错误拒绝启动**（fail-safe，不会写坏库），因此：
  1. 迁移不可逆：回滚跨迁移的二进制＝恢复 §3 步骤 1 的迁移前 `registry.db` 备份＋旧二进制；
  2. **备份点之后产生的 claim/配对事务随备份恢复而丢失**（受影响用户需重新配对/兑换），回滚决策要权衡这两点；
  3. 判定线上 schema：`sqlite3 registry.db 'PRAGMA user_version;'`（0=空库，1/2/3=已到各级）；`ak-cli --version` 的 git sha 对应当时 `SCHEMA_VERSION`（本手册时点=3）。
- 每次部署前必须备份 `registry.db`（§3 步骤 1），这是跨迁移回滚唯一可行的路径。

### 5. TLS 身份密钥（registry.key）：备份与轮换

- **它是什么**：`registry.key`＝注册中心的 Ed25519 设备身份对（`registry-server` 首启自动生成于 `<id-dir>/registry.key`），派生 TLS 自签证书；其公钥 hex 即 **RegistryId＝客户端钉扎值**（`--registry-fp` / 客户端 `registry.pin` / GUI 构建注入 `AK_REGISTRY_ID`，见 [SECURITY](SECURITY.zh-CN.md) 第 4 节）。它**不是** TLS 会话密钥而是长期身份：换文件＝换身份。
- **在哪**：服务器上实际位置未核实；按默认布局＝运行用户 `$HOME/.ak-sending/registry.key`（同目录 `registry.db`）。
- **备份**：与 `registry.db` 一起离线备份（加密介质/密码管理器附件，chmod 600），每轮部署（§3 步骤 1）与每次轮换前各一份。**私钥内容绝不进文档、工单、git**；RegistryId（公钥 hex）可公开。
- **丢失后果**：无法重建身份，只能生成新 key＝全体已钉扎客户端失联重配（等同轮换）。
- **泄露后果与处置**：攻击者可冒充注册中心接管配对协调面（数据面仍有端到端设备指纹校验兜底，但协调面信任被接管）——立即轮换（下条）并公告。
- **轮换步骤**（代价高，见下）：停服 → 备份旧 `registry.key`/`registry.db` → 生成新 key（删走旧文件首启即生成，或预生成后放置）→ 启动并记录新 RegistryId → 更新客户端钉扎（CLI `--registry-fp`；GUI 需以新 `AK_REGISTRY_ID` dart-define 重新构建分发，[PUBLISHING](PUBLISHING.zh-CN.md)）→ 观察 journal 与客户端连接率。
- **当前轮换＝全量客户端更新**：没有"当前＋下一把"双钉扎与域名地址（客户端钉扎与地址均编译期注入，换 IP 或换身份都要全员发新版）——这是 CR-P3-03 的提案内容（地址域名化＋双 pin＋构建脚本统一注入）；在其落地前，轮换生产 registry 身份应视为大变更，需所有者排期客户端发版窗口。

### 6. 防火墙姿态（所有者执行；agent 未访问服务器）

- 现状（2026-11-05 部署形态，[STATUS](STATUS.zh-CN.md)）：公网开放 TCP 7200（registry TLS）、TCP 7100（旧明文 signal）、UDP 7101（STUN）。
- 建议（CR-P2-09＋CR-P1-02 联动）：
  1. **TCP 7200 必须保留**（GUI/CLI 主线路径唯一协调入口）。
  2. **关闭公网 TCP 7100**：GUI 不经旧 signal（审核证据 `flutter_app/lib/services/ffi_simple_transfer_service.dart:186-189`）；旧 signal 的全局连接上限（CR-P1-03 第 6 点）继续暴露在公网无收益。若仍有旧 CLI 用户依赖 7100，先评估再关；关闭由所有者在云安全组/防火墙执行并记录日期。
  3. **UDP 7101 的去留绑定 CR-P1-02**：v1 主线补 STUN 反射候选（批次 C）落地后保留 UDP 7101 供公网穿透；未落地期间它只服务旧 signal 探测，可随 TCP 7100 一并评估收敛。
  4. SSH：收敛来源白名单、禁密码登录（CR-P3-05），密钥登录。

### 7. 客户端可查询服务器版本——设计提案（未实施；v2 契约冻结）

- **现状**：v2 `Welcome`（tag `0x02`，契约 [protocol-v2](contracts/protocol-v2.md) §3）载荷恰为 `session_ttl_secs:u32 + capabilities:u32`（8 字节，`scripts/check_contracts.py` 机械钉死）；`capabilities` 已定义位仅 `0x1` 邀请码管理、`0x2` 配对事务、`0x4` v2 会话（`crates/ak-signal/src/server_v2/mod.rs`），**没有任何字段承载构建版本**；客户端现完全忽略 `capabilities`（`client_v2/conn.rs` 只校验 ttl>0）。
- **为何不塞现有字段**：把 git sha 编进 `capabilities` 剩余位会破坏"能力位"语义且不可读，属契约语义变更；给 `Welcome` 追加尾随节会命中"尾随字节全部拒绝"规则，**旧客户端会断连**——不可作为默认行为。
- **提案（走契约修订流程：集成者核准＋ADR-004 同步，本轮不改冻结契约）**：
  - 新增两个消息：`ServerInfoRequest`（客户端→服务端，无载荷，幂等廉价）与 `ServerInfo { revision:u16长度+≤32B UTF-8 }`（服务端→客户端，内容如 `0.1.0+4739f4d`，即 §1 版本行同一来源）；两者预算入契约 §4 表。
  - **门控＝现有 `Welcome.capabilities` 新能力位 `0x0008`（可查询服务器版本）**：升级后的服务端在 Welcome 置位；客户端只在看到该位后才发 `ServerInfoRequest`。旧服务端不置位→新客户端不问；旧客户端本就忽略 capabilities→不发。**零破坏、纯增量，不触碰任何既有帧形状**。
  - 服务端实现点：`server_v2::CAPABILITIES` 增位；engine 在 Welcome 后按需应答。客户端实现点：`client_v2` 事件面暴露 revision（连接信息/诊断），GUI "关于/连接详情" 与 `ak-cli doctor` 可展示。
  - 安全注意：revision 是构建指纹，不含路径/主机名等敏感信息；应答限频并入现有每连接帧预算即可。
- **实施顺序**：服务端先行（旧客户端无感知）→ 客户端随后按位启用；落地后本手册 §1 "客户端远程查询"由提案改为操作说明。

## 协调服务器运维（signal-server，CLI 原型现状）

本节为 `ak-cli signal-server` 当前实现的运维事实；上文为部署设计边界，两者一致：只有协调，没有文件路由。

### 启动与端口

- `ak-cli signal-server [--tcp-port 7100] [--stun-port 7101]`：单进程、双套接字——TCP 信令与 UDP STUN 响应器；Ctrl-C 优雅关停。
- 防火墙需放行 TCP 信令端口与 UDP STUN 端口各一个。`probe --server IP:7100` 隐含向 server 端口 +1 发 STUN 查询，规划端口时保持 TCP N / UDP N+1 的相邻约定即可。

### 限额默认（Limits，均为协调面资源上限，与任何文件配额无关）

- 会话 TTL 120 秒；每设备最多 4 个并发会话；Hello 新鲜度窗口 10 秒（防重放）。
- 每连接帧速率 32 帧/秒，惰性令牌桶、突发上限 64 帧，按需补充无后台任务。
- 未知客户端输入按封闭 schema 校验：类型字节仅 1..=8，候选数量、名称长度、声明帧长均有硬上限，截断、尾随字节、超限一律拒绝。

### 无文件路由的验证方式（三层）

- 消息集封闭测试：ak-signal 测试 `closed_message_set_is_bounded` 穷举 CtrlMessage 全部 8 种变体，断言每帧不超过硬上限、类型字节在 1..=8 内；新增携带 blob 的变体会先破坏穷举 match 的编译。
- 依赖扫描：`scripts/check_repository.py` 每次运行扫描 Cargo.lock，封禁 relay/turn/iroh/webrtc/str0m/datachannel 类依赖进入依赖图。
- 运行时旁证：ak-net 测试 `stats_reflect_real_loopback_transfer` 用 `DirectChannel::stats()`（ChannelStats：tx/rx 字节、帧数、丢包计数）对真实环回传输对账——帧数跨端一致、字节差 ≤10%，说明传输字节只流经直连 QUIC 通道；配合 ak-itests `t04_signal_irrelevant_to_data`（数据面零信令依赖）与 `t07_coordination_down`（信令关停前后传输照常完成）构成"协调面可有可无、从不处于数据路径"的证明对（ADR-001 不变量）。

### SameEgress 提示

- 行为：会话两端的控制面 TCP peer IP 均已知且相同时，服务器在 SessionOpened 后向双方各发一条 SameEgress 信号；任一端 IP 未知则绝不发送（不猜）。CLI probe 端只显示一次"same egress detected: prefer LAN candidates first"，服务器重复下发也只公告一次。
- 语义与误报：这只是候选排序建议（先试局域网候选）。同一出口 IP 不保证二层可达——同一企业 NAT 或 CGNAT 后的不同子网、两台经公网服务器协调但实际同网的设备都可能命中或错过该提示；LAN 候选探测失败后正常回退到其余候选，不影响传输正确性。

### 日志边界（不记录用户数据）

- 服务器实现不可见文件名、清单与字节（消息集封闭即保证），日志只记录会话与设备级事件（例如"session 共享出口，提示 LAN 优先"一类的 info 行，含会话短 id）。
- 文件清单、路径与内容摘要不进入信令消息，也不进入服务端任何存储；传输量、文件规模等用户数据不写入控制面日志或计费接口（与上文指标约束一致）。

## 持续集成

`.github/workflows/ci.yml` 已入库备用；仓库尚未推送，该工作流从未在 GitHub Actions 上实际运行过（配置与本地门槛一致，命令均为本地已验证的形态）。推送后行为：

- 触发：push 与 PR 到 `main`、`dev/**` 分支；另支持 workflow_dispatch 手动整跑。
- `checks` job（ubuntu-latest，Python 3.12）：运行 `scripts/check_repository.py`（结构、Cargo.lock 禁依赖扫描、文档链接检查）与 `python -m unittest discover -s scripts/tests`（校验器自身的 12 个单元测试）。
- `rust` job（矩阵：ubuntu-latest + windows-latest；stable 工具链含 clippy/rustfmt；Swatinem/rust-cache 缓存 cargo）：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo test -p ak-store --features sqlite`（bundled C SQLite，验证 MSVC/GCC 双平台可编译）、`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`。
- `release-deep-tests` job（仅 workflow_dispatch 手动触发，不进常规 push/PR 路径）：release-only `#[ignore]` 清单——ak-engine 5 GiB 稀疏文件传输、ak-itests 20,000/65,537 文件多文件传输与 512 MiB 吞吐、256 MiB 分块扫描基准。耗时分钟级（常规测试套件为秒级），故单独手动触发。

`deny.toml` 是与 Python 扫描互补的 cargo-deny 配置：[bans] 按精确名拒绝 iroh/iroh-net/iroh-relay/turn/rusturn/turn-rs/webrtc/str0m/libdatachannel/coturn（multiple-versions 仅警告），[licenses] 限定常见宽松许可证名单（MIT/Apache-2.0/BSD/ISC/Unicode/Zlib/BSL-1.0/CC0），[advisories] 拉取 RustSec 漏洞库、漏洞默认拒绝而 unmaintained/yanked 仅警告。本地开发机未安装 cargo-deny，该文件未在本地执行过；需要时在 CI 增加可选 job（如 EmbarkStudios/cargo-deny-action@v2），或开发者执行 `cargo install cargo-deny --locked` 后运行 `cargo deny check`。

本地等效命令（与 [CONTRIBUTING.md](../CONTRIBUTING.md) 的门槛一致，细节以该文件为准）：

```sh
python scripts/check_repository.py
python -m unittest discover -s scripts/tests
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p ak-store --features sqlite
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
# 手动深测（分钟级，默认 #[ignore]）：
cargo test --release -p ak-engine -- --ignored --nocapture
cargo test --release -p ak-itests -- --ignored --nocapture
```
