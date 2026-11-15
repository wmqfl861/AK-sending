# 批次 K 修复轮十四独立复核（2026-11-15）

复核范围：`dev/batch-k-20261114` 的 `943a346..f425256`；实现与文档提交为 `d38cb01`，`f425256` 只在交接回填一行提交号。固定 tip 为 `f42525632c743c575b9b150f267cea69ab3b3275`。结论为 **REJECTED**：cwd 二读失败和显式相对身份路径的原反例均已翻转，但环境变量的字节验证仍有可复现的失败放行；另确认显式相对数据库路径丢失运行 cwd 语义。

逐项裁决只在 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。命令、运行编号、计数和证据边界集中在 [K 交接 §35](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#35-修复轮十四独立复核证据2026-11-15)。本轮仅新增审查报告与同步本地文档，未修改被审代码、创建提交、合并、推送 main 或删除批次分支。下列源码行号均指固定 tip。

已用当前 git 凭据直接查询 GitHub run/jobs API，从 in_progress 等到该完整 SHA 的 completed/success，并核对分支、所有 job 的 head_sha 与常规步骤结果。手动 release 深测跳过不算执行。CI 成功不能消除下述独立反例。

## 1. K10-N1 / P1 余项：环境变量的验证读取失败仍确认截断来源

本轮修复的 cwd 分支确实使用单次 `readlink`，值与字节数均取自 tee 捕获的同一个文件。原 K14 的尾 LF＋第二次 cwd 读取失败注入返回 phase1=3，日志没有第二次失败标记；稳定尾 LF、内嵌 LF、普通 cwd 和不可读 cwd 控制符合预期。这项判断只覆盖已执行的输入，不扩大成任意非零返回且已有 stdout 的 readlink 组合都已验证。

未解决的是相邻的运行环境分支。`scripts/deploy_linux.sh:1247` 读取总字节数，`:1259` 和 `:1260` 再分别读取 USERPROFILE 与 HOME，`:1268` 为每个值重新读取字节证据。`:1270` 仍保留零计数跳过条件：

```bash
re_grep_bytes=$(sudo -n cat /proc/$reg_pid/environ 2>/dev/null | grep -zao -m1 "^${re_var}=.*" | wc -c || true)
re_bash_bytes=$(printf '%s' "$re_val" | wc -c)
if [ "$re_grep_bytes" -gt 0 ] \
   && [ "$((re_grep_bytes - 1))" != "$re_bash_bytes" ]; then
  ... # refusal
fi
```

输入为 NUL 分隔的 `HOME=/home/otheruser`、`USERPROFILE=/home/otheruser\n`（这里的 `\n` 表示真实 LF 字节）、`USER=otheruser`，unit 使用 EnvironmentFile 且没有显式 `--id-dir`。只让第四次 environ 读取，即 USERPROFILE 的验证读，返回 1 且无 stdout；总量读、两次取值和随后 HOME 的验证读全部成功。

Bash 捕获值时已经剥除 USERPROFILE 尾 LF；失败的验证管道给出 `wc -c=0`，`-gt 0` 条件使比较被跳过。实际 phase1、decision、install 均返回 0，确认 `/home/otheruser/.ak-sending`，并把前缀目录中的 `env-prefix-decoy-registry.key` 与 `env-prefix-decoy-registry.db` 真实复制到目标。这不是名称尾部含 LF 的运行目录。相同输入在 `943a346` 也复现，是既有环境保真余项。

全部读取成功的尾 LF 控制在 phase1 返回 3，且没有 stop 或目标 key；普通环境控制完整链成功；第四、第五次读取都失败的持续故障控制也错误放行。专项与主复核分别重放同一完整链，结果一致。Windows 未创建含 LF 的真实目录；错误前缀目录、目标文件、cp/cmp 与生成脚本实际执行，proc/权限故障为隔离模型。

**修复要求：** 将整个运行环境读取为一次成功的完整 NUL 快照，变量存在性、值解析和字节保真检查基于同一份输出。无法取得完整证据时在停服前拒绝，不能将读取失败降级成变量不存在或零字节后跳过。增加仅验证读失败、取值读失败、稳定特殊字节和普通控制；cwd 已修复的反例继续保留。

## 2. K15-N1 / P1：显式相对数据库路径丢失 cwd，生成 unit 改用另一份数据库

这是新发现的既有遗漏，同一输入在 `943a346` 也成立，不归为 `d38cb01` 新引入。`crates/ak-cli/src/main.rs:3792` 原样使用显式 `db_path`，`crates/ak-signal/src/server_v2/store.rs:397` 将它交给 `Connection::open(path)`；相对路径因此依赖运行进程 cwd。

可复现的 unit 与逐参数 NUL cmdline 一致：

```ini
[Service]
User=otheruser
WorkingDirectory=/home/runtime-old
ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /home/otheruser/.ak-sending --db-path home/otheruser/.ak-sending/registry.db
```

运行 cwd 模型为 `/home/runtime-old`。原数据库位于 `/home/runtime-old/home/otheruser/.ak-sending/registry.db`，带合成 marker `runtime-real`；另在 `/home/otheruser/.ak-sending/registry.db` 放入 marker 为 `new-cwd-decoy` 的数据库。两者共享同一个绝对身份目录和 key。

`scripts/deploy_linux.sh:1061`、`:1062` 对 DB 参数只比较文字；`:1238` 起推导的是身份目录。这个输入的身份目录已经绝对，因此 preflight 不读取运行 cwd。`:2308` 保留原 DB 参数；`:2354` 起新增的绝对化也只改 `--id-dir`。`:1991`、`:1996` 最终写出下列有效字段，没有 `WorkingDirectory=`：

```ini
ExecStart=<new-binary> registry-server --port 17200 --id-dir /home/otheruser/.ak-sending --db-path home/otheruser/.ak-sending/registry.db
ProtectSystem=strict
ReadWritePaths=/home/otheruser/.ak-sending
```

系统级 systemd 服务的默认 cwd 为 `/`，这个相对 DB 参数现在指向 `/home/otheruser/.ak-sending/registry.db`。错误对象就在新 unit 的 ReadWritePaths 内，不能把它解释成必然被 ProtectSystem 拦截。实际 Unix 用户权限仍需满足，本文不把 Windows 模型的 install=0 当作 Linux 服务成功启动。

当前与 `943a346` 的真实生成→phase1→decision→compose→write_phase456→install 均返回成功；生成的新 unit 对应另一数据库对象。只把 DB 参数改成原对象绝对路径的控制保留原对象。源代码均从对应 git 版本提取，决策与生成在同一次 Bash 中执行，没有手工替换生产 CHOSEN 结果。

主复核另复制版本明确为 `git f425256` 的原生 Windows CLI 到 TEMP，仅使用 `127.0.0.1:0`，按同一相对目录结构切换 cwd。相同绝对身份目录和同一份 key 下，原 cwd 读取 `runtime-real` 数据库；新 cwd 与相同相对参数实际读取 `new-cwd-decoy` 数据库；绝对参数控制回到原库。原库含合成 invite、诱饵库不含，均为 schema v3，所有实例实际监听且已终止。此验证证明 CLI 会打开不同 SQLite 对象，不是 Linux systemd 部署或物理双机验证。

**修复要求：** 将显式相对 `--db-path` 纳入运行 cwd 取证和字节保真校验，使新 unit 继续指向同一已验证的绝对数据库对象，并匹配所需的可写目录和权限；无法证明或维持时在停服前拒绝。回归保留同身份、不同 cwd、不同数据库的完整链，另加绝对 DB 控制和 cwd 不可读拒绝。仅绝对化身份目录不足以保证数据库连续性。

## 3. 本轮原反例与历史对照的证据精度

`--id-dir state` 原反例现在解析成 `/home/runtime-old/state`，真实迁移的 key/db 都为 runtime-real，生成 unit 的身份参数与 ReadWritePaths 都绝对化；绝对身份参数控制保持，不可读 cwd 在 phase1 拒绝。`943a346` 自身的生成/安装仍复制部署 cwd 的诱饵，构成有效对照。该结论不扩大到显式相对 DB 参数。

重建的 K13 引号 baseline 通过文件传递原始值，使用 `git show` 提取的 baseline write_phase456；生成源码确实保留 `DATA_SOURCE=/home/oth'er'user/.ak-sending`，执行后 key/db 都来自去引号目录。原 caller 提前剥引号的证据问题已修正。该测试范围为 baseline 生成→安装，没有声称它执行了 baseline phase1/decision；原 K13 当前拒绝路径与普通控制由本轮全集保留。

## 4. 可复现步骤

在固定候选仓库根目录，以 PowerShell 分别执行下列代码块。每个 runner 自建 TEMP 目录、只用合成数据，源码从指定 git 修订取得；退出 0 表示反例及控制通过断言。材料保留在输出目录，不写入仓库。前两段的 proc/systemctl/权限为隔离模型，第三段运行本机真实 Windows CLI。

### 4.1 环境字节验证失败的完整链

```powershell
@'
import importlib.util, json, re, shutil, subprocess, tempfile
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    'deploy_tests', Path('scripts/tests/test_deploy_linux_install.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
outdir = Path(tempfile.mkdtemp(prefix='ak-k15-env-evidence-'))
print('EVIDENCE_DIR=' + str(outdir), flush=True)
cases = [
    ('ordinary-control', 'f425256', False, 'none'),
    ('stable-tail-lf-refuses', 'f425256', True, 'none'),
    ('count-read-only-fails', 'f425256', True, 'fourth'),
    ('count-read-only-fails-baseline', '943a346', True, 'fourth'),
    ('both-count-reads-fail', 'f425256', True, 'from-fourth'),
]
results = []
for label, rev, tail_lf, failure in cases:
    h = m.DeployInstallTests()
    h.setUp()
    try:
        h.text = subprocess.check_output(
            ['git', 'show', rev + ':scripts/deploy_linux.sh']).decode('utf-8')
        profile = b'/home/otheruser' + (b'\n' if tail_lf else b'')
        environ = b'HOME=/home/otheruser\0USERPROFILE=' + profile + b'\0USER=otheruser\0'
        h.make_private_discovery_model(
            main_exec=h.K9_BASE, cmdline_extra='--port 17200',
            unit_service_lines=['User=otheruser',
                                'EnvironmentFile=/etc/default/oldregistry'],
            environ=environ)
        envfile = h.probe / 'etc/default/oldregistry'
        envfile.parent.mkdir(parents=True, exist_ok=True)
        envfile.write_bytes(b'HOME=/home/otheruser\nUSERPROFILE="' + profile + b'"\n')
        prefix_dir = h.probe / 'home/otheruser/.ak-sending'
        prefix_dir.mkdir(parents=True, exist_ok=True)
        for filename in ('registry.key', 'registry.db'):
            (prefix_dir / filename).write_text('env-prefix-decoy-' + filename,
                                             encoding='utf-8')
        # Modify only the privileged read model. Calls 1/2/3 succeed;
        # call 4 is the USERPROFILE verification read. Call 5 succeeds
        # in the narrow counterexample (or fails in the persistent control).
        stub = h.probe / 'bin/cat'
        stub_text = stub.read_text(encoding='utf-8')
        needle = 'mapped=()\n'
        assert stub_text.count(needle) == 1
        injection = r'''if [ "$*" = /proc/4242/environ ]; then
  n=0
  [ ! -f "$PROBE_DIR/env-read-count" ] || read -r n < "$PROBE_DIR/env-read-count"
  n=$((n + 1))
  printf '%s\n' "$n" > "$PROBE_DIR/env-read-count"
  printf 'ENV_READ_%s\n' "$n" >> "$PROBE_DIR/stubs.log"
  if { [ "${REVIEW_ENV_FAILURE:-none}" = fourth ] && [ "$n" = 4 ]; } \
     || { [ "${REVIEW_ENV_FAILURE:-none}" = from-fourth ] && [ "$n" -ge 4 ]; }; then
    printf 'ENV_VERIFICATION_READ_FAILED_%s\n' "$n" >> "$PROBE_DIR/stubs.log"
    exit 1
  fi
fi
'''
        stub.write_text(stub_text.replace(needle, injection + needle),
                        encoding='utf-8', newline='\n')
        h.driver_env['REVIEW_ENV_FAILURE'] = failure
        # Actual production decision -> compose -> generator remain in one
        # Bash process. No caller interpolation of decision-derived values.
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
# Reaching this marker records normal fall-through. The baseline ends in
# an exempt AND-list assignment whose false status is not a deploy error.
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
bash "$PROBE_DIR/phase456.sh" > "$PROBE_DIR/phase456.log" 2>&1
printf 'INSTALL_RC=%s\n' "$?"
'''
        driver_rc, stdout = h.run_driver(h.definitions() + m.BOX_MODELS, body,
                                        name='driver.sh', timeout=90)
        (h.probe / 'driver.out').write_text(stdout, encoding='utf-8')
        assert driver_rc == 0, stdout
        phase1 = (h.probe / 'phase1.log').read_text(encoding='utf-8')
        log = h.stubs_log()
        expected_refusal = tail_lf and failure == 'none'
        expected_phase = 3 if expected_refusal else 0
        assert f'PHASE1_RC={expected_phase}' in stdout, stdout
        markers = re.findall(r'^ENV_.*$', log, re.M)
        result = {'label': label, 'revision': rev, 'phase1_rc': expected_phase,
                  'environment': environ.decode().replace('\0', '<NUL>'),
                  'markers': markers}
        if expected_refusal:
            assert 'lost trailing bytes' in phase1, phase1
            assert 'runtime-id-dir:' not in phase1, phase1
            assert 'DECISION_RC=' not in stdout, stdout
            assert 'systemctl stop ' not in log, log
            assert not (h.probe / 'opt-ak/data/registry.key').exists()
        else:
            assert re.search(r'^  runtime-id-dir: /home/otheruser/\.ak-sending$',
                             phase1, re.M), phase1
            assert 'DECISION_RC=0' in stdout, stdout
            assert 'INSTALL_RC=0' in stdout, stdout
            chosen = dict(re.findall(r'^CHOSEN_(\w+)=(.*)$', stdout, re.M))
            assert chosen['DATA_SOURCE'] == '/home/otheruser/.ak-sending'
            assert chosen['PREOWNED'] == '0'
            assert chosen['EXEC'].endswith('--id-dir ' + chosen['RW'])
            generated = (h.probe / 'phase456.sh').read_text(encoding='utf-8')
            assert 'DATA_SOURCE=/home/otheruser/.ak-sending' in generated
            assert 'systemctl stop oldregistry.service' in log, log
            copied = {}
            for filename in ('registry.key', 'registry.db'):
                copied[filename] = (h.probe / 'opt-ak/data' / filename).read_text(encoding='utf-8')
                assert copied[filename] == 'env-prefix-decoy-' + filename, copied
            result.update(decision_rc=0, install_rc=0, chosen=chosen, copied=copied)
        if failure == 'fourth':
            assert markers.count('ENV_VERIFICATION_READ_FAILED_4') == 1, markers
            assert 'ENV_READ_5' in markers, markers
            assert 'ENV_VERIFICATION_READ_FAILED_5' not in markers, markers
        elif failure == 'from-fourth':
            assert 'ENV_VERIFICATION_READ_FAILED_4' in markers, markers
            assert 'ENV_VERIFICATION_READ_FAILED_5' in markers, markers
        else:
            assert not any('FAILED' in marker for marker in markers), markers
        results.append(result)
        print(json.dumps(result, ensure_ascii=True), flush=True)
    finally:
        shutil.copytree(h.probe, outdir / label)
        h.doCleanups()
(outdir / 'results.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
print('ALL_ASSERTIONS_PASSED', flush=True)
'@ | python -
```

### 4.2 相对 DB 参数、可写诱饵与绝对控制

```powershell
@'
import importlib.util, json, shutil, sqlite3, subprocess, tempfile, time
from pathlib import Path, PurePosixPath
ROOT=Path.cwd()
spec=importlib.util.spec_from_file_location('deploy_tests',ROOT/'scripts/tests/test_deploy_linux_install.py')
m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
out=Path(tempfile.mkdtemp(prefix='ak-k15-db-path-rwp-'))
print(out,flush=True)
results=[]
relative='home/otheruser/.ak-sending/registry.db'
identity='/home/otheruser/.ak-sending'
true_path='/home/runtime-old/'+relative
for label,rev,dbpath in [('relative-current','f425256',relative),('absolute-current','f425256',true_path),('relative-baseline','943a346',relative)]:
    h=m.DeployInstallTests();h.setUp()
    h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode()
    args='--port 17200 --id-dir '+identity+' --db-path '+dbpath
    h.make_private_discovery_model(main_exec='/opt/ak-sending/ak-cli registry-server '+args,cmdline_extra=args,unit_service_lines=['User=otheruser','WorkingDirectory=/home/runtime-old'],environ=b'HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0')
    (h.probe/identity.lstrip('/')/'registry.key').write_text('same-synthetic-identity',encoding='utf-8')
    for location,marker in [(true_path,'runtime-real'),('/'+relative,'new-cwd-decoy')]:
        db=h.probe/location.lstrip('/');db.parent.mkdir(parents=True,exist_ok=True)
        if db.exists():db.unlink()
        c=sqlite3.connect(db);c.execute('CREATE TABLE review_marker(value TEXT)');c.execute('INSERT INTO review_marker VALUES (?)',(marker,));c.commit();c.close()
    h.driver_env['PROC_CWD_MAP']='/proc/4242/cwd=/home/runtime-old'
    body=r'''
WORK="$PROBE_DIR"
KEEP_LEGACY_SIGNAL=0 CHECK_ONLY=0
write_phase1 "$PROBE_DIR/phase1.sh"
bash "$PROBE_DIR/phase1.sh" > "$WORK/phase1.log" 2>&1
p1_rc=$?
echo "PHASE1_RC=$p1_rc"
[ "$p1_rc" = 0 ] || exit "$p1_rc"
deploy_decision() {
''' + h.decision_block(h.text,rev) + h.decisions_block(h.text,rev) + r'''
echo "CHOSEN_EXEC=$REGISTRY_EXEC"
echo "CHOSEN_SOURCE=$DATA_SOURCE_DIR"
echo "CHOSEN_RW=$REG_RW"
}
deploy_decision
echo "DECISION_RC=$?"
TS=t12345 BUILD_DIR= PREBUILT_MODE=1
STAGING_DIR="$PROBE_DIR/staging" STAGED_BIN="$PROBE_DIR/staging/ak-cli"
mkdir -p "$STAGING_DIR"
cp "$AK_CLI_STUB" "$STAGED_BIN"
chmod +x "$STAGED_BIN"
SHORT_SHA=deadbee CLOSE_7100=0
write_phase456 "$PROBE_DIR/phase456.sh"
bash "$PROBE_DIR/phase456.sh" > "$PROBE_DIR/phase456.log" 2>&1
echo "INSTALL_RC=$?"
'''
    start=time.monotonic()
    rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='chain.sh',timeout=120)
    (h.probe/'chain.out').write_text(stdout,encoding='utf-8')
    unit=(h.probe/'etc/systemd/system/ak-registry-server.service').read_text(encoding='utf-8')
    exec_line=next(x for x in unit.splitlines() if x.startswith('ExecStart='))
    newdb=exec_line.split('--db-path ',1)[1].split()[0]
    rwp=next(x for x in unit.splitlines() if x.startswith('ReadWritePaths='))[len('ReadWritePaths='):]
    old_obj=str(PurePosixPath('/home/runtime-old')/dbpath)
    new_obj=str(PurePosixPath('/')/newdb)
    def marker_at(path):
        c=sqlite3.connect(h.probe/path.lstrip('/'));v=c.execute('SELECT value FROM review_marker').fetchone()[0];c.close();return v
    result={'case':label,'rev':rev,'driver_rc':rc,'phase1':'PHASE1_RC=0' in stdout,'decision':'DECISION_RC=0' in stdout,'install':'INSTALL_RC=0' in stdout,'unit_exec':exec_line,'unit_has_working_directory':'WorkingDirectory=' in unit,'read_write_paths':rwp,'old_db_object':old_obj,'new_db_object':new_obj,'old_object_marker':marker_at(old_obj),'new_object_marker':marker_at(new_obj),'seconds':round(time.monotonic()-start,3)}
    assert rc==0 and result['phase1'] and result['decision'] and result['install'],stdout
    assert not result['unit_has_working_directory']
    assert result['old_object_marker']=='runtime-real'
    assert (old_obj==new_obj)==(label=='absolute-current')
    if label!='absolute-current':
        assert str(PurePosixPath(new_obj).parent)==rwp==identity
        assert result['new_object_marker']=='new-cwd-decoy'
    else: assert result['new_object_marker']=='runtime-real'
    assert (h.probe/'opt-ak/data/registry.key').read_text()=='same-synthetic-identity'
    shutil.copytree(h.probe,out/label);h.doCleanups()
    results.append(result);print(json.dumps(result),flush=True)
(out/'results.json').write_text(json.dumps(results,indent=2),encoding='utf-8')
'@ | python -
```

### 4.3 Windows 原生 CLI 打开对象验证

先确保 `target/debug/ak-cli.exe` 已构建；本轮复制时实际版本为 `git f425256`，Windows workspace build 的独立结果另见交接。下列代码会输出真实版本，并终止自己启动的所有进程。身份 key 由 CLI 在 TEMP 生成，不进入仓库。此段无需 GitHub、SSH 或远端服务器。

```powershell
@'
import hashlib,json,os,shutil,sqlite3,subprocess,tempfile,time
from pathlib import Path
ROOT=Path.cwd();out=Path(tempfile.mkdtemp(prefix='ak-k15-native-db-rwp-'));print(out,flush=True)
exe=out/'ak-cli.exe';shutil.copy2(ROOT/'target/debug/ak-cli.exe',exe)
version=subprocess.check_output([str(exe),'--version']).decode().strip();print('VERSION='+version,flush=True)
box=out/'box';oldcwd=box/'home/runtime-old';newcwd=box
rel='home/otheruser/.ak-sending/registry.db';keydir=box/'home/otheruser/.ak-sending'
oldpath=oldcwd/rel;newpath=newcwd/rel
oldpath.parent.mkdir(parents=True);keydir.mkdir(parents=True)
records=[]
def launch(label,cwd,dbarg):
    cmd=[str(exe),'registry-server','--port','0','--bind','127.0.0.1','--id-dir',str(keydir),'--db-path',dbarg]
    log=out/(label+'.log');f=log.open('wb');p=subprocess.Popen(cmd,cwd=cwd,stdout=f,stderr=subprocess.STDOUT,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    try:
        for i in range(200):
            if p.poll() is not None:break
            txt=log.read_text(encoding='utf-8',errors='replace')
            if 'v2 registry server listening' in txt:break
            time.sleep(.05)
        txt=log.read_text(encoding='utf-8',errors='replace')
        rec={'case':label,'process_alive_before_termination':p.poll() is None,'listening':'v2 registry server listening' in txt,'db_arg_relative':not Path(dbarg).is_absolute(),'cwd':str(cwd.relative_to(box))}
        assert rec['process_alive_before_termination'] and rec['listening'],txt
    finally:
        if p.poll() is None:p.terminate()
        try:p.wait(timeout=10)
        except subprocess.TimeoutExpired:p.kill();p.wait(timeout=10)
        f.close()
    actual=Path(dbarg) if Path(dbarg).is_absolute() else cwd/dbarg
    con=sqlite3.connect(actual)
    tables=[x[0] for x in con.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
    rec.update(db_object=str(actual.relative_to(box)),user_version=con.execute('PRAGMA user_version').fetchone()[0],invite_count=con.execute('SELECT COUNT(*) FROM invite_codes').fetchone()[0],marker=con.execute('SELECT value FROM review_marker').fetchone()[0] if 'review_marker' in tables else None)
    con.close();records.append(rec);print(json.dumps(rec),flush=True)
launch('original-relative-initialize',oldcwd,rel)
shutil.copy2(oldpath,newpath)
for db,marker in [(oldpath,'runtime-real'),(newpath,'new-cwd-decoy')]:
    con=sqlite3.connect(db);con.execute('CREATE TABLE review_marker(value TEXT)');con.execute('INSERT INTO review_marker VALUES (?)',(marker,))
    if marker=='runtime-real':
        con.execute('INSERT INTO invite_codes (lookup_digest,code_id,owner_device_id,created_unix_ms,state,revision,create_request_id) VALUES (?,?,?,?,?,?,?)',(b'd'*32,b'c'*16,b'o'*32,int(time.time()*1000),'active',1,b'r'*16))
    con.commit();con.close()
key_before=hashlib.sha256((keydir/'registry.key').read_bytes()).digest()
launch('original-relative-confirm',oldcwd,rel)
launch('new-cwd-relative-decoy',newcwd,rel)
launch('new-cwd-absolute-control',newcwd,str(oldpath))
key_after=hashlib.sha256((keydir/'registry.key').read_bytes()).digest()
assert records[1]['marker']=='runtime-real' and records[1]['invite_count']==1
assert records[2]['marker']=='new-cwd-decoy' and records[2]['invite_count']==0
assert records[3]['marker']=='runtime-real' and records[3]['invite_count']==1
assert key_before==key_after
result={'version':version,'sha256':hashlib.sha256(exe.read_bytes()).hexdigest(),'same_identity':True,'loopback_only':True,'results':records}
(out/'results.json').write_text(json.dumps(result,indent=2),encoding='utf-8')
print('PASS',flush=True)
'@ | python -
```

## 5. 文档与范围

`d38cb01` 已纳入 K14 报告、交接 §33、STATUS、状态表、K13 历史归属及执行方 §34；`f425256` 只在交接回填实现提交号。本轮补明 K14 同步“当时本地、随后入库”的归属，新增 K15 与交接 §35，并同步当前入口和状态表。文档归属更正不是拒绝依据。

Rust、依赖和 workflow 在修复区间无差异。固定 tip CI、本机门槛、Shell 隔离模型与 Windows 原生 CLI 实验分别记录。没有生产 SSH、部署、Linux 原生 systemd 验证、物理双机或抓包；未执行的 release 深测与平台门槛在交接如实列出。ADR-001、无 relay/隐藏代理回退和原始字节约束不变；没有将模型输入当作密码学或真实路径认证证明。

历史归属补注（2026-11-16）：本报告及本地同步随后由执行方在 `0d58853` 入库，`0602f92` 仅回填该修复提交号。本页保留出具时点的历史裁决；新候选见 [K16 复核](REVIEW_2026-11-16-batchK16.zh-CN.md)。
