# 交接文档：批次 F（2026-11-12，验收工作单执行至 nat-matrix 三修停止点）

> 本文面向下一任执行者/复核者/所有者：本轮做了什么、证据在哪、哪里停了、等谁决定。
> 权威状态表：[REVIEW_2026-11-11.zh-CN.md](REVIEW_2026-11-11.zh-CN.md) 第 7 节（每项入库提交已同步回写）。
> 上一轮交接：[HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md](HANDOFF-REVIEW-FIXES-2026-11.zh-CN.md)。

---

## 0. 一页结论

- **分支** `dev/review-fixes-20261111`，本轮在验收文档（`fc8d01f`，PR #12 已按指示 ff 合并）之上新增 **11 个提交**（`951b44e..28535e0`），全部已推送。
- **批次 F 完成情况**：F0/F9/F8/F3/F4/F1/F2 全部落地；**F5/F7/F10/F11/F6 未开始**（被 nat-matrix 停止点让位，无技术阻塞）。
- **核心成果（CR-P1-05，唯一未关闭 P1）**：双向打洞全五段落地，CI NAT 矩阵 **3/4 案例通过**——含 v4 双向打洞的端到端证据（pcone↔fullcone 经 NAT 反射地址连通）。**唯 symm↔symm 意外成功**（预期失败）：对称 NAT 的模拟保真度问题，非产品缺陷证据；按"同一门槛三修仍败"规则停止，**三选项待所有者定夺（§4）**。
- **CI 现状**：除 nat-matrix 外 11 个 job 三轮全绿；nat-matrix 在所有者对 symm 案表态前会持续红（脚本未改判定逻辑）。
- **停止点**：nat-matrix 三修规则触发（§3）；其余未做项均为"等待指示可立即开工"状态。

---

## 1. 本轮提交清单（时间序）

