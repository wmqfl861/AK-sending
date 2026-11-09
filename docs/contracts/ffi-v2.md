# FFI 与对外接口契约 v2（ak_pairing_* / ak_sched_* C ABI、JSON、错误码）

> **冻结于第十八波（2026-11-04），修改需集成者核准并同步 ADR-004。**
> §1 已按 **2026-11-05 wave-26 集成者 ABI 裁决**改为实际已发布导出的形态（裁决记录见 §1 末尾）；其余章节仍为第十八波冻结稿，本次随裁决补充 §2 幂等/停止细则与 §4 `peer` 必填说明。
> **2026-11-06 wave-28 增补 §8**：v1 极简配对任务面（`ak_task_*` 八导出）按 §1 裁决第 4 条的“新增符号扩展”路径落地——不改不动任何已发布签名；`check_contracts.py` 检查 2 的“18 个冻结签名”口径仅数 ```c 围栏，§8 用 ```abi 围栏因此不在其内。
> 事实源：`docs/plans/pairing-code-plan-part2.md` B.7、B.8 节。本文为 B8（`crates/ak-ffi`）与 Dart 绑定（`packages/ak_ffi_dart`）的契约。沿用 ADR-003：句柄式 C ABI、调用方缓冲区、原生异步执行、轮询取结果。网络等待不能发生在同步 FFI 调用里。

## 1. 新 C ABI 签名（冻结：实际已发布形态）

下面的 `runtime_id` 标识 `ak_pairing_start` 创建的配对运行时，`op_id` 标识异步控制操作，`task_id` 标识传输实例，三者都为 16 字节，语义不混用。**下列签名与两个已交付 .so 的实际导出一一对应，是唯一契约形态。**

```c
typedef struct AkEngine AkEngine;

/* ---------- 配对平面：每引擎可有多个配对运行时，按 runtime_id 区分 ---------- */

/* 初始化共享运行时，后台启动控制连接；返回不代表已经在线。 */
int32_t ak_pairing_start(
    AkEngine *engine,
    const char *spec_json,
    uint8_t runtime_id_out[16]
);

int32_t ak_pairing_create_start(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const char *spec_json,
    uint8_t op_id_out[16]
);

int32_t ak_pairing_redeem_start(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const char *spec_json,
    uint8_t op_id_out[16]
);

int32_t ak_pairing_list_start(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const char *query_json,
    uint8_t op_id_out[16]
);

int32_t ak_pairing_revoke_start(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const char *spec_json,
    uint8_t op_id_out[16]
);

int32_t ak_pairing_op_result(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const uint8_t op_id[16],
    char *buf,
    size_t cap
);

int32_t ak_pairing_poll_event(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    char *buf,
    size_t cap
);

int32_t ak_pairing_cancel_op(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const uint8_t op_id[16]
);

int32_t ak_pairing_stop_start(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    uint8_t op_id_out[16]
);

/* ---------- 调度平面：每引擎至多一个调度运行时 ----------
 * 调度与常驻接收使用同一个 runtime。以下五个引擎作用域入口不含
 * runtime_id（调度运行时按引擎唯一解析），snapshot/poll_event 也
 * 没有 query 参数（见本节末尾 ABI 裁决第 3 条）。 */

int32_t ak_sched_enqueue(
    AkEngine *engine,
    const char *spec_json,
    uint8_t task_id_out[16]
);

int32_t ak_sched_set_policy(
    AkEngine *engine,
    const char *policy_json
);

int32_t ak_sched_cancel(
    AkEngine *engine,
    const uint8_t task_id[16]
);

int32_t ak_sched_snapshot(
    AkEngine *engine,
    char *buf,
    size_t cap
);

int32_t ak_sched_poll_event(
    AkEngine *engine,
    char *buf,
    size_t cap
);

/* 以下四个 wave-23 入口按实际导出保留 runtime_id 实参：必须命名调度
 * 运行时绑定的那个配对运行时，否则 -4，绝不静默改绑。 */

int32_t ak_sched_listen_start(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const char *spec_json,
    uint8_t op_id_out[16]
);

int32_t ak_sched_retry(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const uint8_t task_id[16]
);

int32_t ak_sched_task_result(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const uint8_t task_id[16],
    char *buf,
    size_t cap
);

int32_t ak_sched_shutdown_start(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const char *spec_json,
    uint8_t op_id_out[16]
);

/* 遗留别名：ak_sched_start(engine, spec_json) —— wave-22 发布的调度
 * 绑定入口（两参数：engine + spec_json，无 runtime_id、无 op_id_out），
 * 与 ak_sched_listen_start 并存，绑定同一个（引擎唯一的）调度运行时；
 * 作为第 19 个 v2 导出保留，供已交付的调用方继续使用。 */
```

