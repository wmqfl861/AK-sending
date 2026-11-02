# M0-02 先行开发计划：最小认证直连适配器

日期：2026-11-02。初始状态：PLANNED。本文件必须先于任何本模块实现单独提交。
基线：`f727edcaf7d164a7940c7d57e4d317b63b5acd54`，来自未合并 PR #2。工作分支：`dev/m0-02-authenticated-direct-20261102`；新 PR 叠加在 M0-01 分支，不合并、不部署、不采购、不强推。
规范：[项目规则](../../AGENTS.md)、[蓝图](../BLUEPRINT.zh-CN.md)、[路线](../ROADMAP.zh-CN.md)、[ADR-001](../adr/001-direct-only.zh-CN.md)。已读取远端最新 PR/head、README、HANDOFF、规则、路线和 M0-01 授权源码。

## 1. 目标与非目标

新增独立 `ak-transport` crate：通过真实原生 UDP + quinn/rustls TLS 1.3 双向认证建立私有构造的 `AuthenticatedDirectConnection`；认证必须固定本地已配对证书，而非相信网络消息、观察布尔值或任意公有 CA。使用 M0-01 ReceiveJob，但只通过新的私有接收会话签发连接绑定的 ReceiveGrant。
本模块仅为手工地址的最小直连实验和授权边界：允许固定格式探测消息，不开放任意文件正文/文件系统读写，不实现完整配对 UI、密钥持久化、发现、STUN/ICE、NAT 穿透、协调服务、网络自动分类/局域网优先选路、文件协议、Flutter、后台保活或发布。
同机 loopback 只能证明真实 UDP/TLS 和接收授权边界，不冒充双机局域网、互联网可达、性能或五端真机验收。整个 M0 不随本模块结束而自动完成。

## 2. 设计决策和接口

1. `LocalIdentity` 从调用方提供的本机证书和 PKCS#8 私钥构造；不生成/上传真实配对密钥，不为秘密实现 Debug。测试用证书仅在测试进程内临时生成。
2. `PairedPeer` 表示本地可信配对配置。首个适配器固定完整 DER 叶证书，包含公钥且比只固定 SPKI 更严格；证书重签/更新也需重新授权。peer_id 为证书 SHA-256，不冒充已完成用户/设备身份体系。固定 TLS 名称 `ak-peer.invalid`，不查询系统 DNS，不使用系统 CA/TOFU 自动信任。使用 rustls 标准 WebPKI 校验有效期、名称、用途及签名，握手完成后再检查真实 peer_identity 的叶证书与固定证书完全相同；未匹配前不暴露应用连接。
3. TLS 仅 1.3；双方必须提供受信证书；固定 ALPN `ak-direct/0.1`；关闭 0-RTT、会话恢复、客户端迁移、QUIC datagram 与单向流。握手和固定探测有超时、流数/缓冲/大小限制。不复制 insecure_connection 示例，不实现接受任意证书的 verifier。
4. `DirectEndpoint` 自己创建并绑定原生 UDP socket；不接受任意自定义 socket、代理、URL、网络上报的“已认证”标志。候选分手工直连/中继/未知；后两类直接拒绝，非法/多播/未指定/零端口目标拒绝。接收方同时核对预期真实源 socket 地址。没有服务器文件路径或备用传输。
5. `AuthenticatedDirectConnection` 字段私有，无公开裸 quinn 句柄、通用流或由布尔值生成的构造器；仅完整握手、固定证书、ALPN 和实际地址检查后生成。每次使用检查断线及地址变化，变化即关闭旧连接。本机接口、路由器、NAT、不可见透明设施不可能由类型封装证明；不把手工地址当“已证实局域网”。
6. 私有 `ReceiveSession` 持有认证连接和 ReceiveJob；连接就绪只生成 ConsentRequest。可信接收端 UI 调用 approve 才返回私有字段的 ReceiveGrant；不得把任何网络消息转换为本机 approve。每次检查同时校验会话、连接/授权代次和期限；暂停/撤销/取消/断线清除许可，恢复需新的认证连接及新确认。保留 M0-01 校验/保存阶段语义，不为文件写入建立绕过接口。
7. 仅开放固定内容的双向探测，以验证真实加密连接；探测不接受任意正文，不表示接收许可或文件写入能力。后续文件模块必须另行设计有界异步 I/O 与撤销栅栏，不能把同步 check 当永久能力。

## 3. 依赖与构建

