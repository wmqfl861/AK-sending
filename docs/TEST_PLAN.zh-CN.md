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

## 自动化覆盖映射（截至 2026-11-03，环回 L2 形态）

本节如实记录截至 2026-11-03（含第三轮并行开发补充）各 T 编号落入自动化测试的位置，执行证据见 [dev-validation-2026-11-02](../reports/dev-validation-2026-11-02.txt)（第三轮后 workspace 308 项测试通过、clippy 零警告、5 GiB 稀疏文件 release 回归 84.3s）。所有自动化测试均为进程内环回形态：单机、QUIC over UDP 环回/本机网卡、真实文件系统。上表中的通过标准凡未列入下表"已自动化"的部分仍是计划。跨 crate 场景测试统一位于 `crates/ak-itests`（每个测试文件对应若干 T 编号，现含 T02/T03/T04/T05/T07/T09/T10/T11/T13 的环回形态与吞吐基准），下表按 crate 内单测/集成测现状标注。

| 编号 | 自动化状态 | 测试位置 | 层级 |
|---|---|---|---|
| T01 | 部分 | 数据面不依赖协调面：`ak-engine` `loopback_directory_transfer_is_byte_identical` 等全部环回传输测试自始至终不经信令服务器；离线配对资料落盘：`ak-identity` `peerbook_save_load_roundtrip_sanitizes_names` | L1＋L2（环回） |
| T02 | 部分 | 本地优先：`ak-net` `classify_*` 系列、`ak-discovery` `rank_orders_lan_then_public_with_v6_first`；服务器不承载文件：`ak-signal` `closed_message_set_is_bounded`、`ak-wire` `message_type_constants_form_the_closed_set`（封闭消息集、无 blob 字段）。本地/公网双候选并存的端到端选择未见自动化 | L1 |
| T03 | 部分 | IPv6 环回形态已自动化：`ak-itests/tests/t03_ipv6_loopback.rs`——L1 `classify(::1)` 与 `fe80::/10` 判定为本地直连；L2 双端各绑 `[::1]:0` 的 QUIC+TLS 握手互钉且两侧 path=LocalDirect；L2 引擎双绑定 `[::1]:0` 全程传输字节一致，且接收端实际绑定地址确为 ::1（证明未回退到 IPv4）。[dev-validation-2026-11-02](../reports/dev-validation-2026-11-02.txt) 的双进程 probe（信令＋STUN＋打洞＋钉扎握手）为单机环回＋本机网卡形态；跨网段/真实 NAT/公网 IPv6 直连未执行 | L1＋L2（环回） |
| T04 | 部分 | `ak-core` `connection_loss_waits_without_relay_fallback`、`relay_and_unknown_are_never_accepted`、`no_candidates_means_no_connection`；`ak-engine` `mid_transfer_disconnect_keeps_checkpoint_and_resumes`（数据路径中断→进度停止、无回退）。[ADR-002](adr/002-protocol-stack-and-order.zh-CN.md) D6 指明后者为 T04 的自动化形态。数据面零信令依赖证明对：`ak-itests/tests/t04_signal_irrelevant_to_data.rs`（先以死端口连接拒绝证明协调面不存在，随后在从未启动任何信令服务的机器上引擎传输照常完成、字节一致）与 T07 场景构成证明对——协调面可有可无，从未处于数据路径（ADR-001 不变量）。T04 原场景"两端不直连但协调服务可用"需真实网络故障注入，仍未自动化 | L1＋L2（环回） |
| T05 | 已自动化 | `ak-core` `relay_and_unknown_are_never_accepted`、`changed_path_is_rejected_on_reassessment`、`incomplete_local_probe_prevents_premature_public_selection` | L1 |
| T06 | 已自动化 | `ak-engine` `wrong_pin_fails_closed_before_any_transfer`、`receiver_authorize_false_rejects_the_sender`；`ak-identity` `cert_with_wrong_pin_is_key_mismatch`、`malformed_and_hostile_certificates_are_rejected_without_panicking`；`ak-signal` `bad_signature_hello_is_disconnected` | L1＋L2（环回） |
| T07 | 部分 | 环回形态已自动化（关停前后两变体）：`ak-itests/tests/t07_coordination_down.rs`——真实进程内信令服务器上完成完整会话时序（Hello/Register/ConnectRequest/SessionOpened/候选与打洞时序），随后关停信令服务器；"传输前关停"与"传输中关停"两变体下直连传输均照常完成、字节一致，传输中变体断言关停后仍有新 chunk 事件；与 T04 零信令依赖测试构成证明对。环回边界如实声明：单机无真实 NAT/公网协调服务，证明的是"数据面不依赖协调面存活"这一架构不变量，"不能推断后续重绑定无需协调"的限定仍成立 | L2（环回） |
| T08 | 部分 | `ak-core` `changed_path_is_rejected_on_reassessment`、`pause_resume_rechecks_connection`；真实路径切换（网络变更）注入未自动化 | L1 |
| T09 | 部分 | `ak-core` `bytes_received_is_not_completion`、`success_requires_verification_then_save`；`ak-storage` `whole_digest_mismatch_keeps_part_and_blocks_final`、`corrupted_part_prefix_truncates_and_retransmits`（不误报完成、可恢复）。存储满预检决策 `ak-engine::check_headroom` 已单测（余量未知放行、恰好放行、不足携真实数字拒绝）。目录撤权/满盘注入未自动化；Windows 上 `available_bytes` 返回 None，预检在真机 Windows 上不触发，空间不足以真实写错误暴露 | L1 |
| T10 | 部分 | `ak-engine` `mid_transfer_disconnect_keeps_checkpoint_and_resumes`、`resume_skips_verified_prefix_without_resending`；`ak-store` `save_load_roundtrip_with_peer_and_files`；`ak-storage` `dropped_sink_resumes_from_verified_prefix`（跨 sink 实例续传）。真实双进程重启未自动化 | L1＋L2（环回） |
| T11 | 已自动化 | `ak-core` `rejects_traversal_absolute_and_separator_attacks`、`rejects_windows_unfriendly_components`、`rejects_overlong_paths`；`ak-storage` `hand_built_escaping_entries_are_rejected`、`symlink_component_inside_save_path_is_rejected`、`source_tree_containing_symlink_is_rejected`、`conflict_fail_errors_and_rename_generates_free_name`。跨 crate 场景测试位于 `crates/ak-itests/tests/t11_names_and_paths.rs`（环回：Unicode/空格/CRLF/全字节二进制/20 层深嵌套传输逐字节一致，Windows 保留名源无法在本机存在、场景内注释跳过） | L1＋L2（环回） |
| T12 | 已自动化 | `ak-core` `handles_more_than_four_gibibytes`、`maximum_u64_does_not_overflow`、`exact_multiple_and_last_partial`；`ak-engine` `five_gib_sparse_file_transfers_with_u64_offsets`（`#[ignore]`、仅 release，2026-11-02 已执行通过，耗时 73.68s）。100 GB 样本待实测 | L1＋L2（环回） |
| T13 | 部分 | `ak-core` `manifest_pages_are_bounded_and_ordered`、`empty_manifest_has_one_empty_page`、`directory_entries_carry_no_digests_and_do_not_count_bytes`；`ak-wire` `manifest_page_round_trip_*` 系列、`rejects_entry_count_over_limit`。环回缩样见 `crates/ak-itests/tests/t13_many_files.rs`：2,000 文件、4,097 条目边界（第二轮缺陷回归）与 20,000 文件 release（328s）。十万文件规模与端到端取消未自动化（带内 Pause/Resume 已于第二轮实现；通过标准中的界面取消依赖未开始的 UI 层） | L1＋L2（环回） |
| T14 | 部分 | 通用字节保真：`ak-engine` `loopback_directory_transfer_is_byte_identical`、`ak-storage` `write_all_chunks_then_finish_commits_identical_file`。相册原件/HEVC/HDR/Live Photo 及跨系统元数据保留未测（无平台适配层） | L2（环回，仅通用字节） |
| T15 | 未覆盖 | 无平台生命周期代码 | — |
| T16 | 未覆盖 | 仅 Windows 本机环回（`ak-cli` 命令行原型）；五端矩阵未开始 | — |

明确声明：截至 2026-11-03，**没有任何 L3/L4 验证被执行**——无真机、无跨网段/真实 NAT、无服务器端抓包；dev-validation 中的双进程 probe 与 T03/T07 场景测试同为单机环回＋本机网卡形态；环回吞吐基准（单流 97.9 / 多流 103.3 MiB/s）是引擎自身效率对照，不代表真实网卡吞吐。"部分"表示场景仅局部或间接自动化，不等于场景通过。

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
