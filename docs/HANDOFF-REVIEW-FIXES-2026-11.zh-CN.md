# 交接文档：2026-11-11/12 审核修复轮（REVIEW_2026-11-11 工作单执行完毕）

> 本文是本轮修复的**完整交接记录**，面向下一任执行者/复核者：所有改动、证据、验证方法、遗留事项与待决点都在这里。
> 权威状态表：[REVIEW_2026-11-11.zh-CN.md](REVIEW_2026-11-11.zh-CN.md) 第 7 节（每项修复在入库提交中同步更新）；本文提供它的展开与背景。

---

## 0. 一页结论

- **分支**：`dev/review-fixes-20261111`（自 `origin/review/claude-20261111` 检出，32 个提交含本交接，全部已推送；验收复核更正）
- **最终验收**：分支顶端最终 run 为 [43121806432](https://github.com/wmqfl861/AK-sending/actions/runs/43121806432) 全绿（含门铃测试补交；此前 D2 主提交的 run 43093828012 亦全绿）（Rust ubuntu/windows、flutter、android、audit、deny、checks、portability×3；release-deep-tests 为手动任务按设计 skipped）
- **完成度**（验收复核更正后的口径）：审核 18 项编号中 **11 项已完成**、4 项部分完成（P1-02 余 CR-P1-05 双向打洞与 C4 真机、P2-02 余命名/密钥、P2-04 余 netem/E2、P2-07 余本 F0 收尾）、3 项待所有者（P2-08/P3-03/P3-05）；另有 R36-P2-04 剩余与 wave-36 N2 两项遗留未开始
- **下一步阻塞点**：① C4 跨 NAT 真机实测（需所有者设备，§8）② 第 8 节 E1-E7 所有者决定（§7）
- **红线遵守情况**：未引入 relay/TURN（服务器不经手文件数据，STUN 地址发现符合 ADR-001）；未访问/部署/修改生产服务器 122.51.121.24 及防火墙；未生成或入库任何签名密钥/SSH 凭证；冻结契约改动均先改文档后跑 `scripts/check_contracts.py`（本轮改动：pairing-flow.md §11、ffi-v2.md §8.1/§8.6/§1）；全程无"模拟器/同机双进程=真机验证"的表述；未合并 PR、未改 main、未强推

---

## 1. 提交清单（时间序，33 个）

### 批次 A——恢复门禁（CR-P1-01 等）
| 提交 | 内容 |
|---|---|
| `3b3eb64` | style(CR-P1-01)：cargo fmt 全仓（24 文件 229 处） |
| `8750830` | fix(CR-P1-01)：修复全部 rustdoc intra-doc 链接错误（29 文件 64 处：未解析链接改 `crate::` 全路径或跨 crate 路径；私有项改普通代码文字；8 处冗余链接目标删除；本机 34→0，clean rebuild 验证） |
| `172a634` | fix(CR-P1-01)：ci.yml `macos-13`（已退役）→ `macos-15`/`macos-15-intel`；删"尚未推送从未执行"过时注释；AGENTS.md/CONTRIBUTING.md 补 fmt+doc 两道门槛 |
| `b911a30` | fix(CR-P3-01)：环境敏感测试三处（IPv6 测试探测 `[::1]` 失败打印原因跳过；RFC 5737 样例避开本机网段；QUIC 帧计数改 max(2,10%) 容差） |
| `47492c3` | fix(CR-P2-06 改法1)：`rust-toolchain.toml` 固定 1.99.1+clippy/rustfmt；ci.yml 三处 `@stable`→`@1.99.1` 同步（两处必须同提交移动） |
| `05f16e9` | fix(CR-P2-01)：rusqlite 0.32.1→0.41.1（libsqlite3-sys 0.30.1→0.39.1，RUSTSEC-2026-0459 修复线；零 API 破损；v1→v2→v3 迁移与断点续迁测试全过）+ CI 新增 cargo audit job |
| `6012947` | docs(CR-P1-01)：验收 CI run 链接回填状态表 |

### 批次 B——安全与可用性
| 提交 | 内容 |
|---|---|
| `7791d77` | fix(CR-P2-04)：ak-net QUIC TransportConfig 常量表接入双侧（keep-alive 10s / max-idle 60s / 流与连接接收窗及发送窗 8MiB，BDP 注释）；Debug 渲染锁定测试 + tokio 虚拟时间空闲测试（32s/65s 存活、65s 后仍双向传数据、非空转对照） |
| `1f5c592` | fix(CR-P1-03)：按来源限额——来源键 IPv4/32、IPv6/64 归一（v4-mapped 展开）；每来源并发 8/握手中 4，超限 accept 即断不 spawn；TLS 期限 30→10s；兑换四层预算（每连接 10 + 每来源 30 + 每身份 30 + 全局熔断 600，共享表跨重连、TTL/容量上限回收）；ConnSlotGuard 扩展（handshake_done 一次性释放）；契约 §11 **先改后跑** check_contracts 5/5；ak-signal 275 测试含 6 个新验收测试（24 socket TCP 洪泛下他源 <1s 完成 TLS+Hello、6 身份兑换洪丰不误伤他源、超限 socket 不 spawn、跨重连预算保持等；127.0.0.1/127.0.0.2 环回别名模拟不同来源） |
| `d23aa9f` | fix(CR-P1-04, CR-P2-02, CR-P2-03)：manifest 三备份属性 + 双 xml 规则文件（cloud-backup 与 device-transfer 双段全域排除，aapt2 dump 实证打包）；release 签名从不在库 key.properties 读取、缺失/字段不全 GradleException 快速失败（实测 6s 失败、无 debug 回退；未生成任何密钥）；.gitignore 补齐；targetSdk 显式 36（实测本机 Flutter 3.47.9 的解析值）+ BACKLOG 升 37 前清单（ACCESS_LOCAL_NETWORK 权限流程、拒绝降级公网候选非中继、API 37 模拟器测试） |
| `e204234` | fix(CR-P2-05 D1)：自适应轮询（事件 50ms 活动/1s 空闲纯函数节奏表；自重排 Timer + 代数计数器防分叉，含竞态回归测试）+ WidgetsBindingObserver（后台活动 1s 心跳/空闲暂停、回前台同步 drain 立即回快节奏）；+8 测试 |
| `955e7e4` | fix(tests)：dual_policy 测试同源文件并发重写竞态（第二个 `fs::write` 先截断，撞上第一个 CLI 传输中读数 → 发送端正当报 source changed；批次 A 代码在 CI 上同样失败过即 run 43013196150，确证先于批次 B 存在） |

### 批次 E——文档与 CI 扩展（与 C 并行）
| 提交 | 内容 |
|---|---|
| `a36a784` | fix(CR-P2-07)：新建 `docs/STATUS.zh-CN.md` 唯一状态源（README/HANDOFF 只链接不复述）；清单 8 处 + 同源 2 处过时句全部修正（grep 验收 0 匹配）；ARCHITECTURE 模块表"现状"列逐格对照代码重写（2026-11-11）；MANIFEST.sha256 头注"已失效待 E4" |
| `4739f4d` | docs(CR-P2-07)：17 个已关闭轮次报告移入 `reports/archive/`，41 处引用跨 14 文件同步（含 ROADMAP/DEV_SUMMARY 的相对路径形式），check_repository 复绿 |
| `24412a5` | fix(CR-P2-09)：ak-cli build.rs 注入 git sha（env 覆盖→git→unknown，不阻塞构建）；全部子命令 `--version` 统一应答（新增 3 测试）；registry-server 初始化 env_logger + 启动版本行；线上版本查询**不动冻结契约**，ServerInfo 提案（Welcome capabilities 0x0008 门控）写入 OPERATIONS §7 待契约修订轮；OPERATIONS 运维手册（构建/部署/回滚——v3 迁移单向、跨迁移回滚需库备份/TLS 密钥备份轮换/防火墙建议，未核实处显式标注） |
| `99b8498` | fix(CR-P2-06 改法2-5, CR-P3-04)：`.fvmrc` 固定 Flutter 3.47.9（三处同步注明）；CI **flutter** job（analyze + flutter test + dart test——嵌套纯 Dart 包 161 测试 flutter test 收集不到，job 内先构建 ak-ffi release DLL 供真 DLL 测试）；CI **android** job（NDK 28.2 钉扎、build_android_ffi.sh 双 ABI 交叉编译 + 13 符号校验、provenance 回显源 SHA/Cargo.lock sha256/rustc/双 ABI nm dump、debug APK 上传 artifact；新第三方 Action 全部 SHA 钉扎）；jniLibs 两个 .so **停止跟踪**（`git ls-files '*.so'`=0，.gitignore 带再生说明，本地再生 + debug APK 实证包含双 ABI）；package_windows.ps1 刷新失败即整体失败 + `-AllowStaleAndroid` 唯一显式复用开关（清单记录 sha256/修订或 mtime+WARNING）+ `-ValidateParameters`（失败/复用路径实测 exit 1/0）；CI **deny** job（cargo deny check advisories bans licenses；deny.toml 修至 cargo-deny 0.20.2 可运行，重复版本维持 warn） |
| `def0240` / `b6810c5` | fix(ci)：新 job 首跑暴露的环境差异三 + 二处（脚本 `$USERPROFILE` 在 Linux 未定义→`HOMEISH` 可移植 + SDK 默认认 ANDROID_HOME；NDK clang `.cmd` 后缀仅 Windows→按 OSTYPE 选择；嵌套包需自身 `dart pub get`；DLL 探测测试硬编码 `ak_ffi.dll`/`.exe`→`defaultAkDllFileName()` 平台化；cancel 测试 15s 耐心上限→60s） |

### 批次 C——跨 NAT 直连（CR-P1-02 三棒 + Dart 传参）
| 提交 | 内容 |
|---|---|
| `b464e53` | fix(CR-P1-02 C1)：接收端双栈绑定（`bind_dual_stack` 优先，Unsupported 回退 v4 并记 `receiver_bind` 事件/CLI 日志行；FFI+CLI 双路径）；echo responder 双栈；候选枚举纯函数化 `filter_and_order_candidates`（v4 原序去重→127.0.0.1→全局单播 v6：排除 fe80/ULA/组播/文档段、每 /64 取一、上限 3→::1）；blob 无需改契约（典型双族集 ≈320B<512B）；t03 新增真实产品路径测试（registry-server + task 子进程，::1 候选传输字节一致、`--advertise ::1` 连接报告） |
| `2792884` | fix(CR-P1-02 C2)：STUN 反射候选——socket 生命周期重构：`bind_dual_stack_udp` →（可选）**同 socket** STUN 查询（2s 预算两发；v4 服务器经 `::ffff:` 映射形）→ `bind_with_socket`，全程一个 OS 句柄（**端口等式测试**证明：反射端口==QUIC 绑定端口）；反射候选插 v4 网段块后、回环前（去重/映射归一/未指定丢弃）；全链路 fail-open（无 STUN/失败/超时不阻塞，2s 封顶有界日志）；FFI 契约 §8.1 **先行**新增 `stun_addr` 键；CLI `--stun` 默认生产 `122.51.121.24:7101`、空串退出；FFI 产品级测试 2 个 + t03 真实子进程测试 3 个（真实 RFC5389 responder/定点反射/静默服务器 fail-open） |
| `5bdd899` | fix(CR-P1-02)：Dart 传参——`AkTaskStartSpec.stunAddr` + `AK_STUN_ADDR`（环境变量>dart-define>生产默认，空串禁用；校验与 Rust 侧语义一致） |
| `64bc896` | fix(CR-P1-02 C3)：双向同时探测——发送端首拨前一个双栈 socket 对**全部候选**打探测轮（150ms 间隔/1s 窗口）；观测头=静态序中首个被回显确认者，否则对端探测源地址（映射归一/未指定丢弃）；观测头插队首拨（2s 专用预算，静态槽保持 10s×3 轮不变）；接收端 echo responder 记录最近观测源供 `close_notify_peer` 领先拨号（限频日志）；严格增量回退（探测无果=原顺序原预算）；**顺带修复既有 bug**：Windows 迟到 ICMP port-unreachable（WSAECONNRESET）曾静默杀死 echo responder（新 itest 3/6 复现定位，非 flake）——两副本容忍与 ak-net probe 相同的瞬态错误集；6 单测 + 2 个产品路径 itest（假回显候选回退到真实接收端并字节一致/观测头为真实通告候选） |
| `f47632c` | fix(tests)：配对测试 harness 窗口 1.5s/5s→10s/30s（慢 runner 上 11 个进程内 TLS 服务器挤 4 核，配对流转录跑不完自身 deadline；等待上限非断言，产品默认 90s/60s 不变） |

### P3-02 与测试稳定性（批次 E 收尾）
| 提交 | 内容 |
|---|---|
| `44e4cbd` | fix(CR-P3-02 根名闸门)：ak-core 纯函数 `windows_incompatible`（`<>"|?*`；22 经典保留名 + CONIN$/CONOUT$/CLOCK$/上标 COM¹-LPT³ 精确 U+00B9/B2/B3；结尾点/空格；`validate_rel_path` 保持平台中立有测试钉死）；FFI `register_consent` 在 pending 锁内、写入 `incoming_request` 前预检 Offer 根名（Windows 上不合规→`offer_refused` 事件+Now(false)，零写入零横幅；非 Windows 如常）；CLI 同闸门 |
| `80eceb2` | fix(CR-P3-02 清单级闸门)：ak-engine `receive_frozen_body` 在 check_headroom 前、任何 sink/part 文件创建前对全部条目逐组件预检（`#[cfg(windows)]`；违反→对端 ProtocolUnsupported+路径原因、checkpoint 记 `failed_name_rejected`、本地 InvalidInput 可读错误；纯决策函数跨平台可测） |
| `e7bb907` / `e716a61` | fix(tests)：`send_admission_cli` 配对三次 CI 失败的根因链——`job_done.peer` 可空导致丢配→FIFO 兜底在乱序完成时错配→**最终方案**：串行/并行语义改由原始事件**行序**推导（前缀 starts−dones，配对启发式全部删除，构造上确定性），完成计数用 `wait_count` |
| `1d5a70c` | fix(tests)：cancel 测试终态竞态（快速 runner 上 16MiB 环回传输抢在取消生效前完成，任务终态 Completed 而测试死等 Cancelled；取消语义本体由黑洞 peer 测试钉死不受影响） |

### 批次 D 收尾（CR-P2-05 全三件）
| 提交 | 内容 |
|---|---|
| `ffd3e74` | docs(CR-P2-05 D3)：`docs/design/android-foreground-service.zh-CN.md`（163 行，仅设计不实现；Android 14 类型+权限强制、Android 15 dataSync 6h/24h 配额与 onTimeout、类型选型表（选定 dataSync 并如实分析 connectedDevice/UIDT）、FGS 后台启动限制以接受/发送点击为前台触发、Doze 交互、手写 Kotlin Service+窄 MethodChannel 方案、测试计划含免真等 6h 的 compat 开关；Android 事实全部对照 developer.android.com 核实并引用，两处待核实显式标注） |
| `206d410` | fix(CR-P2-05 D2)：原生事件推送——新增可选门铃导出 `ak_task_set_event_notify(engine, runtime_id, notify, ctx)`（契约 §1+新 §8.6；**零载荷**，poll 仍为权威来源，未注册=与旧版逐位一致纯轮询）；slot 锁内即时触发（unregister 等待在飞完成→Dart close 无竞态）；teardown 先清门铃；无 panic 路径；Dart `NativeCallable.listener`（pubspec ^3.1.0）+ 容错导出查找（旧 DLL 降级纯轮询）；250ms 活跃兜底 tick 接住错过的门铃；Rust 68/68（3 新）、dart 166/166（含**真 DLL e2e**）、flutter 186/186；双 ABI nm + host DLL 导出核验=冻结 8+新 1，脚本抽查清单同步扩展 |
| （本提交） | fix(CR-P2-05)：补 D2 遗漏 stage 的 196 行门铃测试套件（flutter 186/186 复验） |

---

## 2. 各编号验证指引（复核方法）

| 编号 | 快速验证 |
|---|---|
| P1-01 | `cargo fmt --all -- --check`；`RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps`；读 ci.yml（无 macos-13/过时注释）；AGENTS/CONTRIBUTING 含两门槛 |
| P1-02 | `cargo test --locked -p ak-itests --test t03_ipv6_loopback`（9 测试：IPv6/STUN/探测三棒产品路径）；`cargo test --locked -p ak-ffi --lib`（socket 生命周期/门铃）；CLI 跑 `ak-cli task code-recv` 观测 `[task] stun reflexive`/`probe round observed` 行；**跨 NAT 实效只能 C4 真机证明（§8）** |
| P1-03 | `cargo test --locked -p ak-signal --features server-persistence`（275 测试含 6 个验收测试）；契约 §11 预算表；`python scripts/check_contracts.py` |
| P1-04 | `aapt2 dump xmltree --file AndroidManifest.xml <apk>`（三属性）；res/xml/ 两文件 |
| P2-01 | `cargo audit`（exit 0）；`grep rusqlite Cargo.toml`（0.41） |
| P2-02 | 无 key.properties 跑 `flutter build apk --release` → 6s 内 GradleException（文案自述） |
| P2-03 | `grep targetSdk flutter_app/android/app/build.gradle.kts`（=36）；BACKLOG 升 37 条目 |
| P2-04 | `cargo test --locked -p ak-net`（42 测试含两个传输配置/空闲测试） |
| P2-05 | `flutter test`（186）、`cd flutter_app/packages/ak_ffi_dart && dart test`（166）；`grep doorbell flutter_app/lib/services/simple_transfer_service.dart` |
| P2-06 | `.fvmrc`；`git ls-files '*.so'`（空）；CI flutter/android/deny 三 job（run 43093828012） |
| P2-07 | grep 验收短语 0 匹配（见 `a36a784` 提交说明清单）；`python scripts/check_repository.py` |
| P2-09 | `cargo run -p ak-cli -- registry-server --version`；读 OPERATIONS 手册 |
| P3-01 | 在无 IPv6 容器跑 `cargo test --workspace`（跳过测试打印原因） |
| P3-02 | `cargo test --locked -p ak-core --lib windows`（6）；`-p ak-ffi --lib windows`；`-p ak-engine --lib windows_unsavable` |
| P3-04 | CI deny job；本地 `cargo deny check advisories bans licenses` |

---

## 3. 过程中发现并修复的**既有**缺陷（独立于审核清单）

1. **dual_policy 测试同文件并发重写竞态**（批次 A 代码即失败，run 43013196150）：`955e7e4`
2. **Windows echo responder 被 WSAECONNRESET 静默杀死**（迟到 ICMP port-unreachable；产品代码缺陷，新 itest 3/6 复现定位）：`64bc896`
3. **build_android_ffi.sh 仅 Windows 可移植**（`$USERPROFILE` 未定义、clang `.cmd` 后缀）：`def0240`/`b6810c5`
4. **测试 harness 紧窗口**（1.5s/5s 慢 runner 必炸）：`f47632c`
5. **`job_done.peer` 可空 + 事件配对歧义**（三次不同歧义，最终以行序推导根治）：`e7bb907`/`e716a61`
6. **cancel 测试终态竞态**（快速 runner 传输先完成）：`1d5a70c`
7. **DLL 探测测试平台名硬编码**（flutter 首次在 Linux 跑暴露）：`b6810c5`

---

## 4. CI 证据链（全部 15 个 run）

| Run | 结论 | 触发 | 说明 |
|---|---|---|---|
| [43011364868](https://github.com/wmqfl861/AK-sending/actions/runs/43011364868) | ✅ | 批次 A（05f16e9） | 首个全绿 |
| [43013196150](https://github.com/wmqfl861/AK-sending/actions/runs/43013196150) | ❌ | 纯文档提交 | dual_policy 既有竞态（§3.1），证明先于批次 B |
| [43029286253](https://github.com/wmqfl861/AK-sending/actions/runs/43029286253) | ❌ | 批次 B 首推 | 同上 + cross_code peer:null 丢配（§3.5） |
| [43031622309](https://github.com/wmqfl861/AK-sending/actions/runs/43031622309) | ✅ | 955e7e4 后 | 批次 B 验收 |
| [43046223828](https://github.com/wmqfl861/AK-sending/actions/runs/43046223828) | ❌ | 新 job 首跑 | §3.3 脚本可移植性 ×1、嵌套包 pub get、cancel 15s |
| [43048026569](https://github.com/wmqfl861/AK-sending/actions/runs/43048026569) | ❌ | def0240 后 | §3.3 clang 后缀、§3.7 DLL 测试平台名 |
| [43053077982](https://github.com/wmqfl861/AK-sending/actions/runs/43053077982) | ✅ | b6810c5 后 | 新 job 首次全绿 |
| [43057578762](https://github.com/wmqfl861/AK-sending/actions/runs/43057578762) | ❌ | C2+C3 间 | §3.4 harness 窗口 |
| [43060344462](https://github.com/wmqfl861/AK-sending/actions/runs/43060344462) | ✅ | f47632c 后 | C2 验收 |
| [43069873077](https://github.com/wmqfl861/AK-sending/actions/runs/43069873077) | ❌ | C3 | §3.5 第一歧义 |
| [43073116875](https://github.com/wmqfl861/AK-sending/actions/runs/43073116875) | ✅ | e7bb907 后 | C3 验收 |
| [43077330860](https://github.com/wmqfl861/AK-sending/actions/runs/43077330860) | ❌ | P3-02 | §3.5 第二歧义（FIFO 错配） |
| [43080424593](https://github.com/wmqfl861/AK-sending/actions/runs/43080424593) | ✅ | e716a61 + D3 | 行序断言验收 |
| [43093828012](https://github.com/wmqfl861/AK-sending/actions/runs/43093828012) | ✅ | D2 206d410 | **最终全绿** |
| （最新） | 待查 | 门铃测试补交 | `gh run list -b dev/review-fixes-20261111 --limit 1` |

失败 run 全部有根因定位与对应提交（§3），无"重试掩盖"。

---

## 5. 状态表终态（第 7 节摘要）

- **已完成**：P1-01、P1-03、P1-04、P2-01、P2-03、P2-05（D1+D2+D3 全）、P2-06、P2-07（除 MANIFEST 删除）、P3-01、P3-02、P3-04
- **代码全完成，余项待所有者**：P1-02（余 C4 真机）、P2-02（余应用名统一与真密钥 apksigner 验证）、P2-04（余 netem 基准与 E2 窗口时长）、P2-09（余部署/防火墙执行与 ServerInfo 契约修订）
- **待所有者**：P2-08（=E1 旧模型处置）、P3-03（域名+双钉扎，需买域名）、P3-05（SSH 改密钥）

---

## 6. 已知遗留（本轮未做，均有明确原因）

1. **R36-P2-04 剩余**：FFI/GUI 未参与身份连接锁（GUI 与 CLI 并存时同身份互踢风险）、服务器对同身份新连接不附"被替换"原因——审核批次表（第 5 节）未排程的 wave-36 遗留，保持原样待排期
2. **C3 的四个技术提案**（记录于状态表 P1-02 行）：holepunch::probe 早退钩子（省 1s 窗口）、SendSpec socket 收养（发送侧映射复用，需 ak-net API 新增）、ak-net 瞬态错误助手 pub 化（消除两副本重复）、`probe_observed` FFI 事件（需契约 §8.3 修订）
3. **netem 吞吐基准**（P2-04 收尾项，release 深测 + Linux tc netem 50/100ms）
4. **ServerInfo 契约修订**（P2-09 收尾项，提案在 OPERATIONS §7）
5. **对称 NAT 提示文案**（CR-P1-02 改法 4，建议与 E3 一并决策）

---

## 7. E1-E7 所有者决定请求（附本轮事实依据）

| 编号 | 决定 | 本轮事实依据 / 建议 |
|---|---|---|
| **E1** | 旧模型（邀请码多码调度、旧明文 signal、旧 Dart 层）是否冻结移除 | 旧 v1 signal 的公网端口问题即 P2-09 防火墙项；代码未动（P2-08），决定后可按审核原方案执行 |
| **E2** | 接收确认窗口 30s→120s（或"稍后处理"队列） | QUIC 层已就绪（60s 空闲+10s 保活），改常量即生效；**建议 120s** |
| **E3** | UPnP/NAT-PMP/PCP 是否启用；对称 NAT 文案 | 三棒已落地，建议 **C4 实测后再定**；两端对称 NAT 基本无解需 UI 明示 |
| **E4** | 删除 MANIFEST.sha256 | 已头注失效；**建议删除** |
| **E5** | Android 签名密钥生成与保管 | 管道就绪（key.properties 缺失即失败），密钥由所有者离线生成；CI 如需签名用 repo secret |
| **E6** | 生产服务器：关公网 TCP 7100、部署新版、防火墙 | 运维手册已备（OPERATIONS）；本轮线上二进制版本仍未核实（未访问服务器） |
| **E7** | LICENSE | 仓库公开无 LICENSE，默认全权保留 |

## 8. C4 真机实测协议（需要所有者设备）

- **设备组合（至少两组）**：① 家宽电脑 ↔ 手机移动数据；② 两个不同家宽
- **每组步骤**：
  1. 两端装同分支客户端（Android：CI android job 的 debug APK artifact，或本地 `flutter build apk --debug`；Windows：`scripts/package_windows.ps1` 或本地构建）
  2. 一端 GUI/CLI 生成任务码，另一端输入并发送 ≥100MB 文件
  3. 记录：成功与否、建连耗时、两端 IPv4/IPv6 地址与 NAT 类型（`ak-cli doctor` 辅助）
  4. 任一端抓包（wireshark/tcpdump，过滤两台设备间地址）证明**数据只在对端之间流动**（L3 无中继证据）
  5. 失败组同样记录（候选列表、`stun reflexive`/`probe round observed` 日志行）
- **红线**：模拟器、同机双进程不算真机验证
- **产出**：结果矩阵回填 `docs/TEST_PLAN.zh-CN.md` 对应条目（IPv4/IPv6、NAT 类型、成败、耗时、抓包结论）

---

## 9. 本地门槛基线（复核者复跑命令）

```sh
python scripts/check_repository.py
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --no-fail-fast
cargo test --locked -p ak-store --features sqlite
cargo test --locked -p ak-signal --features server-persistence
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
cargo audit && cargo deny check advisories bans licenses
python scripts/check_contracts.py
# flutter_app/ 下：flutter --no-version-check analyze && flutter --no-version-check test
# flutter_app/packages/ak_ffi_dart/ 下：dart test
```

全部绿基线：workspace 69 个测试目标 0 失败（~1200 项测试）、flutter 186、dart 166、契约 5/5、audit exit 0。
