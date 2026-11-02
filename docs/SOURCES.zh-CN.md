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

本次没有基准测试、手机真机实验或穿透成功率数据。代码库采用哪一个网络库、确切版本及平台插件尚未冻结。