导出计数：**实际 19 个 v2 导出** = `ak_pairing_*` 9 个（start、create_start、redeem_start、list_start、revoke_start、op_result、poll_event、cancel_op、stop_start）+ `ak_sched_*` 10 个（上表 9 个冻结签名——listen_start、enqueue、set_policy、cancel、retry、task_result、snapshot、poll_event、shutdown_start——外加遗留别名 `ak_sched_start`）。`scripts/check_contracts.py` 检查 2 的"18 个冻结签名 + 1 个遗留别名"即此口径。

### ABI 裁决（2026-11-05 wave-26，集成者冻结）

本文件第十八波冻结稿曾为调度族规定了带 `runtime_id` 与 `query_json` 的五参数 `ak_sched_snapshot` 等签名；该形态从未实现。第二十五波终审 P0-01 确认：Dart 侧按文档声明了四参数 typedef 并真实调用，Rust 实际导出三参数，GUI 切换调度策略后读取快照时 Rust 会把 Dart 的短查询字符串当作输出缓冲区、把真正输出缓冲区的地址当作容量，造成原生堆越界写。集成者据此裁决：

1. **调度族 ABI 以实际已发布的 Rust 导出为唯一契约形态。** 两个已交付 .so（Windows DLL 与 Android `libak_ffi.so`）均为上表形状；上表即冻结契约，本文不再描述任何未实现的签名。Dart 侧 typedef 与全部调用点必须与上表逐参数对齐；Rust 侧以单元测试锁定每个导出的名称与调用形状（防止再次漂移），`scripts/check_contracts.py` 检查 2 继续核对符号集合。
2. **引擎作用域的调度入口不含 runtime_id。** runtime_id → 多调度运行时注册表从未实现；每引擎至多一个调度运行时（配对运行时可有多个，`ak_sched_start`/`ak_sched_listen_start` 的 spec 以可选 `runtime_id` 字段选择绑定的配对运行时）。wave-23 的四个入口（listen_start、retry、task_result、shutdown_start）按实际导出保留 `runtime_id` 实参，语义是"必须匹配调度运行时绑定的配对运行时，不匹配返回 -4"。
3. **query 过滤不进 ABI。** 快照查询过滤 Rust/Dart 都未实现，GUI 恒传 `{}`；`ak_sched_snapshot` 冻结为 `(engine, buf, cap)` 三参数，返回全量快照。
4. **v3 扩展路径：以新增符号扩展，不破坏现有 ABI。** 未来需要查询过滤、多调度运行时或逐运行体快照时，新增独立符号（例如 `ak_sched_snapshot_query`），不修改、不复用本节任何现有签名的参数形状——已发布导出的参数形状永久不变。
5. 配对平面（`ak_pairing_*`）签名与第十八波冻结稿一致，未变。

## 2. 冻结事项（冻结）

- 所有指针、输入类型和输出 ID 指针检查先于副作用。
- `start` 返回“已受理”，在线、完成、撤销同步等由结果确认。
- `-3 AK_ERR_BUFFER` 不消费事件、不重复生成邀请码、不重复 enqueue。
- 新操作使用 `request_id` 幂等；取结果重复调用只是读取同一结果。
- `ak_sched_enqueue` 幂等细则（wave-26 落实）：spec 携带 `request_id`（32 hex，可选）时，同一调度运行时内——重放同一 `request_id` 且规格相同 → 返回**原 task id**，不创建第二个任务、不重复任何副作用；同一 `request_id` 换不同规格 → `-2` 明确拒绝；去重判定先于一切副作用（关系解析、spec 登记、提交）。省略 `request_id` 时不做去重，每次调用受理一个新任务。
- 配对运行时停止语义（wave-26 落实，`ak_pairing_stop_start`）：先在同一同步规则下**原子进入终态**，再取消/等待所有启动子任务（含后台连接任务）；停止返回后该运行时**永不再上线**——迟到的连接完成不会复活它、不会发出 `runtime_ready`；已受理操作（含停止前已完成者）的终态结果仍可经 `ak_pairing_op_result` 读取；停止后的新操作立即失败，错误为 `stopped by caller`。
- 保留现有错误码含义；队列满等同步新错误需要明确的新状态码，不能伪装 internal error。
- 配对事件与传输事件分开轮询，避免把无 `job` 字段的配对事件塞进现有进度解析器。
- 同一 native 句柄由一个 Dart isolate 管理，调用串行。
- 每个身份目录只有一个运行时 owner；共享 facade 借用句柄，不重复 dispose。
- 新 ABI 提升版本并公布能力。旧 DLL 缺少新导出时明确不支持，不能让关键字段被旧解析器忽略后继续执行。

