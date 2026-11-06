# FFI 与对外接口契约 v2（ak_pairing_* / ak_sched_* C ABI、JSON、错误码）

> **冻结于第十八波（2026-11-04），修改需集成者核准并同步 ADR-004。**
> §1 已按 **2026-11-05 wave-26 集成者 ABI 裁决**改为实际已发布导出的形态（裁决记录见 §1 末尾）；其余章节仍为第十八波冻结稿，本次随裁决补充 §2 幂等/停止细则与 §4 `peer` 必填说明。
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
