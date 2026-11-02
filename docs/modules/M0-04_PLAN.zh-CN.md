# M0-04：固定构建基线与原生平台探针 — 先行计划

日期：2026-11-02。基线：`ed6778006a5837868bd4fc6f592254f2bf48d9db`（M0-03 收尾修复已本地复测，跨平台与独立复审待回收）。状态：PLANNED。本计划先于任何 M0-04 实现单独提交；PR 叠加、不合并。M0-03 若再发现阻断，先修复和继承，不宣称依赖已验收。

## 1. 目标、范围和分工

补齐 M0 的固定工具链/最低平台/ABI 表、实际原生库链接、轻量化测量入口。新增隔离的 `experiments/platform_probe/native` 实验 crate；复用真实 ak-transport，不在生产 core/transport 添加 rcgen、FFI 或 UI 依赖。该实验不是用户文件引擎，不提供任意目标地址、正文、路径或用户密钥入口。

后续 M0-05 必须另提交计划后做 Flutter 最小壳、移动目录/权限/生命周期风险入口和 release 构建。真机无公网 Wi-Fi、权限拒绝、进程终止和实际设备资源测试仍是显式验收项，没有设备则 BLOCKED，不能用本模块 host loopback 或 iOS 静态链接代替。

## 2. 固定基线

- Rust 1.97.1；实验使用独立 Cargo.lock，真实 Cargo 生成、先提交再 --locked/offline 测试。沿用生产依赖精确版本，不升级。
- Flutter 3.47.0，官方 tag 对应 commit `4cf24164269a5ebf0c16a028a00727d0e77bbb05`；本模块先记录，实际壳构建在 M0-05。不是宣称最新补丁版本。
- 首轮 ABI：Windows x86_64-msvc、Linux x86_64-gnu、macOS aarch64、Android aarch64、iOS aarch64；另用 iOS arm64 simulator 做链接实验，不冒充真机。
- 工程目标最低系统：Windows 11、Ubuntu 24.04、macOS 13、Android 8/API26、iOS15。这是待验证支持目标，不是已测试支持列表；最低/后台 API 分界/目标版本另列真机矩阵。
- Android NDK `29.0.14206865`，API26 clang 链接，记录 ELF 和页面对齐；后续壳 target/compile SDK37、build-tools36.0.0、JDK17、AGP/Gradle 按已核验兼容组合固定。
- Apple CI 选择 macos-26，显式 Xcode26.6 与 iPhoneOS26.5 SDK，最低 iOS15。若 runner 无指定版本则失败，不静默换 latest；主机镜像仍可能更新，记录 image/SDK/build，不宣称整个虚拟机位级可复现。

## 3. C ABI 与线程/资源边界

只导出固定宽度整数 API：version、start、poll(handle)、cancel(handle)、release(handle)。不接受或返回指针、字符串、任意字节、文件路径、网络地址或凭据。预留错误码区分 invalid/busy/failed/cancelled；成功仅表示合成 loopback 实验完成。

- 全局最多一个实验任务；handle 单调递增、checked_add，旧 handle 不复用；忙时拒绝，不建立无界队列。
- 工作线程内部创建 Tokio current-thread runtime、临时合成证书、两个原生 UDP 端点并执行 M0-03 的 Hello/本机授权/Data/End 收尾；两个端点都限定127.0.0.1。证书不持久化、不写日志。
- 主线程 start 只做有界登记和线程创建；poll/cancel/release 不等待网络或阻塞 join。只有 is_finished 后才能 join/release；cancel 标志触发异步操作丢弃，复用已有 RAII 关闭路径。
- 十秒总上限，取消检查有界；状态完成与取消竞争有定义。不会在取消后把结果改回成功。句柄生命周期、重复release、错误handle、忙时start和cancel-before-run均测试。
- C ABI 防止 Rust unwind 穿越边界；内存耗尽/abort不作可恢复承诺。导出符号是局部审查的 unsafe attribute，不把实验 ABI 的限制扩大到生产 crate；生产仍 forbid unsafe。
- 实验库只可通过明确按钮/测试调用启动，不在 load 时打开 socket，不常驻后台，不持有 wake lock。

## 4. 实施与验收

A1：M0-03 原测试和依赖边界不回退；本模块独立 cargo fmt/test/clippy --locked --offline 通过。
A2：三桌面生成真实 release shared library，Python ctypes 使用头文件对应 ABI 加载并运行真实 TLS/有界帧/正常终止；记录真实返回码，不使用固定 pass。
A3：启动、忙时拒绝、完成、取消、释放、旧/非法句柄和溢出边界测试；重复循环后不留下未释放任务。
A4：Android arm64 release shared library 实际链接，检查 ELF 架构/页对齐；iOS arm64 static library 及调用符号的原生程序实际链接；只说明链接，不宣称 App 安装、签名或真机可用。
A5：工作流固定checkout输入SHA、read-only权限、无保存凭据；保存工具链/命令退出码/日志与产物摘要，源码和锁文件不得在验收执行中被修改。
A6：资源测量脚本能记录真实原生库大小/主机实验耗时，明确不等同 APK/IPA/App 下载或手机PSS；完整预算模板保留未测值，未测不得当0。
A7：独立审核并修复，最终实现SHA与测试/回执一致。编译/链接缺失、失败或未解决阻断则PARTIAL/BLOCKED。

## 5. 风险、来源与回退

实验 rcgen 只是本机临时配对 fixture，不是生产密钥生命周期。标量 FFI 证明链接及任务控制，不开放用户文件或认可本机低权限攻击者信任。一个库实例的全局注册表不替代未来真实任务管理器。

当前版本依据官方 Flutter tag、Android NDK/AGP 与 Actions runner-image 清单；在实现证据中记录实际版本和下载摘要。平台探针只暴露早期风险，真机目录/本地网络/后台语义必须实测。无授权采购或签名凭据操作。

回退只移除 experiments/platform_probe、专项脚本/工作流/构建配置，保留计划/审核历史；不修改ADR-001、不更改生产授权边界、不覆盖main。报告写 reports/M0-04_REVIEW.zh-CN.md 与 M0-04_VALIDATION.json；整个M0另有汇总清单，不能因模块PASS自动改为整个阶段PASS。
