# 批次 K 修复轮五独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `a8bb3b9..9c2d3bd`，代码与文档修复为 `e8f811d`，哈希回填为 `9c2d3bd`。被审 tip 固定为 `9c2d3bdbc8d8118f0c0af4d005aa9346db4c05a1`。结论为 **REJECTED**；该轮未合并、未推送 main、未删除批次分支。报告及状态同步出具时仅保留本地，随后由执行方在 `bb1f673` 提交；本页保留历史结论，新候选判断见 [K7 复核](REVIEW_2026-11-15-batchK7.zh-CN.md)。

逐项判断唯一记录在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)：K3-N1/K2-N1/K2-R2/K2-R3 对应 K-deploy，K2-R4 对应 CR-P3-20，K2-R5 对应 K-handoff。命令、计数、CI 运行编号与材料目录集中在 [K 交接 §17](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#17-修复轮五独立复核证据2026-11-15)。下列源码行号均指固定 tip。

已用当前 git 凭据读取 GitHub run/jobs/branches API，确认 tip CI 的完整 head SHA、分支和成功结果。哈希回填没有代码变化。K5 指定输入均有有效修复；以下分别记录尚未满足的边界与本轮引入的检查器回归，不撤销已经成立的行为证据。

## 1. 阻断发现

### K3-N1 / P1 余项：unit 文件缺失但服务仍 active，捕获依然放行并跳过 stop

`scripts/deploy_linux.sh:1254` 把指定 unit 的缺文件诊断判为 `absent`，但 `:1287` 只在 `PU_EXISTS=exists` 时验证运行状态。随后 `stop_unit` 的 `:1269` 分支无条件打印 `nothing to stop`。这仍未满足 K5 已明确要求的“与 active 状态冲突的结果须在切换前拒绝”。unit 文件缺失并不证明服务没有运行进程。

准确触发窗口是 phase1 已成功取得配置之后、构建或 staging 到 phase456 之间，unit 文件被删除并执行 `daemon-reload`，原服务继续运行。初始 phase1 若已看见缺配置的已识别占用者会拒绝；本发现针对随后安装阶段的再次捕获，不能把这两个时间点混为一谈。

离线模型从已运行的旧 registry/STUN 开始，只移除合成 unit 文件并保留 active、MainPID 与旧运行镜像；没有强制覆盖 `cat` 诊断。stub 根据缺文件自然返回 `No files found for <unit>.`，`is-enabled` 返回 `not-found`，`is-active` 返回 `active`。真实生成的 phase456 输出：

```text
switchover begins; captured unit state:
ak-registry-server:absent:not-found:active;
ak-stun-server:absent:not-found:active;
unit ak-registry-server.service not present; nothing to stop (K3-N1)
unit ak-stun-server.service not present; nothing to stop (K3-N1)
installed: ... (ak-cli 0.1.0 (git deadbee stub))
```

安装阶段成功退出，磁盘已变成新版，原 MainPID 与旧运行镜像仍在，未执行 stop。`46473fc` 同一夹具也放行。仓外原因定位对照只在候选捕获条件中增加 `absent && active` 拒绝，即在 switchover 前失败，旧磁盘和旧进程保持原样；该对照不是仓内修复。

systemd v255 上游语义支持这个状态：[systemctl-util.c:533](https://github.com/systemd/systemd/blob/v255/src/systemctl/systemctl-util.c#L533) 在 `LoadState=not-found` 时进入输出缺文件诊断的 finish 路径，而同文件 [unit_exists:725](https://github.com/systemd/systemd/blob/v255/src/systemctl/systemctl-util.c#L725) 使用 `!not-found || !inactive`，明确同时考虑运行状态。已核对的 reload 序列化、反序列化与 coldplug 路径会恢复既有 MainPID/运行状态，缺 fragment 则记录为 `UNIT_NOT_FOUND`。这是源码旁证加离线模型，不是原生 systemd 实验。

修复要求：捕获和 stop 决策同时核对存在性与运行状态。缺文件却 active、处于过渡态或运行状态不可判定时，不得当成普通首次安装跳过；无法建立可恢复前态时，应在切换前拒绝。保留真正缺席且 inactive 的 v255 首次安装回归。

这里只证明 phase456 错误放行，不声称完整部署或 phase7 验收成功。既有 phase7 对旧运行镜像的检查仍能独立拦截，K2-R2 的结论保持。

### K2-R3 / P1 余项：旧名 registry 的 drop-in 被忽略，迁移仍可选错身份目录

`scripts/deploy_linux.sh:928` 的 `find_unit_file` 定位主 fragment，`:1008` 直接对它执行 `sed` 提取 `ExecStart`。`:1701` 采用该输出作为配置来源，虽然 phase1 也输出了真实 cmdline，却未用来核对有效参数。`:1812` 与 `:1823` 随即将主文件中的参数用于新标准 unit 的命令和 `ReadWritePaths`。

合法输入为旧名 `oldregistry.service`，主文件与 drop-in 分别包含：

```ini
# /usr/lib/systemd/system/oldregistry.service
[Service]
ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /var/lib/ak-default
```

```ini
# /etc/systemd/system/oldregistry.service.d/override.conf
[Service]
ExecStart=
ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /var/lib/ak-legacy
```

cgroup 识别到该旧名 unit；NUL 分隔的 `/proc/4242/cmdline` 带实际参数 `/var/lib/ak-legacy`；合成 key/db 只放在该目录。真实 phase1、preflight decision、phase 4–6 decision 与 `compose_registry_exec` 均执行，候选仍成功选择：

```text
cmdline: ... registry-server --port 17200 --id-dir /var/lib/ak-legacy
old-registry-unit: oldregistry.service
ExecStart=... registry-server --port 17200 --id-dir /var/lib/ak-default
CHOSEN_EXEC=<REMOTE_BIN> registry-server --port 17200 --id-dir /var/lib/ak-default
CHOSEN_DATA_SOURCE=
CHOSEN_RW=/var/lib/ak-default
CHOSEN_OLD_REGISTRY=oldregistry
DECISION_DONE
```

`46473fc` 同一输入也丢失实际身份目录。旧名 unit 会被停用，`oldregistry.service.d` 不属于新的 `ak-registry-server.service`，无法靠旧 drop-in 自动延续这些参数。这里已证明实际配置选择错误；没有继续安装或声称真实身份已轮换。

本机已有的 systemd v255 [unit.c:1443](https://github.com/systemd/systemd/blob/v255/src/core/unit.c#L1443) 明确先加载 fragment 再加载 drop-in；[load-fragment.c:872](https://github.com/systemd/systemd/blob/v255/src/core/load-fragment.c#L872) 中空 ExecStart 赋值清空旧命令列表。因此本夹具的覆盖规则有效。

修复要求仍是取得已识别占用者实际生效的配置并保留身份/数据库参数，或在不能判定时拒绝。单独找到主文件不足以满足该要求；应覆盖 drop-in，并校验配置与运行实例之间的相关差异。本轮标准名 vendor unit 的原反例已正确保留 legacy 目录，不能用该成功输入代替这个旧名覆盖场景。

### K2-R4 / P2 新回归：合法多行标题由旧版拒绝变成新版静默漏检

`scripts/check_repository.py:96` 把双引号/单引号标题的字符集从允许换行改成排除 `\n`。STATUS 专项 `:340` 与通用链接扫描 `:535` 共用该 regex，因此同时漏掉包含真实换行的合法标题。

从 git 导出完整 `a8bb3b9` 与 `9c2d3bd` 树，创建真实存在的同名 archive 文档，内容包含既有规则判定矛盾的“已核”与“待补”。STATUS 的“当前状态”“交接入口”保留规范链接，“最新验证”使用下述生成的 archive 链接：

```python
name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
canonical = chr(91) + "K](" + name + ")"
archive = chr(91) + 'K](archive/' + name + ' "handoff\nevidence")'
status = ("# 项目状态\n\n## 当前状态\n\n" + canonical
          + "\n\n## 最新验证\n\n" + archive
          + "\n\n## 交接入口\n\n" + canonical + "\n")
```

旧版完整 checker 拒绝，在 STATUS 链接起始行报告 `every current-batch link must resolve to that exact file`。候选完整 checker 成功退出，`passed=true`、`errors=[]`，`latest_handoff_checked` 仍为规范文档；archive 入口被完全忽略。单引号多行标题同样出现旧拒绝、新放行，括号多行标题也被候选忽略。

本轮读取的 [CommonMark 0.31.2 规范](https://spec.commonmark.org/0.31.2/#link-title) 明确允许标题跨行，只禁止其中出现空行；本夹具没有空行。`:86` 起的 out-of-scope 注释没有增加任何入口格式拒绝，不能解除 K5 已写明的“统一解析实际链接，或明确并强制受支持形式”的要求。

修复要求：恢复合法多行标题的识别，或对不能解析的交接入口明确拒绝，并加入完整检查器回归。K5 的圆括号单行标题、destination 前空格和换行已正确从放行翻为带行号拒绝，其 canonical 对应输入保持通过；此次阻断来自额外的明确回归。

## 2. 有效修复、保留证据与文档归属

K3-N1 的标准 D-Bus ENOENT 在仅 cat、全部调用、cat 加 reload 故障的输入下，候选均于捕获期拒绝，旧 binary/运行进程保留；git 基线确实复现了原先的静默成功或过晚恢复。v255 的真正首次安装仍可完成。

K2-N1 的 qstate 只接受终态，deactivating 的旧反例不再留下新版进程却报告成功；activating/reloading 的追加场景同样进入保守 stop 和确认。恢复 stop 失败时会明确报告 INCOMPLETE。K2-R2 的严格 stop、MainPID→实际映像核验以及 K2-R5 的 skip-doctor 摘要、真实 git 基线行为证据继续保留；既有权限失败与不可定位占用者的拒绝回归也保持。

K5 报告、交接 §15、STATUS、审核状态表与 K4 历史注记确实随 `e8f811d` 入库。但候选的交接开头、§15、K5 报告开头与状态表尾注仍称 K5 同步仅本地，STATUS 最新验证仍指向上轮。本轮将这些位置标明“出具时本地，随后随 `e8f811d` 提交”，并同步本轮入口。链接计数以固定 tip 实测值记入交接证据表；这项数值纠正不单独列为阻断，也不撤销 K2-R5 行为证据。

## 3. 重放与后续处理

从仓库根目录运行以下仓外脚本；它们从 git 提取指定版本，使用合成数据及隔离工具。恢复脚本对删除路径映射并验证其位于合成目录，未执行生产 SSH 或真实部署。

```powershell
python -u "$env:TEMP\ak-k6-recovery-review.py"
python -u "$env:TEMP\ak-k6-absent-conflict-control.py"
python -u "$env:TEMP\ak-k6-transitions-review.py"
python -u "$env:TEMP\ak-k6-migration-repro.py"
python -u "$env:TEMP\ak-k6-checker-replay.py"
```

runner 成功仅表示重放完成，应读取各阶段返回值、最终运行状态、配置与 JSON。checker 每案保存 STATUS 和输出，结束时还原导出树 STATUS；不能用还原后的导出树代替反例。证据目录和集成者重放结果见交接 §17。

后续在本批次分支修复上述余项和回归，增加能执行真实阶段的行为回归，再交独立复核。此次只同步本地文档，没有提交、推送、合并或删除分支。未执行 Linux 原生 systemd/权限部署、物理双机或抓包验收；固定 tip CI 的平台构建和离线模型不替代这些证据。ADR-001 与无 relay/代理回退约束保持不变。