## 3. 状态码与 -3/-4 语义（冻结）

沿用 `crates/ak-ffi` 现有状态码（保留现有错误码含义）：

| 值 | 名称 | 语义 |
|---|---|---|
| `0` | `AK_OK` / `AK_PENDING` | 成功受理 / 轮询暂无结果 |
| `1` | `AK_EVENT` / `AK_DONE` | 轮询取到一条事件 / 异步操作完成 |
| `2` | `AK_FAILED` | 异步操作失败（详情见消息缓冲区） |
| `-1` | `AK_ERR_NULL` | 必需指针参数为空 |
| `-2` | `AK_ERR_INVALID` | JSON 畸形、字段类型错误或校验失败 |
| `-3` | `AK_ERR_BUFFER` | 调用方缓冲区过小——**不消费事件、不重复生成邀请码、不重复 enqueue**；以更大容量重试，last-error 消息给出所需大小 |
| `-4` | `AK_ERR_NOT_FOUND` | 未知的 job id / 已配对指纹 / runtime、op、task 标识 |
| `-5` | `AK_ERR_INTERNAL` | 锁中毒、运行时失败、panic |

新同步错误（如队列满）需要明确的新状态码，不能伪装 internal error（B0 侧对应 `QUEUE_FULL`，见 `scheduler.md` 第 7 节；计划未指定该类新状态码的数值，须由 B8 按“新 ABI 提升版本并公布能力”流程定夺后先回本契约补档再实现）。

## 4. JSON 契约示例（冻结）

```json
{
  "spec_version": 1,
  "request_id": "<32hex>",
  "ttl_secs": 0
}
```

```json
{
  "spec_version": 1,
  "request_id": "<32hex>",
  "invite_code": "<AKI1邀请码>"
}
```

```json
{
  "spec_version": 1,
  "request_id": "<32hex>",
  "pairing_id": "<32hex>",
  "source_roots": ["<本地路径>"],
  "peer": "<对端数据端点 ip:port>",
  "conflict": "fail",
  "multistream": false,
  "no_internet": false,
  "timeout_secs": 30
}
```

`code_id` 与真实对端从已保存的 `pairing_id` 关系解析，不能让 UI 任意覆写。源码路径不出本地应用层及设备直连。

**`peer` 为必填字段（wave-26 冻结说明，对应终审 P1-06 的 Rust/契约侧）**：本冻结版中调用方必须显式给出配对对端的数据端点（`ip:port`，引擎做 SocketAddr 校验，非法值 `-2`）；`pairing_id` 关系决定身份与信任（code group、已验证对端公钥与 pin），UI 不能覆写。由 Ready relation 解析终端数据端点是后续演进（随 §1 裁决第 4 条的 v3 新符号一起设计），本波不改此语义；Dart 绑定类型需补上该字段后，公共调度发送接口才完整可用。

`ak_sched_set_policy` / `ak_sched_start` 的策略字段（wave-26 落实，对应终审 P2-05）——两组模式词与八个限额全部生效，不认识的字段名明确 `-2` 拒绝，不允许"成功返回后按另一套值执行"：

```json
{
  "same_code": "serial",
  "cross_code": "parallel",
  "max_active_tasks_per_code": 4,
  "max_active_code_groups": 4,
  "max_queued_tasks": 256,
  "max_active_tasks": 16,
  "max_queued_tasks_per_code": 16,
  "max_pending_admissions": 16
}
```

