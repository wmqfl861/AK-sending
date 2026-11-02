# 当前交接

## 目标与授权

仓库：`wmqfl861/AK-sending`；默认分支：`main`；本地开发分支：`dev/local-parallel`（未推送）。
用户已授权上传本项目全部文档和代码，并完成目标账户连接。
服务器仅可用于设备发现、信令和 NAT 穿透协调，绝不承载文件数据或密文；ADR-001 不变。
2026-11-02（第二阶段）：用户指示"继续本地开发，多模块多子代理并行，直到项目完成，不用推送"；本地分支推进，未推送、未部署、未强推、未加许可证。

## 2026-11-02 第二阶段完成内容（dev/local-parallel）

按 ADR-002（采纳评审 D1/D5/D6 路线）完成局域网 CLI 原型全栈：

- 提交历史：`c13ecb8`（脚手架/接口冻结）→ `925c63d`（六模块并行实现）→ `4a3bf36`（引擎+CLI 集成与验证）→ 文档/报告提交。
- 10 个 crate：ak-core（领域）、ak-identity、ak-wire、ak-storage、ak-net、ak-discovery、ak-store、ak-signal、ak-engine、ak-cli。
- 数据面：QUIC（quinn 0.11）+ rustls/ring + 自签 Ed25519 证书双向钉扎；`DirectChannel` 无公开构造函数（类型约束）；无 relay/TURN 路径，Cargo.lock 依赖扫描锁定。
- 引擎：完整端到端协议（见 ak-engine 文档），确定性任务 ID、窗口化确认、检查点续传、完成标记晚于校验+落盘。
- 协调面：信令 TCP + STUN UDP 单进程（`ak-cli signal-server`），封闭消息集、限频、会话 TTL，无文件路由（测试锁定）。
- 验证证据：[开发验证记录](../reports/dev-validation-2026-11-02.txt)——workspace 242 项测试、clippy `-D warnings`、仓库检查、12 项 Python 测试、5 GiB 稀疏 release 环回（74s）、真实双进程 CLI 传输逐字节一致。

## 实现与验证边界（如实）

- 已验证范围＝本机环回（L2 形态）：QUIC 握手、传输、续传、大文件、CLI 双进程。跨机器、跨网段、公网 NAT 穿透、抓包（L3/L4）未执行。
- 打洞：holepunch 探测与 probe 命令已实现并有本地 UDP 互探测试；真实 NAT 环境成功率未测（P0-05）。
- 移动端/Flutter、真机验收、性能调优（多流并发、千兆跑满）未开始。
- 已知限制见各 crate 文档：接收端 available_bytes 返回 None（Windows std 无 API）、mtime 不回写、IPv6 前缀按 /64 简化、probe nonce 非密码学随机（身份由 QUIC 握手承担）。

## 历史记录（第一阶段，main 分支）

README 初始化提交：`4923b178860de198867f6de7b7d9cb7942200bb3`。
完整原包提交：`5ce974c279acac2bb27bd24ba4e703042dde2861`。
2026-11-02 方案评审见 [方案评审](REVIEW_2026-11-02.zh-CN.md)；D1/D5/D6 已按所有者指示经 ADR-002 采纳，D2/D3 细节仍待确认。
第一阶段验证记录见 [Rust 验证记录](../reports/rust-validation-2026-11-02.txt)、[上传验收记录](../reports/GITHUB_PUBLISH_VERIFICATION.zh-CN.md)。

## 下一工程节点

1. 两台真实机器（同网段）跑 `ak-cli receive`/`send`，验证非环回路径与防火墙行为；随后 `signal-server` + `probe` 在家宽/热点组合下实测公网直连成功率（P0-05，决定产品文案）。
2. 所有者确认 D2（公网失败提示文案）与 D3（端口映射默认开关）。
3. 视实测结果冻结网络库 ADR（评审 D1 尾项）并重估路线。
4. Flutter UI（Windows → Android 优先）与移动端后台/存储适配。
后续开发先读取本文件、AGENTS、ROADMAP 和现有源码；保留已有历史，禁止强推。
