# 批次 K 修复轮七独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `e286513..12a71c9`；代码与文档修复为 `514cb87`，`12a71c9` 仅作哈希回填。被审 tip 固定为 `12a71c9d6c2ff11be63aa48ed009699c691e1ebe`。结论为 **REJECTED**：K2-R3 仍未核对运行实例使用的默认数据库路径。本轮不合并、不推送 main、不删除批次分支；报告与状态同步只保留本地，不修改被审代码。

逐项判断唯一记录在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。实际命令、计数、GitHub 运行编号、材料目录与未执行项集中在 [K 交接 §21](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#21-修复轮七独立复核证据2026-11-15)。下列源码行号均指固定 tip。

已使用当前 git 凭据查询 GitHub API，并从 in_progress 等待至 completed/success；run/jobs 对应的完整 head SHA 与固定候选一致。常规 CI 成功不覆盖下述独立行为反例。

## 1. 阻断发现

### K2-R3 / P1 余项：省略 `--db-path` 的运行实例仍可被切换到不同的数据库

`scripts/deploy_linux.sh:984`–`:991` 的 `reg_args_agree` 现已遍历 `--id-dir`、`--db-path`、`--port`、`--bind`，但 `:989` 仍只在运行参数的值 `b` 非空时比较。省略参数被当成“不需要核对”，因此列举全部旗标还不等于核对了实际配置。

这里可以确定实际数据库，不需要猜测服务用户、HOME、USERPROFILE 或工作目录：`crates/ak-cli/src/main.rs:164`–`:166` 定义可选 `--db-path`，`:3777`–`:3778` 的 `registry_db_path(id)` 返回 `id.join("registry.db")`，`:3792` 在未传 `db_path` 时采用该默认值。运行 argv 已显式指定绝对 `--id-dir`。

合成旧服务 `oldregistry.service` 占用注册端口；NUL 分隔的运行 argv 与磁盘 ExecStart 如下：

```text
运行：/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /home/otheruser/.ak-sending
配置：/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /home/otheruser/.ak-sending --db-path /home/otheruser/.ak-sending/new.db
```

合成目录内已有 `registry.key`、`registry.db`，没有 `new.db`；占用者、cgroup、cmdline 均可读。真实 phase1 与真实 preflight decision、phase 4–6 decisions、`compose_registry_exec` 链均成功退出，输出：

```text
RUN_RC=0
DECISION_RC=0
CHOSEN_EXEC=<REMOTE_BIN> registry-server --port 17200 --id-dir /home/otheruser/.ak-sending --db-path /home/otheruser/.ak-sending/new.db
CHOSEN_DATA_SOURCE=/home/otheruser/.ak-sending
CHOSEN_RW=/home/otheruser/.ak-sending
CHOSEN_OLD_REGISTRY=oldregistry
DECISION_DONE
```

这会把不同于运行实例的数据库路径带入新 unit 的配置选择：`:1846` 接受 phase1 报出的 ExecStart，`:1957` 调用 compose 保留其数据库参数。发现并复制旧 `registry.db` 不会改写上述 `--db-path new.db`，不能消除这一分歧。

**单变量对照。** 仅给运行 argv 加上语义等价的 `--db-path /home/otheruser/.ak-sending/registry.db`，其余输入保持相同，候选立即变为 phase1 exit 3、decision exit 1，报告 `materially disagrees with the running cmdline`，不产生 CHOSEN 配置。可见同一实际数据库冲突是否被拒绝，仍取决于默认值是否在 argv 中显式拼出。

配置显式写 `registry.db`、运行使用默认值的等价对照通过；双方显式写 `registry.db` 与双方省略数据库旗标的对照也保留原库语义。`bb1f673` 的相同默认冲突输入同样选择 `new.db`，说明这是原要求的剩余缺口，未将它误归为本轮新引入的回归。专项与集成者均独立执行并保存了真实阶段输出。

K7 已要求将身份和数据库的实际来源完整纳入核对，无法确定相关默认值时在切换前拒绝。本输入把先前依赖 HOME 的补充模型收敛为由显式 id-dir 和 CLI 源码唯一确定的数据库默认值，仍属于该要求。修复应比较有效值：运行省略 `--db-path` 时按已确定的身份目录求默认数据库，配置侧也按相同语义求值；无法确定实际来源时拒绝，不能将缺旗标视为无约束。回归应包含默认/显式等价的通过对照与默认/不同显式路径的拒绝对照，并执行到真实配置选择。

上述实验验证配置发现与选择，没有运行真实 registry 服务、安装新 unit 或打开生产数据库，不声称已发生真实资料丢失。

## 2. 已翻转输入、保留行为与说明纠正

K7 的 cmdline 单变量读取失败现已在 phase1 退出 3，输出“config cross-check has no evidence”，不再发出 ExecStart；可读分歧仍拒绝，匹配输入仍保留旧目录。相同失败输入在 `bb1f673` 仍会双阶段通过并选择 ak-default。本轮新增的非空要求和无条件最终核对确实有效。

K7 的显式 `old.db/new.db` 冲突现已拒绝，一致的显式 `old.db` 经真实决策与 compose 链保留。上一节的默认值余项不撤销这两个精确反例的翻转。

K2-R4 的合法尖括号空格 archive 输入经完整导出树检查，现在会在 STATUS 起始行指出实际 archive 路径与规范交接不一致。合法多行标题的各引法均拒绝该 archive 入口；空行标题由 backstop 报完整目标；规范尖括号及规范目标加 fragment 的对照通过。未发现该轮承诺的合法 inline destination/title 范围内新的阻断。

K3-N1 的真正首次安装与缺文件但仍运行的拒绝、K2-N1 的完整恢复窗口、K2-R2 的严格 stop 与实际 MainPID 映像核验、K2-R5 的诚实摘要及真实 git 旧阶段对照，继续保留。实际 absent 分支允许 `inactive|failed`，执行方注释已作对应纠正；这不扩大成所有 failed 残留进程情形均经验证。

另有非阻断的规范说明纠正：[CommonMark 0.31.2 link destination](https://spec.commonmark.org/0.31.2/#link-destination) 对尖括号内容明确要求 **“contains no line endings”**；可跨非空行的是 title。候选源码注释、测试注释和交接把两者混淆。候选会接受尖括号 fragment 中含单个换行的非法输入，也不能保证拒绝所有含空行的非法形态；本轮没有据此证明合法 archive 链接逃逸，不另立代码阻断，但不能继续将这种扩展称为 CommonMark 规则或宣称所有未解析形态均被拒绝。

`514cb87` 确已纳入 K7 报告、交接与入口同步，以及 K6 的历史归属纠正；`12a71c9` 确实只回填哈希。候选仍残留“K7 仅本地”和上一轮最新入口的措辞，本轮改为“出具时本地，随后由执行方提交”，并同步 K8 入口。两代 regex 在同一文档树的计数相等，但执行方的工作树计数不能代表固定 tip；按树区分的实测值只记在交接证据表。这些纠正不另列阻断，也不修改前轮历史结论。

## 3. 可复现步骤与后续处理

在仓库根目录用 PowerShell 运行以下最小重放。它只使用既有离线 harness，所有特权路径均映射到临时目录；运行结束清理合成夹具，不连接生产。

```powershell
@'
import importlib.util
from pathlib import Path
import subprocess

spec = importlib.util.spec_from_file_location(
    "deploy_tests", Path("scripts/tests/test_deploy_linux_install.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
for running_db in (None, "registry.db"):
    h = m.DeployInstallTests()
    h.setUp()
    try:
        h.text = subprocess.check_output(
            ["git", "show", "12a71c9:scripts/deploy_linux.sh"]).decode("utf-8")
        base = "/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /home/otheruser/.ak-sending"
        configured = base + " --db-path /home/otheruser/.ak-sending/new.db"
        h.make_private_discovery_model(main_exec=configured)
        argv = base.split()
        if running_db:
            argv += ["--db-path", "/home/otheruser/.ak-sending/" + running_db]
        (h.probe / "proc/4242/cmdline").write_bytes(
            b"\0".join(a.encode() for a in argv) + b"\0")
        rc, out = h.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || exit 90\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        phase = (h.probe / "p1.out").read_text(encoding="utf-8")
        decision_rc, decision = h.run_decision_and_config(phase, "decision.sh")
        print("RUNNING_DB=", running_db, "HARNESS_RC=", rc)
        print(out, phase, "DECISION_RC=", decision_rc, decision)
    finally:
        h.doCleanups()
'@ | python -
```

首个输入的 phase1/decision 为 0/0 且选择 new.db；后一个仅将默认值显式化的输入为 3/1。仓外完整 runner 还保留等价配置、双方默认与旧 git 基线，位置和命令见交接证据表。runner 本身 exit 0 只表示重放完成，应读取每个阶段退出值和 CHOSEN 输出。

后续在批次分支修复有效身份/数据库配置核对并加入行为回归，再交独立复核。Linux 原生 systemd/权限部署、物理双机与生产抓包仍未执行；平台 CI、静态检查和离线模型不代替这些验收。ADR-001、无 relay、无隐藏代理回退约束保持不变。