字段语义与冻结范围见 `docs/contracts/scheduler.md` §3（本表键名为 FFI 层拼写：`same_code`/`cross_code` 对应契约的 `same_code_mode`/`cross_code_mode`）。快照 JSON 的 `policy` 对象同样包含全部八个字段。

新监听 spec 的 `expect` 仍必填，推荐显式 `"book"`。空簿时可以先运行配对服务，但文件监听保持未启用或拒绝全部，直到首个 Ready 配对完成；不能通过自动改成 `"any"` 启动。

## 5. 事件轮询模型（冻结）

- `ak_pairing_poll_event` 与 `ak_sched_poll_event` 分开轮询：配对事件与传输事件分开，避免把无 `job` 字段的配对事件塞进现有进度解析器。
- 事件以小 JSON 行经轮询（pull）取回（ADR-003 形态不变）；进度事件允许合并，但终态必须能通过任务查询补取（`ak_sched_task_result` / `ak_sched_snapshot`）。
- 计划未逐项列举配对/传输事件的枚举值；事件字段在 B8 与 Dart 绑定实现时必须先回本契约补档并通告集成者，方可冻结（不得直接进代码）。

## 6. CLI 子命令与 daemon 扩展（冻结，B.8 节）

建议命令：

| 命令 | 行为 |
|---|---|
| `pair invite [--ttl 10m \| --never]` | 生成并登记邀请码，只有持久 claim 成功后输出 |
| `pair redeem` | 默认交互输入邀请码 |
| `pair redeem --code-stdin` | 从标准输入读取，避免进入 shell 历史和进程参数 |
| `pair list --invites [--all]` | 分页列邀请码元数据；不输出明文秘密 |
| `pair revoke <code-id>` | 撤销邀请码，等待服务器确认或报告待同步 |
| `pair list` | 保持原有配对设备列表语义 |
| `pair code` | 保持旧六位短码功能，输出明确标记“指纹核对短码” |
| `send PATH... --pairing-id <id>` | 经已配对关系提交一个传输任务 |
| `send PATH... --pairing-id <id> --enqueue` | 入队即返回 task ID |
| `sched list` | 查看运行与排队任务 |
| `sched cancel <task-id>` | 取消一个任务 |
| `sched retry <task-id>` | 恢复同一实例 |
| `sched set-policy ...` | 动态修改两维模式并返回生效状态 |

`send PATH1 PATH2` 仍是一个多根任务。要提交两个独立任务，应调用两次或使用以后明确设计的批量入队接口。

新 daemon 参数：

```text
--pairing
--pairing-only
--signal-host HOST
--signal-port PORT
--signal-pin HEX

--same-code-mode serial|parallel
--cross-code-mode parallel|serial
--max-active-tasks N
--max-active-tasks-per-code N
--max-active-code-groups N
--max-queued-tasks N
--no-persist-queue

--expect book|...
--shutdown-mode drain|cancel
```

实施注意：

- 当前 CLI `--expect` 解析并不把字符串 `book` 作为显式分支，需补上并保持原来“省略时使用配对簿”的安全语义。
- `daemon --pairing-only` 允许只提供在线配对能力，不需要启动文件接收。
- CLI 管理命令通过本地 IPC 调用已有 runtime，不能每次独立登录信令服务。
- Windows 使用限当前用户的 named pipe；Linux 使用私有目录中的 Unix socket。Android GUI 内直接调用共享 Rust runtime。
- IPC 只传本地命令及状态，不做文件数据代理，也不监听公网地址。
- 没有常驻 runtime 时，命令明确提示启动方式，不能生成码后退出却声称持续在线可自动配对。
- 原 `receive --jobs N` 保留批处理语义；新并发资源上限使用 `--max-active-tasks`。

服务器命令增加：

```text
signal-server
  --tls-cert PATH
  --tls-key PATH
  --registry-db PATH
  --registry-id HEX
  --invite-max-active-per-owner N
  --redeem-rate-per-device N
  --redeem-rate-per-ip N
```

数据库、证书私钥、运行状态放在私有运行目录。缺少 TLS 或注册库时不能静默退回明文邀请码服务。

## 7. 验证门槛（B8 对照）

- 指针先检；`-3` 无副作用；异步请求；取消/释放；真实 DLL 双实例 e2e。
- B7（CLI）侧另按 B.9 表验证：旧命令回归；两进程入队仍串行；同身份不重复登录；秘密不进输出日志。

