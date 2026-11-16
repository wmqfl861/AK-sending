# 批次 K 修复轮二十二独立复核（第 23 轮，2026-11-16）

复核范围：`dev/batch-k-20261114` 的 `a369e1c..f578f39`；实现与文档提交 `8419310`，`f578f39` 只回填交接的一行哈希。固定 tip 为 `f578f39f4fa90b8feace73b7648301611156ef2c`。结论为 **REJECTED**：K22 原反例已得到针对性修复，但真实 find 与测试模型的记录协议不一致，仍会漏掉普通来源或迁移错误身份；起点符号链接、诊断路径匹配及监听参数比较也有下述缺口。固定 tip CI 已通过 GitHub API 等到 completed/failure。

逐项裁决只见 [审核状态表 §7](REVIEW_2026-11-11.zh-CN.md#7-状态表执行-agent-每完成一项就更新)。实际命令、运行编号、计数、仓外材料与未执行范围统一记在 [K 交接 §51](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#51-修复轮二十二独立复核证据2026-11-16)。下文说明机制和可重放输入；所有源码行号指固定 tip。依据 REJECTED，仅同步本地审核文档，保留 main 与开发分支，不创建提交、合并、推送或删除分支。

## 1. K22 修复的机械核对

`has_opt_token()` 以 noglob token 遍历判断独立选项或等号前缀，替换 compose 与 ReadWritePaths 守卫中的子串检测；末次核验要求组合命令确实绑定身份。原 DB 名含 `--id-dir.sqlite` 而身份选项缺省的空格/等号夹具，当前完整 phase1→decision→install 链补出 `--id-dir <REMOTE_DATA>`，DB 参数保真，迁移字节与最终身份值一致。普通 DB 与真存在身份选项的控制保持，后者没有重复绑定。`a369e1c`、`110bb34` 自身完整链仍生成缺身份绑定的 unit。

真实 CLI 安全探针确认当前生成 argv 读取迁移目录或显式身份目录，基线缺绑定 argv 读取新 HOME 下的位置。所有位置预放非合法长度的合成 seed，在身份加载处失败，未 bind/spawn。专项使用已有 git a369e1c Windows debug binary，且机械确认 CLI 源码到本轮 tip 无变化；不称该专项是 tip 新构建或原生 systemd 验证。

真实 GNU find 的 LF 单来源、LF 配对及 CR 名记录均作为完整名称进入拒绝分支；原始日志字节保留完整名称，来源不动。producer 失败也拒绝。但 ordinary/spaced 控制在真实 find 下退化，不能由仓库 basename stub 的绿灯推定 producer 保真，见 K23-N1/N2。

原 `No such file or directory` 目录名加 Permission denied 的污染反例，当前 helper 返回不可判定，phase1 拒绝；普通名 Permission denied 也拒绝，普通真 ENOENT 继续。可读特殊名与普通来源会触发多候选拒绝；旧版本 helper 仍把原污染当成不存在。该专项的 phase1 控制使用仓库 basename 枚举模型以隔离诊断分类，真实 find 整链另列。路径逐字匹配的声明仍不成立，见 K23-N3。

文档差异已核对：`8419310` 纳入 K22 报告、交接 §49、STATUS、审核状态表、K21 归属补注及执行方 §50；`f578f39` 仅回填 §50 的实现提交号。历史运行声明与本轮实际运行分别记录。

## 2. K23-N1 / P1：find 的完整路径被再次添加 /home，漏来源并可实际复制嵌套诱饵

`scripts/deploy_linux.sh:1997` 使用：

```bash
find /home -mindepth 1 -maxdepth 1 -print0
```

GNU find 返回 `/home/otheruser\0`，不是 `otheruser\0`。`:2041` 却仍调用 `registry_data_probe "/home/$home_dir/.ak-sending"`，探测对象变为 `/home//home/otheruser/.ak-sending`。NUL 边界解决了名称分隔，却没有修正 producer 与 consumer 对记录内容的不同约定。

在无 live occupant 的 TEMP 模型中，普通来源 `/home/otheruser/.ak-sending` 被完全漏掉；两个普通来源和空格来源也被当作 none-found，继续安装并 enable。模型没有真实创建新 key，不把继续安装称为已经生成新身份。

更直接的夹具同时创建：

```text
/home/otheruser/.ak-sending          synthetic-ordinary-key/db
/home/home/otheruser/.ak-sending     synthetic-nested-decoy-key/db
```

当前自身完整链把嵌套路径作为唯一候选，目标实际 key/db 字节来自诱饵，最终 unit 绑定目标目录；`a369e1c` 同夹具仍复制真正普通来源。基线两个普通来源触发多候选拒绝，空格来源完整进入白名单并拒绝。各来源保持原样，无 live occupant/stop；这是错误来源迁移的真实文件操作加离线服务模型证据。

测试掩盖机制位于 `scripts/tests/test_deploy_linux_install.py:789` 起的 FIND_STUB：匹配 `/home` 后直接输出人为文件或 basename 并退出，根本到不了后面的 `exec -a find`。`_run_p1_homes()`（`:1651`）同样写 basename NUL 记录。因而“真实 find stub，普通/空格控制保持”的声明不成立。新 producer 来自 `8419310`，与原 consumer 的组合构成本轮新增回归。

修复要求：冻结并忠实实现完整路径的记录协议；stub 对实际使用的 `/home` 分支必须执行真实 find，或与真实工具输出作独立一致性检查。回归必须检查普通来源、多个来源、空格来源及嵌套诱饵的实际被探测路径和迁移字节，不能只断言 NUL 存在。

## 3. K23-N2 / P2：/home 起点为符号链接时，find 成功但无记录，旧来源被当作不存在

同一 `:1997` 没有说明起点符号链接语义。GNU find 默认 `-P`，命令行起点为目录符号链接时不下降；结合 `-mindepth 1` 会成功退出且零条输出。`ls -1 /home` 则会列出目录链接目标的内容。这不是命令失败，现有 producer rc 守卫不能发现。

TEMP 中实际创建 `home -> actual-homes` 目录符号链接，目标含普通身份来源。当前完整链将其视为 none-found 后继续 enable，未复制旧 key/db；`a369e1c` 的真实 ls 生产者正确列出并迁移旧来源。原 key/db 不变，没有 live occupant/stop，也未真实生成新身份。链接、GNU 工具和文件操作真实运行，systemctl/proc/权限仍是本机合成模型。

这项与重复前缀独立：producer 已经没有记录，单改 consumer 无效。它也是 `8419310` 切换生产者引入的回归。修复要求：明确处理或拒绝命令行起点符号链接；如果支持，保持受控的起点跟随语义，并保留 NUL 与失败处理；如果不支持，应显式拒绝，不能把可访问的旧来源静默当空主机。

## 4. K23-N3 / P2：诊断路径仍是 case 模式，合法空目录触发新增拒绝

`scripts/deploy_linux.sh:1187` 在生成 helper 后为：

```bash
case "$pe_out" in
  "ls: cannot access '"$1"': No such file or directory") echo no; return 0 ;;
  *) ...; return 2 ;;
esac
```

`$1` 位于两段引号之间，未被引用，仍参与 Bash case 模式匹配；注释所称 LITERALLY/no globbing 与执行语义不符。

真实 GNU ls 对不存在的 `/home/spare[1]/.ak-sending` 返回准确同路径 ENOENT；当前 helper 却不匹配字面方括号，返回不可判定。创建只有空 home `spare[1]`、没有任何身份/DB 的夹具，在仓库声明的 basename 枚举模型下当前 phase1 拒绝；普通 `spare1` 控制继续，`a369e1c` 和 `110bb34` 同空 bracket home 也继续。此处没有身份候选可进入后续路径白名单，不能用“身份路径本来就会拒绝”解释退化。真实 find 还叠有 K23-N1；这里明确隔离诊断分类，不把它称为修好枚举后的完整部署验证。

另一个仅属 helper 合同的探针：请求 `/home/missing[1]/.ak-sending`，显式注入另一条 `/home/missing1/.ak-sending` 的 ENOENT，被误接受为 no；带星号请求同样能匹配不同路径，普通不同路径控制则拒绝。这反证“路径不同一律不可判定”，但不声称真实 GNU ls 会为别的路径报错，也不声称真实 Permission denied 已被绕过。

归属为 `8419310`。修复要求：将完整期望诊断作为字面字符串比较，保留原权限污染拒绝、普通真 ENOENT、方括号同路径 ENOENT 和不同路径拒绝控制。该发现的实际完整 phase1 影响是保守拒绝与兼容性回归，没有证明错误迁移。

## 5. K23-N4 / P2：监听参数仍按子串提取，漏掉 configured/running 的实质差异

`scripts/deploy_linux.sh:1438`、`:1439` 对 port、`:1443`、`:1444` 对 bind 仍使用贪婪 sed 子串解析。例如：

```text
configured:
registry-server --port 18200 --id-dir /home/otheruser/--port --db-path /home/otheruser/--id-dir.sqlite
running:
registry-server --port 17200 --id-dir /home/otheruser/--port --db-path /home/otheruser/--id-dir.sqlite
```

两侧都从身份路径尾部的 `--port` 匹配，提取出后继 `--db-path`，于是把真实不同的监听端口判为一致。当前完整链接受、stop 已捕获为 enabled/active 的旧服务模型，生成带 configured 端口的 unit。bind 变体把身份尾段改为 `--bind`，configured 为 `127.0.0.1`、running 为 `0.0.0.0`，也被相同方式遮蔽，最终 unit 带 loopback 绑定。

只把身份路径改为普通 `/home/otheruser/identity` 的控制均在 phase1 以 materially disagrees 拒绝，零 stop。当前、`a369e1c`、`110bb34` 各自完整链均复现；`git blame` 归属 `7e8bfb2`，是本轮新发现的既有缺口，不归因于 `8419310`。生成 argv 经真实 CLI 接受后在合成 seed 处预设失败，无真实监听；不得把 unit 参数变化扩大为已在原生服务器造成断连。

修复要求：对监听参数也采用完整 token 边界和有效值校验，比较真正的 port/bind；不能让其他参数值中的字样遮蔽差异。保留两个合法身份路径夹具、普通路径拒绝控制与旧版本自身完整链对照。

## 6. K23-N5 / P2：新 LF 回归测试无条件做 NTFS 名称转换，Linux CI 实际失败

`scripts/tests/test_deploy_linux_install.py:1605` 的 `_ntfs_backing()` 注释声称非 Windows 保持原名，但 `:1609` 无条件把 LF/tab 转成 U+F00A/U+F009。`:1620` 起通过 Bash 创建的 Linux 文件名含真实 LF；`:4821` 起却按 U+F00A 名回读，造成 FileNotFoundError。`:6502` 的旧版对照也复用该 helper，浅历史跳过不能证明它可移植。

固定 tip 的 GitHub Linux repository job 日志明确在 `:4823` 报错，找不到 `home/other\uf00auser/.ak-sending/registry.key`。这是实际 Linux CI 证据，不是本机推测。目标测试的 Linux 重放命令为：

```sh
python -m unittest scripts.tests.test_deploy_linux_install.DeployInstallTests.test_k22n2_lf_home_candidate_refuses_whole_never_dropped -v
```

该回归来自 `8419310`。修复要求：只在实际需要 NTFS/MSYS backing 映射的平台转换名称，其他平台按原字节读取；在 Linux 与 Windows 分别运行 LF 控制及可获得基线的对照。不能以本机 Windows 测试通过代替固定提交的 Linux 门槛。

## 7. CI、范围与收尾

通过当前 Git 凭据只读查询固定 SHA 的 runs、run/jobs 及失败 job 日志，已等到全部 job 终态。Repository checks 失败；aarch64-apple-darwin portability 被取消，不能计通过；manual release-deep 跳过。其他 job 的实际结论和本机门槛只见交接证据表。没有重试或重跑 CI 来替换失败证据。

Rust、依赖、workflow、Dart/Flutter 源码在本批范围内未变。全部防御夹具位于 TEMP，文件名和 key/db 内容为合成标记；真实 Bash/GNU 工具和文件操作与 systemctl、proc、权限模型分别标记。本轮没有生产访问、SSH、真实密钥、物理双机或原生 Linux 部署。历史 release 深测超时继续保留，不称已解决。

## 8. 逐字复现代码

在固定 `f578f39` 工作树的仓库根目录执行。代码从固定提交读取各版本部署脚本，在测试隔离目录内运行真实生成器。脚本输出 ALL_EXPECTATIONS_MATCHED 表示缺陷与控制均符合上述断言，不表示被审候选通过。证据目录须使用新的 TEMP 子目录；实际执行计数与仓外材料索引只见交接。

### 8.1 真实 find/ls：普通、诱饵、LF/CR、失败与起点符号链接

保存为 TEMP 下的 `review_enumeration_combined.py`，执行：

```powershell
python <TEMP>/review_enumeration_combined.py <TEMP>/k23-enumeration-evidence
```

```python
import importlib.util,json,os,re,shutil,subprocess,sys,time
from pathlib import Path
ROOT=Path.cwd(); OUT=Path(sys.argv[1]);OUT.mkdir(parents=True,exist_ok=True)
spec=importlib.util.spec_from_file_location('m',ROOT/'scripts/tests/test_deploy_linux_install.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
results=[];start=time.monotonic()
CASES=[
 ('symlink-tip','f578f39',['otheruser'],False),
 ('symlink-baseline','a369e1c',['otheruser'],False),
 ('ordinary-tip','f578f39',['otheruser'],False),
 ('ordinary-baseline','a369e1c',['otheruser'],False),
 ('two-ordinary-tip','f578f39',['otherone','otheruser'],False),
 ('two-ordinary-baseline','a369e1c',['otherone','otheruser'],False),
 ('nested-decoy-tip','f578f39',['otheruser'],True),
 ('nested-decoy-baseline','a369e1c',['otheruser'],True),
 ('lf-only-tip','f578f39',['other\nuser'],False),
 ('lf-only-baseline','a369e1c',['other\nuser'],False),
 ('lf-paired-tip','f578f39',['other\nuser','otheruser'],False),
 ('lf-paired-baseline','a369e1c',['other\nuser','otheruser'],False),
 ('cr-only-tip','f578f39',['other\ruser'],False),
 ('spaced-tip','f578f39',['other user'],False),
 ('spaced-baseline','a369e1c',['other user'],False),
 ('find-failed-tip','f578f39',['otheruser'],False),
]
for label,rev,homes,decoy in CASES:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  sources=[]
  for index,home in enumerate(homes):
   marker='synthetic-ordinary' if home=='otheruser' else 'synthetic-existing'
   setup=r'''
set -eu
if command -v cygpath >/dev/null; then p=$(cygpath -u "$MODEL_ROOT"); else p=$MODEL_ROOT; fi
mkdir -p "$p/home/$MODEL_HOME/.ak-sending"
printf '%s' "$MODEL_MARKER-key" > "$p/home/$MODEL_HOME/.ak-sending/registry.key"
printf '%s' "$MODEL_MARKER-db" > "$p/home/$MODEL_HOME/.ak-sending/registry.db"
'''
   env=dict(os.environ,MODEL_ROOT=str(h.probe),MODEL_HOME=home,MODEL_MARKER=marker)
   p=subprocess.run([m.BASH,'--noprofile','--norc','-c',setup],env=env,capture_output=True,timeout=30)
   assert p.returncode==0,(label,p.stdout,p.stderr)
   native=home.translate({i:0xf000+i for i in range(1,32)}) if os.name=='nt' else home
   live=h.probe/'home'/native/'.ak-sending'
   assert (live/'registry.key').read_text()==marker+'-key',(label,list((h.probe/'home').iterdir()),native)
   sources.append((live,marker))
  if label.startswith('symlink-'):
   original=h.probe/'home'; linked=h.probe/'actual-homes'
   assert original.parent==h.probe and linked.parent==h.probe
   original.rename(linked)
   original.symlink_to(linked,target_is_directory=True)
   assert original.is_symlink()
  if decoy:
   live=h.probe/'home/home/otheruser/.ak-sending';live.mkdir(parents=True)
   (live/'registry.key').write_text('synthetic-nested-decoy-key');(live/'registry.db').write_text('synthetic-nested-decoy-db')
   sources.append((live,'synthetic-nested-decoy'))
  # Execute genuine GNU producers, remapping only the sandbox root. Real find
  # emits full paths; remove PROBE_DIR only (never /home or the entry prefix).
  find=h.probe/'bin/find';real_find=m._posix_tool('find')
  find.write_text(r'''#!/usr/bin/env bash
set -u
printf 'find %s\n' "$*" >> "$PROBE_DIR/stubs.log"
if [ -n "${LS_FAIL_MATCH:-}" ]; then echo 'find: forced offline failure' >&2; exit 1; fi
args=("$@")
[ "${args[0]}" = /home ] || exit 89
args[0]="$PROBE_DIR/home"
(exec -a find "REAL_FIND" "${args[@]}") > "$PROBE_DIR/real-find.bin"
rc=$?
while IFS= read -r -d '' entry; do
 printf '%s\0' "${entry#"$PROBE_DIR"}"
done < "$PROBE_DIR/real-find.bin" | /usr/bin/tee "$PROBE_DIR/logical-find.bin"
exit "$rc"
'''.replace('REAL_FIND',real_find),encoding='utf8',newline='\n')
  ls=h.probe/'bin/ls';text=ls.read_text(encoding='utf8');real_ls=m._posix_tool('ls')
  old='for e in ${LS_HOME_ENTRIES:-otheruser}; do printf \'%s\\n\' "$e"; done'
  assert old in text
  text=text.replace(old,'(exec -a ls "'+real_ls+'" -1 "$PROBE_DIR/home") | /usr/bin/tee "$PROBE_DIR/real-ls.bin"')
  ls.write_text(text,encoding='utf8',newline='\n')
  h.driver_env['SS_EMPTY']='1'
  if label=='find-failed-tip':h.driver_env['LS_FAIL_MATCH']='/home'
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
  p1=(h.probe/'phase1.log').read_text(encoding='utf8')
  rec={'case':label,'revision':rev,'homes':homes,'driver_rc':rc,'phase1_rc':int(re.search(r'PHASE1_RC=(\d+)',stdout)[1]),'data_reports':[x for x in p1.splitlines() if x.startswith('registry-data:')],'chosen':[x for x in stdout.splitlines() if x.startswith('CHOSEN')],'refusals':[x for x in stdout.splitlines() if 'FAIL:' in x],'phase1_errors':[x for x in p1.splitlines() if 'ERROR:' in x]}
  for tag in ['DECISION','INSTALL']:
   mt=re.search(tag+r'_RC=(\d+)',stdout)
   if mt:rec[tag.lower()+'_rc']=int(mt[1])
  for filename in ['real-find.bin','logical-find.bin','real-ls.bin']:
   if (h.probe/filename).exists():rec[filename+'_hex']=(h.probe/filename).read_bytes().hex()
  log=h.stubs_log();rec['stopped']=bool(re.search(r'^systemctl stop ',log,re.M));rec['enabled_new_units']='systemctl enable --now ak-registry-server.service ak-stun-server.service' in log
  rec['home_is_symlink']=(h.probe/'home').is_symlink()
  rec['all_sources_unchanged']=all((live/'registry.key').read_text()==marker+'-key' and (live/'registry.db').read_text()==marker+'-db' for live,marker in sources)
  rec['existence_probes']=[x for x in log.splitlines() if x.startswith('ls -d /home')]
  for name in ['registry.key','registry.db']:
   dst=h.probe/'opt-ak/data'/name;rec['destination_'+name]=dst.read_text() if dst.exists() else None
  unit=h.probe/'etc/systemd/system/ak-registry-server.service'
  if unit.exists():rec['unit_fields']=[x for x in unit.read_text().splitlines() if x.startswith(('ExecStart=','ReadWritePaths='))]
  shutil.copytree(h.probe,OUT/label,dirs_exist_ok=True)
  (OUT/label/'result.json').write_text(json.dumps(rec,indent=2),encoding='utf8')
  results.append(rec);print(json.dumps(rec),flush=True)
 finally:h.doCleanups()
(OUT/'results.json').write_text(json.dumps(results,indent=2),encoding='utf8')
for r in results:
 assert not r['stopped'] and r['all_sources_unchanged'],r
 case=r['case']
 if case.startswith('symlink-'):assert r['home_is_symlink'],r
 if case=='symlink-tip':assert r['real-find.bin_hex']==r['logical-find.bin_hex']=='',r
 if case in ('lf-only-tip','lf-paired-tip','cr-only-tip','find-failed-tip'):
  assert r['phase1_rc']==3 and not r['enabled_new_units'] and 'install_rc' not in r,r
 elif case in ('two-ordinary-baseline','spaced-baseline'):
  assert r['phase1_rc']==0 and r['driver_rc']==1 and not r['enabled_new_units'] and 'install_rc' not in r,r
 else:
  assert r['phase1_rc']==r['driver_rc']==r['decision_rc']==r['install_rc']==0 and r['enabled_new_units'],r
 if case in ('ordinary-tip','two-ordinary-tip','spaced-tip','lf-only-baseline','symlink-tip'):
  assert not r['data_reports'] and r['destination_registry.key'] is None and r['destination_registry.db'] is None,r
 if case=='nested-decoy-tip':
  assert r['data_reports']==['registry-data: /home//home/otheruser/.ak-sending (key:yes db:yes)'],r
  assert r['destination_registry.key']=='synthetic-nested-decoy-key' and r['destination_registry.db']=='synthetic-nested-decoy-db',r
 if case in ('ordinary-baseline','nested-decoy-baseline','lf-paired-baseline','symlink-baseline'):
  assert r['destination_registry.key']=='synthetic-ordinary-key' and r['destination_registry.db']=='synthetic-ordinary-db',r
print('ALL_EXPECTATIONS_MATCHED; elapsed='+str(time.monotonic()-start),flush=True)
```

### 8.2 监听参数差异的完整链与普通控制

保存为 `review_listen_substring.py`，执行：

```powershell
python <TEMP>/review_listen_substring.py --output <TEMP>/k23-listen-evidence
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
CASES=[]
for option in ['port','bind']:
 for rev in ['f578f39','a369e1c','110bb34']:
  idpath='/home/otheruser/--'+option
  suffix=' --id-dir '+idpath+' --db-path /home/otheruser/--id-dir.sqlite'
  configured=('--port 18200' if option=='port' else '--port 17200 --bind 127.0.0.1')+suffix
  running=('--port 17200' if option=='port' else '--port 17200 --bind 0.0.0.0')+suffix
  CASES.append((option+'-masked-'+rev,rev,running,configured,idpath))
 idpath='/home/otheruser/identity'
 suffix=' --id-dir '+idpath+' --db-path /home/otheruser/--id-dir.sqlite'
 configured=('--port 18200' if option=='port' else '--port 17200 --bind 127.0.0.1')+suffix
 running=('--port 17200' if option=='port' else '--port 17200 --bind 0.0.0.0')+suffix
 CASES.append((option+'-ordinary-control','f578f39',running,configured,idpath))
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
 if 'ordinary-control' in r['case']:
  assert r['phase1_rc']==3 and not r['stopped'] and 'install_rc' not in r,r
  assert any('materially disagrees' in line for line in r['refusals']),r
 else:
  assert r['phase1_rc']==r['decision_rc']==r['install_rc']==0 and r['stopped'] and r['old_active_captured'],r
  assert r['copied_key']=='runtime-real-key' and r['copied_db']=='runtime-real-db',r
  ex=next(x for x in r['unit_fields'] if x.startswith('ExecStart='))
  assert r['configured'] in ex,r
print('ALL_EXPECTATIONS_MATCHED',flush=True)
```

### 8.3 字面诊断的 helper 合同与空 home phase1 对照

保存为 `review_diag_literal.py`，执行：

```powershell
python <TEMP>/review_diag_literal.py <TEMP>/k23-diag-evidence
```

```python
import importlib.util,json,re,shutil,subprocess,sys,time
from pathlib import Path
ROOT=Path.cwd();OUT=Path(sys.argv[1]);OUT.mkdir(parents=True,exist_ok=True)
spec=importlib.util.spec_from_file_location('m',ROOT/'scripts/tests/test_deploy_linux_install.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
results=[];start=time.monotonic()
h=m.DeployInstallTests();h.setUp()
try:
 h.text=subprocess.check_output(['git','show','f578f39:scripts/deploy_linux.sh']).decode('utf8')
 body=r'''
write_phase1 "$PROBE_DIR/phase1.sh"
sed -n '/^priv_exists() {/,/^}/p' "$PROBE_DIR/phase1.sh" > "$PROBE_DIR/priv-exists.sh"
. "$PROBE_DIR/priv-exists.sh"
# Contract-only diagnostic mismatch model. No filesystem/permissions claim.
sudo() { printf '%s\n' "$DIAG" >&2; return 2; }
for path in '/home/missing[1]/.ak-sending' '/home/missing1/.ak-sending' '/home/missing*/.ak-sending'; do
 DIAG="ls: cannot access '/home/missing2/.ak-sending': No such file or directory"
 [ "$path" != '/home/missing[1]/.ak-sending' ] || DIAG="ls: cannot access '/home/missing1/.ak-sending': No such file or directory"
 set +e
 val=$(priv_exists "$path" 2> "$PROBE_DIR/mismatch-err")
 rc=$?
 printf 'REQUEST=%s DIAG=%s VALUE=%s RC=%s\n' "$path" "$DIAG" "$val" "$rc"
done
'''
 rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='mismatch.sh')
 rec={'kind':'diagnostic-mismatch-model','driver_rc':rc,'stdout':stdout}
 assert rc==0,rec
 assert "REQUEST=/home/missing[1]/.ak-sending DIAG=ls: cannot access '/home/missing1/.ak-sending': No such file or directory VALUE=no RC=0" in stdout,rec
 assert "REQUEST=/home/missing1/.ak-sending DIAG=ls: cannot access '/home/missing2/.ak-sending': No such file or directory VALUE= RC=2" in stdout,rec
 assert "REQUEST=/home/missing*/.ak-sending DIAG=ls: cannot access '/home/missing2/.ak-sending': No such file or directory VALUE=no RC=0" in stdout,rec
 shutil.copytree(h.probe,OUT/'mismatch-model',dirs_exist_ok=True);results.append(rec);print(json.dumps(rec),flush=True)
finally:h.doCleanups()

# Ordinary empty home versus bracket empty home, with the declared test
# producer's basename record shape, so this isolates diagnostic classification.
for rev,home in [('f578f39','spare[1]'),('f578f39','spare1'),('a369e1c','spare[1]'),('110bb34','spare[1]')]:
 h=m.DeployInstallTests();h.setUp()
 try:
  h.text=subprocess.check_output(['git','show',rev+':scripts/deploy_linux.sh']).decode('utf8')
  (h.probe/'home'/home).mkdir(parents=True)
  h.driver_env.update(SS_EMPTY='1',LS_HOME_ENTRIES=home,FIND_HOME_ENTRIES=home)
  body=r'''
write_phase1 "$PROBE_DIR/phase1.sh"
bash "$PROBE_DIR/phase1.sh" > "$PROBE_DIR/phase1.log" 2>&1
printf 'PHASE1_RC=%s\n' "$?"
'''
  rc,stdout=h.run_driver(h.definitions()+m.BOX_MODELS,body,name='chain.sh')
  p1=(h.probe/'phase1.log').read_text(encoding='utf8')
  rec={'kind':'phase1-empty-home','revision':rev,'home':home,'driver_rc':rc,'phase1_rc':int(re.search(r'PHASE1_RC=(\d+)',stdout)[1]),'phase_errors':[x for x in p1.splitlines() if 'ERROR:' in x or 'cannot determine' in x]}
  if rec['phase1_rc']==0:
   drc,dout=h.run_decision_and_config(p1,'decision.sh');rec['decision_rc']=drc;rec['decision_out']=dout
  log=h.stubs_log();rec['stopped']=bool(re.search(r'^systemctl stop ',log,re.M));rec['enabled_new_units']='systemctl enable --now' in log
  assert rc==0 and not rec['stopped'] and not rec['enabled_new_units'],rec
  if rev=='f578f39' and '[' in home:assert rec['phase1_rc']==3 and 'decision_rc' not in rec,rec
  else:assert rec['phase1_rc']==0 and rec['decision_rc']==0,rec
  shutil.copytree(h.probe,OUT/(rev+'-'+home),dirs_exist_ok=True);results.append(rec);print(json.dumps(rec),flush=True)
 finally:h.doCleanups()
(OUT/'results.json').write_text(json.dumps(results,indent=2),encoding='utf8');print('ALL_EXPECTATIONS_MATCHED elapsed='+str(time.monotonic()-start),flush=True)
```

