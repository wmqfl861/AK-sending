# 验收与无中继证明计划

当前均为计划，执行证据另见 reports；不能由此表推断已测试。

| 编号 | 场景 | 通过标准 |
|---|---|---|
| T01 | 关闭路由外网，协调服务停机 | 已配对设备仍能本地传输，支持离线配对流程 |
| T02 | 本地与公网候选同时可达 | 优先本地，服务器不承载文件 |
| T03 | 外网 IPv4/IPv6 可直连 | 双端认证通过，原件摘要一致 |
| T04 | 两端不直连但协调服务可用 | 文件进度停止，无 TURN/relay/上传回退 |
| T05 | 注入 relay/未知路径候选 | 拒绝，保留任务，可操作错误 |
| T06 | 身份替换/未加密/未认证路径 | 发送文件前拒绝 |
| T07 | 连接已建立后停协调服务 | 网络不变时既有直连继续，不能推断后续重绑定无需协调 |
| T08 | 路径变更为未知/代理 | 暂停正文，重新验证，失败不发 |
| T09 | 接收目录撤权/存储满 | 不误报完成，可恢复 |
| T10 | 断网/双端进程重启 | 从验证持久 checkpoint 恢复，避免无故全量重传 |
| T11 | 文件名冲突/.. /链接竞态 | 不越界、不静默覆盖 |
| T12 | >4 GiB/100 GB 单文件 | u64 计算无溢出，内存有界，最终校验相同 |
| T13 | 十万小文件/空目录 | 清单分页、结构可核验、界面可取消 |
| T14 | 相册原件/HEVC/HDR/Live Photo | 选定源资源逐字节保留，不偷偷转码 |
| T15 | 蜂窝网络/锁屏/后台终止 | 遵守授权，检查点可恢复，不夸大后台能力 |
| T16 | 五端互通 | 20 个有向组合＋同平台，报告实际支持版本 |

100 GB/十万文件是测试样本，不是产品上限；手机按实际容量安排大文件样本。

## 自动化覆盖映射（截至 2026-11-04，环回 L2 形态）

本节如实记录截至 2026-11-04（含第八轮并行开发补充、第十三轮安卓平台波与第十四/十五轮）各 T 编号落入自动化测试的位置，执行证据见 [dev-validation-2026-11-02](../reports/dev-validation-2026-11-02.txt) 与 [开发总结](../reports/DEV_SUMMARY.zh-CN.md)。2026-11-03 复测：`cargo test --workspace` 417 项通过、0 失败、5 项 ignored；`cargo test -p ak-store --features sqlite` 26 项通过；clippy `--workspace --all-targets -- -D warnings` 与 sqlite 特性双配置零警告。第十二轮后 workspace 复测 430 项通过（见验证记录"第十二轮验证"节）。第十五轮后（2026-11-04）实测：`cargo test --workspace --no-fail-fast` 449 项通过、7 项 ignored，另有 1 项既有时序敏感失败——`ak-net stats_reflect_real_loopback_transfer`（第四轮测试，统计快照时帧计数 68 对 66 的竞争；单独复跑 3/3 通过，属 flaky 非功能回归，未修复如实记录）。所有自动化测试均为进程内环回形态：单机、QUIC over UDP 环回/本机网卡、真实文件系统（跨设备里程碑的模拟器组合为实测执行记录，非自动化用例，见 T16 行）。上表中的通过标准凡未列入下表"已自动化"的部分仍是计划。跨 crate 场景测试统一位于 `crates/ak-itests`（每个测试文件对应若干 T 编号，现含 T02/T03/T04/T05/T06/T07/T08/T09/T10/T11/T13 的环回形态、两个信息性吞吐/块尺寸基准、产品全流程 e2e 四幕与确定性随机性质套件），下表按 crate 内单测/集成测现状标注。

workspace 默认排除的 7 项 `#[ignore]` release 深测（均已于 release 形态执行通过，见验证记录）：`ak-engine five_gib_sparse_file_transfers_with_u64_offsets`（5 GiB 稀疏，首跑 73.68s，回归 95.96s/84.3s/111.49s）；`ak-itests bench_throughput`（512 MiB×2）；`ak-itests bench_chunk_sweep`（256 MiB×8）；`ak-itests t13_many_files` 的 20,000 文件规模变体（328s，空闲机刷新 263.7s）与 65,537 文件边界变体（257 页清单，完成）；第十五轮新增 `ak-itests property_random` 的 25 种子随机树传输变体与 10 种子随机中断续传变体（release 全量 94.8s，零缺陷）。

