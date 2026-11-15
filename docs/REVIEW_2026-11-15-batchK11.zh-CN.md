# 批次 K 修复轮十独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `806ea6c..09fceb6`；修复实现为 `a50bb1a`，`09fceb6` 只回填两行哈希。被审 tip 固定为 `09fceb64a2fb729fc8dbf0d0c20c64abcec67045`。结论为 **REJECTED**：上轮原始输入均已翻转，K10-N2 的目标冲突可判修复，但 K10-N1 仍会把不可靠或被截断的路径认作运行身份目录；另确认停服前复制数据库会遗漏随后已提交的状态。

逐项状态只在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。命令、计数、GitHub 运行编号、仓外材料及未执行项集中在 [K 交接 §27](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#27-修复轮十独立复核证据2026-11-15)。本轮只新增报告并作本地文档同步，不改被审代码、不提交、不合并或推送 main、不删除批次分支。下列源码行号均指固定 tip。

已使用当前 git 凭据直接查询 GitHub run/jobs API，从 in_progress 等待至 completed/success；完整 head SHA 与固定候选一致，全部常规 jobs 及门槛步骤成功。手动 release deep job 的跳过与本机未执行项单列在交接证据表。这些结果不能覆盖以下独立行为反例。

## 1. K10-N1 / P1：原输入已修复，但仍错误确认运行身份目录

### 1.1 unit 的 WorkingDirectory 不是运行进程 cwd 的证据

`scripts/deploy_linux.sh:1286` 从 `systemctl cat` 文本提取 `WorkingDirectory=`，`:1289`–`:1293` 直接用它解析空值或相对基目录。脚本没有读取运行进程的 `/proc/<PID>/cwd`。服务已启动、随后 unit 的工作目录被修改而尚未重启，是合法的配置/运行状态差异；相同的 ExecStart 和 argv 并不能排除这种差异。

完整安装模型输入如下，两个目录均放置不同的合成 key/db，旧 registry 和 signal 均具有 unit 文件、enabled/active 状态和已知 PID：

```text
unit:    WorkingDirectory=/home/configured-new
runtime: /proc/4242/cwd=/home/runtime-old
environ: USERPROFILE=（存在但为空）, HOME=/home/otheruser
argv:    ["/opt/ak-sending/ak-cli", "registry-server", "--port", "17200"]（逐参数 NUL）
```

真实 phase1、真实 preflight/phase456 决策和实际生成的安装脚本均成功；`CHOSEN_DATA_SOURCE=/home/configured-new/.ak-sending`，最终目标是 `configured-decoy` 数据，新 unit 指向目标。CLI 默认链得到相对 `.ak-sending`，原运行实例应使用 `/home/runtime-old/.ak-sending`。因此该修复只是将空值反例从“错误回退 HOME”改为“无运行 cwd 核验地相信配置 cwd”，尚未满足上轮的可靠运行证据要求。

现有 readlink stub 经独立控制验证，能按 `PROC_EXE_MAP` 返回 `/home/runtime-old`；实际失败链的 stub 日志中没有 cwd 查询。`WorkingDirectory=-/path` 的 `-` 只改变缺失目录的失败策略，也不能证明 cwd 就是 `/path`。

### 1.2 真正变量值中的 LF 仍被截断

`:1231`–`:1232` 虽然改成 `grep -za '^USERPROFILE='` / `'^HOME='`，后面仍接 `head -n 1`；它重新按 LF 截断变量自身的值。例如真实 NUL 记录为：

```python
b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\nsuffix\0USER=otheruser\0"
```

unit 使用 `EnvironmentFile=/etc/default/oldregistry`，合成配置文件为：

```text
HOME=/home/otheruser
USERPROFILE='/home/otheruser
suffix'
USER=otheruser
```

该单引号多行值符合 [systemd.exec 的 EnvironmentFile 语义](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html#EnvironmentFile=)。脚本仍输出 `runtime-id-dir: /home/otheruser/.ak-sending`，phase1/decision/install 均成功，实际复制 prefix 目录中的 key/db。`crates/ak-cli/src/main.rs:709`–`:716` 的 `var_os → PathBuf → join` 会保留 LF，真正目录应为 `/home/otheruser\nsuffix/.ak-sending`。值尾 LF 的控制也被截断。

Rust 默认链已逐字摘录并在本机编译，确认 LF 保留及空值不回退 HOME；Windows 不支持在文件名中落盘 LF，所以本次实际验证的是错误 prefix 数据被复制，正确 Linux 路径由 CLI 字节语义确定。这里没有声称进行了 Linux 原生启动。仅把 `head -n` 换成 `head -z` 也不充分：命令替换会丢失尾 LF，后续逐行报告/解析同样必须正确传递或明确拒绝此类路径。

两个错误均经 `:2054`–`:2059` 成为 `existing_data`，再经 `:2183`–`:2184` 进入迁移，实际新 unit 和落盘内容都受影响。**修复要求：** 取得绑定当前运行实例的 cwd 证据，不能确认时拒绝；环境记录及路径字节须端到端保真，不支持的路径可以明确拒绝，不能截断后当作另一个有效目录。回归应覆盖配置 cwd 与运行 cwd 不同、目标变量自身含 LF、值尾 LF，检查真实决策、unit 与落盘内容。

## 2. K10-N2：原目标冲突反例已修复

集成者从 K10 报告原样提取输入，以旧候选和当前候选分别重放。来源仍为正确 HOME 时，旧候选在目标有 decoy 的情况下成功切换并保留无关字节；当前候选在 `:1504`–`:1505` 拒绝，安装退出码为 5，目标 key/db 字节保留且新 unit 未创建。移除 decoy 后实际安装成功，目标为正确来源字节。

一致目标、拷贝校验、损坏拷贝拒绝、全新脏目标拒绝及原位升级的提交内回归也已执行。此判断限于目标冲突及已执行输入；下面的数据库最终状态缺口是独立问题。

“exit 5 全不动”应限定为此次反例的数据字节和服务切换结果。`:1445` 已创建/设置目录，`:1457`–`:1481` 可能生成备份，不能据此声明整个目标文件系统没有变化。

## 3. K11-N1 / P1：数据库在旧服务停止前复制，已提交的后续状态被留在旧来源

这是**新确认的既有缺口，不是 `a50bb1a` 引入的回归**。`:1497`–`:1515` 在旧服务仍运行时复制 key/db 并进行 cmp；`:1816` 才开始停止 managed units，`:1820` 才停止识别到的旧 registry。两者之间没有冻结写入、最终同步或停服后的再次核验。一次 cmp 只能说明复制时点的字节相同。

沿用真实 HOME 默认目录输入，省略 `--id-dir` 和 `--db-path`。用项目 `crates/ak-signal/src/server_v2/store.rs` 的真实 DDL 创建 schema v3 SQLite，放入一条合成 `active/revision=1` 记录。在 systemctl 模型进入 `stop oldregistry.service`、尚未标为 inactive 的位置，提交与撤销状态更新相同的 SQL 事务。生产部署脚本没有改写，真实 phase1、决策、compose 和安装脚本完整执行。

```text
当前候选、无晚提交控制：
  phase1=0, decision=0, INSTALL_RC=0
  source: active, revision=1
  target: active, revision=1

当前候选、复制校验后但旧服务停止前提交撤销：
  phase1=0, decision=0, INSTALL_RC=0
  source: revoked, revision=2
  target: active, revision=1
  new ExecStart: <REMOTE_BIN> registry-server --port 17200 --id-dir <REMOTE_DATA>
```

`806ea6c` 的完全相同输入也得到旧目标状态，证实归属。当前候选日志先报告 `cmp-verified`，再捕获旧服务为 active，随后出现注入的真实 SQLite commit，最终仍成功。`main.rs:3777` / `:3792` 明确新服务的默认数据库是目标 `registry.db`；保留旧来源文件不能让新服务读到它的最终状态。

生产 `store.rs:665`、`:687`、`:706` 分别开启事务、更新撤销字段、提交后返回；旧服务正常运行时发生这类持久提交是正常行为。这里证明的是部署结果中的 schema 状态回退；没有启动真实 registry、没有通过协议执行真实撤销，也没有声称实际用户邀请码已被恢复。

**修复要求：** 最终数据库来源必须与停止写入的时点一致，或继续使用原数据库路径。迁移时应在可恢复的停服窗口内取得完整一致的最终状态；失败仍须遵守已建立的恢复要求。新增回归必须包含复制后、停服前提交的交错输入，并验证实际目标数据库内容，不能只检查复制瞬间的 cmp 或日志。

## 4. 文档与证据边界

`a50bb1a` 确实提交了 K10 报告、交接 §25、STATUS、状态表和 K9 历史归属同步；`09fceb6` 确实只回填两行哈希。候选仍有“K10 仅本地”和旧下一步，本轮按已提交的历史事实纠正并切换 K11 入口。fixture 已改为逐参数 NUL argv；本轮独立重放也显式构造精确 argv。Rust、依赖和 workflow 在修复区间无差异。

以上行为证据为真实 Bash 生成器、实际落盘与 SQLite 事务，加隔离的权限/systemctl/proc 模型。没有 SSH、生产部署、Linux 原生 systemd 验证或物理双机测试。ADR-001、无 relay/隐藏代理回退、原始字节不压缩不转码约束不变。静态检查、模型、CI 平台构建不能互相代替。

## 5. 可复现步骤

在固定候选仓库根目录用 PowerShell 执行以下代码。只使用提交内 harness、真实 git 版本和临时文件，所有特权路径均映射到 fixture。runner 的退出码只表示重放完成；应读取各案阶段退出值、CHOSEN、DATA 和实际 unit。cwd 案模拟“配置已改、进程未重启”；LF 案按上述 Linux 路径语义评估；数据库案只在 systemctl stub 中加入允许发生的停服前事务，未修改生产脚本。

```powershell
@'
import importlib.util, json, re, shlex, sqlite3, subprocess, sys
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "deploy_tests", Path("scripts/tests/test_deploy_linux_install.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
base = "/opt/ak-sending/ak-cli registry-server --port 17200"
normal = b"HOME=/home/otheruser\0USER=otheruser\0"
cases = [
    ("profile-lf", "09fceb6", b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\nsuffix\0USER=otheruser\0"),
    ("cwd-changed", "09fceb6", b"HOME=/home/otheruser\0USERPROFILE=\0USER=otheruser\0"),
    ("db-clean", "09fceb6", normal),
    ("db-late", "09fceb6", normal),
    ("db-late-baseline", "806ea6c", normal),
]
for label, rev, environ in cases:
    h = m.DeployInstallTests()
    h.setUp()
    try:
        h.text = subprocess.check_output(
            ["git", "show", rev + ":scripts/deploy_linux.sh"]).decode("utf-8")
        lines = ["User=otheruser", "Environment=HOME=/home/otheruser"]
        if label == "profile-lf":
            lines = ["User=otheruser", "EnvironmentFile=/etc/default/oldregistry"]
        elif label == "cwd-changed":
            lines += ["Environment=USERPROFILE=", "WorkingDirectory=/home/configured-new"]
        else:
            lines += ["UnsetEnvironment=USERPROFILE"]
        h.make_private_discovery_model(main_exec=base, cmdline_extra="--port 17200",
            unit_service_lines=lines, environ=environ)
        (h.probe / "proc/4242/cmdline").write_bytes(
            b"\0".join(x.encode() for x in base.split()) + b"\0")
        if label == "profile-lf":
            f = h.probe / "etc/default/oldregistry"
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text("HOME=/home/otheruser\nUSERPROFILE='/home/otheruser\nsuffix'\nUSER=otheruser\n")
        if label == "cwd-changed":
            h.driver_env["PROC_EXE_MAP"] = "/proc/4242/cwd=/home/runtime-old"
            for rel, value in [("home/runtime-old", "runtime-real"),
                               ("home/configured-new", "configured-decoy")]:
                d = h.probe / rel / ".ak-sending"
                d.mkdir(parents=True, exist_ok=True)
                for name in ("registry.key", "registry.db"):
                    (d / name).write_text(value + "-" + name)
        st = h.probe / "systemctl-state"
        st.mkdir(exist_ok=True)
        for unit, pid in (("oldregistry.service", 4242), ("oldsignal.service", 4243)):
            for suffix, value in (("enabled", "1"), ("active", "active"), ("pid", str(pid))):
                (st / (unit + "." + suffix)).write_text(value)
            (st / (str(pid) + ".exe")).write_text("/opt/ak-sending/ak-cli")
        (h.probe / "etc/systemd/system/oldsignal.service").write_text(
            "[Service]\nExecStart=/opt/ak-sending/ak-cli signal-server\n")
        src = h.probe / "home/otheruser/.ak-sending/registry.db"
        if label.startswith("db-"):
            src.unlink()
            store = Path("crates/ak-signal/src/server_v2/store.rs").read_text()
            ddl = re.findall(r'const DDL_[A-Z_]+: &str = "(.*?)";', store, re.S)
            assert len(ddl) == 5
            with sqlite3.connect(src) as c:
                c.executescript("\n".join(ddl))
                c.execute("PRAGMA user_version=3")
                c.execute("INSERT INTO invite_codes (lookup_digest,code_id,owner_device_id,created_unix_ms,state,revision,create_request_id) VALUES (?,?,?,?,?,?,?)",
                          (b"d"*32, b"c"*16, b"o"*32, 1, "active", 1, b"r"*16))
            c.close()
            if label.startswith("db-late"):
                hook = h.probe / "late_commit.py"
                hook.write_text("import sqlite3\nwith sqlite3.connect(" + repr(str(src)) + ") as c:\n"
                    " c.execute(\"UPDATE invite_codes SET state='revoked', revision=revision+1 WHERE code_id=? AND revision=1 AND state='active'\", (b'c'*16,))\n"
                    "print('LATE_COMMIT=revoked', flush=True)\n")
                ctl = h.probe / "bin/systemctl"
                old = ctl.read_text()
                inject = ('  stop)\n    if [ "$u" = oldregistry.service ]; then\n      '
                    + shlex.quote(sys.executable.replace("\\", "/")) + " "
                    + shlex.quote(hook.as_posix()) + " || exit 1\n    fi\n")
                assert "  stop)\n" in old
                ctl.write_text(old.replace("  stop)\n", inject, 1), newline="\n")
        rc, out = h.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || exit 90\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        phase = (h.probe / "p1.out").read_text()
        drc, decision = h.run_decision_and_config(phase, "decision.sh")
        body = 'WORK="$PROBE_DIR"\nKEEP_LEGACY_SIGNAL=0\nCHECK_ONLY=0\ndeploy_decision() {\n'
        body += h.decision_block(h.text, rev) + h.decisions_block(h.text, rev)
        body += '\n}\ndeploy_decision\n'
        body += ('TS=t12345 BUILD_DIR= PREBUILT_MODE=1 STAGING_DIR="$PROBE_DIR/staging" '
                 'STAGED_BIN="$PROBE_DIR/staging/ak-cli" SHORT_SHA=deadbee CLOSE_7100=0 '
                 'write_phase456 "$PROBE_DIR/actual456.sh" || exit 90\n')
        body += 'bash "$PROBE_DIR/actual456.sh" > "$PROBE_DIR/actual456.out" 2>&1; echo "INSTALL_RC=$?"\n'
        irc, iout = h.run_driver(h.definitions() + m.BOX_MODELS, body,
                                name="actual-driver.sh", timeout=120)
        dst = h.probe / "opt-ak/data"
        if label.startswith("db-"):
            def rows(path):
                with sqlite3.connect(path) as c:
                    result = c.execute("SELECT state,revision FROM invite_codes").fetchall()
                c.close()
                return result
            data = {"source": rows(src), "target": rows(dst / "registry.db")}
        else:
            data = {name: (dst / name).read_text() for name in ("registry.key", "registry.db")}
        print("CASE", label, "REV", rev, "HARNESS_RC", rc, out,
              "DECISION_RC", drc, decision, "INSTALL_DRIVER_RC", irc, iout,
              "DATA", json.dumps(data), flush=True)
        print((h.probe / "actual456.out").read_text(), flush=True)
        print((h.probe / "etc/systemd/system/ak-registry-server.service").read_text(), flush=True)
    finally:
        h.doCleanups()
'@ | python -
```