| 提交 | 项 | 内容一句话 |
|---|---|---|
| `951b44e` | F0 / CR-P2-12 + E4 | STATUS 重写至 11-12 真实状态；状态表 17 处"本提交"→提交号；ROADMAP wave-36 收口；4 处失效代码格式路径修正；`check_repository.py` 新增代码格式 `reports/` 路径存在性检查（负例验证过）；**E4 执行：删除 MANIFEST.sha256**；AGENTS.md 增集成者收尾职责 |
| `0975d91` | F9 / CR-P3-07 | ci.yml 顶层 `permissions: contents: read`；dtolnay（6 处，1.99.1 rev 的 SHA）与 Swatinem（5 处，v2.9.3 SHA）SHA 钉扎；job 更名 `Rust 1.99.1`；双文件头注对齐"同提交升级"；26 条 uses 清单复核。CI 实跑验证 [43152070083](https://github.com/wmqfl861/AK-sending/actions/runs/43152070083) 全绿 |
| `aa28a94` | F8 / CR-P2-04 收尾 | `bench_netem.rs` #[ignore] 深测：64MiB 三档（基线/50ms/100ms，RTT≈2×delay 注明）、netem `limit 100000` 防假丢包、RAII 清理+结束断言、四级无特权跳过全打印原因；底线 8/4/2 MiB/s 为窗口塌缩绊线；首次特权数字如实标"待补" |
| `2ceb37e` | F3 / CR-P2-13 | STUN responder 提取 `stun::serve_stun`（旧 signal-server 原位复用，零行为变化）；`ak-cli stun-server` 子命令（默认 7101）；验收测试=独立 serve_stun 上真实 `query_on` 反射应答 + doctor --stun 真机 smoke；OPERATIONS §0 进程端口表 + 4 步迁移 + systemd 模板；E6 警示更新 |
| `d0668b4` | F4 / CR-P2-10 | 兑换熔断公平份额（窗口满只拒 ≥max(⌊600/活跃源⌋,30)）；连接 256 软帽+512 硬帽（⌈256/活跃源⌉ 下限 2）；IPv6 分层前缀 /56=4×、/48=16×（契约 §11 先行）；四表 IdleTable O(log n) 淘汰（65,536 实测）；9 新测试含三条验收 |
| `69048c3` | （计划外） | keeper 替换配速：`invite revoke` 与常驻 daemon 的连接战争（CI 两次 flake 根因）——替换性断开≠网络抖动，keeper 等 30s 再收复；新测试钉住配速/粘滞分离语义；ak-session 101/101、e2e 3/3 |
| `57658d3` | F1 / CR-P2-11 | 共享模块 `ak-session::pairing::task_net`（1,758 行）：36 重复函数→4 个适配残件；task.rs −1,225、task_cli.rs −1,100；测试净变化 0（1,220 过）；纯移动经规范化字节比对；5 处副本分歧标注 F1-DIVERGENCE。CI [43163891058](https://github.com/wmqfl861/AK-sending/actions/runs/43163891058) 全绿 |
| `de93ac5` | F2 / CR-P1-05 主体 | 双向打洞五段：ak-net `dial_with_socket`（端口等式测试）→ engine `SendSpec::adopt_socket` → task_net 发送端 bind→同 socket STUN→反射进自身 blob→QUIC 收养 + 接收端 QUIC socket try_clone 打洞（150ms/4s/≤8 目标、仅发送、早停）→ FFI `sender_bind` 事件（契约散文登记）+ CLI `--stun` → NAT 矩阵脚本+CI job。workspace 1231/0/8 |
| `72e787b` | F2 矩阵修复① | svc 回程路由（smoke 在 NAT 前跑、原始源地址回包即丢）+ 全命名空间 rp_filter=0（打洞流量来自未交谈地址，严格反过滤=假阴性）+ 失败自诊断 dump_routes |
| `b99a5ef` | F2 矩阵修复② | `local a="$1" b="$WORK/$a"` set -u 陷阱（builtin 参数先展开后赋值）→拆分；全脚本审计仅一处 |
| `28535e0` | F2 停止记录 | 三轮结果与三选项写入状态表，按规则停止 |

---

## 2. nat-matrix 三轮完整记录（停止点的全部证据）

| 轮 | Run | 现象 | 定性与处置 |
|---|---|---|---|
| 1 | [43180910462](https://github.com/wmqfl861/AK-sending/actions/runs/43180910462) | 全案例 "alice cannot reach the services host" | 基建：svc 无回程路由 → `72e787b` |
| 2 | [43183991492](https://github.com/wmqfl861/AK-sending/actions/runs/43183991492) | smoke 通过；`line 351: case_name: unbound variable` | 基建：bash set -u 陷阱 → `b99a5ef` |
| 3 | [43186738509](https://github.com/wmqfl861/AK-sending/actions/runs/43186738509) | **产品 3/4 通过**（见下） | 对称案例模拟保真度问题 → 停止 |

第 3 轮产品结果（每案 8MiB 字节一致断言）：

| 案例 | 结果 | 连接路径 |
|---|---|---|
| pcone↔pcone | ✅ PASS | IPv6 直连（`[2001:77:1::2]`） |
| pcone↔fullcone | ✅ PASS | **v4 NAT 反射（10.10.0.1）——v4 双向打洞端到端证据** |
| v6 双端有状态防火墙 | ✅ PASS | IPv6 直连 |
| symm↔symm | ❌ **意外成功** | 预期失败而实际连通 |

### 2.1 symm 案悬而未决的技术实质

`MASQUERADE --random-fully` 在 CI 内核上未必按"逐目标随机外端口"工作：conntrack 对同一内网 (src,port) 向不同目标的出站可能复用同一映射（等效端口受限锥形）——此时打洞本就应当成功，**是模拟不像对称 NAT，不是产品违规穿透**。但也存在（无法不经抓包排除的）另一种可能：逐目标端口确实随机了、而两端 punch+STUN 的组合真的打穿了——那反而是超预期的好消息。二者只能靠 `tcpdump` 区分。

### 2.2 待所有者三选一（已记录于状态表 CR-P1-05 行）

1. **精确模拟**：symm 案改用显式逐目标端口重映射（iptables/nft 或用户态 NAT），工作量大但保真
2. **降级 informational**（最务实）：symm 案记录不判失败——验收矩阵的"必须成功"三案已全部通过
3. **抓包分析**：CI 加 tcpdump 人工分析该成功是否真实穿透

> 注：选项落定前，每次推送 nat-matrix job 都会红（判定逻辑未动）；这是已知且有意保留的状态，不是回归。

---

## 3. 未完成批次（无技术阻塞，等指示即开工）

按验收文档第 8 节原序，规格都在 [REVIEW_2026-11-12-acceptance.zh-CN.md](REVIEW_2026-11-12-acceptance.zh-CN.md)：

| 项 | 内容 | 前置 |
|---|---|---|
| F5 / E2 | 确认窗口 30s→120s、发送端空闲预算 ≥150s、UI 倒计时、文档同步；验收=「90s 后才同意仍完成」集成测试 | F1（已满足） |
| F7 | CR-P3-06 探测早退（首个确认后 200ms 内返回，ak-net `holepunch::probe` 加钩子）+ `probe_observed` 诊断事件（契约 §8.3）+ GUI 诊断导出 | F2（已满足） |
| F10 / E1 | 删除旧 Dart 服务层（随时可做）；旧 Rust 代码特性隔离 | 隔离须在 F3 之后（已满足） |
| F11 / E3 | 对称 NAT 文案接入 UI（文案已由验收方定稿，见验收文档 §6 E3 行）+ widget 测试 | F2（已满足） |
| F6 | 契约修订轮：ServerInfo（capabilities 0x0008）、R36-P2-04 剩余（服务器"被替换"原因 + FFI/GUI 参与连接锁）、复核 wave-36 N2 | F4（已满足）；都改 protocol-v2，合成一次修订 |
| （低优）F12 / CR-P3-08 | 守护进程事件关联 | 视 E1 而定 |

---

## 4. 验证配方（复核者复跑）

```sh
# Rust 全量（~1,231 通过 / 0 失败 / 8 ignored 基线）
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --no-fail-fast
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
python scripts/check_contracts.py        # 5/5
python scripts/check_repository.py       # passed:true（含新的代码格式路径检查）
# flutter_app/：flutter --no-version-check analyze && flutter --no-version-check test
# flutter_app/packages/ak_ffi_dart/：dart test
# NAT 矩阵（Linux 特权）：sudo bash scripts/nat_matrix_test.sh --skip-build
#   非特权/非 Linux 环境=打印原因跳过（exit 0）
```

抽查点：task_net 端口等式测试（`cargo test -p ak-session --lib task_net`，29 项）；t03 产品路径（11 项）；公平份额三验收（`-p ak-signal --features server-persistence`，183+26+52+26+1）；keeper 配速（`-p ak-session --lib keeper`）；FFI 门铃（`-p ak-ffi --lib`，109）。

---

## 5. 红线遵守与诚信声明

- 未引入 relay/TURN；测试拓扑中的 MASQUERADE/DNAT 是 NAT 模拟件，产品路径始终直连
- 未访问/部署/修改生产服务器；E5/E6/P3-03/P3-05 全部留待所有者
- 未生成任何签名密钥；仓库无密钥/密码/真实路径新增
- 冻结契约改动均先改文档后过 check_contracts（本轮：pairing-flow.md §11、ffi-v2.md §8.1 散文 sender_bind 注记）
- NAT 矩阵在脚本头/测试文档/TEST_PLAN 三处均声明**模拟非真机**；netem 基准首次特权数字标"待补"；symm 案如实记为存疑
- 未改 main、未强推、未合并 PR（PR #12 的 ff 合并是工作单明确授权的唯一例外）

## 6. 给所有者的提醒（C4 前置清单更新）

C4 真机实测（验收文档 §7）的前置在原文三项之上新增一项：**symm 案三选一的落定**（否则矩阵 job 持续红会混淆"C4 前置=矩阵通过"的判定）。其余前置：CR-P1-05 完成（✅，3/4+待决）、GUI 诊断导出（F7，未做）、E6 部署（所有者）。E5 签名密钥、P3-03 域名、P3-05 SSH 同前。