第十三轮（2026-11-04，安卓移植波）：`flutter test` 66/66 覆盖安卓平台适配（服务/页面层——tier-0 so 加载决策、身份目录异步门控、Download 输出链与接收页预填；L1 形态，不涉真实跨设备传输）；`apk-debug.apk` 161MB 构建成功、双 ABI `libak_ffi.so` 在包内属构建事实，不构成任何 T 编号通过；T16 安卓端装机与互通验证待模拟器系统镜像重装完成后执行（见下表 T16 行）。

第十四轮（2026-11-04，配对链路波）：配对簿管理全链自动化——ak-ffi `ak_peer_list`/`ak_peer_add`/`ak_peer_remove` 三导出 23 项 C ABI 测试（含 CLI 写簿与 API 写簿互通、upsert 保 TOFU 配对时间、坏簿/坏指纹/控制字符名拒绝）、ak_ffi_dart 85/85（peers 9 项，含 CLI 写簿往返）、Flutter 配对页 84/84（配对页 10 项；切换点测试为真实 facade→C ABI→DLL 往返）；落点见下表 T01/T06 行（簿管理侧与准入延续），均为环回/单机形态。

第十五轮（2026-11-04）：`ak-cli daemon` 常驻接收 83/83（NDJSON 事件流单元测试＋真实双任务冒烟；失败任务不断进程）——CLI 侧常驻形态，移动后台生命周期（T15 场景本身）仍无代码；确定性随机性质套件（25 种子随机树传输＋10 种子随机中断续传不变量，release 94.8s 零缺陷；快速回归种子 1/2/3 入默认 `cargo test`）为 T10/T11 既有断言的随机化加强；安卓存储通道（Kotlin SAF 写闭环探针）属通道落地＋构建级验证（APK 179MB 重建），模拟器端 SAF 选择器走查未执行。

