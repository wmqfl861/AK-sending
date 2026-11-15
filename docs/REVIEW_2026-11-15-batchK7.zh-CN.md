# 批次 K 修复轮六独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `9c2d3bd..e286513`；代码与文档修复为 `bb1f673`，`e286513` 仅回填提交号。被审 tip 固定为 `e28651322f2d2c9eac94366da965da494d78c4bd`。结论为 **REJECTED**。本轮不合并、不推送 main、不删除批次分支；报告与状态同步出具时只保留本地，不修改被审代码。随后执行方在 `514cb87` 提交这些文档；新候选复核见 [K8 复核](REVIEW_2026-11-15-batchK8.zh-CN.md)，不改写本轮历史结论。

逐项判断唯一记录在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。命令、计数、GitHub 运行编号、材料目录与未执行项集中在 [K 交接 §19](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#19-修复轮六独立复核证据2026-11-15)。下列源码行号均指固定 tip。

已使用当前 git 凭据查询 GitHub API：首次查询时 tip CI 尚在运行，后续轮询确认完成，run/jobs 的分支与完整 head SHA 均匹配候选。以下阻断来自完整阶段重放和完整检查器输入，不能被常规门禁成功覆盖；它们是原要求的剩余缺口，不撤销 K6 精确反例已经翻转的事实。

## 1. 阻断发现

### K2-R3 / P1 余项：运行参数读取失败或数据库参数冲突时，仍可选择错误的迁移配置

`scripts/deploy_linux.sh:1045` 以 `cat ... | tr ... || true` 读取运行实例的 cmdline，读取失败会留下空字符串。`:1096` 只在字符串非空时调用 `reg_args_agree`，因此失去运行证据反而跳过核对。该函数在 `:971` 起只比较 `--id-dir` 和 `--port`，显式 `--db-path` 冲突也不参与判断。随后 `:1816` 取 phase1 报告的 ExecStart，`:1927` 调用 `compose_registry_exec`，把所选参数带入新 unit。

**可读时拒绝，读取失败时放行的身份反例。** 合成旧名 vendor unit `oldregistry.service` 的磁盘配置与仍在占用端口的运行实例分别为：

```text
配置：/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /var/lib/ak-default
运行：/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /var/lib/ak-legacy
```

运行参数保存为 NUL 分隔的 `/proc/4242/cmdline`；合成 cgroup、端口占用、旧 key/db 与 cmdline 文件保持不动。仅用现有 harness 的 `CAT_FAIL_MATCH=/proc/4242/cmdline` 注入该只读探测失败，真实 phase1 和真实 preflight decision、phase 4–6 decisions、compose 链均成功退出，输出：

```text
RUN_RC=0
CHOSEN_EXEC=<REMOTE_BIN> registry-server --port 17200 --id-dir /var/lib/ak-default
CHOSEN_DATA_SOURCE=
CHOSEN_RW=/var/lib/ak-default
CHOSEN_OLD_REGISTRY=oldregistry
DECISION_DONE
```

去掉唯一故障注入，同一夹具的 phase1 退出 3，并报告 `materially disagrees with the running cmdline`，decision 退出 1。`e8f811d` 在读取失败的同一输入上也放行。这里已经证明“不能确定运行身份”没有在切换前拒绝；不能把不可读证据当成可选信息而继续迁移。

**数据库冲突反例。** 配置与运行实例都使用 `--port 17200 --id-dir /var/lib/ak-legacy`，但磁盘配置含 `--db-path /var/lib/ak-legacy/new.db`，NUL cmdline 含 `--db-path /var/lib/ak-legacy/old.db`。合成旧数据库存在，目标 `new.db` 不存在。候选真实 phase1、decision 和 compose 均成功，最终 `CHOSEN_EXEC` 采用 `new.db`。数据库参数一致的对照保留 `old.db`；`e8f811d` 的不一致输入同样选择 `new.db`。

这是正常的“磁盘配置与仍运行的旧实例不同”窗口，无需伪造 systemd 的 drop-in 顺序。systemd v255 的 [systemctl cat 文档](https://github.com/systemd/systemd/blob/v255/man/systemctl.xml#L349) 明确说明它显示磁盘上的 fragment/drop-in，可能不同于 manager 已加载的理解。读取合并文本不能代替运行实例核对。

另有补充输入：运行实例没有显式 `--id-dir`，旧服务的 `User=otheruser`、`HOME=/home/otheruser` 与合成身份位于默认目录，磁盘配置新增 `/var/lib/ak-default`。候选选了新的身份目录，却把探测到的默认目录选为复制到 `$REMOTE_DATA` 的来源。默认参数不能简单等同于“不需要比较”；该补充模型依赖明确的服务环境前提，不作为独立发现。

修复要求：对已识别占用者，运行参数必须成功取得并能够解释；失败、空值或无法确定相关默认值时在切换前拒绝。把身份和数据库的实际来源完整纳入配置核对，既保留合法 drop-in 覆盖，也拒绝影响身份/数据库的实质冲突。回归应保留读取成功/失败的单变量对照与数据库一致/不一致的对照，执行到真实配置选择。

上述实验只执行配置发现和决策，不是实际安装，也没有声称真实身份已经轮换或数据库已丢失。K6 的 drop-in 清空覆盖原输入现已正确保留 legacy 目录；余项来自同一保真要求的其他明确输入。

### K2-R4 / P2 余项：尖括号目标含空格时，完整解析和兜底同时漏掉交接入口

`scripts/check_repository.py:104` 的目标捕获为 `([^\s)]+)`；新增 `HANDOFF_LINK_PREFIX_RE` 在 `:123` 也于空格停止。尖括号内的完整目标因此未被捕获，`:445` 起对截断 token 求交接文件名，`:448`–`:449` 得到空 key 后直接跳过。`:612` 的通用链接扫描复用完整正则，也没有补报这个入口。

从 git 导出完整候选树，在 `docs/archive space/` 创建与当前批次同名的独立交接，内容包含既有规则会拒绝的“已核”和“待补”。STATUS 的“当前状态”“交接入口”保留规范链接，“最新验证”的输入如下构造：

```python
name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
canonical = chr(91) + "K](" + name + ")"
archive = chr(91) + "K](<archive space/" + name + ">)"
status = ("# 项目状态\n\n## 当前状态\n\n" + canonical
          + "\n\n## 最新验证\n\n" + archive
          + "\n\n## 交接入口\n\n" + canonical + "\n")
```

完整候选 checker 成功退出：`passed=true`、`errors=[]`，`latest_handoff_checked` 仍为 `docs/` 下的规范文档。完整 regex 只取得前后规范链接；兜底在 archive 入口只取得 `archive`，实际文件名根本没有进入判定。为该目标再加合法多行标题，结果相同。旧版 `9c2d3bd` 同样放行，所以这是余项，不能写成本轮新引入的回归。

[CommonMark 0.31.2 的 link destination 定义](https://spec.commonmark.org/0.31.2/#link-destination) 允许尖括号目标中的空格，官方示例将 `</my uri>` 解析为 `/my%20uri`。该输入不依赖 reference link、嵌套 label 或其他尚未承诺的语法；本轮还明确声称已处理尖括号目标。

仓外原因定位对照只在内存模块中将完整目标捕获换成 `(<[^<>\n]*>|[^\s)]+)`，同一完整 check 即在 STATUS 第 9 行报 `every current-batch link must resolve to that exact file`。该对照未修改仓内代码，也不是可直接合入的完整解析修复。

修复要求：先取得完整 destination，再解析交接文件名与实际路径；完整解析器不能接受的交接形态必须有不截断目标的明确拒绝。加入含空格的尖括号 archive 输入及其 canonical 对照，继续保留多行标题、空行标题与隐藏入口回归。解包尖括号还应先于 fragment 去除：当前合法尖括号加 anchor 的 canonical 链接会留下前导 `<` 并误报坏链；这是已有支持边界，本轮不另立新阻断。

## 2. 有效修复、保留证据与文档归属

K3-N1 的删除文件模型现在于捕获期退出 5，未进入 switchover，旧磁盘、MainPID 与运行镜像保留。专项又明确设置 `is-enabled=not-found`，保留自然产生的 v255 缺文件诊断，重放 active、过渡态、未知状态与运行查询失败，均在切换前拒绝；真实生成的 stop 函数也拒绝跳过这些输入。`e8f811d` 同一 active 模型仍会静默安装，真正 absent+inactive 的首次安装继续通过。

需修正执行方请求中的一个限定：实际 capture 与 stop 都允许 `inactive|failed`，并非“仅 inactive”；交接 §18 已如实写出此行为。本轮 failed 对照没有存活进程，未用不符合 systemd 状态语义的 failed+MainPID 模型制造反例，也未将这个措辞差异单列阻断。

K2-R3 的原始 drop-in 清空覆盖、单一主配置、按运行参数唯一消解的追加项、歧义拒绝和显式身份分歧拒绝均已执行。K2-R4 的 K6 多行标题 archive 输入现在均带起始行号拒绝，规范对应输入继续通过，空行标题和藏在标题内的交接形态显式拒绝。K2-N1 的完整恢复窗口、K2-R2 的严格 stop 与 MainPID 实际映像验证、K2-R5 的诚实摘要和真实 git 基线行为证据均保留。

K6 报告、交接 §17、STATUS、状态表与 K5 归属纠正确实随 `bb1f673` 入库，`e286513` 只有哈希回填。但候选交接开头/§17、K6 报告开头及状态表尾注仍写 K6 同步仅本地，STATUS 最新验证仍指向上轮；本轮改为“出具时本地，随后由执行方提交”，同步 K7 入口。候选对当前 tip 的链接计数也仍沿用旧树，实际值及计数范围只记于交接证据表；这些文档纠正不撤销 K2-R5 行为证据，也不另列代码阻断。

## 3. 重放与后续处理

从仓库根目录运行下列仓外脚本；每次生成独立的合成证据目录，不连接生产。state runner 对删除路径先映射并校验其位于合成目录。完整 checker 的每个 STATUS 输入和 JSON 都单独保存，导出树的 STATUS 最后还原。

```powershell
python -u "$env:TEMP\ak-k7-state-replay.py"
python -u "$env:TEMP\ak-k7-state-stop.py"
python -u "$env:TEMP\ak-k7-state-baseline.py"
python -u "$env:TEMP\ak-k7-migration-confirm.py"
python -u "$env:TEMP\ak-k7-checker-replay.py"
```

runner 成功仅表示重放完成；应读取各阶段退出值、CHOSEN 配置、保存的 STATUS 和完整 JSON。专项与集成者的独立重放材料位置见交接 §19。新增报告和本轮状态同步出具时保留为本地文档改动，随后由执行方在 `514cb87` 提交；K7 复核期间 main 保持原提交。

后续在批次分支修复运行配置核对和完整目标漏检，加入上述行为回归后再交复核。未执行 Linux 原生 systemd/权限部署、物理双机或生产抓包；CI 平台构建、本机门禁与离线模型不代替这些验收。ADR-001 及无 relay、无隐藏代理回退约束保持不变。
