# 批次 K 修复轮十八独立复核（第 19 轮，2026-11-16）

复核范围：`dev/batch-k-20261114` 的 `4db2b4f..1d76871`；实现和文档提交为 `6d6d336`，`1d76871` 仅回填交接的一行提交号。固定 tip 为 `1d768710b4239fa4fcf03c263d5f2e7b7a1b0c01`。结论为 **REJECTED**：字节证据生产者守卫和原相对等号误拒均已修复，但等号路径开放后，一个合法的字面 DB 文件名能被改成另一数据库；新校验还接受尾部缺值或重复 DB 选项。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。实际命令、运行编号、计数、仓外证据及未执行边界只记在 [K 交接 §43](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#43-修复轮十八独立复核证据2026-11-16)。固定 tip CI 已通过 GitHub API 等到 completed/success；这不能覆盖下面的独立行为反例。本轮只在本地同步报告与文档，不修改被审实现、不创建提交、不合并、不推送、不删除分支。行号均指固定 tip。

## 1. 原修复的实际效果

`scripts/deploy_linux.sh:1403–1418` 已删除字节证据的 `grep | wc` 管道。证据 grep 重定向到临时文件，使用 `|| re_grep_rc=$?` 保护 errexit；任何非零状态进入拒绝分支，随后才对健康输出执行 `wc -c`。独立故障注入确认部分输出、完整输出和无输出后的错误均在消费计数前拒绝；生产者错误后 wc 没有被调用。仅令证据重定向失败时，grep 未执行，Bash 的重定向错误仍被捕获并进入产品拒绝诊断。临时文件创建或计数工具失败也没有继续选择来源、停服或写目标。

原尾 LF 夹具在当前拒绝，而 `4db2b4f` 相同 partial-match-then-error 故障仍完成离线安装并实际复制无 LF 前缀目录中的诱饵 key/db。健康尾 LF 继续因长度不符拒绝；普通 USERPROFILE 与仅 HOME 的健康控制完成正确来源的全链。这项修复通过。`:1400–1401` 注释仍写 `PIPESTATUS[0]`，与实际重定向捕获不符，属于非阻断文案残留。

`:2630–2646` 对改写后的 DB 参数已按独立选项的后继 token 或等号后缀取值，不再要求裸路径 token。原 `--db-path=state/registry.db` 输入现在完整链通过，生成 `--db-path=/home/runtime-old/state/registry.db`，身份参数字节不变，按生成参数读取合成 SQLite 得到运行库标记；`4db2b4f` 相同输入仍在 decision 误拒。绝对等号、普通空格、点前缀、路径内含选项字样和既有方括号拒绝控制也保留。原 K18 反例的修复有效；以下发现不撤回这两个精确判断。

## 2. K19-N1 / P1：原值仍按子串预解析，新校验接受了错误的绝对数据库

合法且 unit 与运行 argv 完全一致的输入如下，运行 cwd 为 `/home/runtime-old`：

```text
/opt/ak-sending/ak-cli registry-server --id-dir /home/otheruser/.ak-sending --db-path=--db-path --port 17200
```

DB 选项的值是字面文件名 `--db-path`，应继续使用 `/home/runtime-old/--db-path`。它是允许的 PathBuf 值，也只含现有白名单字符。主复核实际运行当前 CLI，并在 TEMP 中预置长度错误的合成 `registry.key`，使其在加载身份时必然退出、不能到达网络服务创建。程序通过真实 Clap 解析，打印当前版本标记后才返回预期的种子长度错误。因此该输入的合法性有真实解析证据，不依赖 systemctl stub。

改写前的 `:2575` 仍是贪婪子串提取：

```bash
rel_db=$(printf '%s\n' "$REGISTRY_EXEC" | sed -n 's/.*--db-path[= ]\([^ ]*\).*/\1/p' | head -1)
```

它将值内部的 `--db-path` 误当成选项起点，取到后面的 `--port`。`:2600` 因而构造 `/home/runtime-old/--port`，改写生成：

```text
--db-path=/home/runtime-old/--port --port 17200
```

新校验虽然正确解析了结果 token，却只与这个已算错的 `abs_db` 比较，于是接受。前置配置/运行参数比较 `:1107–1108` 和 cwd 判断 `:1575` 也保留同类子串解析，未能拦截。

主复核和专项均执行了实际 phase1、decision、安装脚本生成及离线安装。当前在已显式初始化的旧服务 active 状态下调用 stop，生成 unit 指向 `/home/runtime-old/--port`；依据该参数打开真实合成 SQLite 得到 `misparsed-token-decoy`。正确库 `/home/runtime-old/--db-path` 仍含 `runtime-real`，身份参数保持原值，生成的 ReadWritePaths 包含两个库所在目录。这是已复现的错误 DB 对象选择，不只是日志文字不准确。离线 install 成功不等于真实 systemd 服务已启动，也没有据此声称生产数据已被改动。

**归属：** `git blame` 将 `:2575` 追溯到 `0d58853`，错误预解析是既有缺口；`6d6d336` 开放等号形式后，使直接基线 `4db2b4f` 中于停服前拒绝的同一输入新通过并指向错误对象。旧版因裸 token 校验而拒绝并不意味着旧预解析正确；不能把该 sed 本身说成此次新增。

**修复要求：** 从原配置和运行 argv 阶段就按完整选项边界解析绑定值，以实际绑定的原值推导绝对路径，再核验生成参数。配置比较、cwd 判断、改写前取值和结果校验应使用一致语义，不能只修最后一次解析。保留此合法文件名、参数位置变换、普通/等号形式、身份不变及真实/诱饵 SQLite 的当前与基线对照；无法忠实承载时应在停服前拒绝。

## 3. K19-N2 / P2：未校验结束状态和选项个数，缺值或重复参数也被放行

本项运行 argv 始终是唯一合法的 `--db-path=state/registry.db`。只模拟运行后 unit 被编辑为以下形态，不伪造一个不可能启动的旧进程：

```text
--db-path=state/registry.db --db-path
--db-path=state/registry.db --db-path=state/registry.db
```

前置比较只提取一个值，两者都可进入改写。尾缺值形态在新循环结束后留下 `re_pending=1`，但 `:2645` 没有检查它；重复形态反复覆盖 `re_parsed`，`re_seen` 只是布尔值，最后一个值相等即可通过。当前完整链停用已运行的旧服务，并把缺值或重复参数原样带入生成 unit；直接基线的等号形态在 decision 拒绝。

主复核用真实当前 CLI 检查生成参数，分别得到 `a value is required for '--db-path <DB_PATH>'` 和 `cannot be used multiple times`。为防止误启动，检查将永久无效的 port 放在最后；断言必须先得到上述 DB 参数错误，而不把普通 port 错误当成证据。离线 systemctl 模型没有执行原生 registry CLI，故其 install 成功不能证明这个无效 unit 能启动。已证明的影响是预检和结果验证未拒绝，停服后才会遭遇不可用的命令。

重复空格形式在 `4db2b4f` 也能通过，说明前置形状检查不足是既有弱点；本轮新增等号接受路径扩大了这一放行范围。修复应在停服前要求唯一、完整的 DB 绑定值，检查 pending 结束状态，并按实际 CLI 语义拒绝重复项。与 K19-N1 一并统一解析，保留“合法运行 argv + 改坏 unit”的回归。

## 4. 验证层级与可重放材料

主复核下列代码读取固定 git 版本的真实生成器与决策，所有 proc、unit、key/db 与 SQLite 均为合成夹具，生成脚本、Bash、cp/cmp 与 SQLite 查询实际执行。系统状态和权限由 Windows Bash 模型提供。真实 CLI 辅助探针只验证参数解析与预设失败，没有启动服务。没有生产访问、SSH、原生 Linux systemd 部署、物理双机、抓包或 relay。

将代码保存到仓外系统 TEMP，从仓库根执行，输出目录使用新的 TEMP 子目录。`ALL_EXPECTATIONS_MATCHED` 表示当前缺陷及基线对照符合断言，不是产品通过。原修复的主复核 runner、各专项矩阵、CLI 预设失败证据及全仓门槛均列在交接证据表；报告不以静态检查替代行为验证。

### 4.1 合法字面文件名导致错库

```powershell
python <TEMP目录>/review_db_value.py --output <新的TEMP输出目录>
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
CASES=[('value-active-tip', '1d76871', '/home/otheruser/.ak-sending', '--db-path', '='), ('value-active-baseline', '4db2b4f', '/home/otheruser/.ak-sending', '--db-path', '=')]
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
  args='--id-dir '+identity+' --db-path'+sep+dbarg+' --port 17200'
  h.make_private_discovery_model(main_exec='/opt/ak-sending/ak-cli registry-server '+args,cmdline_extra=args,unit_service_lines=['User=otheruser','WorkingDirectory=/home/runtime-old'],environ=b'HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0')
  live=h.probe/identity.lstrip('/');live.mkdir(parents=True,exist_ok=True)
  (live/'registry.key').write_text('synthetic-runtime-key',encoding='utf8')
  (live/'registry.db').write_text('synthetic-identity-default-db',encoding='utf8')
  live_db=dbarg if dbarg.startswith('/') else '/home/runtime-old/'+dbarg
  alt_db=None if dbarg.startswith('/') else '/'+dbarg
  for obj,marker in [(live_db,'runtime-real'),(alt_db,'new-cwd-decoy'),('/home/runtime-old/--port','misparsed-token-decoy')]:
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
  rec['input_args']=args
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
 if r['case'].endswith('-baseline'):
  assert r['phase1_rc']==0 and r['driver_rc']==1 and 'install_rc' not in r and not r['stopped'],r
  assert 'rewrite did not take' in '\n'.join(r['refusal_lines']),r
 else:
  assert r['phase1_rc']==r['decision_rc']==r['install_rc']==0 and r['stopped'] and r['captured_old_state_active'],r
  assert r['generated_identity_arg']==r['input_identity'],r
  assert r['generated_db_arg']=='/home/runtime-old/--port' and r['selected_db_marker']=='misparsed-token-decoy',r
print('ALL_EXPECTATIONS_MATCHED',flush=True)
```

### 4.2 尾缺值与重复选项

先确保本机已执行 `cargo build --locked --workspace`，用于下列生成参数的真实 CLI 解析检查。

```powershell
python <TEMP目录>/review_db_edges.py --output <新的TEMP输出目录>
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
CASES=[('trailing-active-tip', '1d76871', '/home/otheruser/.ak-sending', 'state/registry.db', '='), ('trailing-active-baseline', '4db2b4f', '/home/otheruser/.ak-sending', 'state/registry.db', '='), ('duplicate-active-tip', '1d76871', '/home/otheruser/.ak-sending', 'state/registry.db', '=')]
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
  config_args=args+(' --db-path' if label.startswith('trailing-') else ' --db-path='+dbarg)
  h.make_private_discovery_model(main_exec='/opt/ak-sending/ak-cli registry-server '+config_args,cmdline_extra=args,unit_service_lines=['User=otheruser','WorkingDirectory=/home/runtime-old'],environ=b'HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0')
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
  rec['configured_args']=config_args;rec['running_args']=args
  rec['chosen']=dict(re.findall(r'^CHOSEN_(\w+)=(.*)$',stdout,re.M))
  install_log=h.probe/'phase456.log'
  if install_log.exists():
   rec['captured_old_state_active']='oldregistry:exists:enabled:active' in install_log.read_text(encoding='utf8')
  rec['refusal_lines']=[x for x in stdout.splitlines() if 'FAIL:' in x]
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  if unit.exists():
   u=unit.read_text(encoding='utf8');rec['unit_fields']=[x for x in u.splitlines() if x.startswith(('ExecStart=','ReadWritePaths=','User=','WorkingDirectory='))]
   ex=re.search(r'^ExecStart=(.*)$',u,re.M)[1]
   parsed=shlex.split(ex)
   parsed[0]=str((ROOT/'target/debug/ak-cli.exe').resolve())
   pi=parsed.index('--port');del parsed[pi:pi+2]
   parsed.extend(['--port','never-a-valid-port'])
   actual=subprocess.run(parsed,cwd=h.probe,capture_output=True,text=True,timeout=10)
   rec['cli_args']=parsed;rec['cli_rc']=actual.returncode;rec['cli_stderr']=actual.stderr
   expected_error='a value is required' if label.startswith('trailing-') else 'cannot be used multiple times'
   assert actual.returncode==2 and expected_error in actual.stderr,rec
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
 if r['case'].endswith('-baseline'):
  assert r['phase1_rc']==0 and r['driver_rc']==1 and 'install_rc' not in r and not r['stopped'],r
  assert 'rewrite did not take' in '\n'.join(r['refusal_lines']),r
 else:
  assert r['phase1_rc']==r['decision_rc']==r['install_rc']==0 and r['stopped'] and r['captured_old_state_active'],r
  assert r['generated_identity_arg']==r['input_identity'] and r['cli_rc']==2,r
print('ALL_EXPECTATIONS_MATCHED',flush=True)
```