| 编号 | 自动化状态 | 测试位置 | 层级 |
|---|---|---|---|
| T01 | 部分 | 数据面不依赖协调面：`ak-engine` `loopback_directory_transfer_is_byte_identical` 等全部环回传输测试自始至终不经信令服务器；离线配对资料落盘：`ak-identity` `peerbook_save_load_roundtrip_sanitizes_names`；配对簿管理全链（第十四轮：ak-ffi peer 三导出 23 项 C ABI＋Dart peers 9 项＋Flutter 配对页 10 项，CLI 写簿与 API 写簿互通、真 DLL 往返） | L1＋L2（环回） |
| T02 | 部分 | 本地优先：`ak-net` `classify_*` 系列、`ak-discovery` `rank_orders_lan_then_public_with_v6_first`；服务器不承载文件：`ak-signal` `closed_message_set_is_bounded`、`ak-wire` `message_type_constants_form_the_closed_set`（封闭消息集、无 blob 字段）。本地/公网双候选并存的端到端选择未见自动化 | L1 |
| T03 | 部分 | IPv6 环回形态已自动化：`ak-itests/tests/t03_ipv6_loopback.rs`——L1 `classify(::1)` 与 `fe80::/10` 判定为本地直连；L2 双端各绑 `[::1]:0` 的 QUIC+TLS 握手互钉且两侧 path=LocalDirect；L2 引擎双绑定 `[::1]:0` 全程传输字节一致，且接收端实际绑定地址确为 ::1（证明未回退到 IPv4）。[dev-validation-2026-11-02](../reports/dev-validation-2026-11-02.txt) 的双进程 probe（信令＋STUN＋打洞＋钉扎握手）为单机环回＋本机网卡形态；跨网段/真实 NAT/公网 IPv6 直连未执行 | L1＋L2（环回） |
| T04 | 部分 | `ak-core` `connection_loss_waits_without_relay_fallback`、`relay_and_unknown_are_never_accepted`、`no_candidates_means_no_connection`；`ak-engine` `mid_transfer_disconnect_keeps_checkpoint_and_resumes`（数据路径中断→进度停止、无回退）。[ADR-002](adr/002-protocol-stack-and-order.zh-CN.md) D6 指明后者为 T04 的自动化形态。数据面零信令依赖证明对：`ak-itests/tests/t04_signal_irrelevant_to_data.rs`（先以死端口连接拒绝证明协调面不存在，随后在从未启动任何信令服务的机器上引擎传输照常完成、字节一致）与 T07 场景构成证明对——协调面可有可无，从未处于数据路径（ADR-001 不变量）。T04 原场景"两端不直连但协调服务可用"需真实网络故障注入，仍未自动化 | L1＋L2（环回） |
| T05 | 已自动化 | `ak-core` `relay_and_unknown_are_never_accepted`、`changed_path_is_rejected_on_reassessment`、`incomplete_local_probe_prevents_premature_public_selection` | L1 |
| T06 | 已自动化 | `ak-engine` `wrong_pin_fails_closed_before_any_transfer`、`receiver_authorize_false_rejects_the_sender`；`ak-identity` `cert_with_wrong_pin_is_key_mismatch`、`malformed_and_hostile_certificates_are_rejected_without_panicking`；`ak-signal` `bad_signature_hello_is_disconnected`；身份替换攻击面三场景（第四轮深化，`ak-itests/tests/t06_identity_swap.rs`）：mid-transfer 端口被他人密钥抢占→钉扎拒绝且零落盘、正确指纹对第三方证书→握手期拒绝、配对簿准入端到端（簿内传输/簿外拒绝） | L1＋L2（环回） |
| T07 | 部分 | 环回形态已自动化（关停前后两变体）：`ak-itests/tests/t07_coordination_down.rs`——真实进程内信令服务器上完成完整会话时序（Hello/Register/ConnectRequest/SessionOpened/候选与打洞时序），随后关停信令服务器；"传输前关停"与"传输中关停"两变体下直连传输均照常完成、字节一致，传输中变体断言关停后仍有新 chunk 事件；与 T04 零信令依赖测试构成证明对。环回边界如实声明：单机无真实 NAT/公网协调服务，证明的是"数据面不依赖协调面存活"这一架构不变量，"不能推断后续重绑定无需协调"的限定仍成立 | L2（环回） |
| T08 | 部分 | `ak-core` `changed_path_is_rejected_on_reassessment`、`pause_resume_rechecks_connection`；可行形态已自动化（第四轮，`ak-itests/tests/t08_reconnect_recheck.rs`）：端口变更→旧地址有界拒绝→同身份新地址重钉扎→同 job_id 续传完成（QUIC 连接迁移不在范围，测试内已注明）。真实路径切换（网络变更）注入未自动化 | L1＋L2（环回） |
| T09 | 部分 | `ak-core` `bytes_received_is_not_completion`、`success_requires_verification_then_save`；`ak-storage` `whole_digest_mismatch_keeps_part_and_blocks_final`、`corrupted_part_prefix_truncates_and_retransmits`（不误报完成、可恢复）。存储满预检决策 `ak-engine::check_headroom` 已单测（余量未知放行、恰好放行、不足携真实数字拒绝）。目录撤权/满盘注入未自动化；Windows 上 `available_bytes` 返回 None，预检在真机 Windows 上不触发，空间不足以真实写错误暴露 | L1 |
| T10 | 部分 | `ak-engine` `mid_transfer_disconnect_keeps_checkpoint_and_resumes`、`resume_skips_verified_prefix_without_resending`；`ak-store` `save_load_roundtrip_with_peer_and_files`；`ak-storage` `dropped_sink_resumes_from_verified_prefix`（跨 sink 实例续传）；多流模式中断→part/checkpoint 存活→二次多流续传一致、多流＋多根组合（第四轮，`ak-itests/tests/t10_multistream_resume.rs`）；随机中断续传不变量随机化加强（第十五轮 `ak-itests/tests/property_random.rs` 10 种子：leg-1 失败关闭、中断仅数据面 0 最终文件/≥1 part 存活、同 job_id 重传逐字节一致——进程内 abort 形态，非真实断网注入）。真实双进程重启未自动化 | L1＋L2（环回） |
| T11 | 已自动化 | `ak-core` `rejects_traversal_absolute_and_separator_attacks`、`rejects_windows_unfriendly_components`、`rejects_overlong_paths`；`ak-storage` `hand_built_escaping_entries_are_rejected`、`symlink_component_inside_save_path_is_rejected`、`source_tree_containing_symlink_is_rejected`、`conflict_fail_errors_and_rename_generates_free_name`。跨 crate 场景测试位于 `crates/ak-itests/tests/t11_names_and_paths.rs`（环回：Unicode/空格/CRLF/全字节二进制/20 层深嵌套传输逐字节一致，Windows 保留名源无法在本机存在、场景内注释跳过）；极端名边界（第六轮，`t11_extreme_names.rs`）：248 字节满长名/CJK/emoji/`\\?\` 超长路径往返一致、249 字节 part 后缀边界（`.akpart-<hash16>` 回退修复后走完成路径）、尾随点/空格 18 形态校验层拒绝 | L1＋L2（环回） |
| T12 | 已自动化 | `ak-core` `handles_more_than_four_gibibytes`、`maximum_u64_does_not_overflow`、`exact_multiple_and_last_partial`；`ak-engine` `five_gib_sparse_file_transfers_with_u64_offsets`（`#[ignore]`、仅 release，2026-11-02 已执行通过，耗时 73.68s）。100 GB 样本待实测 | L1＋L2（环回） |
| T13 | 部分 | `ak-core` `manifest_pages_are_bounded_and_ordered`、`empty_manifest_has_one_empty_page`、`directory_entries_carry_no_digests_and_do_not_count_bytes`；`ak-wire` `manifest_page_round_trip_*` 系列、`rejects_entry_count_over_limit`。环回缩样见 `crates/ak-itests/tests/t13_many_files.rs`：2,000 文件、4,097 条目边界（第二轮缺陷回归）与 20,000 文件 release（328s，第六轮空闲机刷新 263.7s）；ResumeRequest 多帧分页（第四轮）后 100,000 条目分片单元验证（2 片 65,535＋34,465 全量按序到达）、65,537 文件 release 边界实测完成（257 页清单）。十万文件规模与端到端取消未自动化（带内 Pause/Resume 已于第二轮实现；通过标准中的界面取消依赖未开始的 UI 层） | L1＋L2（环回） |
| T14 | 部分 | 通用字节保真：`ak-engine` `loopback_directory_transfer_is_byte_identical`、`ak-storage` `write_all_chunks_then_finish_commits_identical_file`。相册原件/HEVC/HDR/Live Photo 及跨系统元数据保留未测（无平台适配层） | L2（环回，仅通用字节） |
| T15 | 未覆盖 | 移动平台生命周期仍无代码（蜂窝/锁屏/后台终止场景未测）；CLI 侧常驻接收形态已有 `ak-cli daemon`（第十五轮，83/83＋真实双任务冒烟：失败任务不断进程、NDJSON 事件流）——非移动后台场景主张 | — |
| T16 | 部分 | **Android→Windows 组合已在模拟器形态验证（CLI 路线，逐字节一致）**：2026-11-04 里程碑——Android 端 `ak-cli`（x86_64-linux-android 交叉编译，8,904,736B）发送 → Windows 宿主 receiver（经 `10.0.2.2` 模拟器 NAT 环回别名），job `afe43780` 双端 complete 且同 job id、`cmp` 逐字节一致、Android 端 TOFU 记录宿主指纹（证据：验证记录"跨设备里程碑"节，提交 `b4fb427`）。已执行 1 个有向组合（模拟器形态、CLI 路线）；其余组合未执行——Windows↔Android 反向、GUI 路线（模拟器 System UI ANR 受阻，CLI 旁路已证）、物理双机、macOS/Linux/iOS 均未；Windows 本机环回（CLI＋Flutter Windows GUI 单机）为既有基础形态 | L2（模拟器跨网络栈；非物理双机） |