## 8. 任务面扩展：`ak_task_*` 八导出（wave-28 冻结，按 §1 裁决第 4 条新增符号）

v1 极简配对的产品模型（用户冻结）：三动作——生成任务码（选角色）/ 输码加入 / 连接列表+断开；连接后发送方选文件发送，接收方收到请求可接受/拒绝；设置仅一个默认保存文件夹。**全程零 IP/端口/指纹进 UI。** 本节符号全部为新增，已发布签名的参数形状一字不动；除下表两个例外，状态码、`-3` 无损重试、`request_id` 幂等、`last_error` 全路径等纪律与 §2/§3 完全一致。

> 本节签名用 ```abi 围栏（非 ```c）：检查 2 的 18 签名口径只数 §1 的 c 围栏，任务面导出在其符号集核对范围之外（前缀 `ak_task_` 不属于 `ak_pairing_`/`ak_sched_` 前缀过滤器），两侧约定以本表为准。

```abi
int32_t ak_task_start(
    AkEngine *engine,
    const char *spec_json,
    uint8_t runtime_id_out[16]);

int32_t ak_task_create_code(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const char *role_and_cands_json,
    uint8_t op_id_out[16]);

int32_t ak_task_join(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const char *code_and_cands_json,
    uint8_t op_id_out[16]);

int32_t ak_task_send(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const uint8_t conn_id[16],
    const char *paths_json,
    uint8_t op_id_out[16]);

int32_t ak_task_list(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    char *buf,
    size_t cap);

int32_t ak_task_disconnect(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const uint8_t task_id[16]);

int32_t ak_task_set_save_dir(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const char *path);

int32_t ak_task_consent(
    AkEngine *engine,
    const uint8_t runtime_id[16],
    const uint8_t job_id[16],
    int32_t accept);
```

**结果与事件通道不新增符号**（wave-28 裁决）：任务 op 结果经既有 `ak_pairing_op_result(engine, runtime_id, op_id, buf, cap)` 读取、事件经既有 `ak_pairing_poll_event(engine, runtime_id, buf, cap)` 读取、取消经 `ak_pairing_cancel_op`——`runtime_id` 指任务运行时即可；两平面表不相交，配对平面先查、未命中落任务面，`-4` 语义不变。

### 8.1 spec JSON

```json
// ak_task_start（注册中心配置沿用现有环境/dart-define 解析：Dart 层
// 从 AK_REGISTRY_ADDR/AK_REGISTRY_ID 或 --dart-define 编译默认解析后传入）
{"spec_version":1,
 "registry":"127.0.0.1:7200","registry_id":"<64hex>",
 "id_dir":"C:/ak/id","device_name":"phone","save_dir":"D:/inbox"}

// ak_task_create_code（wave-29：ttl_secs 省略或 0 映射为任务码默认
// 600 秒——v1 任务码绝不以 registry 的"长期有效"语义滞留）
{"spec_version":1,"role":"sender|receiver",
 "candidates":["192.168.1.4:7000"],"ttl_secs":0,"request_id":"<32hex>"}

// ak_task_join（code 为真实 AKI1 展示形状：AKI1 前缀 + 32 个
// Crockford Base32 符号，去分隔符规范化后共 36 字符）
{"spec_version":1,"code":"AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX",
 "candidates":[],"request_id":"<32hex>"}

// ak_task_send（paths 为本地路径数组）
{"spec_version":1,"paths":["C:/pick/a.jpg"],"request_id":"<32hex>"}
```

`ak_task_start` 校验并保存 registry 端点与 pin、在 `id_dir` 下引导身份；默认保存夹为 `<id_dir>/downloads`。`role` 决定生码方角色，加入方得对侧角色；receiver 角色生码会先绑定真实常驻接收端（wildcard bind），候选拨号地址由本机接口推导（接口 IPv4 + 显式 loopback 兜底，LAN 优先排序）。

### 8.2 op 结果 JSON（经 ak_pairing_op_result）

```json
{"op":"task_create_code","code":"AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX",
 "role":"receiver","code_id":"<32hex>","expires_ms":0,"endpoint":"192.168.1.4:53112"}
{"op":"task_join","conn":"<32hex>","code_id":"<32hex>","role":"sender",
 "peer_endpoint":"192.168.1.4:53112","probe":"unverified"}
{"op":"task_send","conn":"<32hex>","files":2,"bytes":123456,
 "peer":"192.168.1.4:53112"}
// 失败形态同配对面：{"op":"task_join","error":"unknown task code \"…\""}
```

