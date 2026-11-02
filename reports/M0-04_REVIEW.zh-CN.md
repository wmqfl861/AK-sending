# M0-04 原生平台探针：开发后审核

日期：2026-11-02。状态：**REVIEWED_NATIVE_PROBE_ONLY**；不是整个M0、完整App或真机验收。

## 提交与范围

先行计划 `926a413d3468b50308f7fca15f69d199134b7aac`；首实现 `6ba736588a08705149cde56a2ab2fcf32f54b057`；最终受测且独立复审 `171b3be9974f00139212360ebd8ec1acc9cde42e`，树 `dab58e318bf1890b4919283d17ae5410479bd81a`。后续报告提交只改证据，不冒充已测代码SHA。PR #5仍草稿、未合并；未部署、采购或强推。

在独立Cargo workspace新增原生实验crate、真实Cargo生成锁文件、5个固定宽度整数C ABI、单槽任务控制、头文件/原生链接程序、验证脚本、五端CI、工具链配置及资源测量模板。生产core/transport源码、认证配置和依赖未更改，M0-03报告原样继承。

## 计划映射

| 项目 | 结果 |
|---|---|
| A1 依赖与回归 | 独立锁文件已提交；正式预取--locked后离线测试/构建。3桌面native每系统6项Rust、fmt/clippy通过；生产core CI和移动库CI成功 |
| A2 三桌面实际ABI | Linux、Windows、macOS均实际加载release动态库；ctypes执行3次真实127.0.0.1双向TLS/有界帧往返及取消释放，不是固定返回pass |
| A3 生命周期 | busy、单调句柄、不复用旧handle、取消/成功终态CAS、释放活线程拒绝、panic隔离、重复任务清理有实际回归；卸载前必须release |
| A4 移动实际链接 | Android arm64真实release ELF，ELF64/AArch64及所有LOAD的16KiB对齐检查通过；iOS arm64静态库及引用全部5个符号的C程序链接通过 |
| A5 证据 | 精确head checkout、只读权限、未保存Git凭据、源码/lock无修改检查；五份产物均实际下载并校验 |
| A6 测量入口 | 实际记录原生库字节与摘要、host合成探针耗时；完整App/设备模板保留null，不把未测当0 |
| A7 独立复审 | 最终171b3复审完成、未发现重大问题；不等同正式APPROVED或专项安全审计 |

实验只用合成证书、127.0.0.1和固定合成内容。自动批准仅属于本机fixture，不是产品接收策略。API无指针、用户文件路径、任意目标地址或正文参数；加载库不自动开socket。一个进程中只应加载一份库，工作线程未释放时不得卸载。10秒实验总上限与10ms取消轮询不是OS线程抢占保证。

## 实际验证及修复

最终原生CI：[41455473740](https://github.com/wmqfl861/AK-sending/actions/runs/41455473740)，三桌面实际运行及Android/iOS实际链接均成功。生产core CI41455473751、mobile-core CI41455473689也成功。

初轮41454690278的Android准备失败：sdkmanager不在runner PATH。171b3改为明确安装路径调用，并记录版本/安装日志；不降低NDK/API/16KiB校验，不跳过Android。失败历史保留。

工具链：Rust1.97.1；Android NDK29.0.14206865/API26 clang；Apple Xcode26.6/iPhoneOS SDK26.5，最低目标iOS15/macOS13。runner镜像不是位级固定；记录日志不等于位级可复现。Flutter3.47.0完整commit/engine只在配置中固定，本模块尚未构建Flutter App。

五份archive均核对ZIP SHA-256、results.json精确SHA、全部命令退出码、每份原始日志摘要及实际库摘要。3桌面分别9项记录命令，Android7项含ELF校验，iOS8项。移动链接不包含设备运行测试。详见 [机器索引](M0-04_VALIDATION.json)。

| 原生产物 | 实际字节 |
|---|---:|
| Linux .so | 3,510,128 |
| Windows .dll | 2,299,904 |
| macOS .dylib | 3,013,872 |
| Android .so | 3,542,360 |
| iOS .a（链接输入） | 38,690,176 |

这些数字不能代替应用下载、安装大小或手机内存。iOS静态归档是链接输入，不能拿它作为最终IPA体积。

最终独立审核执行者chatgpt-codex-connector[bot]（199175422）；[回执6578890225](https://github.com/wmqfl861/AK-sending/pull/5#issuecomment-6578890225)明确审查171b3be997，未发现重大问题；汇总6578606119 completed/无未解决发现。不是请求或表情推断通过。

本地实际native6项Rust、fmt/clippy、31项Python、Linuxrelease库ctypes运行通过。本地库与CI库构建环境不同，不能互换摘要；最终判断使用上述精确远端SHA的CI。artifact保留14天，交付包另留原始日志，过期后不声称远端仍可下载。

## 剩余边界与交接

没有物理Android/iOS、真实双机无公网Wi-Fi、平台目录权限语义、后台/杀进程、完整App安装/签名或手机资源指标。最低系统是待验证支持目标，不是成功支持声明。没有用户文件引擎、可靠保存、续传、NAT。

下一模块M0-05按已提交计划实现最小Flutter壳及移动风险入口。它仍须独立审核和实测，五端链接成功不会自动解除整个M0的设备/网络/预算阻断。回退限于实验crate及脚本/配置/CI，保留计划与审核历史，不改ADR-001或main。