明确声明：截至 2026-11-04，**没有任何 L3/L4 验证被执行**——无物理真机、无跨网段/真实 NAT、无服务器端抓包。跨设备互通已执行且仅执行 1 个组合：Android 模拟器→Windows 宿主（CLI 路线，经模拟器 NAT `10.0.2.2`，逐字节一致；模拟器与宿主同机，非物理双机）；GUI 路线跨设备未执行（模拟器 System UI ANR 受阻）。dev-validation 中的双进程 probe 与 T03/T07 场景测试同为单机环回＋本机网卡形态；环回吞吐基准（单流 97.9 / 多流 103.3 MiB/s）是引擎自身效率对照，不代表真实网卡吞吐。"部分"表示场景仅局部或间接自动化，不等于场景通过。

## 证明服务器不传文件

在客户端和所有控制服务入口同时记录受控实验。准备随机合成文件，不用真实用户隐私文件。
比较 1 MiB、1 GiB 及长任务的控制流量；控制流量允许随连接时间/重试变化，不能简单要求绝对恒定。
检查每种服务消息 schema；禁止任意 blob。不能仅以在密文中搜索原文件字节来证明无文件流量。
先保留信令但阻断两端数据路径，确认进度停住；再恢复直连，确认任务续传。启动、重新握手、切网阶段也要抓包，避免“先中继后直连”。
核对服务部署无 TURN/文件上传入口；依赖、环境代理和重连逻辑审计。
报告仅覆盖已测矩阵，不承诺识别所有操作系统外部透明代理。

## 层级区分

L0：文档/仓库静态检查；L1：领域模型单测；L2：真实传输集成；L3：网络故障注入；L4：五端真机与发布。
本次任何 L0 成功不能替代 L1–L4。模型中人为填写的身份/路径观察只用于测试策略决策。