### 8.3 事件类型（经 ak_pairing_poll_event，任务运行时）

```json
{"event":"runtime_ready","runtime":"<32hex>"}
{"event":"op_done","op_id":"<32hex>","op":"task_create_code|task_join|task_send"}
{"event":"op_failed","op_id":"<32hex>","op":"…"}
{"event":"connected","conn":"<32hex>","code_id":"<32hex>","role":"sender|receiver",
 "peer_endpoint":"ip:port","code":"AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX"}
{"event":"probe_result","conn":"<32hex>","endpoint":"ip:port","reachable":null}
{"event":"incoming_request","job":"<32hex>","conn":"<32hex>",
 "root":"photos","files":3,"bytes":123456,"saveDir":"D:/inbox"}
{"event":"done","conn":"<32hex>","saveDir":"D:/inbox","files":3,"bytes":123456}
{"event":"disconnected","conn":"<32hex>","reason":"local|peer|receiver_failed|done"}
{"event":"code_cancelled","code_id":"<32hex>"}
{"event":"handshake_failed","code_id":"<32hex>","reason":"peer certificate rejected|handshake transport failure"}
{"event":"consent_expired","job":"<32hex>","conn":"<32hex>"}
{"event":"request_closed","job":"<32hex>","conn":"<32hex>","reason":"disconnected"}
```

wave-29 增补：`disconnected` 新增 `done` 理由（单传输任务的完成终态——双端连接条目同时收敛，见 §8.4）；`code_cancelled` 在生码方取消一个未被加入的码（`ak_task_disconnect` 按 `code_id`）时发出。

wave-30 增补（复审 R29 修复）：`incoming_request` 携带 `saveDir`（该请求将落盘的真实目标快照，UI 横幅显示用）；`consent_expired` 是同意窗口 30 秒到期的**请求终态事件**（native 为期限事实源，UI 本地计时仅兜底）；`request_closed` 是连接断开驱动 pending 请求终结的终态事件；`handshake_failed` 是单次对端握手失败的有界记录（固定词表，**不代表监听器终结**——接收循环继续）。

wave-31 修订（复审 R30-P3-01）：`handshake_failed` 形状修正为与原生输出一致——`code_id` 为 32hex 任务码 id **字符串**（非整数错误码），且**无 `conn` 字段**（未配对的失败握手没有可归属连接）。

wave-31 修订（复审 R30-P2-02/P2-03、P2-06 残项）：**双方连接事实共享同一码 id**——`connected` 事件（双端各一）与 `ak_task_join` 结果均携带 `code_id`（32hex，与 create 结果一致），加入侧据此填充 conn→码映射兜底；**发送/接收进度行统一以连接 id 为 `job`**（接收行原"当前码 id"废止，见 §8.4）；`incoming_request` 登记与发布为同一临界区（连接已断后到达的 Offer 不再发布事件、直接拒绝，`request_closed` 之后的迟到 `incoming_request` 结构性不可能，见 §8.4）；接收保存夹为**每次传输一个不可变绑定**——`incoming_request.saveDir` 与 `done.saveDir` 携带引擎实际落盘的同一目录（`done.saveDir` 为新增字段：实际落盘结果，UI 完成态可显示真实目录）。

wave-31 实现边界（P2-06）：冻结语义原定"save_dir 在同意决定时刻解析"（接受时从当前设置读取、落盘跟随）。ak-engine 的全部接收入口在入口处（Offer/同意点之前）即绑定 `LocalSaveTarget`，同意应答通道为纯 `oneshot<bool>`，FFI 层无法在决定时刻把新目录传给已进入引擎的接收——本波落地的是 wave-28 原报告允许的替代方案"绑定并展示同一个不可变实际目标"：绑定发生在传输开始（连接接受）时刻，事件与 `done` 展示同一绑定值，事件/横幅与实际落盘**永不错位**；pending 同意期间修改设置对进行中传输无效（对下一个请求生效）。

`ak_task_list` 快照：`{"runtime":"<32hex>","save_dir":"D:/inbox",
"connections":[{"conn":"<32hex>","role":"sender","state":
"probing|connected|unreachable|disconnected","peer_endpoint":"ip:port",
"code":"AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX"}]}`。