保留 ak-core 零第三方依赖；ak-transport 精确固定 quinn 0.11.12、rustls 0.23.45、tokio 1.54.1、ring 0.17.14；测试依赖 rcgen 0.14.10。禁用默认特性，仅启用 tokio runtime/net/time、rustls ring/std 与必要测试宏；不引入第二 crypto provider、系统 CA、HTTP、压缩、relay、GUI 或服务端框架。
固定 Rust 1.97.1 保持不变。由真实 Cargo 解析生成 Cargo.lock 后提交，不手写依赖校验值。本地执行环境无 Rust 且无法 DNS 访问 GitHub；真实编译在已有 GitHub CI 中进行。若需一次性依赖引导，只输出候选锁文件/格式化补丁和日志，不自动推代码、不授予写 token。引导运行不计作最终锁定验证；最终必须使用已提交 lock、cargo fetch --locked，然后离线 test/clippy，并检查源码无被测试步骤修改。

官方接口依据（开发时查阅）：
- https://docs.rs/quinn/0.11.12/quinn/struct.ServerConfig.html
- https://docs.rs/quinn/0.11.12/quinn/crypto/rustls/index.html
- https://docs.rs/rustls/0.23.45/rustls/server/struct.WebPkiClientVerifier.html
- https://docs.rs/tokio/1.54.1/tokio/
- https://docs.rs/rcgen/0.14.10/rcgen/

## 4. 实施步骤

先提交本计划；再实现身份/配置、原生端点/私有认证连接、接收授权桥接与负例；更新 workspace/CI/验证范围；引导锁文件并提交后跑最终三桌面检查；执行移动端最小构建探针并记录实际结果和剩余风险；实施者复核后请求独立代码审核，逐项修复复测；最后提交审核、验证索引和交接。发生实质范围改变先补计划，不事后改验收标准。

## 5. 验收用例

| 编号 | 必须验证 |
|---|---|
| A1 | 原 ak-core 50 项 Rust 用例、Python 基线、格式和 clippy 不回退 |
| A2 | 两个真实 UDP 端点完成 TLS 1.3 双向认证并交换固定探测；记录地址来自实际 socket 而非布尔值 |
| A3 | 错误服务端/客户端证书、身份替换、过期证书、错误名称或 ALPN 拒绝；不向错误对端发放认证包装类型 |
| A4 | 原生端点配置关闭 0-RTT、恢复、迁移、datagram/单向流；中继/未知/非法候选在连接前被拒绝 |
| A5 | 断线/超时/预期地址不符不得获得或继续使用认证连接；地址变化检查失效关闭 |
| A6 | 未确认、错误任务/会话许可、旧确认、撤销、暂停、取消、过期、重连后的旧许可全部拒绝；新连接需要新确认 |
| A7 | 编译失败用例证明外部代码不能构造认证连接/ReceiveGrant 或取得底层 quinn 连接；合法公开 API 集成测试通过 |
| A8 | 固定探测拒绝错误/过长消息，IO 有时限，不暴露任意文件流；不写用户文件/私钥到日志 |
| A9 | Cargo.lock 提交后，Linux/Windows/macOS 真实编译、测试、fmt/clippy；锁定预取后离线执行，验证不改源码 |
| A10 | 输出实际依赖/feature 清单；核心仍零外部依赖，产物与证据绑定被测 SHA，不把配置扫描当无中继证明 |
| A11 | Android arm64/iOS arm64 最小 library 构建探针：记录成功或具体阻塞、工具链与处理路径，不将交叉构建当真机通过；若有阻塞明确列为尚未完成，不隐瞒 |
| A12 | 开发后审核报告包含具体被审 SHA、发现/修复/复测；独立审核只有收到实际第二执行者结果才算完成 |

## 6. 风险、完成判定与回退

真实身份只证明配对证书私钥控制权，不证明操作系统/路由完全可信；完整证书固定不支持无感轮换；手工地址要求接收方知道对方 UDP 源端口，NAT/动态地址是后续模块。一个端点只服务一个预配置对端，此处不建设多用户服务。移动端风险包括 ring/NDK/Xcode 交叉编译、网络/本地网络权限、系统目录授权和生命周期，后面仍需真机。
若关键桌面握手/负例、锁定构建或审核存在阻断，标记 PARTIAL/BLOCKED 而非 REVIEWED；移动探针结果单列，不宣称整个 M0 完成。保留全部失败尝试。回退只撤销本模块的新 crate、workspace 和 CI 变更，保留计划/审核历史；不改变 ADR-001，不削弱 M0-01 授权。最后更新 HANDOFF/README/ROADMAP 的真实状态，不自动合并任何 PR。
