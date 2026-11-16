# 批次 K 修复轮十七独立复核（第 18 轮，2026-11-16）

历史归属补注（2026-11-16）：本报告及出具时的本地同步随后由执行方在 `6d6d336` 入库，`1d76871` 仅回填该修复提交号。本页保留当时的历史裁决；新候选复核见 [K19 复核](REVIEW_2026-11-16-batchK19.zh-CN.md)。

复核范围：`dev/batch-k-20261114` 的 `7a93314..4db2b4f`；代码与文档提交为 `063b80a`，`4db2b4f` 只在交接回填一行提交号。固定 tip 为 `4db2b4fe843c529dc53a064e8bc0fa461f3caeac`。结论为 **REJECTED**：实际值提取的退出码分流和原身份路径子串反例均已修复，但后续字节证据 grep 仍能在部分输出后报错时确认截断来源；新的结果校验还误拒绝合法的相对等号 DB 参数。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。实际命令、运行编号、计数、仓外材料、平台及未执行边界只记在 [K 交接 §41](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#41-修复轮十七独立复核证据2026-11-16)。本轮只新增报告和同步本地文档，不修改被审实现、不创建提交、不合并、不推送、不删除分支。下列源码行号均指固定 tip。

## 1. K10-N1 / P1 余项：实际提取已修复，随后字节证据的生产者错误仍被忽略

`scripts/deploy_linux.sh:1343–1365` 的第二次 grep 以 `|| rc=$?` 保护 errexit，按请求的矩阵分流：提取成功继续；提取 rc=1 且存在性 rc=0 拒绝快照不一致；两者 rc=1 允许合法缺失；提取 rc≥2 拒绝，诊断包含 `cannot determine the value`。独立模型核验了 USERPROFILE 和 HOME、无输出和完整输出后报错、正常缺失及空变量。原“存在性成功、实际提取报错无输出”反例现在在 phase1 拒绝，基线仍实际复制 HOME 诱饵。不能把后面的另一缺口说成这次 triage 没有生效。

后续 `:1387` 仍只有管道最后一个命令的状态：

```bash
re_grep_bytes=$(grep -zao -m1 "^${re_var}=.*" "$reg_env_raw" | wc -c)
```

生成的 phase1 使用 `set -eu`，没有 pipefail；`wc` 成功不证明 grep 已完整输出。`:1389–1390` 只验证计数非零及 `re_grep_bytes - 1 == re_bash_bytes`，未验证该次 grep 的退出码或末字节确实为 NUL。

输入快照含 `USERPROFILE=/home/runtime-real\n\n\0`，此处转义代表实际两个 LF 与 NUL。存在性、实际提取都健康返回 rc=0；Bash 命令替换移去 NUL 和尾 LF，留下无换行前缀。仅对后续 `-zao` 字节证据 grep 注入短输出：先由真实 grep 得到完整匹配，再只输出去掉最后两个字节的前缀，然后返回 rc=2。该输出仍带一个 LF，缺失的是另一个 LF 和 NUL。`wc` 把剩余 LF 当成计数中的最后一个字节，减一后恰好等于 Bash 已截断的字符串长度，校验错误通过。

主复核显式初始化旧服务 enabled/active/PID/running 状态。当前版 phase1、实际 decision、生成安装脚本与离线 install 均返回零，报告 `/home/runtime-real/.ak-sending`，真正复制的 key/db 内容是 `prefix-decoy-*`。相同尾 LF 快照在字节 grep 健康时正确拒绝，未 stop、未写目标；所以不是绕过输入约束的合法路径转换。

**归属：** `7a93314` 相同故障同样放行。`git blame` 将该管道及计数守卫追溯到 `0d58853`；这是本轮新发现的既有保真守卫余项，**不是 `063b80a` 新引入的实际值提取回归**。专项复用夹具把无 LF 前缀目录标作 `runtime-real-*`，只作为夹具字符串；主复核已改成明确的 `prefix-decoy-*`，两者均不能解释为原始带 LF 路径的正确来源。

**修复要求：** 对已知存在记录的字节证据 grep 也在 errexit 安全上下文捕获生产者状态，任何非零先拒绝，再消费完整结果和计数；不能以 `wc` 成功替代上游成功。保留部分输出后错误、完整输出后错误、无输出错误及健康尾 LF 拒绝控制。

## 2. K15-N1 / P2 新回归：等号形式改写成功，却被要求裸路径 token 的校验误拒绝

原身份目录包含 `--db-path-archive` 的空格形式夹具已经修复：遍历完整 token 后，身份参数字节不变，真实 DB 参数变成运行 cwd 下的绝对路径；基线仍输出损坏身份参数和相对 DB。普通空格、点前缀、绝对路径和相对方括号拒绝控制也符合预期。

但 `:2590–2592` 为等号形式生成单个 `--db-path=$abs_db` token，而 `:2606–2608` 只接受完全等于裸 `$abs_db` 的 token。下面是合法且 unit 与逐参数 NUL cmdline 一致的输入，运行 cwd 为 `/home/runtime-old`：

```text
--port 17200 --id-dir /home/otheruser/.ak-sending --db-path=state/registry.db
```

当前 phase1 成功，实际 decision 在 `:2610–2611` 以 `the --db-path rewrite did not take` 退出，未 stop、未 install。同一夹具在 `7a93314` 完整链成功，最终 unit 保留身份参数，DB 为 `--db-path=/home/runtime-old/state/registry.db`，按真实生成参数读取合成 SQLite 得到运行库标记。主复核只改变 DB 分隔符，身份参数保持空格形式，排除了身份等号解析的干扰。

这是 **`063b80a` 引入的可用性回归**，不描述为当前错源安装。绝对等号路径绕过相对改写，仍通过；它不能覆盖相对等号形式。新 `_make_k18_db_fixture` 在测试文件 `:1304` 固定使用空格，因此本轮新增回归没有覆盖该分支。

**修复要求：** 按真实 DB 选项与其值验证两种支持的拼写，或者先把两种形式归一为同一种 token 表达。应核验解析出的实际 DB 值等于已解析绝对路径，不能以任意位置有裸路径 token 代替选项绑定。补齐相对等号形式的当前/基线完整链、身份不变及绝对等号控制。

## 3. CI 重跑、事件时序与轮询预算

已直接用当前 git 凭据查询 GitHub run、attempt 和 jobs API。旧 SHA 的首次尝试保留 Linux `Tests (workspace)` failure，第二次尝试中的同名 Linux 作业及后续门槛 success，head SHA 精确相同。第二次视图中的其他成功作业仍有首次执行时间，不能把它们说成全套重新执行。旧区间 `0602f92..7a93314` 的 Rust 树无差异，重跑支持“同提交出现间歇性失败”的判断。

源码也支持执行方的状态与事件排序解释：`ak-session/src/scheduler/core.rs:396–409` 更新完成状态并创建事件，`ak-ffi/src/pairing.rs:1429–1440` 另有异步事件转发，快照回复可先于终态事件进入可轮询队列。当前 `ffi_pairing.rs:1052–1075` 等待事件自身，同时保留原断言。提取实际 helper 与等待/断言代码的行为探针中，迟到 completed 使旧单次 drain 失败、当前通过；永久缺失 completed 仍在预算后断言失败。因此这项加固对当前单任务 fixture 合理，没有把失败改成跳过或无条件成功。

**预算边界：** `drain_sched_events` 在 `:776–789` 内部一直轮询至 `AK_PENDING`，没有内部 deadline；新增 30 秒只在每次 drain 返回后检查，另有一次 25ms sleep 和调用开销。持续产生事件、永不返回 pending 的合成 mock 能越过外层预算，需要独立 watchdog。该 mock 超出此测试“已完成单次传输、有限生命周期事件”的实际输入，本轮没有复现真实 fixture 挂住，故不作为新的阻断回归；不能宣称它是覆盖任意生产者的严格墙钟上限。若要承诺该上限，应把 deadline/每批数量预算传入 drain。

API 重跑 success 本身不能唯一证明首次失败根因。本轮没有重新请求 K17 已被拒绝的日志下载接口，也没有切换身份；具体失败用例名称仍来自执行方诊断，源码排序与独立行为探针提供机制佐证。固定 tip CI 已等到 completed/success，本机常规门槛也通过；本机 release 中断恢复再次出现既有 IdleTimeout 断言失败，大文件深测因容量未启动。结果与未执行边界单列于交接，不用跨平台成功覆盖任何失败或跳过。

## 4. 可重放代码与验证层级

将以下代码分别保存到仓外系统 TEMP 中，从仓库根执行；输出目录须为新的 TEMP 子目录。脚本读取固定 git 版本的真实生成器与决策，故障仅注入临时 stubs。命令退出零表示发现和控制符合断言，**不是产品通过**。所有路径、key/db、SQLite 内容均为合成夹具。

Shell 生成与执行、cp/cmp 和 SQLite 读取是实际本机操作；proc、sudo、systemctl 和权限是 Windows Bash 离线模型。没有原生 Linux systemd 部署、生产访问、物理双机或抓包，不能把 install=0 写成真实服务部署成功。ADR-001、无 relay、凭据不入库及无强推约束保持。

### 4.1 字节证据生产者故障

```powershell
python <TEMP目录>/review_byte_evidence.py --active --output <新的TEMP输出目录>
```

```python
import importlib.util, json, re, shutil, subprocess, sys
from pathlib import Path

ROOT = Path.cwd()
args = sys.argv[1:]
OUT = Path(__file__).resolve().parent / 'grep'
if '--output' in args:
    index = args.index('--output')
    OUT = Path(args[index + 1]).resolve()
    del args[index:index + 2]
ACTIVE = '--active' in args
if ACTIVE:
    args.remove('--active')
OUT.mkdir(exist_ok=True)
spec = importlib.util.spec_from_file_location('deploy_tests', ROOT / 'scripts/tests/test_deploy_linux_install.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

CASES = [('tip-byte-healthy', '4db2b4f', 'present', 0, 0, False, 'refuse', 'USERPROFILE'), ('tip-byte-partial-error', '4db2b4f', 'present', 0, 0, True, 'prefix-decoy', 'USERPROFILE'), ('baseline-byte-partial-error', '7a93314', 'present', 0, 0, True, 'prefix-decoy', 'USERPROFILE')]
if args:
    CASES = [x for x in CASES if x[0] in args]
results = []
for label, rev, profile_kind, fail_at, fail_rc, emit, expected, variable in CASES:
    h = m.DeployInstallTests()
    h.setUp()
    try:
        h.text = subprocess.check_output(['git', 'show', rev + ':scripts/deploy_linux.sh']).decode('utf-8')
        env = b'HOME=/home/otheruser\0'
        if profile_kind != 'absent':
            env += b'USERPROFILE=' + (b'/home/runtime-real\n\n' if profile_kind == 'present' else b'') + b'\0'
        env += b'USER=otheruser\0'
        h.make_private_discovery_model(main_exec=h.K9_BASE, cmdline_extra='--port 17200',
            unit_service_lines=['User=otheruser', 'EnvironmentFile=/etc/default/oldregistry'], environ=env)
        envfile = h.probe / 'etc/default/oldregistry'
        envfile.parent.mkdir(parents=True, exist_ok=True)
        envfile.write_bytes(env.replace(b'\0', b'\n'))
        for path, marker in [('home/runtime-real/.ak-sending', 'prefix-decoy'),
                             ('home/otheruser/.ak-sending', 'fallback-decoy'),
                             ('home/runtime-old/.ak-sending', 'cwd-real')]:
            d = h.probe / path
            d.mkdir(parents=True, exist_ok=True)
            for name in ('registry.key', 'registry.db'):
                (d / name).write_text(marker + '-' + name, encoding='utf-8')
        h.driver_env.update(REVIEW_FAIL_AT=str(fail_at), REVIEW_FAIL_RC=str(fail_rc),
                            REVIEW_EMIT=str(int(emit)), REVIEW_VAR=variable, PROC_CWD_MAP='/proc/4242/cwd=/home/runtime-old')
        stub = h.probe / 'bin/grep'
        stub.write_text(r'''#!/usr/bin/env bash
set -u
if [ "${1:-}" = -zao ] && [ "${2:-}" = -m1 ] && [ "${3:-}" = '^USERPROFILE=.*' ] && [ "$REVIEW_EMIT" = 1 ]; then
  @GREP@ "$@" > "$PROBE_DIR/byte-record"
  count=$(wc -c < "$PROBE_DIR/byte-record")
  head -c "$((count - 2))" "$PROBE_DIR/byte-record"
  printf 'REVIEW_BYTE_GREP_PARTIAL_ERROR=2\n' >> "$PROBE_DIR/stubs.log"
  exit 2
fi
if [ "${1:-}" = -za ] && [ "${2:-}" = -m1 ] && [ "${3:-}" = "^${REVIEW_VAR}=" ]; then
  n=0
  [ ! -f "$PROBE_DIR/profile-grep-count" ] || read -r n < "$PROBE_DIR/profile-grep-count"
  n=$((n + 1))
  printf '%s\n' "$n" > "$PROBE_DIR/profile-grep-count"
  if [ "$n" = "$REVIEW_FAIL_AT" ]; then
    printf 'REVIEW_PROFILE_GREP_%s_ERROR=%s\n' "$n" "$REVIEW_FAIL_RC" >> "$PROBE_DIR/stubs.log"
    if [ "$REVIEW_EMIT" = 1 ]; then @GREP@ "$@"; fi
    exit "$REVIEW_FAIL_RC"
  fi
  @GREP@ "$@"
  rc=$?
  printf 'REVIEW_PROFILE_GREP_%s_RC=%s\n' "$n" "$rc" >> "$PROBE_DIR/stubs.log"
  exit "$rc"
fi
exec @GREP@ "$@"
'''.replace('@GREP@', m._posix_tool('grep')), encoding='utf-8', newline='\n')
        stub.chmod(0o755)
        body = r'''
WORK="$PROBE_DIR"
KEEP_LEGACY_SIGNAL=0
CHECK_ONLY=0
write_phase1 "$PROBE_DIR/phase1.sh" || exit 90
bash "$PROBE_DIR/phase1.sh" > "$PROBE_DIR/phase1.log" 2>&1
phase_rc=$?
printf 'PHASE1_RC=%s\n' "$phase_rc"
[ "$phase_rc" = 0 ] || exit 0
review_decision() {
''' + h.decision_block(h.text, rev) + h.decisions_block(h.text, rev) + r'''
printf 'DECISION_COMPLETED=1\n'
}
set -e
review_decision
printf 'DECISION_RC=%s\n' "$?"
printf 'CHOSEN_EXEC=%s\n' "$REGISTRY_EXEC"
printf 'CHOSEN_DATA_SOURCE=%s\n' "$DATA_SOURCE_DIR"
printf 'CHOSEN_RW=%s\n' "$REG_RW"
mkdir -p "$PROBE_DIR/staging"
cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"
chmod +x "$PROBE_DIR/staging/ak-cli"
TS=t12345
BUILD_DIR=
PREBUILT_MODE=1
STAGING_DIR="$PROBE_DIR/staging"
STAGED_BIN="$PROBE_DIR/staging/ak-cli"
SHORT_SHA=deadbee
CLOSE_7100=0
write_phase456 "$PROBE_DIR/phase456.sh" || exit 91
bash "$PROBE_DIR/phase456.sh" > "$PROBE_DIR/phase456.log" 2>&1
printf 'INSTALL_RC=%s\n' "$?"
'''
        if ACTIVE:
            body = r'''
printf '1\n' > "$ST/oldregistry.service.enabled"
printf 'active\n' > "$ST/oldregistry.service.active"
printf '4242\n' > "$ST/oldregistry.service.pid"
printf '%s\n' "$REMOTE_BIN" > "$ST/4242.exe"
printf '%s\n' "$REMOTE_BIN" > "$ST/oldregistry.service.running"
cp "$PROBE_DIR/ak-cli.oldstub" "$ST/oldregistry.service.running.bin"
printf 'REVIEW_OLD_ACTIVE_BEFORE=active\n' >> "$PROBE_DIR/stubs.log"
''' + body
        driver_rc, stdout = h.run_driver(h.definitions() + m.BOX_MODELS, body, timeout=180)
        (h.probe / 'driver.out').write_text(stdout, encoding='utf-8')
        phase = (h.probe / 'phase1.log').read_text(encoding='utf-8')
        log = h.stubs_log()
        phase_rc = int(re.search(r'^PHASE1_RC=(\d+)$', stdout, re.M).group(1))
        row = dict(label=label, rev=rev, variable=variable, driver_rc=driver_rc, phase1_rc=phase_rc,
                   markers=re.findall(r'^(?:REVIEW_|ENV_).*$', log, re.M),
                   reported_id=re.findall(r'^  runtime-id-dir: (.*)$', phase, re.M),
                   stopped='systemctl stop ' in log, active_model=ACTIVE,
                   errors=re.findall(r'^ERROR:.*$', phase, re.M))
        assert driver_rc == 0, stdout
        if expected == 'refuse':
            assert phase_rc == 3 and not row['stopped'], row
            assert 'lost trailing bytes' in phase, phase
            assert not (h.probe / 'opt-ak/data/registry.key').exists()
        else:
            assert phase_rc == 0 and 'DECISION_RC=0' in stdout and 'INSTALL_RC=0' in stdout, stdout
            copied = {n: (h.probe / 'opt-ak/data' / n).read_text(encoding='utf-8') for n in ('registry.key', 'registry.db')}
            assert all(v == expected + '-' + n for n, v in copied.items()), copied
            row.update(decision_rc=0, install_rc=0, copied=copied)
        if fail_at == 2:
            assert f'REVIEW_PROFILE_GREP_1_RC={1 if profile_kind == "absent" and variable == "USERPROFILE" else 0}' in log, log
            assert f'REVIEW_PROFILE_GREP_2_ERROR={fail_rc}' in log, log
        dest = OUT / label
        assert not dest.exists(), dest
        shutil.copytree(h.probe, dest)
        row['evidence'] = str(dest)
        results.append(row)
        (OUT / 'results.json').write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps(row, ensure_ascii=False), flush=True)
    finally:
        h.doCleanups()
```

### 4.2 相对等号形式 DB 参数

```powershell
python <TEMP目录>/review_token_walk.py equals-active-tip equals-active-baseline ordinary-active-tip --output <新的TEMP输出目录>
```

```python
import importlib.util,json,re,shlex,shutil,sqlite3,subprocess,sys,time
from pathlib import Path
ROOT=Path.cwd()
args_input=sys.argv[1:]
OUT=Path(__file__).parent
if '--output' in args_input:
 ix=args_input.index('--output')
 OUT=Path(args_input[ix+1]).resolve()
 del args_input[ix:ix+2]
OUT.mkdir(parents=True,exist_ok=True)
spec=importlib.util.spec_from_file_location('deploy_tests',ROOT/'scripts/tests/test_deploy_linux_install.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
CASES=[('substring-id-active-tip', '4db2b4f', '/home/otheruser/--db-path-archive/.ak-sending', 'state/registry.db', ' '), ('substring-id-active-baseline', '7a93314', '/home/otheruser/--db-path-archive/.ak-sending', 'state/registry.db', ' '), ('ordinary-active-tip', '4db2b4f', '/home/otheruser/.ak-sending', 'state/registry.db', ' '), ('equals-active-tip', '4db2b4f', '/home/otheruser/.ak-sending', 'state/registry.db', '='), ('equals-active-baseline', '7a93314', '/home/otheruser/.ak-sending', 'state/registry.db', '='), ('absolute-active-tip', '4db2b4f', '/home/otheruser/.ak-sending', '/home/runtime-old/state/registry.db', ' '), ('absolute-equals-active-tip', '4db2b4f', '/home/otheruser/.ak-sending', '/home/runtime-old/state/registry.db', '='), ('bracket-tip', '4db2b4f', '/home/otheruser/.ak-sending', 'state/db[1].sqlite', ' '), ('dot-active-tip', '4db2b4f', '/home/otheruser/.ak-sending', './state/registry.db', ' ')]
if args_input:
 CASES=[c for c in CASES if c[0] in args_input]
def token_value(line,key):
 argv=shlex.split(line)
 for i,x in enumerate(argv):
  if x==key:return argv[i+1]
  if x.startswith(key+'='):return x[len(key)+1:]
 return None
results=[];start=time.monotonic()
for label,rev,identity,dbarg,sep in CASES:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  args='--port 17200 --id-dir '+identity+' --db-path'+sep+dbarg
  h.make_private_discovery_model(main_exec='/opt/ak-sending/ak-cli registry-server '+args,cmdline_extra=args,unit_service_lines=['User=otheruser','WorkingDirectory=/home/runtime-old'],environ=b'HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0')
  live=h.probe/identity.lstrip('/');live.mkdir(parents=True,exist_ok=True)
  (live/'registry.key').write_text('synthetic-runtime-key',encoding='utf8')
  (live/'registry.db').write_text('synthetic-identity-default-db',encoding='utf8')
  live_db=dbarg if dbarg.startswith('/') else '/home/runtime-old/'+dbarg
  alt_db=None if dbarg.startswith('/') else '/'+dbarg
  for obj,marker in [(live_db,'runtime-real'),(alt_db,'new-cwd-decoy')]:
   if obj is None:continue
   path=h.probe/obj.lstrip('/');path.parent.mkdir(parents=True,exist_ok=True)
   if path.exists():path.unlink()
   con=sqlite3.connect(path);con.execute('CREATE TABLE review_marker(value TEXT)');con.execute('INSERT INTO review_marker VALUES (?)',(marker,));con.commit();con.close()
  if '-active-' in label:
   # Match the ss/proc listener model with actual stateful systemctl state.
   state=h.probe/'systemctl-state';state.mkdir(parents=True,exist_ok=True)
   for suffix,value in [('enabled','1'),('active','active'),('pid','4242')]:
    (state/('oldregistry.service.'+suffix)).write_text(value+'\n',encoding='utf8')
   (state/'4242.exe').write_text('/opt/ak-sending/ak-cli\n',encoding='utf8')
   (state/'oldregistry.service.running').write_text('/opt/ak-sending/ak-cli\n',encoding='utf8')
   (state/'oldregistry.service.running.bin').write_text(m.AK_CLI_OLD,encoding='utf8')
  h.driver_env['PROC_CWD_MAP']='/proc/4242/cwd=/home/runtime-old'
  body=r'''
WORK="$PROBE_DIR"
KEEP_LEGACY_SIGNAL=0
CHECK_ONLY=0
write_phase1 "$PROBE_DIR/phase1.sh" || exit 90
bash "$PROBE_DIR/phase1.sh" > "$PROBE_DIR/phase1.log" 2>&1
phase_rc=$?
printf 'PHASE1_RC=%s\n' "$phase_rc"
[ "$phase_rc" = 0 ] || exit 0
set -e
review_decision() {
'''+h.decision_block(h.text,rev)+h.decisions_block(h.text,rev)+r'''
printf 'DECISION_COMPLETED=1\n'
}
review_decision
printf 'DECISION_RC=%s\n' "$?"
printf 'CHOSEN_EXEC=%s\n' "$REGISTRY_EXEC"
printf 'CHOSEN_DATA_SOURCE=%s\n' "$DATA_SOURCE_DIR"
printf 'CHOSEN_RW=%s\n' "$REG_RW"
printf 'CHOSEN_PREOWNED=%s\n' "$REMOTE_DATA_PREOWNED"
mkdir -p "$PROBE_DIR/staging"
cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"
chmod +x "$PROBE_DIR/staging/ak-cli"
TS=t12345
BUILD_DIR=
PREBUILT_MODE=1
STAGING_DIR="$PROBE_DIR/staging"
STAGED_BIN="$PROBE_DIR/staging/ak-cli"
SHORT_SHA=deadbee
CLOSE_7100=0
write_phase456 "$PROBE_DIR/phase456.sh" || exit 91
set +e
bash "$PROBE_DIR/phase456.sh" > "$PROBE_DIR/phase456.log" 2>&1
printf 'INSTALL_RC=%s\n' "$?"
'''
  driver_rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='driver.sh',timeout=180)
  (h.probe/'driver.out').write_text(stdout,encoding='utf8')
  p1=(h.probe/'phase1.log').read_text(encoding='utf8');log=h.stubs_log()
  rec={'case':label,'revision':rev,'input_identity':identity,'input_db':dbarg,'driver_rc':driver_rc,'phase1_rc':int(re.search('PHASE1_RC=(\\d+)',stdout)[1]),'runtime_fields':[x for x in p1.splitlines() if re.match(r'  runtime-',x)],'stopped':bool(re.search(r'^systemctl stop ',log,re.M))}
  dec=re.search('DECISION_RC=(\\d+)',stdout);ins=re.search('INSTALL_RC=(\\d+)',stdout)
  if dec:rec['decision_rc']=int(dec[1])
  if ins:rec['install_rc']=int(ins[1])
  rec['chosen']=dict(re.findall(r'^CHOSEN_(\w+)=(.*)$',stdout,re.M))
  install_log=h.probe/'phase456.log'
  if install_log.exists():
   rec['captured_old_state_active']='oldregistry:exists:enabled:active' in install_log.read_text(encoding='utf8')
  rec['refusal_lines']=[x for x in stdout.splitlines() if 'FAIL:' in x]
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  if unit.exists():
   u=unit.read_text(encoding='utf8');rec['unit_fields']=[x for x in u.splitlines() if x.startswith(('ExecStart=','ReadWritePaths=','User=','WorkingDirectory='))]
   ex=re.search(r'^ExecStart=(.*)$',u,re.M)[1]
   db=token_value(ex,'--db-path');ident=token_value(ex,'--id-dir')
   rec['generated_db_arg']=db;rec['generated_identity_arg']=ident
   obj=h.probe/db.lstrip('/')
   con=sqlite3.connect(obj);rec['selected_db_marker']=con.execute('SELECT value FROM review_marker').fetchone()[0];con.close()
  dest=OUT/label;shutil.copytree(h.probe,dest,dirs_exist_ok=True)
  (dest/'result.json').write_text(json.dumps(rec,indent=2),encoding='utf8')
  results.append(rec);print(json.dumps(rec),flush=True)
 finally:h.doCleanups()
(OUT/('results-'+ '-'.join(c[0] for c in CASES[:2])+'.json')).write_text(json.dumps(results,indent=2),encoding='utf8')
print('ELAPSED_SECONDS='+str(time.monotonic()-start),flush=True)
for r in results:
 if r['case']=='equals-active-tip':
  assert r['phase1_rc']==0 and r['driver_rc']==1 and 'install_rc' not in r and not r['stopped'],r
  assert 'rewrite did not take' in '\n'.join(r['refusal_lines']),r
 elif r['case']=='bracket-tip':
  assert r['phase1_rc']==0 and r['driver_rc']==1 and 'install_rc' not in r and not r['stopped'],r
  assert 'cannot bake faithfully' in '\n'.join(r['refusal_lines']),r
 elif r['case']=='substring-id-active-baseline':
  assert r['phase1_rc']==r['decision_rc']==r['install_rc']==0 and r['generated_db_arg']==r['input_db'] and r['generated_identity_arg']!=r['input_identity'] and r['selected_db_marker']=='new-cwd-decoy',r
 else:
  assert r['phase1_rc']==r['decision_rc']==r['install_rc']==0 and r['generated_db_arg'].startswith('/') and r['generated_identity_arg']==r['input_identity'] and r['selected_db_marker']=='runtime-real',r
print('ALL_EXPECTATIONS_MATCHED',flush=True)

```