### 8.4 行为语义（冻结；wave-29 修订处标注）

- **幂等（wave-29 P2-01 修订）**：三个异步 op 均按 `request_id` 幂等——去重判定（查找、冲突拒绝、op id 预留）是**同一临界区内的一次原子受理**，并发同请求得到同一 op id；canonical spec 以 `操作类型|conn_id|spec` 为域，同 id 异操作/异连接/异 spec 一律 `-2`；**已受理请求的重放先返回原 op 与原答案**（即使其连接此后已断开），仅全新请求才做连接存活校验。
- **接收侧身份准入（wave-29 P1-04 新增）**：接收方常驻接收的 TLS `authorize` 只接受**该码已完成配对的已验证对端公钥指纹**，且仅当该配对连接仍存活；配对完成前一切拨号在 TLS 准入门被拒绝并关闭，**不消耗接收器**（监听继续）。`incoming_request` 归属按 Offer 实际发送方指纹匹配连接（而非仅按监听码查表）；无法归属的 Offer 无提示直接拒绝。
- **发送**：`ak_task_send` 走既有引擎发送，对端 endpoint 候选按 LAN 优先排序逐个尝试；连接预算沿用 wave-26 P1-01 的 10 秒分拆预算（自引擎 `Connecting` 信号起算，源扫描不计入；连接建立后不再受其约束），候选间短暂停顿后回退；连接后失败不再换址重试。wave-29：整表纯连接级失败（从未建立连接）经短暂停顿后**整体重试**（默认至多 3 轮）——对端配对簿记可能滞后加入方一拍，身份校验不受影响。
- **接收同意（wave-29 P2-07 修订；wave-31 R30-P2-03 修订）**：Offer 级 `TransferConsent` 为**非阻塞形状**（回调登记 pending oneshot 后立即返回，等待由引擎异步 await，绝不阻塞 Tokio worker）；每个入站 Offer 铸一个 pending 请求 id，发 `incoming_request` 事件，挂起直至 `ak_task_consent` 决定、**30 秒窗口自动拒绝**（计时器任务，绝不自动接受）、该连接断开或运行时 teardown（后三者同样以拒绝收场）。**pending 插入、连接存活复查与 `incoming_request` 发布是同一临界区内的一次原子登记**：与连接断开竞争时，要么断开先行——复查见 `disconnected`，Offer 当场拒绝、**不发布任何事件**；要么登记先行——断开的 `request_closed`（同一锁序发布）只能落在 `incoming_request` 之后。迟到 `incoming_request` 在 `request_closed` 之后出现是结构性不可能。决定即移除条目：`ak_task_consent` 对未知/已决定请求返回 `-4`。拒绝经 DECLINED 线上语义告知对端、零字节落盘。
- **常驻接收（wave-29 P2-05/P2-06 修订；wave-31 P2-06 残项修订）**：一个码绑定一个端口、一个**循环服务**多连接——陌生拨号被拒后续继监听；**拒绝不是任务终态**（同码可重试，再次发送产生新的 `incoming_request`）；**完成是单传输任务终态**（`done` 后双端连接条目收敛为 `disconnected`/`done`，接收器停止）。保存夹为**每次传输开始（连接接受）时的一个不可变绑定**：引擎 `ReceiveSpec` 实际写入的目录、`incoming_request.saveDir` 与 `done.saveDir` 三者携带**同一个绑定值**（事件不再二次读取全局设置，横幅与实际落盘永不错位）；`ak_task_set_save_dir` 对后续任务生效，pending 同意期间的修改对进行中传输无效。
- **断开（wave-29 P2-03/P2-04 修订）**：`ak_task_disconnect` 幂等，且覆盖**已生成未加入的码**——以 create 结果的 `code_id` 为参数即撤销 registry 注册行、释放接收器/echo 监听并发出 `code_cancelled`（重复取消静默 `AK_OK`；未知 id `-4`）。连接断开时：本侧 `local`，注册中心撤码；**空闲时**（无在传输的 QUIC 连接可供对端感知）额外向已配对对端通告的数据端点发起**认证的 TASK-CLOSE 帧通知**（TLS 钉扎配对指纹、复用既有 Error 帧语义），持有监听的对端立即以 `ByPeer` 收敛为 `disconnected`/`peer`；纯发送侧未绑定数据监听，无法被空闲通知触达，其下次发送连接失败即为诚实结果。同引擎对端另有即时缓存通知。引擎 teardown 对本运行时未撤销的码做**有界尽力撤码**（应用退出不遗留无主 active 行）。
- **断开原因域（wave-30 修订）**：`disconnected` 的四个理由（`local|peer|receiver_failed|done`）均为**数据面终态**；注册/协调连接断开不产生任何任务终结事件（任务只在数据面链路丢失、对端动作或本地断开时终结），`receiver_failed` 仅由真实数据面链路丢失触发。`ak_task_list` 的 `unreachable`（wave-30 P2-09）= 探测全部失败的如实状态（不再无条件跃迁 connected），终态不被探测结果覆盖。
- **任务码 TTL（wave-29 P2-04）**：`ttl_secs` 省略或 `0` 在 FFI 层映射为库默认 600 秒；不再把 0 原样作为 registry 的"长期有效"语义上送。
- **发送/接收进度（wave-31 R30-P2-02 修订）**：进入引擎级 `ak_poll_progress` 队列，**发送行与接收行的 `job` 均为连接 id 的 32hex**——配对事务 id 是双方连接事实唯一共同持有的关联键（发送行自始如此；接收行原"当前码 id"废止——加入侧只握 connId，码 id 键的接收进度对它不可见）。双端 `connected` 事件与 `ak_task_join` 结果携带共享 `code_id`（32hex），加入侧据此建立 conn→码映射。

