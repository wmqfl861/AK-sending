# 技术资料与事实依据

下列为本次查阅的公开一手资料；它们不是本项目的测试证据。产品需求与 ADR 来自用户当前对话。

- [S1] IETF RFC 8489 — STUN：公网映射与连接检查工具，不单独保证 NAT 穿透。https://www.rfc-editor.org/rfc/rfc8489
- [S2] IETF RFC 8445 — ICE：候选收集、检查和选定。AK 的实现不得启用其 relay 数据路线。https://www.rfc-editor.org/rfc/rfc8445
- [S3] iroh Endpoint Builder：relay 参与连接建立和打洞，empty builder 默认无查询服务且禁 relay。https://docs.rs/iroh/latest/iroh/endpoint/struct.Builder.html
- [S4] iroh RelayMode：Disabled 禁止 relay 监听与拨号；不是独立穿透能力的保证。https://docs.rs/iroh/latest/iroh/endpoint/enum.RelayMode.html
- [S5] Flutter 官方部署平台列表。具体最低版本与 ABI 需 P0 冻结并真机验证。https://docs.flutter.dev/reference/supported-platforms
- [S6] Android Storage Access Framework：用户授权 URI/目录，受系统保护位置限制。https://developer.android.com/training/data-storage/shared/documents-files
- [S7] Apple Providing access to directories。此次正文自动抓取受动态页面限制；目录授权设计沿用前文方案，具体 API/系统版本须 P0 再次核验。https://developer.apple.com/documentation/uikit/providing-access-to-directories
- [S8] GitHub REST API 排障：Resource not accessible by integration 指向权限不足。https://docs.github.com/en/rest/using-the-rest-api/troubleshooting-the-rest-api
- [S9] OpenAI GitHub 连接帮助：仓库授权与连接账户要正确；标准只读连接不能因勾选仓库自动获得写入。https://help.openai.com/en/articles/11145903-connecting-github-to-chatgpt

## 2026-11-02 方案评审补充

供 [方案评审](REVIEW_2026-11-02.zh-CN.md) 引用；同样不是本项目的测试证据。

- [S10] arXiv 2510.27500 — Challenging Tribal Knowledge：IPFS 网络 440 余万次 NAT 穿透测量，打洞成功率约 70%（±7.1%），TCP 与 QUIC 无统计差异。https://arxiv.org/html/2510.27500v1
- [S11] Tailscale 防火墙集成文档：在部分路由器/防火墙上开启 UPnP 或 NAT-PMP 可建立直连，并提示安全取舍。https://tailscale.com/docs/integrations/firewalls
- [S12] iroh 博客 iroh on QUIC Multipath（2025-08）：打洞协调经 relay，连接初期数据先经 relay 再切换直连。https://iroh.computer/blog/iroh-on-QUIC-multipath
- [S13] IETF draft-bruynooghe-n0-quic-nat-traversal-00（2026-07）：假设两端已能建立（经 relay 的）QUIC 连接，再在连接内交换候选。https://www.ietf.org/archive/id/draft-bruynooghe-n0-quic-nat-traversal-00.html
- [S14] Apple TN3179 Understanding local network privacy：iOS 14+/macOS 15+ 本地网络授权、Info.plist 键、组播授权的适用范围、后台未授权时的拒绝行为。https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy
- [S15] Android 用户发起的数据传输任务（UIDT）：Android 14+、权限、通知、可被系统停止的条件、旧版本回退方案。https://developer.android.com/develop/background-work/background-tasks/uidt
- [S16] Android 前台服务超时：以 Android 15 为目标时 dataSync 每 24 小时合计 6 小时。https://developer.android.com/develop/background-work/services/fgs/timeout
- [S17] WWDC25 Finish tasks in the background：BGContinuedProcessingTask 的用途、用户发起、进度界面与终止规则。https://developer.apple.com/videos/play/wwdc2025/227/
- [S18] Apple 开发者论坛：用 BGContinuedProcessingTask 后台上传 7 GB 文件时的进度上报问题。https://developer.apple.com/forums/thread/809182
- [S19] Android 访问共享存储中的媒体文件：Android 10+ 写入自有 MediaStore.Downloads 文件无需存储权限、RELATIVE_PATH 子目录。https://developer.android.com/training/data-storage/shared/media
- [S20] LocalSend：Apache-2.0，Flutter 实现，覆盖 Windows/macOS/Linux/Android/iOS，仅局域网、HTTPS REST 协议。https://github.com/localsend/localsend

本项目仍没有自己的基准测试、手机真机实验或穿透成功率数据；S10 是第三方网络的测量结果。代码库采用哪一个网络库、确切版本及平台插件尚未冻结。
