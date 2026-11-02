# M0-02 开发后审核与验收

日期：2026-11-02。状态：**REVIEWED（仅最小认证直连适配器范围）**。
[先行计划](../docs/modules/M0-02_PLAN.zh-CN.md) · [机器证据索引](M0-02_VALIDATION.json) · [PR #3](https://github.com/wmqfl861/AK-sending/pull/3)。整个 M0 尚未完成，当前不是文件传输客户端。

## 1. 提交顺序与被审范围

| 阶段 | 提交 |
|---|---|
| M0-01 基线 | `f727edcaf7d164a7940c7d57e4d317b63b5acd54` |
| 先行计划，先于实现 | `25252ab3fe833e148bf132a4c80516cadcc920b9` |
| 首个实现 | `7f1bb8e459a145fe2ee2ade9cabe1b33b328b5ab` |
| 原样回收 Cargo 生成锁文件和 rustfmt | `17c21410d5dcc48a24c4f7ffff77e4eab2c07ea5` |
| 独立审查取消清理修复 | `76d818effdcb38307ce2fb8a84a523cc6847af26` |
| Android 预取修复 | `bfc29a200f13ce03a163c4f5537018ee4fbded83` |
| Windows 日志编码修复 | `d28402186207cd1ed47749615984a730cb32ebde` |
| 最终受测且独立复审：LF/CRLF 测试修正 | `f0daac8aa464e645483920adc4f51188891a17bb` |

最终代码树 `e8f3e7a4777e9864d9b6aea9eeab6a5f290ba7bc`。审核/交接提交只改文档、证据索引和文件摘要，不把后续文档 HEAD 伪称为被审代码 SHA。工作分支叠加在 M0-01 分支，PR #1/#2/#3 本次均未合并；未部署、采购或强推。

实现范围：新增 `crates/ak-transport/` 的身份、端点、私有连接、接收会话与测试；更新 workspace/锁文件、架构阶段、依赖准入和验证脚本、三桌面与移动编译工作流。`ak-core` 运行时零第三方依赖且行为未修改。

## 2. 计划逐项验收

| 计划 | 实现与证据 |
|---|---|
| A1 原核心不回退 | 原核心 46 单测、3 集成、1 文档编译失败用例全部保留；新增后总计 81 Rust/24 Python，每桌面均通过 |
| A2 真实认证双端 | 两个原生 UDP 端点实际完成 TLS 1.3 与固定探测；断言真实 local/remote socket 地址及证书身份 |
| A3 身份拒绝 | 错误客户端/服务端固定证书、缺失客户端证书、过期服务端证书、错误名称/ALPN、接收会话身份替换均有负例；私有连接仅从实际握手产生 |
| A4 配置边界 | TLS 1.3、完整叶证书固定、固定 ALPN，关闭 0-RTT/恢复/tickets/迁移/datagram/单向流；中继/未知/非法候选拒绝，无代理或自定义 socket 注入 |
| A5 断线和超时 | 实际连接关闭、预期地址不符、内部超时、外部取消均拒绝继续使用；地址快照不符通过单元故障注入测试，不冒充真实 NAT 迁移实验 |
| A6 接收许可 | 等待本机确认；任务/对端/控制器实例由核心绑定，新增连接与授权代次绑定；跨会话、旧请求、旧许可、暂停/撤销/取消、过期和重连负例通过；验证/保存恢复不开正文旁路 |
| A7 API 私有边界 | 三个外部编译失败用例阻止构造连接、伪造 ReceiveGrant、取得裸 quinn 句柄；10 项公开接口集成测试 |
| A8 有界探测 | 仅固定 8 字节 PING/PONG，错误/超长报文拒绝；流、缓冲、等待均有界；无用户正文、任意流或磁盘写接口 |
| A9 锁定验证 | Cargo.lock 由真实 Cargo 生成后提交；正式 CI 先 fetch --locked，再离线 test/clippy，源码与 lock 无变更检查通过 |
| A10 依赖与证据 | 精确版本/特性准入、实际 feature 图入日志；artifact、结果和日志摘要绑定实际 SHA。配置扫描不是无中继网络证明 |
| A11 移动探针 | Android arm64/iOS arm64 的 library cargo check 均通过；只证明该目标的编译检查，不宣称完整 App 链接或真机 |
| A12 开发后审核 | 实施者复核、独立审查发现、修复和独立复审均有实际记录；最终复审 SHA 与受测 SHA 相同 |

客户端完成 TLS 本地阶段可能早于服务端拒绝该客户端证书；负例确保服务端不产生认证包装对象，客户端瞬时连接随后关闭。证书只证明已配对私钥控制权，不替代用户身份或可信操作系统。

## 3. 审核发现与修复

| 发现 | 来源 | 处理与复测 |
|---|---|---|
| 探测 future 被外层 timeout/drop 取消时，借用连接仍存活 | 独立审查 P2，[4440551574](https://github.com/wmqfl861/AK-sending/pull/3#discussion_r4440551574) | 新增跨全部 await 的 RAII ProbeGuard，只有完整成功才解除；2 项公开 API 回归分别测试外层超时和丢弃已 poll future，覆盖主动/被动两角色；取消后不能转接收会话 |
| Android target-only 预取遗漏构建主机依赖，离线检查失败 | CI 41418768262 | 改为锁定全目标预取，再保持目标离线 cargo check；不去掉 --offline，也不把失败探针标为通过 |
| Windows cp1252 控制台无法镜像 Cargo 特性树 Unicode，未生成最终报告 | CI 41419227868 | 主入口设置 UTF-8 输出，保留原始 UTF-8 日志、真实退出码与失败聚合；新增 cp1252/StringIO 回归 |
| 新回归把 Windows CRLF 与 LF 字节直接比较 | CI 41420426147 | 每台主机显式覆盖 LF 和 CRLF，逐字节验证 UTF-8；不跳过 Windows、不放宽编码断言；最终三桌面均通过 |

独立执行者为 `chatgpt-codex-connector[bot]`（ID 199175422）。最终 [复审回执 6573677421](https://github.com/wmqfl861/AK-sending/pull/3#issuecomment-6573677421) 明确审查 `f0daac8aa4`，未发现重大问题；汇总评论 6573217263 标记 completed、无未解决发现。不是以请求评论或表情推断完成，也不等同 GitHub 正式 APPROVED 或专项安全审计。

## 4. 实际测试与证据

最终 [桌面 CI 41420861725](https://github.com/wmqfl861/AK-sending/actions/runs/41420861725) 显式 checkout 最终受测代码，Ubuntu 24.04、Windows Server 2022、macOS 14 全部通过。每系统：核心 50 项 Rust；传输 18 单测 + 8 授权集成 + 2 取消集成 + 3 编译失败文档测试，共 81 项 Rust；24 项 Python。Rust 1.97.1、Python 3.13.1；fmt、clippy -D warnings、静态检查、实际 feature 图和 tracked-source 无变更检查均通过。

最终 [移动 CI 41420861650](https://github.com/wmqfl861/AK-sending/actions/runs/41420861650)：`aarch64-linux-android`、`aarch64-apple-ios` 的 `cargo check --package ak-transport --lib --target TARGET --locked --offline` 均通过。NDK/Xcode 使用 runner 安装版本并在 job 日志记录，尚非正式发布 SDK 固定链。

三个桌面 artifact 均已实际下载，ZIP SHA-256 与 GitHub 元数据一致；各 results.json 的 commit、9 项命令退出码、81/24 用例计数及所有日志摘要均复核。详见机器索引。工作树 SHA-256 反映实际字节，Windows Cargo.lock 的 CRLF 可与 Unix 不同；Git 树与 clean-source 检查用于判断内容提交是否变化，不能把跨系统换行摘要差异误作改源码。

候选 CI 41417370236 在工作树生成 lock/rustfmt 并通过 79 项 Rust，只作为引导；8 个候选文件的 Git blob 与回收提交逐一匹配。最终验收使用已提交锁文件，不能用候选成功替代。初始缺锁、Android 预取、Windows 编码/换行失败和并发取消轮次保留；取消不计作通过。CI artifact 保留期 14 天，索引长期保留但不能声称过期后远端 ZIP 仍可下载；交付包另附已下载的最终原始日志。

本地实施容器无 Rust；实际本地执行 Python 3.13.5 的 24 项测试与 L0 仓库检查通过，不冒称本地编译。锁文件共 100 个包（2 个工作区 + 98 个外部，包括开发/平台目标），不是 98 个当前运行时依赖；核心仍零外部依赖。

## 5. 剩余风险、交接与回退

当前仅同机真实 UDP/TLS；没有双机无公网局域网、互联网/NAT、手机端点实跑、目录授权/生命周期、文件 I/O、持久化续传、跨网无中继抓包、性能/包体/内存预算或正式安全审计。手工地址不证明路径是 LAN；透明转发设施无法由包装类型排除。完整叶证书固定使证书续签也需重新配对；当前每端点仅预配一个对端，调用方需可信地提供对端实际源地址。
同步授权检查只是瞬时判断，未来文件写入必须另设计异步撤销栅栏；校验/保存状态是可信引擎事件，不代表已执行磁盘提交。库编译探针不是最终 App 链接与真机。声明的最低 Rust 版本未另跑 MSRV；依赖升级需重验特性、路径与安全边界。

下一模块建议 M0-03 有界会话控制协议与授权绑定的流入口，须先提交独立计划，不顺势扩为文件、NAT、配对 UI 与 GUI 的大模块。详见 [交接](../docs/HANDOFF.zh-CN.md)。回退仅撤销本模块 crate/workspace/CI/配置变更，保留计划、审核历史与 M0-01 授权；不改变 ADR-001、不引入中继、不覆盖 main。