### 8.5 实现事实（wave-29 终稿；wave-35/36 增补）

wave-28 的并行占位槽位已全部落地为真实路径：任务码经 ak-session `TaskPairing` 在真实注册中心 claim/redeem（`max_redeems=1`，配对 transcript 至 `PairReady` 跨引擎/跨设备成立）；加入方钉扎的公钥来自中继的已验证对端指纹；`probe_result` 由真实 UDP 探测回答（echo responder 带 wave-29 防反射约束：应答携带全零 nonce 且永不回答应答本身，并有每来源应答预算）；接收侧身份准入与同意归属按 §8.4；空闲断开通知与码取消按 §8.4。已知诚实边界：纯发送侧设备空闲时无法被主动通知（无数据监听），其发现时机为其下一次发送失败；探测结论只证明候选端口的 UDP 可达性，不构成身份授权。

**控制连接维护（wave-35 keeper，提交 `e7fc1e0`）**：每个任务运行时的共享 registry 控制连接由常驻 keeper 维护——每 **20 秒**一次保活 ping＋连接状态变 `Disconnected` 时自动 `reconnect()`（配对订阅跨重连保留，owner loop 在断连窗口后继续应答 redeem）。保活预算的依据是 registry v2 的 **30 秒 `FRAME_IO_DEADLINE` 静默断连**（完整入站帧静默 30 秒即断开，出站消息不刷新该期限；历史上 45 秒间隔已被实测切断）——**不是** 120 秒 SESSION_TTL（源码注释中该归因以 30 秒帧期限为准，2026-11-09 终审 R36-P1-01/P2-04 更正）。诚实边界：一批重连尝试全部失败后客户端停留在 `Connecting`，keeper 不再自愈（R36-P1-02，未修）；`ak-cli task` 等待链没有等价 keeper（R36-P1-01，未修）；同设备多实例共用身份会互踢控制连接（R36-P2-04，未修）。

**C07/C12 实现边界（wave-33b，提交 `bcf5de0`）**：C07＝FFI 绑定租约——facade 在 `engine_free` 之前把租约翻为 not-alive，借用句柄的后续调用得到显式 `StateError` 而非触达已释放指针（Dart 侧该租约撤销直接闩入 C12 故障态）。C12＝Dart 事件泵预算与故障闩——每 tick 最多 64 条事件＋64 条进度行；**连续 10 次**原生轮询失败（事件/进度轮询异常与坏进度行 alike，且自 2026-11-09 R36-P2-02 起只有**两通道当 tick 均成功**才清零计数）闩上终态 `engineFailed`：泵停止、服务拒绝一切后续操作；自 R36-P2-03 起该闩经可回放的 `engineHealth` 流送达 UI（主页故障卡），待处理操作与接收等待以同一故障及时结束。恢复＝仅重建服务（GUI 侧即重启应用）。
