# 批次 K 修复轮四独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `31cf22c..a8bb3b9`，代码与文档修复为 `46473fc`，哈希回填为 `a8bb3b9`。被审 tip 固定为 `a8bb3b9fa103329b5c4bd410fab45d689da5d8b1`。结论为 **REJECTED**；该轮未合并、未推送 main、未删除批次分支。报告及状态同步出具时仅保留本地，随后由执行方在 `e8f811d` 提交，未改写该轮被审代码。本页保留历史结论，修复轮五的新判断见 [K6 复核](REVIEW_2026-11-15-batchK6.zh-CN.md)。

逐项判断唯一记录在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)：K3-N1/K2-N1/K2-R2/K2-R3 对应 K-deploy，K2-R4 对应 CR-P3-20，K2-R5 对应 K-handoff。命令、计数、CI 运行编号与材料目录集中在 [K 交接 §15](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#15-修复轮四独立复核证据2026-11-15)。下列源码行号均指固定 tip。

已通过当前 git 凭据读取 GitHub run/jobs/branches API，确认 tip CI 的完整 head SHA、分支和成功结果。`a8bb3b9` 确实只替换交接与状态表中的修复提交号。K4 指定输入的修复已有实际行为证据；以下是对相同要求追加的边界反例，不把已修复的输入重新判成失败。

## 1. 阻断发现

### K3-N1 / P1：标准 D-Bus ENOENT 错误仍被当成 unit 不存在

`scripts/deploy_linux.sh:1220` 仍将包含 `No such file` 的任意诊断判为 `PU_EXISTS=absent`。真实 systemd 连接错误也会包含该子串：

```text
Failed to connect to bus: No such file or directory
```

上游 systemd v255 的 [systemctl-edit.c:35](https://github.com/systemd/systemd/blob/v255/src/systemctl/systemctl-edit.c#L35) 在 `verb_cat` 的 `acquire_bus` 失败时直接返回；[systemctl-util.c:49](https://github.com/systemd/systemd/blob/v255/src/systemctl/systemctl-util.c#L49) 将连接错误交给 `bus_log_connect_error`；[bus-util.c:54](https://github.com/systemd/systemd/blob/v255/src/shared/bus-util.c#L54) 输出 `Failed to connect to bus: %m`。ENOENT 展开即上述标准诊断。

对已有 enabled/active 服务的离线模型，令 phase456 中所有 systemctl 调用持续返回该诊断和失败码。真实生成脚本仍捕获为 `absent::`，打印 `nothing to stop`，安装新 binary 并写入新 units；直到 `daemon-reload` 失败才进入恢复，最终报告 INCOMPLETE。不可判定的状态没有在切换前拒绝。此夹具中旧 binary/unit 成功恢复，没有观察到旧文件被删除。

更窄的故障注入只令 `systemctl cat` 返回上述错误，其他操作成功：捕获结果成为 `absent:enabled:active`，phase456 成功退出且完全没有 stop 调用；磁盘为新镜像，运行进程仍持有旧镜像。这里只执行安装阶段，不把它写成完整部署/phase7 验收成功；既有 MainPID 核验仍能独立识别旧进程。

K4 的 `No files found for <unit>.` 首次安装输入已修复，未识别的普通 D-Bus stub 文案也会拒绝。但本轮测试的 `systemctl: D-Bus connection error (stub)` 不含缺失关键词，没有覆盖真实 ENOENT 连接错误。修复应使用可靠的结构化存在状态，或严格限定到该 unit 的缺失诊断并固定 locale；连接、权限、I/O 错误以及与 active 状态冲突的结果均须在切换前拒绝。

### K2-N1 / P1：`deactivating` 被当成已停止，恢复仍可留下新版进程却报告成功

`scripts/deploy_linux.sh:1337` 的 `qstate()` 将 `inactive|failed|deactivating` 全部归为返回值 1。原 inactive 分支从 `:1394` 起在得到 1 后跳过 stop 和确认；原 absent 分支 `:1348` 起也使用该分类。

真实生成 phase456 的状态性复现如下：旧 registry/STUN 均 disabled/inactive；`enable --now` 成功启动新版 registry，随后 STUN 启动失败；此时将 registry 置为合法的异步停止状态 `deactivating`，保留其 MainPID 和运行镜像。恢复后磁盘与 enable 状态已回退，但输出为：

```text
RECOVERY OK: previous binary/units restored, enable/active state re-asserted and running images verified
registry.active = deactivating
registry.pid = 5001
registry.running.bin = git deadbee（新版合成镜像）
```

调用轨迹只有切换前的 registry stop，恢复阶段没有等待停止完成或再次 stop。`deactivating` 并不证明进程已退出：systemd v255 [service.c:62](https://github.com/systemd/systemd/blob/v255/src/core/service.c#L62) 将 `SERVICE_STOP`、`SERVICE_STOP_SIGTERM`、`SERVICE_STOP_SIGKILL` 等映射为 `UNIT_DEACTIVATING`，停止路径还会等待进程并设置计时器。本复现模拟恢复查询遇到另一 stop job 或慢停止过程，属于离线状态模型，不是原生 systemd 部署实验。

仓外单变量对照仅从提取文本的终态集合移除 `deactivating`：同一夹具即进入保守 stop，最终 inactive、无 PID、无新版镜像。此对照用于定位原因，不是本轮代码修复。修复要求是把过渡状态与已确认终态区分；等待/停止并确认结束，无法确认时报告 INCOMPLETE。

K4 的无 stdout 恢复查询失败输入已经按预期保守 stop 并报告 INCOMPLETE；旧 active 服务的查询失败也会 restart 到旧镜像；捕获 unknown 在停服前拒绝。这些有效修复不覆盖上述合法过渡状态。

### K2-R3 / P1：标准名 registry 被排除在全目录查询之外，vendor unit 的身份参数仍会丢失

`scripts/deploy_linux.sh:902` 对标准名 unit 仍只检查 `/etc/systemd/system/$u.service`。新增全目录发现的 `:970` 显式排除 `ak-registry-server.service`，因此标准名 registry 位于 `/usr/lib/systemd/system` 等合法目录时不会输出 `ExecStart=`。`:1660` 取得空配置，`:1771` 的真实 `compose_registry_exec` 随即选择默认身份目录。

合成夹具将 cgroup 设为 `ak-registry-server.service`，unit 仅放在 `/usr/lib/systemd/system/ak-registry-server.service`，内容包含：

```text
ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /var/lib/ak-legacy
```

该路径下的合成 key/db 和带完整参数的 `/proc/4242/cmdline` 均已准备。真实 phase1 成功退出，却同时打印标准 unit absent 和该标准 unit 正在占用端口。继续执行真实 preflight decision、phase 4–6 配置决策及 compose 函数，仍成功退出，所选结果为：

```text
existing registry data dir: none-found
existing registry ExecStart: none
registry-port occupant unit: none-detected
CHOSEN_EXEC=<REMOTE_BIN> registry-server --port 17200 --id-dir <REMOTE_DATA>
CHOSEN_DATA_SOURCE=
CHOSEN_RW=<REMOTE_DATA>
```

已识别的现有 registry 配置被丢弃，新 unit 指向另一身份目录且没有迁移源。这已经机械执行到真实配置选择；没有执行后续部署，不能写成真实身份已轮换。

K4 的 Permission denied 目录/key 反例、旧名 `oldregistry.service` 的 vendor 目录反例、配置探测失败与无处定位拒绝均已有有效修复。余项要求对所有已识别的 registry 占用者取得其实际加载配置，包含标准名；不能用标准名排除条件假定其必在 `/etc`。配置不能取得时应在切换前拒绝。

### K2-R4 / P2：合法圆括号标题仍让当前批次 archive 链接绕过完整检查器

`scripts/check_repository.py:89` 的公共 regex 只支持单引号、双引号标题，没有 CommonMark 同样合法的 `(title)` 形式。STATUS 专项 `:330` 和通用链接 walk `:525` 共用该 regex，因而同时漏掉这类链接。

从 git 导出完整候选树，创建真实存在的 `docs/archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md`，内容为既有矛盾规则会拒绝的“已核”与“待补”。STATUS 的“当前状态”“交接入口”保留规范链接，“最新验证”的 archive 链接使用圆括号标题。精确输入可由以下代码生成：

```python
name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
canonical = chr(91) + "K](" + name + ")"
archive = chr(91) + "K](archive/" + name + " (handoff evidence))"
status = ("# 项目状态\n\n## 当前状态\n\n" + canonical
          + "\n\n## 最新验证\n\n" + archive
          + "\n\n## 交接入口\n\n" + canonical + "\n")
```

完整候选检查器成功退出，返回 `passed=true`、`errors=[]`，检查对象仍为规范文档，archive 入口被完全漏过。CommonMark [规范示例 505](https://spec.commonmark.org/0.31.2/#example-505) 明确展示三种标题形式等价。另用 destination 前空格或换行的合法形式，亦得到相同漏检；合法性见 [示例 510](https://spec.commonmark.org/0.31.2/#example-510)。

K4 的跨行 label 与双引号 title 精确输入已经从旧版放行翻转为带正确起始行号拒绝，单行两个顺序与历史缺失目标仍拒绝。余项修复应统一解析实际链接，或明确并强制受支持的输入形式，对不能判定的交接入口拒绝；不能依靠未强制的“supported form”注释静默跳过合法 Markdown。

## 2. 保留的有效修复与文档核对

K2-R2 的严格 stop 与 phase7 MainPID→`/proc/<PID>/exe` 核验没有被撤回，旧路径和 deleted executable 回归继续有效。上述 K3-N1 的误分类发生在严格 stop 之前；K2-N1 的过渡态反例发生在恢复阶段，分别记录，不撤销既有运行进程核验结论。

K2-R5 的 skip-doctor 摘要与真实旧 helper/旧部署阶段对照继续有效。指定 git 基线实际执行，没有用 skip 掩盖此次行为证据。部署 baseline 缺 marker 可 SkipTest、SSH helper 缺 marker 为 AssertionError 的声明边界已经写入当前测试注释，不再把“只有 git 不可读才 skip”泛化。

K4 报告、交接 §13、STATUS、状态表和 K3 历史注记确实随 `46473fc` 入库。但候选的交接开头、§13、K4 报告开头及状态表尾注仍称 K4 同步仅本地，STATUS 与 K-CI/main 行也仍停在 K4。此次同步将其标为“出具时本地，随后随 `46473fc` 入库”，并更新 K5 入口与证据。文档历史归属的纠正不替代代码余项修复，也不要求为 CI 运行编号再造被审提交。

## 3. 重放与后续处理

从本仓库根目录执行仓外复核脚本：

```powershell
python -u "$env:TEMP\ak-k5-v255-contrast.py"
python -u "$env:TEMP\ak-k5-probes-repro.py"
python -u "$env:TEMP\ak-k5-vendor-repro.py"
python -u "$env:TEMP\ak-k5-recovery-boundary-repro.py" .
python -u "$env:TEMP\ak-k5-recovery-confirm.py" .
python -u "$env:TEMP\ak-k5-checker-replay.py"
```

部署重放使用实际 generator/决策代码和合成 fixture；恢复脚本另将删除路径映射到合成目录并拒绝越界。checker 从 git 导出完整树，逐案修改导出树 STATUS 并运行其完整检查器。runner 成功只表示重放完成；应读取各阶段返回值、最终进程/配置状态和 JSON。材料目录与集成者独立重放结果见交接证据表。

后续仍在当前批次分支修复上述边界并补入行为回归，再交独立复核决定合并。本轮只同步文档，保全本地/远端 main 和候选代码；没有提交、推送或删除分支。没有生产 SSH、Linux 原生 systemd/权限部署、物理双机或抓包验收；离线模型和上游源码核对不能写成真实对端身份、路径或生产验证。ADR-001 与禁止 relay/代理回退的产品决策保持不变。
