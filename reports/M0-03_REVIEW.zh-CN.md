# M0-03 开发后审核与交接

日期：2026-11-02。状态：**REVIEWED，仅有界会话协议范围**。整个 M0 尚未完成，当前不是用户文件传输产品。

## 基线、计划与实际受测提交

- 基线：`35d4508943ae04e0978ef98637cdcaed9fc95e38`。
- 先行计划：`02c6b55cc6d3b18c3f42609d5f426de7a35dd717`，见 [计划](../docs/modules/M0-03_PLAN.zh-CN.md)。
- 首实现：`a89b3179e83a8a1acd086372bbe9d94231865680`；格式修正：`97ce865bf0724b396f168a3fd9510b867f1a7887`。
- 最终受测并独立复审：`ed6778006a5837868bd4fc6f592254f2bf48d9db`；代码树 `36a7fc7faaf39f78a4239b6e746366971ca56b5e`。
- 本报告的后续文档提交不冒充上述被审代码 SHA。PR #4 保持草稿，未合并；未部署、采购或强推。

## 修改范围与计划验收

生产变更限于 ak-transport 的私有 TLS exporter 桥接、接收许可退役检查、session 编解码/状态与公开 API 回归。生产依赖、Cargo.lock 和原认证配置不变；ak-core 仍零第三方运行时依赖。

| 计划 | 实现与实际证据 |
|---|---|
| A1 原功能不回退 | 原81项Rust保留，增加27项，三桌面每系统108项Rust与24项Python通过 |
| A2 有界真实协议 | 同机原生UDP/TLS完成Hello/本机授权/Ready/两帧Data/Ack/End；单帧最大16KiB、固定头68字节 |
| A3 本机授权 | Ready和每次接收前后验证真实grant；错误、跨连接、过期、撤销、暂停、取消及拒绝均不能交付正文 |
| A4 非法帧 | magic/version/type/reserved/task/exporter/长度/截断/序号/尾随字节和溢出负例通过；正文分配前检查头 |
| A5 失败关闭 | 已poll操作的外层timeout/drop、abort和异常终止立即关闭；未poll借用操作无副作用；不复用半帧 |
| A6 重放绑定 | 标准TLS exporter两端一致，任务变化/重连变化；重复和乱序帧拒绝。网络不调用approve |
| A7 私有接口 | 新增两个外部compile_fail，阻止伪造会话和取得裸流；既有三个传输私有接口负例保留 |
| A8 平台 | Linux/Windows/macOS完整测试；Android/iOS arm64库编译检查通过，非设备验收 |
| A9 独立审核 | 发现P2后真实复现、修复、回归；最终复审和CI均绑定ed6778006a |

## 独立发现、修复与复测

独立执行者 `chatgpt-codex-connector[bot]`（ID 199175422）指出 [P2 #4442711123](https://github.com/wmqfl861/AK-sending/pull/4#discussion_r4442711123)：接收端返回Ended后立即释放，会关闭连接并丢弃尚未被发送方消费的EndAck。实施者实际添加立即drop回归并观察失败，不仅凭推测修改。

修复：发送端消费EndAck和FIN后发出专用正常关闭码；接收端在原5秒总超时和RAII守卫范围内等待该关闭，再检查本机grant/期限并退役许可，才返回Ended。异常关闭码、取消和超时仍失败关闭。新增双方立即drop（12轮）、异常终止、收尾取消三项回归。认证对端可以谎报自己已消费数据，因此这不是对端诚实证明，更不是磁盘可靠保存承诺。

最终 [复审回执6578073443](https://github.com/wmqfl861/AK-sending/pull/4#issuecomment-6578073443) 明确审查 `ed6778006a`，未发现重大问题；汇总6577807804标记completed、无未解决发现。不是请求即通过，也不是正式APPROVED或专项安全审计。

首轮CI还发现导出语句未保留rustfmt换行；已修正并重跑，保留失败轮次41449496048。没有跳过fmt、平台或离线约束。

## 实际验证与证据

[桌面CI 41451061284](https://github.com/wmqfl861/AK-sending/actions/runs/41451061284)：Ubuntu24.04、Windows Server2022、macOS14每系统108项Rust、24项Python，fmt、clippy -D warnings、静态检查、feature图和源码未修改检查全部通过。Rust1.97.1、Python3.13.1，锁定预取后离线执行。

[移动CI 41451061545](https://github.com/wmqfl861/AK-sending/actions/runs/41451061545)：Android/iOS arm64 library check通过；未在本模块执行App链接、安装或真机。移动SDK固定与原生链接由M0-04补齐。

三桌面artifact均下载并核对ZIP SHA-256；逐项验证results.json的commit、9项真实命令退出码和全部日志摘要。108项由实际日志汇总为46+3+32+2+8+11+1+5，不能把循环12轮另加为12项测试。详见 [证据索引](M0-03_VALIDATION.json)。artifact保留14天，交付包另附已下载原始日志；过期后不声称远端ZIP仍可下载。

本地取得只读工具链artifact后实际使用Rust1.97.1和vendor离线执行108项Rust、fmt/clippy，Python3.13.5执行24项Python及静态检查。工具链准备不是验收，实际运行日志单独保留；最终七个变更源码/测试Git blob与本地受测字节核对一致。没有把本地初始化git的SHA当远端提交SHA。

## 剩余风险、下一任务及回退

Ack仅表示协议帧接收，Ended仅表示该有界正文会话结束；无文件校验、磁盘保存、持久化续传、文件I/O撤销栅栏。返回的Vec不是持续写盘许可；End后的领域状态不是Completed。单帧有界不代表全文件总量授权或磁盘配额。

当前仅同机真实UDP/TLS，不证明无公网Wi-Fi、真实双机、NAT、手机权限/后台行为或性能达标。路径与配对限制继承M0-02。下一任务M0-04固定工具链/原生链接，M0-05最小Flutter及手机风险入口；每个模块独立先行计划。M0-04分支已基于最终实现SHA建立，不合并任何PR。

回退仅移除新增session/API/测试与private桥接，保留计划、审核历史及M0-01/02授权/认证边界；不改变ADR-001、不改main。
