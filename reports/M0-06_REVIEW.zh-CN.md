# M0-06：独立LAN探针开发后审核

日期2026-11-02（America/Los_Angeles，CI UTC日期为11月3日）。状态 **REVIEWED_LAN_HARNESS_ONLY**；不是物理设备、真实Wi-Fi或整个M0验收。

## 先行计划、范围与提交

基线532fa08e7bfcd84cf017583ef5406ee9931a12b1；先行计划4bf8fa66b9863c5ca2baa498c8521c37fa84f42d；端点排空先行补充531a4e442a227618b1798e0aebeb3b800d27a01d；最终受测及独立复审310079dbf9d07e9105bb419515ecc9b3f8debae6，树aaf4ccbd83e4f153ba2b459203c3eda527b49389。计划均先于实现。PR #7草稿，未合并。

新增隔离native LAN模块/9个标量ABI、顺序有界公开DER导入、临时双向配对身份、固定角色、双方本机一次性合成许可、任务绑定、取消/过期/释放；CLI和独立进程测试。生产只增加消耗式DirectEndpoint.shutdown及2项回归，认证、授权、会话协议、依赖及两份Cargo.lock不变。没有文件路径/任意正文API、NAT、发现、代理或中继。

## 验收映射

| 项目 | 实际结果 |
|---|---|
| A1 基线与依赖 | 本地110项核心Rust、16项native Rust、49项Python、fmt/clippy通过；生产core/mobile回归CI成功；锁文件不变 |
| A2 状态与边界 | 新增10项native单测：地址/端口、顺序导入4096上限、无配对不能start、不可变对端、自证书拒绝、公共读取、取消终态、旧句柄、释放、过期与panic |
| A3 独立进程 | Linux/Windows/macOS三个主机实际各起两个Python子进程加载release库；正常配对、错证书、取消监听3组用例通过；核对证书ID且未start前无socket |
| A4 独立网络栈 | Linux CI真实network namespace+veth，两个私网地址且无默认路由；同3组用例通过。不是物理双机、Wi-Fi或手机权限验收 |
| A5 移动链接与旧壳 | Android arm64真实ELF及16KiB对齐、9个定义导出通过；iOS引用全部新旧接口的程序实际链接；既有五端Flutter壳构建回归通过，此时尚无LAN UI |
| A6 审核 | 真实独立审查针对310079dbf9完成，未发现重大问题；不等同正式APPROVED或安全专项审计 |

prepare异步生成私钥且不打开网络；start是用户在已核验完整指纹后授予的单次合成实验意图。接收方实际TLS/Hello完成后才使用现有ReceiveGrant。临时身份180秒、等候60秒、实际传输步骤5秒均有上限。固定两帧内容逐字节校验，不声明文件完整性或保存。

## 复核与实际证据

实施者发现立即结束runtime可能丢失QUIC收尾报文。在修改生产接口前提交补充计划，以close+有界wait_idle代替固定sleep；新增空闲端点与真实认证对端关闭2项回归。取消与成功在同一Mutex内判定终态，release只在工作线程退出后执行。

最终native CI **41565411859**，五平台全部成功；core **41565411792**、mobile-core **41565411791**、旧Flutter壳 **41565411783**均成功。五份native artifact已实际下载，核对ZIP、精确SHA、results.json全部命令退出码、所有原始日志摘要及原生库摘要；Linux11项命令、Windows/macOS各10项、Android9项含派生检查、iOS8项。三桌面各16项native Rust。CI公开产物保留14天，另保存已下载原始文本日志。

独立执行者chatgpt-codex-connector[bot]（199175422），回执 **6593782051** 明确审查310079dbf9；汇总6593753797为completed、无未解决发现。请求审核不算完成证据。

本地确实执行Rust1.97.1（来自核验后的只读工具链包）与Python测试、release构建、两独立进程实验。本地namespace实验由于容器权限被拒绝，记录NOT_RUN；未绕过权限，也未把CI执行改称本地成功。最终源代码/脚本/工作流子树与本地受测字节逐项一致。

## 限制与交接

仅手动RFC1918 IPv4或精确127.0.0.1实验地址；输入范围不是对LAN拓扑的证明，无法排除用户系统透明隧道。未检查接口子网广播地址；误配会失败，不是自动发现/拓扑判定。IPv6属于后续能力，不把本实验限制当最终产品限制。

每库最多一个LAN槽加一个既有loopback槽；应用应互斥入口，不把两槽说成无限任务。临时证书不持久化，不等于生产密钥管理。真实手机、断外网Wi-Fi、权限/目录/后台/杀进程及资源预算仍NOT_RUN。M0-07已另先行计划补Flutter操作入口和设备证据工具，不将M0-06成功等同完整M0。

回退仅本模块实验/脚本/专项CI和消耗式shutdown接口，保留计划/审核历史，不改ADR-001、不合并部署。机器证据见[M0-06_VALIDATION.json](M0-06_VALIDATION.json)。
