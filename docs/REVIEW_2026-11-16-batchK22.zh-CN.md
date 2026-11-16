# 批次 K 修复轮二十一独立复核（第 22 轮，2026-11-16）

复核范围：`dev/batch-k-20261114` 的 `714099a..a369e1c`；实现与文档提交 `6964239`，`a369e1c` 只回填交接的一行哈希。固定 tip 为 `a369e1c250cd64c80922683ef82dd3e983416c7a`。结论为 **REJECTED**：K21 的身份参数贪婪误绑和空格家目录分词反例已翻转，但生成命令仍可能漏写身份参数，目录枚举及存在性错误分类也仍可漏掉真实候选。下述问题均有旧版本证据，不能称为 `6964239` 新增回归；各自的历史验证层级见正文。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。实际命令、运行编号、计数、仓外材料与未执行范围统一记在 [K 交接 §49](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#49-修复轮二十一独立复核证据2026-11-16)。本轮通过当前 Git 凭据查询 GitHub API，已等到固定 tip CI completed/success；现有 CI 通过不覆盖下述独立反例。依据 REJECTED，仅同步本地审核文档，保留 main 和批次分支，不提交、合并、推送或删除分支。以下源码行号均指固定 tip。

## 1. K21 修复的机械核对

`bound_id_dir()`、`id_binding_invalid()` 已应用于 phase1 的两侧身份比较与运行身份推导；driver 的 ReadWritePaths 提取、相对身份提取和改写也使用完整 token 边界。等号形式取自身后缀，空格形式取后继 token，遍历禁止 glob 展开。K21 §5.1 原夹具在当前两种拼写下均逐字绑定 `/home/otheruser/--id-dir`，迁移真实 key/db，保留正确的生成 argv 与 ReadWritePaths；`110bb34` 自身完整链仍绑定诱饵目录并生成损坏 argv。普通目录、相对 `state/--id-dir` 及合法 `--id-dir=--id-dir` 控制保持。

空等号、后继长选项、悬空及重复绑定的 helper 矩阵将各形态分别放在 configured/running 两侧，均在比较前返回语法拒绝；合法字面控制通过。当前真实构建的 git a369e1c CLI 对相同非法输入返回 Clap 错误，诊断与实现引用的错误串一致；原合法路径及修复后的 argv 通过参数解析，在预置无效长度的合成 key 加载处失败。该探针不启动服务。

家目录循环现以 `while IFS= read -r` 消费 `sudo ls -1` 成功取得的文本。重放 K21 §5.2 并让模型调用真实 GNU ls，`other user` 保持完整候选，在安装前由路径白名单拒绝；带空格加普通目录保留完整候选集合并触发多候选拒绝；普通单目录正确迁移。`714099a`、`110bb34` 自身完整链仍复制前缀诱饵。该修复覆盖空格分词，不能扩大为文件名记录协议已无歧义，见 K22-N2。

文档范围也已核对：`6964239` 纳入 K21 报告、交接 §47、STATUS、状态表及执行方 §48；`a369e1c` 仅回填 §48 的实现提交号。执行方的历史本机运行声明与本轮独立运行分别记录，不由提交存在推定历史测试发生。

## 2. K22-N1 / P1：其他参数中的身份选项子串抑制默认参数，停服后生成指向另一身份位置的命令

旧 unit 的 `User=otheruser`、`Environment=HOME=/home/otheruser`、`UnsetEnvironment=USERPROFILE` 与运行环境一致。configured 和 running argv 完全相同，真正的 `--id-dir` 缺省：

```text
registry-server --port 17200 --db-path /home/otheruser/--id-dir.sqlite
```

等号形式 `--db-path=/home/otheruser/--id-dir.sqlite` 同样触发。所有路径字符都在已有白名单内；DB 是 TEMP 中真实创建的 SQLite，旧身份目录 `/home/otheruser/.ak-sending` 内是合成 key/db 标记。

phase1 正确输出 `runtime-id-dir: /home/otheruser/.ak-sending`。但 `scripts/deploy_linux.sh:464` 的 `grep -q -- '--id-dir'` 把 DB 文件名中的子串当成独立身份选项存在，`:465` 因而省略应该追加的 `--id-dir $REMOTE_DATA`。后续 `:2828` 的新 helper 正确返回空，只让 `REG_RW` 回退；`:2882` 的语法门允许真正缺省，`:2887` 的空值分支不做改写，也没有最终身份绑定要求。

主复核执行生成器的完整 phase1→decision→install 链，捕获旧 `enabled/active` 状态并实际调用模型 stop，目标 key/db 的实际字节确实来自正确的旧身份目录。生成 unit 却是：

```text
User=akreg
ExecStart=<新安装的ak-cli> registry-server --port 17200 --db-path /home/otheruser/--id-dir.sqlite
Environment=RUST_LOG=info
ReadWritePaths=<标准数据目录>
```

这里最终 argv 缺少 `--id-dir`，unit 也未保留旧服务环境。`crates/ak-cli/src/main.rs:709` 的实际默认规则为 `USERPROFILE`、否则 `HOME`、否则 `.`，然后追加 `.ak-sending`；不会自动使用已复制的标准数据目录。部署脚本在创建新服务用户时还以 `$REMOTE_BASE` 为其 home（`:2061`），与 `$REMOTE_DATA` 不同。

当前真实 CLI 的安全探针进一步确认了读取对象：将生成 argv 中合成的绝对模型路径映射到 TEMP，在合成新服务环境下，缺省身份的生成命令尝试读取 `new-service-home/.ak-sending/registry.key`；同一原始命令使用旧环境时读取 `old-runtime-home/.ak-sending/registry.key`。把 DB 名改成 `ordinary.sqlite` 后，生成器正确补上身份选项，同一新环境下 CLI 实际读取已迁移的 `opt-ak/data/registry.key`。各目标都事先放置非合法长度的合成文本，初始化在 bind/spawn 前失败；不声称已在原生 systemd 中更换身份或启动服务。

`a369e1c`、直接前版 `714099a` 和 `110bb34` 的两种拼写各自完整链均复现；普通 DB 控制保持。存在性判断的 `git blame` 为 `dbb46fc`。这是本轮发现的既有缺口，与已修复的 K21 值提取误绑不同。

**修复要求：** 身份选项的存在性判断也必须遵守完整参数边界。真正缺省时明确绑定承载已迁移身份的绝对目录，并验证最终 argv 的实际身份值。回归应覆盖 DB 值内含 `--id-dir` 而真正身份选项缺省的两种拼写，检查旧服务状态、实际迁移字节和 CLI 最终身份加载对象。

## 3. K22-N2 / P2：LF 分隔不能表示含换行的目录名，漏计候选后继续迁移

无 live occupant 的合成主机存在 `/home/other\nuser/.ak-sending` 和 `/home/otheruser/.ak-sending`；这里 `\n` 表示文件名内真实 LF。两个目录分别保存不同的合成 key/db。真实 GNU `ls -1` 的输出字节为：

```text
6f746865720a757365720a6f74686572757365720a
```

即 `other LF user LF otheruser LF`。`:1939` 将这种输出读入 `home_list`，`:1965–1968` 的循环无法区分名称内部 LF 与记录分隔符。它探测不存在的 `/home/other/.ak-sending` 和 `/home/user/.ak-sending`，从未探测完整的含 LF 目录，phase1 只报告普通目录。`:2690–2697` 随后把它视为唯一候选并选择迁移。

当前、`714099a` 和 `110bb34` 各自完整链均继续安装；目标实际 key/db 来自普通目录，模型执行 `enable --now`，含 LF 的真实来源保持原样。这里没有旧 active occupant，也没有 stop。只有含 LF 来源的补充案将其视为 none-found 后继续 enable，但模型没有创建真实密钥，不能称为已经生成新身份。tab 加普通目录、两个普通目录均正确触发多候选拒绝，枚举命令失败则在 phase1 拒绝。

原 `ls -1` producer 的 `git blame` 为 `e8b621a`；当前 read consumer 为 `6964239`。旧版本自身完整链也复现，故不归因于本轮新增回归。Windows 上的实验用真实 MSYS mkdir 和 GNU ls：MSYS 把 POSIX LF/tab 名映射为 NTFS backing 名中的 U+F00A/U+F009，返回的 GNU ls 字节确实包含 LF/tab。Python 只使用 backing 名保存证据；该证据不是原生 Linux 部署验证。

**修复要求：** 枚举必须使用无歧义记录边界，例如 NUL；进入当前行式报告前，明确拒绝不能忠实表示的名称，并保留 producer 的失败状态。不能先按 LF 拆开名称，再对白名单过滤后的片段做候选判断。

## 4. K22-N3 / P2：路径中的错误文本被当成不存在证据，权限错误被吞掉

`scripts/deploy_linux.sh:1134–1143` 的 `priv_exists()` 对失败诊断整行做任意位置子串匹配。`:1140` 的分支为：

```bash
*"No such file or directory"*) echo no; return 0 ;;
```

当真实合成目录名为 `No such file or directory`，仓库现有离线权限模型返回：

```text
ls: cannot access '/home/No such file or directory/.ak-sending': Permission denied
```

原命令退出状态是错误；函数却因被引用的路径含有该短语，将它转换成成功的 `no`。完整链夹具同时存在该候选和普通 `/home/otheruser/.ak-sending`，只对前者注入已有 `LS_PERMDENY_PATH` 模型错误。当前 phase1、decision、install 全部继续，报告仅剩普通来源，实际复制其 key/db 并 enable。无 live occupant、无 stop，两个来源保持原样。

把同一权限错误施加于普通目录名时，phase1 正确拒绝；让含错误文本的目录可读时，也正确发现多候选并拒绝。因而问题是路径文本污染错误分类，不是多候选策略缺席。从 `a369e1c`、`714099a`、`110bb34` 各自生成脚本提取的原 helper 均做同样误分类，`git blame` 为 `46473fc`。完整链只在当前版本执行；基线证据为函数级，不扩大为基线完整链实测。

**修复要求：** 只有明确的“不存在”证据才能返回 `no`；被诊断引用的路径字节不得参与错误类别判断。权限拒绝或无法分类时应保留错误，在选择来源、停止或启用服务前退出。回归应保留错误文本目录名、普通目录名和可读多候选控制。

## 5. 验证边界与后续

本轮只评审本项目自有工具及离线模型。Bash、生成脚本、文件复制、GNU ls、SQLite 创建和 CLI 参数/身份加载实际执行；proc、systemctl 与权限行为为模型。K22-N3 是既有模型的故障注入，没有真实权限请求或权限绕过。无生产访问、SSH、物理双机、抓包、relay、真实密钥或用户数据入库。

Rust、依赖和 workflow 在固定范围内没有变化。CI 的 Linux/Windows Rust、Android 构建和 NAT netns 模拟各有自己的证据层级；portability check 不称应用构建。手动 release 深测跳过不算通过，历史 release chunk sweep 超时也未在本轮重跑或解决。本机实际门槛与这些边界统一见交接。

执行方修复上述边界后，应保留每项反例的当前/基线证据层级以及正常控制，再提交下一轮复核。此轮不满足快进合入 main 的条件。

## 6. 可重放代码

将下面各段分别保存到仓外新 TEMP 目录，从仓库根执行；输出也使用新 TEMP 目录。代码直接从固定提交提取生成器，通过仓库现有模型运行，无 SSH 调用。`ALL_EXPECTATIONS_MATCHED` 表示缺陷和控制符合断言，不表示产品通过。以下代码与主复核执行文件逐字一致。CLI 脚本需要本轮 `cargo build --locked --workspace` 的 git a369e1c debug 二进制。

### 6.1 身份存在性完整链

```powershell
python <TEMP>/review_compose.py --output <TEMP>/compose-evidence
```

```python
import importlib.util,json,re,shlex,shutil,sqlite3,subprocess,sys,time
from pathlib import Path
ROOT=Path.cwd(); selection=sys.argv[1:]; OUT=Path(__file__).parent/'identity-evidence'
if '--output' in selection:
 i=selection.index('--output');OUT=Path(selection[i+1]);del selection[i:i+2]
OUT.mkdir(parents=True,exist_ok=True)
spec=importlib.util.spec_from_file_location('deploy_tests',ROOT/'scripts/tests/test_deploy_linux_install.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
identity='/home/otheruser/.ak-sending'
base='--id-dir '+identity+' --port 17200'
CASES=[
('id-substring-db-space-tip','a369e1c','--port 17200 --db-path /home/otheruser/--id-dir.sqlite','--port 17200 --db-path /home/otheruser/--id-dir.sqlite','/home/otheruser/.ak-sending'),
('id-substring-db-equals-tip','a369e1c','--port 17200 --db-path=/home/otheruser/--id-dir.sqlite','--port 17200 --db-path=/home/otheruser/--id-dir.sqlite','/home/otheruser/.ak-sending'),
('id-substring-db-space-baseline','714099a','--port 17200 --db-path /home/otheruser/--id-dir.sqlite','--port 17200 --db-path /home/otheruser/--id-dir.sqlite','/home/otheruser/.ak-sending'),
('id-substring-db-equals-baseline','714099a','--port 17200 --db-path=/home/otheruser/--id-dir.sqlite','--port 17200 --db-path=/home/otheruser/--id-dir.sqlite','/home/otheruser/.ak-sending'),
('id-substring-db-space-110bb34','110bb34','--port 17200 --db-path /home/otheruser/--id-dir.sqlite','--port 17200 --db-path /home/otheruser/--id-dir.sqlite','/home/otheruser/.ak-sending'),
('id-substring-db-equals-110bb34','110bb34','--port 17200 --db-path=/home/otheruser/--id-dir.sqlite','--port 17200 --db-path=/home/otheruser/--id-dir.sqlite','/home/otheruser/.ak-sending'),
('id-ordinary-db-control','a369e1c','--port 17200 --db-path /home/otheruser/ordinary.sqlite','--port 17200 --db-path /home/otheruser/ordinary.sqlite','/home/otheruser/.ak-sending'),
]
if selection: CASES=[c for c in CASES if c[0] in selection]
results=[]
for label,rev,running,configured,idpath in CASES:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  h.make_private_discovery_model(main_exec='/opt/ak-sending/ak-cli registry-server '+configured,cmdline_extra=running,unit_service_lines=['User=otheruser','Environment=HOME=/home/otheruser','UnsetEnvironment=USERPROFILE'],environ=b'HOME=/home/otheruser\0USER=otheruser\0')
  for path,key,db in [(idpath,'runtime-real-key','runtime-real-db'),('/home/runtime-old/--port','substring-decoy-key','substring-decoy-db')]:
   live=h.probe/path.lstrip('/');live.mkdir(parents=True,exist_ok=True)
   (live/'registry.key').write_text(key,encoding='utf8');(live/'registry.db').write_text(db,encoding='utf8')
  dbfile=h.probe/'home/otheruser'/('ordinary.sqlite' if label=='id-ordinary-db-control' else '--id-dir.sqlite')
  c=sqlite3.connect(dbfile);c.execute('create table marker(value text)');c.execute("insert into marker values ('runtime-real')");c.commit();c.close()
  decoy=h.probe/'root/.ak-sending';decoy.mkdir(parents=True,exist_ok=True);(decoy/'registry.key').write_text('synthetic-new-home-key',encoding='utf8')
  h._make_oldregistry_running()
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
  rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='driver.sh',timeout=180)
  (h.probe/'driver.out').write_text(stdout,encoding='utf8')
  p1=(h.probe/'phase1.log').read_text(encoding='utf8');log=h.stubs_log()
  rec={'case':label,'revision':rev,'configured':configured,'running':running,'driver_rc':rc,'phase1_rc':int(re.search(r'PHASE1_RC=(\d+)',stdout)[1]),'stopped':bool(re.search(r'^systemctl stop ',log,re.M)),'refusals':[x for x in (p1+'\n'+stdout).splitlines() if 'ERROR:' in x or 'FAIL:' in x], 'phase1_runtime':[x for x in p1.splitlines() if 'runtime-id-dir:' in x]}
  for tag in ['DECISION','INSTALL']:
   x=re.search(tag+r'_RC=(\d+)',stdout)
   if x:rec[tag.lower()+'_rc']=int(x[1])
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  if unit.exists():
   u=unit.read_text(encoding='utf8');rec['unit_fields']=[x for x in u.splitlines() if x.startswith(('ExecStart=','ReadWritePaths=','User=','WorkingDirectory='))]
   rec['old_active_captured']='oldregistry:exists:enabled:active' in (h.probe/'phase456.log').read_text(encoding='utf8')
   dest=h.probe/'opt-ak/data';rec['copied_key']=(dest/'registry.key').read_text(encoding='utf8') if (dest/'registry.key').exists() else None
   rec['copied_db']=(dest/'registry.db').read_text(encoding='utf8') if (dest/'registry.db').exists() else None
  target=OUT/label;shutil.copytree(h.probe,target,dirs_exist_ok=True)
  (target/'result.json').write_text(json.dumps(rec,indent=2),encoding='utf8')
  print(json.dumps(rec),flush=True);results.append(rec)
 finally:h.doCleanups()
(OUT/'results.json').write_text(json.dumps(results,indent=2),encoding='utf8')
for r in results:
 assert r['phase1_rc']==r['decision_rc']==r['install_rc']==0 and r['stopped'] and r['old_active_captured'],r
 assert r['copied_key']=='runtime-real-key' and r['copied_db']=='runtime-real-db',r
 assert r['phase1_runtime']==['  runtime-id-dir: /home/otheruser/.ak-sending'],r
 ex=next(x for x in r['unit_fields'] if x.startswith('ExecStart='))
 toks=shlex.split(ex[len('ExecStart='):])
 has_id=any(t=='--id-dir' or t.startswith('--id-dir=') for t in toks)
 assert has_id==(r['case']=='id-ordinary-db-control'),r
print('ALL_EXPECTATIONS_MATCHED',flush=True)
```

### 6.2 从生成 argv 验证真实 CLI 的身份读取对象

```powershell
python <TEMP>/review_cli.py <TEMP>/compose-evidence <TEMP>/cli-evidence
```

```python
import json, os, shlex, subprocess, sys
from pathlib import Path

root = Path.cwd()
evidence = Path(sys.argv[1])
out = Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)
cli = root / 'target/debug' / ('ak-cli.exe' if os.name == 'nt' else 'ak-cli')
version = subprocess.check_output([str(cli), '--version'], text=True).strip()
assert 'git a369e1c' in version, version
guards = out / 'guards'
new_home = guards / 'new-service-home'
old_home = guards / 'old-runtime-home'
for path, marker in [(new_home, b'new-service-invalid-marker'), (old_home, b'old-runtime-invalid')]:
    (path / '.ak-sending').mkdir(parents=True, exist_ok=True)
    (path / '.ak-sending/registry.key').write_bytes(marker)

records = []
def probe(name, args, model_home, expected_key):
    assert expected_key.is_file() and expected_key.stat().st_size != 32
    child_env = dict(os.environ, HOME=str(model_home), USERPROFILE=str(model_home))
    result = subprocess.run([str(cli)] + args, cwd=out, env=child_env,
                            capture_output=True, text=True, timeout=10)
    assert result.returncode == 1, (name, result.returncode, result.stdout, result.stderr)
    assert str(expected_key) in result.stderr, (name, expected_key, result.stderr)
    assert 'seed file must hold exactly 32 bytes' in result.stderr, result.stderr
    assert not result.stdout, result.stdout
    records.append({'case': name, 'args': args, 'rc': result.returncode,
                    'stderr': result.stderr, 'expected_key': str(expected_key)})

for row in json.loads((evidence / 'results.json').read_text(encoding='utf-8')):
    case = evidence / row['case']
    command = next(s[len('ExecStart='):] for s in row['unit_fields'] if s.startswith('ExecStart='))
    args = shlex.split(command)[1:]
    for i, token in enumerate(args):
        if token.startswith('/home/'):
            args[i] = str(case / token.lstrip('/'))
        elif token.startswith('--db-path=/home/'):
            args[i] = '--db-path=' + str(case / token[len('--db-path=/'):])
        elif '/opt-ak/data' in token:
            args[i] = str(case / 'opt-ak/data')
    expected = case / 'opt-ak/data/registry.key' if row['case'] == 'id-ordinary-db-control' else new_home / '.ak-sending/registry.key'
    probe(row['case'], args, new_home, expected)

original = ['registry-server', '--port', '17200', '--db-path', str(evidence / 'id-substring-db-space-tip/home/otheruser/--id-dir.sqlite')]
probe('original-old-environment', original, old_home, old_home / '.ak-sending/registry.key')
report = {'version': version, 'probes': records}
(out / 'results.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
print(json.dumps({'result': 'ALL_EXPECTATIONS_MATCHED', 'version': version, 'cases': len(records)}))
```

### 6.3 真实 GNU ls 的 LF 候选矩阵

```powershell
python <TEMP>/review_home_lf.py <TEMP>/lf-evidence
```

```python
import importlib.util,json,os,re,shutil,subprocess,sys,time
from pathlib import Path
ROOT=Path.cwd(); OUT=Path(sys.argv[1]) if len(sys.argv)>1 else Path(__file__).parent/'home-enumeration';OUT.mkdir(parents=True,exist_ok=True)
spec=importlib.util.spec_from_file_location('m',ROOT/'scripts/tests/test_deploy_linux_install.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
results=[];start=time.monotonic()
CASES=[
 ('lf-paired-tip','a369e1c','other\nuser','candidate'),
 ('lf-paired-previous','714099a','other\nuser','candidate'),
 ('lf-paired-baseline','110bb34','other\nuser','candidate'),
 ('lf-only-tip','a369e1c','other\nuser','none'),
 ('tab-paired-tip','a369e1c','other\tuser','candidate'),
 ('two-ordinary-tip','a369e1c','otherone','candidate'),
 ('ls-failed-tip','a369e1c','otheruser','none'),
]
for label,rev,home,extra in CASES:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  # Real MSYS mkdir, GNU ls, and reads all use the POSIX name. On Windows,
  # MSYS maps LF/tab to private Unicode code points in the NTFS backing name.
  # Native Python uses only that backing name to preserve the resulting evidence.
  posix_env=dict(os.environ,MODEL_ROOT=str(h.probe),MODEL_HOME=home)
  setup=r"""
set -eu
if command -v cygpath >/dev/null; then p=$(cygpath -u "$MODEL_ROOT"); else p=$MODEL_ROOT; fi
mkdir -p "$p/home/$MODEL_HOME/.ak-sending"
printf '%s' 'synthetic-existing-key' > "$p/home/$MODEL_HOME/.ak-sending/registry.key"
printf '%s' 'synthetic-existing-db' > "$p/home/$MODEL_HOME/.ak-sending/registry.db"
"""
  p=subprocess.run([m.BASH,'--noprofile','--norc','-c',setup],env=posix_env,capture_output=True,timeout=30)
  assert p.returncode==0,(label,p.stdout,p.stderr)
  native_home=home.translate({10:0xf00a,9:0xf009}) if os.name=='nt' else home
  live=h.probe/'home'/native_home/'.ak-sending'
  assert (live/'registry.key').read_text()=='synthetic-existing-key'

  physical_candidates=['/home/'+home+'/.ak-sending']
  if extra!='none':
   second=h.probe/('home/other' if extra=='prefix' else 'home/otheruser/.ak-sending');second.mkdir(parents=True)
   marker='synthetic-split-decoy' if extra=='prefix' else 'synthetic-ordinary'
   (second/'registry.key').write_text(marker+'-key',encoding='utf8');(second/'registry.db').write_text(marker+'-db',encoding='utf8')
   if extra=='candidate':physical_candidates.append('/home/otheruser/.ak-sending')
  # Preserve every model behavior except enumerate actual probe/home with actual GNU ls,
  # since the existing LS_HOME_ENTRIES stub independently word-splits whitespace names.
  ls=h.probe/'bin/ls';text=ls.read_text(encoding='utf8')
  real=m._posix_tool("ls")
  text=text.replace('for e in ${LS_HOME_ENTRIES:-otheruser}; do printf \'%s\\n\' "$e"; done', '"'+real+'" -1 "$PROBE_DIR/home"')
  assert '"'+real+'" -1 "$PROBE_DIR/home"' in text
  ls.write_text(text,encoding='utf8',newline='\n')
  if label=='ls-failed-tip':h.driver_env['LS_FAIL_MATCH']='/home'
  raw_list_script=r"""
if command -v cygpath >/dev/null; then p=$(cygpath -u "$MODEL_ROOT"); else p=$MODEL_ROOT; fi
/usr/bin/ls -1 "$p/home"
"""
  raw_listing=subprocess.check_output([m.BASH,'--noprofile','--norc','-c',raw_list_script],env=posix_env)
  (h.probe/'real-ls.bin').write_bytes(raw_listing)
  assert home.encode('utf8')+b'\n' in raw_listing,(home,raw_listing)
  h.driver_env['SS_EMPTY']='1' 
  body=r"""
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
"""+h.decision_block(h.text,rev)+h.decisions_block(h.text,rev)+r"""
printf 'DECISION_COMPLETED=1\n'
}
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
set +e
bash "$PROBE_DIR/phase456.sh" > "$PROBE_DIR/phase456.log" 2>&1
printf 'INSTALL_RC=%s\n' "$?"
"""
  rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='driver.sh',timeout=180)
  (h.probe/'driver.out').write_text(stdout,encoding='utf8')
  p1=(h.probe/'phase1.log').read_text(encoding='utf8')
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  rec={'case':label,'revision':rev,'home':home,'driver_rc':rc,'phase1_rc':int(re.search(r'PHASE1_RC=(\d+)',stdout)[1]),'data_reports':[x for x in p1.splitlines() if x.startswith('registry-data:')],'chosen':[x for x in stdout.splitlines() if x.startswith('CHOSEN')],'refusals':[x for x in stdout.splitlines() if 'FAIL:' in x]}
  for tag in ['DECISION','INSTALL']:
   mt=re.search(tag+r'_RC=(\d+)',stdout)
   if mt:rec[tag.lower()+'_rc']=int(mt[1])
  rec['physical_candidates']=physical_candidates;rec['real_ls_hex']=raw_listing.hex();rec['native_home']=native_home
  log=h.stubs_log();rec['stopped']=bool(re.search(r'^systemctl stop ',log,re.M));rec['enabled_new_units']='systemctl enable --now ak-registry-server.service ak-stun-server.service' in log
  rec['source_key_unchanged']=(live/'registry.key').read_text()=='synthetic-existing-key'
  dest=h.probe/'opt-ak/data/registry.key';rec['destination_key']=dest.read_text() if dest.exists() else None
  destdb=h.probe/'opt-ak/data/registry.db';rec['destination_db']=destdb.read_text() if destdb.exists() else None
  if unit.exists():rec['unit_fields']=[x for x in unit.read_text().splitlines() if x.startswith(('ExecStart=','ReadWritePaths='))]
  shutil.copytree(h.probe,OUT/label,dirs_exist_ok=True)
  (OUT/label/'result.json').write_text(json.dumps(rec,indent=2),encoding='utf8')
  results.append(rec);print(json.dumps(rec),flush=True)
 finally:h.doCleanups()
(OUT/'results.json').write_text(json.dumps(results,indent=2),encoding='utf8')
for r in results:
 assert not r['stopped'] and r['source_key_unchanged'],r
 if r['case']=='ls-failed-tip':
  assert r['phase1_rc']==3 and not r['enabled_new_units'] and 'install_rc' not in r,r
 elif r['case'] in ('two-ordinary-tip','tab-paired-tip'):
  assert r['phase1_rc']==0 and r['driver_rc']==1 and len(r['data_reports'])==2 and not r['enabled_new_units'] and 'install_rc' not in r,r
  assert 'multiple identity directories' in '\n'.join(r['refusals']),r
  assert r['destination_key'] is None and r['destination_db'] is None,r
 elif r['case']=='lf-only-tip':
  assert r['phase1_rc']==r['driver_rc']==r['decision_rc']==r['install_rc']==0,r
  assert not r['data_reports'] and r['enabled_new_units'] and 'CHOSEN_DATA_SOURCE=' in r['chosen'],r
  assert r['destination_key'] is None and r['destination_db'] is None,r
 else:
  assert r['phase1_rc']==r['driver_rc']==r['decision_rc']==r['install_rc']==0 and r['enabled_new_units'],r
  assert len(r['physical_candidates'])==2 and r['data_reports']==['registry-data: /home/otheruser/.ak-sending (key:yes db:yes)'],r
  assert r['destination_key']=='synthetic-ordinary-key' and r['destination_db']=='synthetic-ordinary-db',r
print('ALL_EXPECTATIONS_MATCHED; elapsed='+str(time.monotonic()-start),flush=True)
```

### 6.4 权限诊断的当前完整链

```powershell
python <TEMP>/review_home_diag.py <TEMP>/diag-evidence
```

```python
import importlib.util,json,re,shutil,subprocess,sys,time
from pathlib import Path
ROOT=Path.cwd(); OUT=Path(sys.argv[1]) if len(sys.argv)>1 else Path(__file__).parent/'home-enumeration';OUT.mkdir(parents=True,exist_ok=True)
spec=importlib.util.spec_from_file_location('m',ROOT/'scripts/tests/test_deploy_linux_install.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
results=[];start=time.monotonic()
CASES=[
 ('error-text-name-denied-tip','a369e1c','No such file or directory','candidate'),
 ('ordinary-name-denied-tip','a369e1c','otherone','candidate'),
 ('error-text-name-readable-tip','a369e1c','No such file or directory','candidate'),
]
for label,rev,home,extra in CASES:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  live=h.probe/'home'/home/'.ak-sending';live.mkdir(parents=True)
  (live/'registry.key').write_text('synthetic-existing-key',encoding='utf8');(live/'registry.db').write_text('synthetic-existing-db',encoding='utf8')
  physical_candidates=['/home/'+home+'/.ak-sending']
  if extra!='none':
   second=h.probe/('home/other' if extra=='prefix' else 'home/otheruser/.ak-sending');second.mkdir(parents=True)
   marker='synthetic-split-decoy' if extra=='prefix' else 'synthetic-ordinary'
   (second/'registry.key').write_text(marker+'-key',encoding='utf8');(second/'registry.db').write_text(marker+'-db',encoding='utf8')
   if extra=='candidate':physical_candidates.append('/home/otheruser/.ak-sending')
  # Preserve every model behavior except enumerate actual probe/home with actual GNU ls,
  # since the existing LS_HOME_ENTRIES stub independently word-splits whitespace names.
  ls=h.probe/'bin/ls';text=ls.read_text(encoding='utf8')
  real=m._posix_tool("ls")
  text=text.replace('for e in ${LS_HOME_ENTRIES:-otheruser}; do printf \'%s\\n\' "$e"; done', '"'+real+'" -1 "$PROBE_DIR/home"')
  assert '"'+real+'" -1 "$PROBE_DIR/home"' in text
  ls.write_text(text,encoding='utf8',newline='\n')
  h.driver_env['SS_EMPTY']='1'
  if '-denied-' in label:h.driver_env['LS_PERMDENY_PATH']='/home/'+home+'/.ak-sending'
  body=r"""
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
"""+h.decision_block(h.text,rev)+h.decisions_block(h.text,rev)+r"""
printf 'DECISION_COMPLETED=1\n'
}
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
set +e
bash "$PROBE_DIR/phase456.sh" > "$PROBE_DIR/phase456.log" 2>&1
printf 'INSTALL_RC=%s\n' "$?"
"""
  rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='driver.sh',timeout=180)
  (h.probe/'driver.out').write_text(stdout,encoding='utf8')
  p1=(h.probe/'phase1.log').read_text(encoding='utf8')
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  rec={'case':label,'revision':rev,'home':home,'driver_rc':rc,'phase1_rc':int(re.search(r'PHASE1_RC=(\d+)',stdout)[1]),'data_reports':[x for x in p1.splitlines() if x.startswith('registry-data:')],'chosen':[x for x in stdout.splitlines() if x.startswith('CHOSEN')],'refusals':[x for x in stdout.splitlines() if 'FAIL:' in x]}
  for tag in ['DECISION','INSTALL']:
   mt=re.search(tag+r'_RC=(\d+)',stdout)
   if mt:rec[tag.lower()+'_rc']=int(mt[1])
  rec['physical_candidates']=physical_candidates;rec['phase_errors']=[s for s in p1.splitlines() if 'ERROR:' in s or 'Permission denied' in s]
  log=h.stubs_log();rec['stopped']=bool(re.search(r'^systemctl stop ',log,re.M));rec['enabled_new_units']='systemctl enable --now ak-registry-server.service ak-stun-server.service' in log
  rec['source_key_unchanged']=(live/'registry.key').read_text()=='synthetic-existing-key'
  dest=h.probe/'opt-ak/data/registry.key';rec['destination_key']=dest.read_text() if dest.exists() else None
  destdb=h.probe/'opt-ak/data/registry.db';rec['destination_db']=destdb.read_text() if destdb.exists() else None
  if unit.exists():rec['unit_fields']=[x for x in unit.read_text().splitlines() if x.startswith(('ExecStart=','ReadWritePaths='))]
  shutil.copytree(h.probe,OUT/label,dirs_exist_ok=True)
  (OUT/label/'result.json').write_text(json.dumps(rec,indent=2),encoding='utf8')
  results.append(rec);print(json.dumps(rec),flush=True)
 finally:h.doCleanups()
(OUT/'results.json').write_text(json.dumps(results,indent=2),encoding='utf8')
for r in results:
 assert not r['stopped'] and r['source_key_unchanged'],r
 if r['case']=='ordinary-name-denied-tip':
  assert r['phase1_rc']==3 and not r['enabled_new_units'] and 'install_rc' not in r,r
 elif r['case']=='error-text-name-readable-tip':
  assert r['phase1_rc']==0 and r['driver_rc']==1 and len(r['data_reports'])==2 and not r['enabled_new_units'] and 'install_rc' not in r,r
  assert 'multiple identity directories' in '\n'.join(r['refusals']),r
 else:
  assert r['phase1_rc']==r['driver_rc']==r['decision_rc']==r['install_rc']==0 and r['enabled_new_units'],r
  assert len(r['physical_candidates'])==2 and r['data_reports']==['registry-data: /home/otheruser/.ak-sending (key:yes db:yes)'],r
  assert r['destination_key']=='synthetic-ordinary-key' and r['destination_db']=='synthetic-ordinary-db',r
print('ALL_EXPECTATIONS_MATCHED; elapsed='+str(time.monotonic()-start),flush=True)
```

### 6.5 权限诊断的固定版本 helper 对照

```powershell
python <TEMP>/review_home_diag_helper.py <TEMP>/diag-helper-evidence
```

```python
import importlib.util,json,shutil,subprocess,sys,time
from pathlib import Path
out=Path(sys.argv[1]);root=Path.cwd()
spec=importlib.util.spec_from_file_location('m',root/'scripts/tests/test_deploy_linux_install.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
results=[]
for rev in ['a369e1c','714099a','110bb34']:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode()
  body=r'''
write_phase1 "$PROBE_DIR/phase1.sh"
sed -n '/^priv_exists() {/,/^}/p' "$PROBE_DIR/phase1.sh" > "$PROBE_DIR/priv-exists.sh"
. "$PROBE_DIR/priv-exists.sh"
for path in '/home/No such file or directory/.ak-sending' '/home/ordinary/.ak-sending'; do
 export LS_PERMDENY_PATH="$path"
 set +e
 raw=$(sudo -n env LC_ALL=C ls -d "$path" 2>&1)
 rc=$?
 printf 'RAW_RC=%s RAW=%s\n' "$rc" "$raw"
 val=$(priv_exists "$path" 2> "$PROBE_DIR/helper-err")
 rc=$?
 printf 'PATH=%s VALUE=%s RC=%s\n' "$path" "$val" "$rc"
done
'''
  rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='helper.sh')
  rec={'revision':rev,'rc':rc,'stdout':stdout}
  assert rc==0 and 'PATH=/home/No such file or directory/.ak-sending VALUE=no RC=0' in stdout,rec
  assert 'PATH=/home/ordinary/.ak-sending VALUE= RC=2' in stdout,rec
  assert stdout.count('RAW_RC=2')==2,rec
  shutil.copytree(h.probe,out/('helper-'+rev),dirs_exist_ok=True)
  results.append(rec)
 finally:h.doCleanups()
(out/'helper-results.json').write_text(json.dumps(results,indent=2),encoding='utf8')
print(json.dumps(results,indent=2));print('ALL_EXPECTATIONS_MATCHED')
```
