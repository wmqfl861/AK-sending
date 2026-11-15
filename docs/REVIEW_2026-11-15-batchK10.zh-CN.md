# 批次 K 修复轮九独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `ccd8fb2..806ea6c`；代码与文档修复为 `e03a44f`，`806ea6c` 仅作哈希回填。被审 tip 固定为 `806ea6cc1a23db7b0dd56b38e8857483063c69a5`。结论为 **REJECTED**：原 K9-N1 的精确 root-decoy 枚举来源反例已修复，但新环境推导会选错身份目录，且正确的来源选择仍不能阻止安装阶段启用目标目录中的无关身份与数据库。K8 的默认数据库修复维持通过。

逐项状态唯一记录在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。实际命令、计数、GitHub 运行编号、仓外材料和未执行项集中在 [K 交接 §25](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#25-修复轮九独立复核证据2026-11-15)。本轮出具时仅作本地报告和文档同步，不改被审代码、不提交、不合并或推送 main、不删除批次分支；报告及同步随后由执行方在 `a50bb1a` 提交。下列源码行号均指固定 tip。

已使用当前 git 凭据直接查询 GitHub run/jobs API，从 in_progress 等待至 completed/success；完整 head SHA 与固定候选一致，全部常规 jobs 及门槛步骤成功。本机门禁也已完成。这些结果不覆盖下述新增的独立行为反例。

## 1. K10-N1 / P1：环境解析丢失变量存在性与 NUL 边界，错误确认运行身份目录

新代码 `scripts/deploy_linux.sh:1201` 把 `/proc/<PID>/environ` 的 NUL 全部转换成 LF，`:1206`–`:1207` 再按行提取变量，`:1228`–`:1231` 用非空判断选择 USERPROFILE 或 HOME。这不等价于 `crates/ak-cli/src/main.rs:709`–`:716` 的实际默认链：

```rust
std::env::var_os("USERPROFILE")
    .or_else(|| std::env::var_os("HOME"))
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from("."))
```

**空值反例。** 运行 unit 与 argv 都省略 `--id-dir`，unit 包含以下有效配置；逐参数 NUL cmdline 与 ExecStart 一致，运行 environ 含 `HOME=/home/otheruser` 和 `USERPROFILE=`。

```ini
[Service]
User=otheruser
WorkingDirectory=/var/lib/oldregistry
Environment=HOME=/home/otheruser
Environment=USERPROFILE=
ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200
```

CLI 的 `var_os("USERPROFILE")` 为 `Some("")`，不会回退 HOME；join 后得到相对 `.ak-sending`，在上述运行工作目录下对应 `/var/lib/oldregistry/.ak-sending`。脚本却输出 `runtime-id-dir: /home/otheruser/.ak-sending`，phase1 和真实后续决策均成功，`CHOSEN_DATA_SOURCE` 也选 HOME。若不能确认实际运行 cwd，正确行为应是切换前拒绝，不能把空值当成变量缺失后猜另一个目录。该反例由专项与集成者分别重放，Rust 默认链另以本机 rustc 编译核对。

**值内换行反例。** 运行 environ 的真实 NUL 记录为：

```python
[b"HOME=/home/otheruser", b"EXTRA=hello\nUSERPROFILE=/root", b"USER=otheruser"]
```

这里没有 USERPROFILE 变量，只有 EXTRA 的值含 LF 和类似赋值的文本；unit 使用 `Environment="EXTRA=hello\nUSERPROFILE=/root"`。CLI 应选择 HOME。脚本的 NUL→LF 转换丢失原记录边界，随后凭空读出 `USERPROFILE=/root`，phase1 和 decision 均成功，报告及迁移源改为 `/root/.ak-sending`。这是实际运行输出，不只是对正则的推测。

`:1958`–`:1962` 又把该错误字段当作权威迁移源，因而此处是会传递到配置选择的身份来源错误。两个输入均为本轮新加入环境解析的缺陷；不否定普通非空 HOME 输入上的 K9 精确修复。

**修复要求：** 按 NUL 记录解析 environ，分别保留变量是否存在与值本身；空值、相对路径等依赖 cwd 的输入必须绑定实际运行 cwd 或明确拒绝。无法准确解释的环境证据不能输出“已确定”的运行身份目录。回归须执行真实 phase1、后续选择与安装相关决策，保留普通 HOME、显式 id-dir 及已有拒绝对照。

## 2. K10-N2 / P1：目标已有无关 key/db 时跳过迁移，仍成功切换到该目标

此项是新确认的既有缺口，**不是本轮引入的回归**。`scripts/deploy_linux.sh:467` 对原来省略身份目录的 ExecStart 补上 `$REMOTE_DATA`；`:2088` 将已确定的运行目录设置为 `DATA_SOURCE_DIR`。实际安装的 `:1422`–`:1426` 却只在目标文件不存在时复制每个 `registry.key` / `registry.db`。目标存在时不验证归属或一致性，也不拒绝，随后仍写入指向目标的新 unit 并切换服务。

沿用 K9 的有效 HOME 输入、逐参数 NUL argv 与 home 下正确合成 key/db，只增加 `$REMOTE_DATA/registry.key`、`$REMOTE_DATA/registry.db`，内容分别为 `unrelated-target-key`、`unrelated-target-db`。真实 phase1 → preflight decision → phase-4/6 decisions → compose 的来源选择确实正确；再用这些决策值生成并执行真实 phase456，得到：

```text
RUN_RC=0
DECISION_RC=0
CHOSEN_DATA_SOURCE=/home/otheruser/.ak-sending
INSTALL_RC=0
TARGET_registry.key unrelated-target-key
TARGET_registry.db unrelated-target-db
ExecStart=<REMOTE_BIN> registry-server --port 17200 --id-dir <REMOTE_DATA>
```

移除目标 decoy、其余输入不变，安装仍成功，但目标内容改为正确 home 来源的 `private-key-bytes` / `private-db-bytes`。专项还把旧 registry 和 signal 的模型补齐为 unit 文件存在、enabled+active、PID 已知后重放，结果不变；旧服务状态确实在切换前被捕获为 active。前一候选 `ccd8fb2` 同样允许该目标输入进入切换决策，符合既有缺口归属。

因此，本轮选对 `CHOSEN_DATA_SOURCE` 不能被扩大为“新服务一定继续使用原运行身份”。K9 已要求来源贯穿保留/迁移决策，并要求对增减目标目录数据作对照，这一链条仍未闭合。

**修复要求：** 在切换前解决目标数据冲突：可证明目标就是该运行实例的数据时保留，否则保留可靠的原运行来源，或明确拒绝并交人工处理。不能静默跳过迁移后启用无关目标，也不能未经消解就覆盖另一份数据。回归必须检查实际生成 unit 和实际落盘 key/db，覆盖目标无数据、有冲突数据及部分存在的情况；不能止于 CHOSEN 字符串。

上述安装证据仅限隔离的文件系统与 systemctl 模型中执行真实生成脚本。key/db 是合成文本，未启动真实 registry、未打开真实 SQLite，不声称真实身份已经轮换或用户数据库已经损坏。

## 3. 已确认修复与非阻断纠正

K9 原输入在无 root decoy 和有 root decoy 时，精确逐参数 NUL 重放均保留 `/home/otheruser/.ak-sending` 为迁移源；`12a71c9` 对照仍选 root。显式 id-dir、无占用者唯一候选、多候选拒绝、缺失环境证据拒绝的提交内回归也已执行。K8 默认数据库等价与冲突回归保留，未撤回此前通过判断。

执行方新增 fixture 的“argv-precise”声明需纠正：`scripts/tests/test_deploy_linux_install.py:974` 如此描述，但 `:980` 把 `"--port 17200"` 交给 `:940`–`:942`，实际写成一个 argv。独立复核明确覆盖为 `b"--port\0" + b"17200\0"` 后再测试，以上通过及失败结论不依赖该不精确输入。应修正 fixture，避免继续声称提交内的原数据已逐参数分隔；本轮不另据此列代码阻断。

环境交叉检查也尚不能宣称完整合并 systemd 指令语义。已执行的多赋值输入 `Environment="USER=otheruser" "HOME=/var/lib/configured-home"` 与 runtime HOME 不同，却因 `:1208`–`:1209` 只匹配行首 HOME/USERPROFILE 而未拒绝。该案中 runtime 来源本身仍正确，不单独列 P1；执行方的“所有矛盾均拒绝”措辞必须限定到实际支持范围，或补足解析与拒绝策略。

`reg_args_agree` 的旧头注释已正确更新。`e03a44f` 确实提交 K9 报告、交接 §23、STATUS、状态表及 K8 归属纠正；`806ea6c` 确实只有哈希回填。候选仍残留“K9 仅本地”和旧下一步，本轮同步改为历史归属与 K10 入口。链接计数继续按固定树与本地同步工作树分别记在交接证据表，不以不同快照的数值互相替代。

## 4. 可复现步骤与处理边界

在固定候选仓库根目录用 PowerShell 执行。使用已有 harness，所有特权路径均映射到临时目录；没有 SSH 或生产访问。安装用例执行的 phase456 从真实 decision 的变量生成，未采用 harness 的占位配置。

```powershell
@'
import importlib.util
from pathlib import Path
import subprocess

spec = importlib.util.spec_from_file_location(
    "deploy_tests", Path("scripts/tests/test_deploy_linux_install.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
base = "/opt/ak-sending/ak-cli registry-server --port 17200"
normal_lines = ["Environment=HOME=/home/otheruser", "UnsetEnvironment=USERPROFILE"]
normal_env = b"HOME=/home/otheruser\0USER=otheruser\0"
cases = [
    ("empty-userprofile", ["Environment=HOME=/home/otheruser", "Environment=USERPROFILE="],
     b"HOME=/home/otheruser\0USERPROFILE=\0USER=otheruser\0", False, False),
    ("value-with-lf", ["Environment=HOME=/home/otheruser",
     'Environment="EXTRA=hello\\nUSERPROFILE=/root"'],
     b"HOME=/home/otheruser\0EXTRA=hello\nUSERPROFILE=/root\0USER=otheruser\0", False, False),
    ("target-empty", normal_lines, normal_env, False, True),
    ("target-decoy", normal_lines, normal_env, True, True),
]
for label, unit_env, environ, target_decoy, install in cases:
    h = m.DeployInstallTests()
    h.setUp()
    try:
        h.text = subprocess.check_output(
            ["git", "show", "806ea6c:scripts/deploy_linux.sh"]).decode("utf-8")
        h.make_private_discovery_model(main_exec=base,
            unit_service_lines=["User=otheruser", "WorkingDirectory=/var/lib/oldregistry"] + unit_env,
            environ=environ)
        (h.probe / "proc/4242/cmdline").write_bytes(
            b"\0".join(a.encode() for a in base.split()) + b"\0")
        for rel in ("var/lib/oldregistry/.ak-sending", "root/.ak-sending"):
            d = h.probe / rel
            d.mkdir(parents=True, exist_ok=True)
            (d / "registry.key").write_text("synthetic-key")
            (d / "registry.db").write_text("synthetic-db")
        st = h.probe / "systemctl-state"
        st.mkdir(exist_ok=True)
        for unit, pid in (("oldregistry.service", 4242), ("oldsignal.service", 4243)):
            for suffix, value in (("enabled", "1"), ("active", "active"), ("pid", str(pid))):
                (st / (unit + "." + suffix)).write_text(value)
            (st / (str(pid) + ".exe")).write_text("/opt/ak-sending/ak-cli")
        (h.probe / "etc/systemd/system/oldsignal.service").write_text(
            "[Service]\nExecStart=/opt/ak-sending/ak-cli signal-server\n")
        if target_decoy:
            d = h.probe / "opt-ak/data"
            d.mkdir(parents=True)
            (d / "registry.key").write_text("unrelated-target-key")
            (d / "registry.db").write_text("unrelated-target-db")
        rc, out = h.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || exit 90\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        phase = (h.probe / "p1.out").read_text(encoding="utf-8")
        drc, decision = h.run_decision_and_config(phase, "decision.sh")
        print("CASE", label, "HARNESS_RC", rc, out, phase, "DECISION_RC", drc, decision, flush=True)
        if install:
            body = 'WORK="$PROBE_DIR"\nKEEP_LEGACY_SIGNAL=0\nCHECK_ONLY=0\n'
            body += 'deploy_decision() {\n' + h.decision_block(h.text, "current")
            body += h.decisions_block(h.text, "current") + '\n}\ndeploy_decision\n'
            body += ('TS=t12345 BUILD_DIR= PREBUILT_MODE=1 STAGING_DIR="$PROBE_DIR/staging" '
                     'STAGED_BIN="$PROBE_DIR/staging/ak-cli" SHORT_SHA=deadbee CLOSE_7100=0 '
                     'write_phase456 "$PROBE_DIR/actual456.sh" || exit 90\n')
            body += 'bash "$PROBE_DIR/actual456.sh" > "$PROBE_DIR/actual456.out" 2>&1; echo "INSTALL_RC=$?"\n'
            irc, iout = h.run_driver(h.definitions() + m.BOX_MODELS, body,
                                    name="actual-driver.sh", timeout=120)
            print("INSTALL_DRIVER_RC", irc, iout,
                  (h.probe / "actual456.out").read_text(encoding="utf-8"), flush=True)
            for name in ("registry.key", "registry.db"):
                print("TARGET_" + name, (h.probe / "opt-ak/data" / name).read_text())
            print((h.probe / "etc/systemd/system/ak-registry-server.service").read_text())
    finally:
        h.doCleanups()
'@ | python -
```

空 USERPROFILE 案错误选 HOME，值内 LF 案错误选 root；目标对照均选择正确 HOME 来源，安装却在存在 decoy 时保留 decoy 并切换。应读取阶段退出值、CHOSEN、目标文件和新 unit；runner exit 0 只表示重放完成。

本轮门禁与最终 tip CI 结果见交接证据表，不能替代上述缺失的行为覆盖。Linux 原生 systemd/权限部署、物理双机、生产抓包仍未执行。ADR-001、无 relay、无隐藏代理回退、原始字节不压缩不转码约束不变。修复须继续在本批次分支完成，再交独立复核；本轮不合并 main。
