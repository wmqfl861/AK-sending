/// Wave-22 Dart-binding tests for the `ak_pairing_*` / `ak_sched_*`
/// families, driven entirely through the pointer-free seams
/// ([AkPairingCalls] / [AkSchedCalls]) against the WIRE SHAPES THE ENGINE
/// ACTUALLY SHIPS (crates/ak-ffi/src/pairing.rs): spec-JSON construction,
/// invite summary/event decoding, the `-3`-grows-losslessly loops of
/// op-result/event/snapshot polling, error-code surfaces, and the
/// scheduler policy validation of scheduler.md §3.
///
/// No DLL involved — the real-library round-trip is the integrator's e2e
/// step.
library;

import 'dart:convert' show jsonDecode, jsonEncode, utf8;

import 'dart:typed_data' show Uint8List;

import 'package:test/test.dart';

import 'package:ak_ffi_dart/ak_ffi_dart.dart';

void main() {
  group('邀请码格式（Crockford Base32）', () {
    test('normalizeInviteCode 接受规范分组、无分组与小写形式并输出规范分组', () {
      const canonical = 'AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ';
      expect(normalizeInviteCode(canonical), canonical);
      expect(
          normalizeInviteCode('aki10123456789abcdefghjkmnpqrstvwxyz'),
          canonical);
      expect(
          normalizeInviteCode('AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-wxyz'),
          canonical);
      expect(isValidInviteCode(canonical), isTrue);
    });

    test('normalizeInviteCode 拒绝同形字符、空白、排除字母、错误分组与长度', () {
      expect(
          () => normalizeInviteCode(
              'АKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ'),
          throwsFormatException,
          reason: 'Unicode 同形字符');
      expect(
          () => normalizeInviteCode(
              'AKI1-0123 4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ'),
          throwsFormatException,
          reason: '任意空白');
      expect(
          () => normalizeInviteCode(
              'AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ\n'),
          throwsFormatException);
      expect(
          () => normalizeInviteCode(
              'AKI1-0123-4567-89oB-CDEF-GHJK-MNPQ-RSTV-WXYZ'),
          throwsFormatException,
          reason: 'Crockford 排除 O');
      expect(
          () => normalizeInviteCode('AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYI'),
          throwsFormatException,
          reason: 'Crockford 排除 I');
      expect(
          () => normalizeInviteCode(
              'AKI1-01234-567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ'),
          throwsFormatException,
          reason: '连字符错位');
      expect(() => normalizeInviteCode('AKI1-0123'), throwsFormatException);
      expect(() => normalizeInviteCode(''), throwsFormatException);
      expect(
          () =>
              normalizeInviteCode('AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXZ'),
          throwsFormatException,
          reason: '31 个载荷字符是错误长度');
      expect(isValidInviteCode('AKI1-0123'), isFalse);
    });
  });

  group('spec JSON 构造（与引擎解析器对齐）', () {
    test('create/redeem 规格逐字段匹配引擎解析字段', () {
      const rid = '0123456789abcdef0123456789abcdef';
      // const regId = '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef';
      expect(
        AkInviteCreateSpec(requestId: rid, ttlSecs: 0).toJson(),
        {'spec_version': 1, 'request_id': rid, 'ttl_secs': 0},
      );
      expect(
        AkInviteCreateSpec(ttlSecs: 3600).toJson(),
        {'spec_version': 1, 'ttl_secs': 3600},
        reason: 'request_id 可省略（引擎侧自动生成）',
      );
      expect(
        AkInviteRedeemSpec(
                requestId: rid.toUpperCase(),
                inviteCode: 'aki10123456789abcdefghjkmnpqrstvwxyz')
            .toJson(),
        {
          'spec_version': 1,
          'request_id': rid,
          'invite_code': 'AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ',
        },
      );
      expect(
          () => AkInviteRedeemSpec(inviteCode: 'AKI1-not-a-code').toJson(),
          throwsA(isA<FormatException>()));
    });

    test('request_id / ttl_secs / code_id / limit 的镜像校验在到达 FFI 前拒绝',
        () {
      const rid = '0123456789abcdef0123456789abcdef';
      // const regId = '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef';
      expect(() => AkInviteCreateSpec(requestId: 'zz', ttlSecs: 0).toJson(),
          throwsA(isA<AkSpecException>()));
      expect(() => AkInviteCreateSpec(ttlSecs: 30).toJson(),
          throwsA(isA<AkSpecException>()),
          reason: '有限 TTL 最短 60 秒');
      expect(() => AkInviteCreateSpec(ttlSecs: 31 * 24 * 3600).toJson(),
          throwsA(isA<AkSpecException>()),
          reason: '有限 TTL 最长 30 天');
      expect(
        AkInviteListQuery(
                requestId: rid, stateFilter: InviteListStateFilter.all)
            .toJson(),
        {'spec_version': 1, 'request_id': rid, 'state_filter': 'all', 'limit': 32},
      );
      expect(
        AkInviteListQuery(
                cursor: 'AB' * 16, stateFilter: InviteListStateFilter.revoked)
            .toJson()['cursor'],
        'ab' * 16,
        reason: '分页游标规范化为小写',
      );
      expect(() => AkInviteListQuery(limit: 33).toJson(),
          throwsA(isA<AkSpecException>()));
      expect(() => AkInviteRevokeSpec(codeId: 'ab').toJson(),
          throwsA(isA<AkSpecException>()));
      expect(
        AkInviteRevokeSpec(requestId: rid, codeId: 'AB' * 16).toJson(),
        {'spec_version': 1, 'request_id': rid, 'code_id': 'ab' * 16},
      );
    });

    test('start spec：registry/registry_id/id_dir 必填，解析器镜像校验', () {
      // const rid = '0123456789abcdef0123456789abcdef';
      const regId = '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef';
      final json = AkPairingStartSpec(
        registry: '127.0.0.1:7101',
        registryIdHex: regId,
        idDir: r'C:\Users\me\.ak-sending',
        deviceName: 'desk',
      ).toJson();
      expect(json['registry'], '127.0.0.1:7101');
      expect(json['registry_id'], regId);
      expect(json['id_dir'], r'C:\Users\me\.ak-sending');
      expect(json['device_name'], 'desk');
      expect(json['owner_loop'], isTrue, reason: '常驻 owner 循环默认开启');

      expect(
          () => AkPairingStartSpec(
                  registry: 'ak.example.com:7101',
                  registryIdHex: regId,
                  idDir: '/id')
              .toJson(),
          throwsA(isA<AkSpecException>()),
          reason: '主机名不是 ip:port');
      expect(
          () => AkPairingStartSpec(
                  registry: '127.0.0.1:7101', registryIdHex: 'zz', idDir: '/id')
              .toJson(),
          throwsA(isA<AkSpecException>()));
      expect(
          () => AkPairingStartSpec(
                  registry: '127.0.0.1:7101', registryIdHex: regId, idDir: '')
              .toJson(),
          throwsA(isA<AkSpecException>()));
      expect(
          AkPairingStartSpec(
                  registry: '[::1]:7101', registryIdHex: regId, idDir: '/id')
              .toJson()['registry'],
          '[::1]:7101',
          reason: '括号 IPv6 字面量可用');
    });
  });

  group('SchedulerPolicy（值对象 §3 词汇 + 引擎线键）', () {
    test('默认策略 wire 键与引擎 parse_policy 一致并受范围与交叉约束', () {
      final policy = SchedulerPolicy();
      expect(policy.modeLabel, 'serial × parallel');
      expect(
        policy.toJson(),
        {
          'same_code': 'serial',
          'cross_code': 'parallel',
          'max_active_tasks_per_code': 4,
          'max_active_code_groups': 4,
          'max_queued_tasks': 256,
        },
      );
      expect(
        SchedulerPolicy().withModes(
            sameCodeMode: SchedCodeMode.parallel,
            crossCodeMode: SchedCodeMode.serial),
        SchedulerPolicy(
            sameCodeMode: SchedCodeMode.parallel,
            crossCodeMode: SchedCodeMode.serial),
      );
      expect(() => SchedulerPolicy(maxActiveTasks: 65).toJson(),
          throwsA(isA<AkSpecException>()),
          reason: 'max_active_tasks 1..=64');
      expect(
          () => SchedulerPolicy(
                  maxActiveTasks: 4, maxActiveTasksPerCode: 5)
              .toJson(),
          throwsA(isA<AkSpecException>()),
          reason: '每码上限不能超过全局上限');
      expect(
          () => SchedulerPolicy(
                  maxQueuedTasks: 8, maxQueuedTasksPerCode: 9)
              .toJson(),
          throwsA(isA<AkSpecException>()),
          reason: '每码队列不能超过全局队列');
    });

    test('fromJson 同时接受引擎键（same_code）与契约键（same_code_mode）', () {
      expect(
        SchedulerPolicy.fromJson({
          'same_code': 'parallel',
          'cross_code': 'serial',
          'max_active_tasks_per_code': 2,
        }).modeLabel,
        'parallel × serial',
      );
      expect(
        SchedulerPolicy.fromJson({
          'same_code_mode': 'parallel',
          'cross_code_mode': 'serial',
          'revision': 7,
        }).modeLabel,
        'parallel × serial',
        reason: '快照/事件两种拼法都容忍',
      );
      expect(
          () => SchedulerPolicy.fromJson({'same_code': 'turbo'}),
          throwsFormatException);
    });
  });

  group('sched spec 构造', () {
    const rid = '0123456789abcdef0123456789abcdef';
      // const regId = '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef';
    const pid = 'fedcba0987654321fedcba0987654321';

    test('enqueue 规格逐字段匹配 ffi-v2.md §4 冻结示例', () {
      final spec = AkSchedEnqueueSpec(
        requestId: rid,
        pairingId: pid,
        sourceRoots: [r'D:\photos', r'E:\docs'],
      );
      expect(
        spec.toJson(),
        {
          'spec_version': 1,
          'request_id': rid,
          'pairing_id': pid,
          'source_roots': [r'D:\photos', r'E:\docs'],
          'conflict': 'fail',
          'multistream': false,
          'no_internet': false,
          'timeout_secs': 30,
        },
      );
      expect(
          () => AkSchedEnqueueSpec(
                  requestId: rid, pairingId: pid, sourceRoots: [])
              .toJson(),
          throwsA(isA<AkSpecException>()));
      expect(
          () => AkSchedEnqueueSpec(
                  requestId: rid, pairingId: 'zz', sourceRoots: ['a'])
              .toJson(),
          throwsA(isA<AkSpecException>()));
    });

    test('start spec 携带 runtime_id 与初始策略 wire 键', () {
      expect(
        AkSchedStartSpec(requestId: rid, runtimeIdHex: 'AB' * 16).toJson(),
        {
          'spec_version': 1,
          'request_id': rid,
          'runtime_id': 'ab' * 16,
          'same_code': 'serial',
          'cross_code': 'parallel',
          'max_active_tasks_per_code': 4,
          'max_active_code_groups': 4,
          'max_queued_tasks': 256,
        },
      );
      expect(
        AkSchedStartSpec(requestId: rid).toJson().containsKey('runtime_id'),
        isFalse,
        reason: '缺省绑定引擎当前活跃的配对 runtime',
      );
      expect(
          () => AkSchedStartSpec(requestId: rid, runtimeIdHex: 'zz').toJson(),
          throwsA(isA<AkSpecException>()));
    });
  });

  group('邀请码与事件的 JSON 解析', () {
    test('InviteSummary 解码全部状态；create 信封解码一次性明文码与掩码提示',
        () {
      final row = {
        'code_id': 'ab' * 16,
        'state': 'active',
        'created_ms': 1730000000000,
        'expires_ms': 0,
        'revision': 1,
      };
      final summary = InviteSummary.fromJson(row);
      expect(summary.state, InviteState.active);
      expect(summary.isExpiredAt(1893456000000), isFalse,
          reason: 'expires_ms=0 是长期有效');
      final expired = InviteSummary.fromJson(
          {...row, 'state': 'expired', 'expires_ms': 100});
      expect(expired.isExpiredAt(200), isTrue);

      final created = InviteCreated.fromJson({
        'op': 'create',
        'code': 'aki10123456789abcdefghjkmnpqrstvwxyz',
        'hint': 'AKI1-0123-456…',
        'code_id': 'ab' * 16,
        'expires_ms': 1730003600000,
        'revision': 1,
      });
      expect(created.invitationCode,
          'AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ',
          reason: '创建结果把码规范化成展示格式');
      expect(created.hint, 'AKI1-0123-456…');
      expect(created.summary.state, InviteState.active,
          reason: '新 claim 的码按定义为 active');
      expect(created.summary.expiresMs, 1730003600000);
      // 秘密不进 toString（日志纪律）。
      expect(created.toString(), contains('<redacted>'));
      expect(created.toString(), isNot(contains('0123-4567')));

      // 列表信封：entries + next_cursor。
      expect(
        InviteSummary.listFromJson({
          'op': 'list',
          'next_cursor': '00' * 16,
          'entries': [row],
        }),
        [summary],
      );
      expect(
          InviteSummary.listNextCursor(
              {'op': 'list', 'next_cursor': '00' * 16, 'entries': []}),
          '00' * 16);
      expect(() => InviteSummary.fromJson({...row, 'state': 'paused'}),
          throwsFormatException);
      expect(() => InviteSummary.listFromJson({'invites': []}),
          throwsFormatException,
          reason: '引擎信封键是 entries');
    });

    test('PairingEvent.fromJson 映射引擎线名并保留未知事件', () {
      PairingEvent decode(Map<String, Object?> json) =>
          PairingEvent.fromJson(
              jsonDecode(jsonEncode(json)) as Map<String, dynamic>);
      expect(decode({'event': 'runtime_ready', 'runtime': '10' * 16}),
          isA<ServiceOnlineEvent>(),
          reason: 'runtime_ready = 服务在线（accepted ≠ online）');
      expect(
        decode({'event': 'runtime_failed', 'runtime': '10' * 16, 'error': 'tls'}),
        isA<ServiceOfflineEvent>()
            .having((e) => e.reason, 'reason', 'tls'),
      );
      expect(
        decode({'event': 'op_done', 'op_id': '05' * 16, 'op': 'create'}),
        isA<OpDoneEvent>().having((e) => e.op, 'op', 'create'),
      );
      expect(
        decode({'event': 'op_failed', 'op_id': '06' * 16, 'op': 'redeem'}),
        isA<OpFailedEvent>(),
      );
      expect(
        decode({
          'event': 'redeem_offered',
          'code_id': 'ab' * 16,
          'pairing_id': 'ef' * 16,
          'peer': 'cd' * 32,
        }),
        isA<RedeemPendingEvent>()
            .having((e) => e.peer, 'peer', 'cd' * 32)
            .having((e) => e.pairingId, 'pairingId', 'ef' * 16),
        reason: '调度队列的 redeem_offered 复用同一类型',
      );
      // 服务层合成的时间线词汇也能解码（Fake 驱动路径）。
      expect(decode({'event': 'peer_prepared', 'pairing_id': 'ef' * 16}),
          isA<PeerPreparedEvent>());
      expect(
        decode({'event': 'pairing_complete', 'pairing_id': 'ef' * 16, 'peer': 'cd' * 32}),
        isA<PairingCompleteEvent>(),
      );
      expect(
        decode({'event': 'redeem_failed', 'error': 'PAIR_UNAVAILABLE'}),
        isA<RedeemFailedEvent>()
            .having((e) => e.error, 'error', 'PAIR_UNAVAILABLE'),
      );
      expect(
        decode({'event': 'invitation_revoked', 'code_id': 'ab' * 16, 'synced': false}),
        isA<InvitationRevokedEvent>().having((e) => e.synced, 'synced', false),
      );
      final unknown = decode({'event': 'code_exhausted', 'left': 3});
      expect(unknown, isA<PairingUnknownEvent>());
      expect(unknown.name, 'code_exhausted');
      // 缺字段/错类型是契约破裂，必须抛出而不是静默。
      expect(
          () => decode({
                'event': 'redeem_offered',
                'code_id': 'ab' * 16,
                'peer': 'cd' * 32,
                'pairing_id': 5,
              }),
          throwsFormatException);
      expect(() => decode({'event': 'peer_prepared'}), throwsFormatException);
    });

    test('SchedulerEvent.fromJson 映射引擎线名（task/group/outcome）', () {
      SchedulerEvent decode(Map<String, Object?> json) =>
          SchedulerEvent.fromJson(
              jsonDecode(jsonEncode(json)) as Map<String, dynamic>);
      expect(
        decode({'event': 'task_queued', 'task': '11' * 16, 'group': '22' * 16}),
        isA<TaskQueuedEvent>().having((e) => e.codeId, 'group', '22' * 16),
      );
      expect(
        decode({'event': 'task_admitted', 'task': '11' * 16, 'group': '22' * 16}),
        isA<AdmittedEvent>(),
      );
      expect(decode({'event': 'task_started', 'task': '11' * 16}),
          isA<StartedEvent>());
      expect(
        decode({
          'event': 'task_finished',
          'task': '11' * 16,
          'group': '22' * 16,
          'outcome': 'failed',
          'error': 'io',
        }),
        isA<FinishedEvent>()
            .having((e) => e.outcome, 'outcome', 'failed')
            .having((e) => e.error, 'error', 'io'),
      );
      expect(
        decode({
          'event': 'task_cancelled',
          'task': '11' * 16,
          'group': '22' * 16,
          'was_running': true,
        }),
        isA<CancelledEvent>().having((e) => e.wasRunning, 'was_running', true),
      );
      expect(
        decode({'event': 'policy_changed', 'revision': 3, 'draining': true}),
        isA<PolicyChangedEvent>()
            .having((e) => e.revision, 'revision', 3)
            .having((e) => e.draining, 'draining', true),
      );
      expect(decode({'event': 'draining', 'active_tasks': 2}),
          isA<DrainingEvent>()
              .having((e) => e.activeTasks, 'active_tasks', 2));
      // 会话级线与未知名 → 未知回退。
      expect(decode({'event': 'transport_disconnected'}),
          isA<SchedulerUnknownEvent>());
      expect(decode({'event': 'backoff_scheduled', 'task': '11' * 16, 'attempt': 1, 'until_ms': 5}),
          isA<SchedulerUnknownEvent>());
      expect(() => decode({'event': 'draining'}), throwsFormatException);
    });
  });

  group('PairingFfi（Fake 驱动的调用缝）', () {
    test('start 受理后绑定 runtime id；负码转为 AkFfiException', () {
      final fake = FakePairingCalls();
      final pairing = PairingFfi(calls: fake);
      final runtime = pairing.start(_startSpec());
      expect(runtime.runtimeIdHex, fake.startRuntimeIdHex);
      final spec =
          jsonDecode(fake.invocations.single.$2) as Map<String, dynamic>;
      expect(spec['id_dir'], '/tmp/ak');
      expect(spec['registry'], '127.0.0.1:7101');

      final broken = FakePairingCalls()..startCode = AkStatus.errInvalid;
      expect(
        () => PairingFfi(calls: broken).start(_startSpec()),
        throwsA(isA<AkFfiException>()
            .having((e) => e.code, 'code', AkStatus.errInvalid)),
      );
    });

    test('create/redeem/list/revoke 受理后返回 op 轮询句柄且 spec 逐字到达',
        () {
      final fake = FakePairingCalls();
      final runtime = PairingFfi(calls: fake).start(_startSpec());
      const rid = '0123456789abcdef0123456789abcdef';
      // const regId = '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef';

      final created = runtime.createInvite(requestId: rid, ttlSecs: 3600);
      expect(created.kind, PairingOpKind.create);
      expect(created.opIdHex, hasLength(32));
      final createSpec =
          jsonDecode(fake.invocations[1].$2) as Map<String, dynamic>;
      expect(createSpec['ttl_secs'], 3600);

      runtime.redeemInvite('AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ',
          requestId: rid);
      final redeemSpec =
          jsonDecode(fake.invocations[2].$2) as Map<String, dynamic>;
      expect(redeemSpec['invite_code'],
          'AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ');

      runtime.listInvites(
          stateFilter: InviteListStateFilter.all, requestId: rid);
      expect(
          (jsonDecode(fake.invocations[3].$2)
              as Map<String, dynamic>)['state_filter'],
          'all');

      runtime.revokeInvite('ab' * 16, requestId: rid);
      expect((jsonDecode(fake.invocations[4].$2)
          as Map<String, dynamic>)['code_id'], 'ab' * 16);

      // 引擎拒绝受理（-4）以 AkFfiException 冒泡。
      final stranger =
          FakePairingCalls()..rejectStartsWithCode = AkStatus.errNotFound;
      final strangerRuntime = PairingFfi(calls: stranger).start(_startSpec());
      expect(
          () => strangerRuntime.listInvites(requestId: rid),
          throwsA(isA<AkFfiException>()
              .having((e) => e.code, 'code', AkStatus.errNotFound)));
    });

    test('PairingOpHandle.poll：-3 无损扩容重试直到 done；pending 与 failed 分派',
        () {
      final fake = FakePairingCalls();
      final runtime = PairingFfi(calls: fake).start(_startSpec());
      final op = runtime.listInvites(
          stateFilter: InviteListStateFilter.all,
          requestId: '0123456789abcdef0123456789abcdef');
      fake.opResults[op.opIdHex] = FakeOpScript(
        code: AkStatus.done,
        json: jsonEncode({
          'op': 'list',
          'next_cursor': '00' * 16,
          'entries': [
            {
              'code_id': 'ab' * 16,
              'state': 'active',
              'created_ms': 1730000000000,
              'expires_ms': 0,
              'revision': 1,
            }
          ]
        }),
        pendingPolls: 1,
      );
      // 强制第一次尝试必须 -3（缓冲区过小，不消费、不重复受理）。
      fake.minBufferCap = 512;

      expect(op.poll().isPending, isTrue, reason: '第一次轮询仍是 pending');
      final done = op.poll();
      expect(done.isDone, isTrue);
      expect(InviteSummary.listFromJson(done.map!), hasLength(1));
      expect(fake.sawCaps, containsAll(<int>[256, 512]),
          reason: '缓冲区经历了增长');

      final failing = runtime.revokeInvite('ab' * 16);
      fake.opResults[failing.opIdHex] = FakeOpScript(
        code: AkStatus.failed,
        json: '{"op":"revoke","error":"code not found"}',
      );
      final failed = failing.poll();
      expect(failed.isFailed, isTrue);
      expect(failed.error, 'code not found');

      final unknown = runtime.stop();
      expect(
          unknown.poll,
          throwsA(isA<Object>()),
          reason: '无脚本的 op 在结果轮询中是 -4（未知 op）');
    });

    test('PairingOpHandle.wait 轮询循环收敛到 done 并能被注入的延迟驱动',
        () async {
      final fake = FakePairingCalls();
      final runtime = PairingFfi(calls: fake).start(_startSpec());
      final op = runtime.createInvite(
          requestId: '0123456789abcdef0123456789abcdef');
      fake.opResults[op.opIdHex] = FakeOpScript(
        code: AkStatus.done,
        json: jsonEncode({
          'op': 'create',
          'code': 'AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ',
          'hint': 'AKI1-0123-456…',
          'code_id': 'ab' * 16,
          'expires_ms': 0,
          'revision': 1,
        }),
        pendingPolls: 2,
      );
      var delayCalls = 0;
      final map = op.wait(
        delay: (_) async {
          delayCalls += 1;
        },
      );
      final created = InviteCreated.fromJson(await map);
      expect(created.invitationCode,
          'AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ');
      expect(created.summary.codeId, 'ab' * 16);
      expect(delayCalls, 2, reason: '两次 pending 各等一拍');

      final failing = runtime.createInvite(
          requestId: '0123456789abcdef0123456789abcdef');
      fake.opResults[failing.opIdHex] = FakeOpScript(
        code: AkStatus.failed,
        json: '{"op":"create","error":"RATE_LIMITED"}',
      );
      await expectLater(
        failing.wait(delay: (_) async {}),
        throwsA(isA<AkFfiException>()),
      );
    });

    test('pollEvent：空队列返回 null；-3 扩容不丢事件；事件按序类型化', () {
      final fake = FakePairingCalls();
      final runtime = PairingFfi(calls: fake).start(_startSpec());
      expect(runtime.pollEvent(), isNull);

      fake
        ..minBufferCap = 512
        ..eventQueue.addAll([
          {'event': 'runtime_ready', 'runtime': '10' * 16},
          {
            'event': 'redeem_offered',
            'code_id': 'ab' * 16,
            'pairing_id': 'ef' * 16,
            'peer': 'cd' * 32,
          },
        ]);
      final first = runtime.pollEvent();
      expect(first, isA<ServiceOnlineEvent>());
      final second = runtime.pollEvent();
      expect(second,
          isA<RedeemPendingEvent>().having((e) => e.codeId, 'codeId', 'ab' * 16));
      expect(runtime.pollEvent(), isNull, reason: '队列已空');
      expect(fake.eventQueue, isEmpty, reason: '两个事件都恰好消费一次');
    });

    test('cancelOp 撤销在途 op；未知 op 是 -4', () {
      final fake = FakePairingCalls();
      final runtime = PairingFfi(calls: fake).start(_startSpec());
      final op = runtime.listInvites(
          requestId: '0123456789abcdef0123456789abcdef');
      runtime.cancelOp(op);
      expect(fake.cancelledOps, contains(op.opIdHex));
      expect(
        fake.cancelOp(runtime.runtimeId, akIdFromHex('ee' * 16)),
        AkStatus.errNotFound,
      );
    });
  });

  group('id 助手', () {
    test('16 字节 id 与 32 位十六进制互转；requestId 形状', () {
      final id = akIdFromHex('0f1e2d3c4b5a69788796a5b4c3d2e1f0');
      expect(akIdToHex(id), '0f1e2d3c4b5a69788796a5b4c3d2e1f0');
      expect(() => akIdFromHex('0f1e'), throwsFormatException);
      expect(() => akIdToHex(Uint8List(3)), throwsArgumentError);
      expect(newRequestId(), matches(RegExp(r'^[0-9a-f]{32}$')));
    });
  });

  group('SchedRuntimeFfi（Fake 驱动的调用缝）', () {
    test('start 绑定 runtime 并发送初始策略；enqueue 立即返回 task id', () {
      final fake = FakeSchedCalls();
      final runtime = SchedRuntimeFfi(calls: fake);
      runtime.start(SchedulerPolicy().withModes(sameCodeMode: SchedCodeMode.parallel),
          runtimeIdHex: '77' * 16);
      expect(fake.startSpecs, hasLength(1));
      final startSpec =
          jsonDecode(fake.startSpecs.single) as Map<String, dynamic>;
      expect(startSpec['runtime_id'], '77' * 16);
      expect(startSpec['same_code'], 'parallel');
      expect(startSpec['cross_code'], 'parallel');

      final taskId = runtime.enqueue(AkSchedEnqueueSpec(
        requestId: '0123456789abcdef0123456789abcdef',
        pairingId: 'fedcba0987654321fedcba0987654321',
        sourceRoots: [r'D:\photos'],
      ));
      expect(akIdToHex(taskId), hasLength(32));
      expect(fake.enqueueSpecs, hasLength(1));
    });

    test('setPolicy 发送冻结策略线键；cancel 对未知任务是 -4', () {
      final fake = FakeSchedCalls();
      final runtime = SchedRuntimeFfi(calls: fake);
      runtime.setPolicy(
          SchedulerPolicy().withModes(crossCodeMode: SchedCodeMode.serial));
      expect(
        jsonDecode(fake.policySpecs.single),
        SchedulerPolicy()
            .withModes(crossCodeMode: SchedCodeMode.serial)
            .toJson(),
      );
      expect(
          () => runtime.cancelTask(akIdFromHex('ee' * 16)),
          throwsA(isA<AkFfiException>()
              .having((e) => e.code, 'code', AkStatus.errNotFound)));
      final taskId = runtime.enqueue(AkSchedEnqueueSpec(
        requestId: '0123456789abcdef0123456789abcdef',
        pairingId: 'fedcba0987654321fedcba0987654321',
        sourceRoots: ['a'],
      ));
      runtime.cancelTask(taskId); // 已受理任务：ok
    });

    test('snapshot 与 pollEvent：JSON 解码、-3 扩容、事件类型化', () {
      final fake = FakeSchedCalls()..minBufferCap = 2048;
      final runtime = SchedRuntimeFfi(calls: fake);
      fake.snapshotJson = jsonEncode({
        'policy': {
          'same_code': 'serial',
          'cross_code': 'parallel',
          'max_active_tasks_per_code': 4,
          'max_active_code_groups': 4,
          'max_queued_tasks': 256,
        },
        'policy_revision': 1,
        'draining': false,
        'active_tasks': 1,
        'queued_tasks': 0,
        'shutting_down': false,
        'tasks': [
          {'id': '11' * 16, 'state': 'Transferring', 'group': '22' * 16}
        ],
      });
      final snapshot = runtime.snapshot();
      expect(
          SchedulerPolicy.fromJson(snapshot!['policy'] as Map<String, dynamic>)
              .modeLabel,
          'serial × parallel');
      expect((snapshot['tasks'] as List).single['state'], 'Transferring');
      expect(fake.sawCaps, containsAll(<int>[1024, 2048]));

      expect(runtime.pollEvent(), isNull);
      fake.eventQueue.addAll([
        {'event': 'policy_changed', 'revision': 2, 'draining': false},
        {'event': 'draining', 'active_tasks': 1},
      ]);
      expect(runtime.pollEvent(),
          isA<PolicyChangedEvent>().having((e) => e.revision, 'revision', 2));
      expect(runtime.pollEvent(), isA<DrainingEvent>());
      expect(runtime.pollEvent(), isNull);
    });
  });
}

// ---------------------------------------------------------------------------
// Fakes（指针自由调用缝的确定性实现；-3 语义与契约一致）。
// ---------------------------------------------------------------------------

AkPairingStartSpec _startSpec() => AkPairingStartSpec(
    registry: '127.0.0.1:7101',
    registryIdHex: '00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff',
    idDir: '/tmp/ak');

/// One scripted op outcome: [pendingPolls] polls answer `AK_PENDING`,
/// then [code] (`1` done / `2` failed) delivers [json].
final class FakeOpScript {
  FakeOpScript({
    required this.code,
    required this.json,
    this.pendingPolls = 0,
  });

  final int code;
  final String json;
  int pendingPolls;
}

/// Deterministic [AkPairingCalls]: captures every spec JSON, hands out
/// sequential op ids, and honors the frozen buffer contract (`-3` writes
/// and consumes nothing).
final class FakePairingCalls implements AkPairingCalls {
  final List<(String, String)> invocations = [];
  final Map<String, int> opResultCalls = {};
  final Map<String, FakeOpScript> opResults = {};
  final List<Map<String, Object?>> eventQueue = [];
  final List<String> cancelledOps = [];
  final List<int> sawCaps = [];

  /// Buffer calls with `cap` below this answer `-3` (drives the grow loop).
  int minBufferCap = 0;

  /// Start answer; anything non-zero is returned verbatim.
  int startCode = AkStatus.ok;

  /// When set, `*_start` calls answer this code instead of accepting.
  int? rejectStartsWithCode;

  int _nextOp = 1;

  late final String startRuntimeIdHex = '10' * 16;
  late final Uint8List _startRuntimeId = akIdFromHex(startRuntimeIdHex);

  @override
  int start(String specJson, Uint8List runtimeIdOut) {
    invocations.add(('ak_pairing_start', specJson));
    if (startCode != AkStatus.ok) {
      return startCode;
    }
    runtimeIdOut.setAll(0, _startRuntimeId);
    return AkStatus.ok;
  }

  @override
  int createStart(
          Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _accept('ak_pairing_create_start', specJson, opIdOut);

  @override
  int redeemStart(
          Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _accept('ak_pairing_redeem_start', specJson, opIdOut);

  @override
  int listStart(Uint8List runtimeId, String queryJson, Uint8List opIdOut) =>
      _accept('ak_pairing_list_start', queryJson, opIdOut);

  @override
  int revokeStart(
          Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _accept('ak_pairing_revoke_start', specJson, opIdOut);

  @override
  int stopStart(Uint8List runtimeId, Uint8List opIdOut) => _accept(
      'ak_pairing_stop_start', '{"spec_version":1}', opIdOut);

  int _accept(
      String symbol, String specJson, Uint8List opIdOut) {
    invocations.add((symbol, specJson));
    final reject = rejectStartsWithCode;
    if (reject != null) {
      return reject;
    }
    final sequence = _nextOp;
    _nextOp += 1;
    final id = akIdFromHex(sequence.toRadixString(16).padLeft(32, '0'));
    opIdOut.setAll(0, id);
    _issuedOps.add(akIdToHex(id));
    return AkStatus.ok;
  }

  @override
  int opResult(
      Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap) {
    sawCaps.add(cap);
    final key = akIdToHex(opId);
    final script = opResults[key];
    if (script == null) {
      return AkStatus.errNotFound;
    }
    opResultCalls[key] = (opResultCalls[key] ?? 0) + 1;
    if (script.pendingPolls > 0) {
      script.pendingPolls -= 1;
      return AkStatus.pending;
    }
    if (cap < minBufferCap ||
        utf8.encode(script.json).length + 1 > cap) {
      return AkStatus.errBuffer; // 不消费：稍后以更大缓冲重读同一结果
    }
    _writePayload(buffer, script.json);
    return script.code;
  }

  @override
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap) {
    sawCaps.add(cap);
    if (eventQueue.isEmpty) {
      return AkStatus.pending;
    }
    if (cap < minBufferCap ||
        utf8.encode(jsonEncode(eventQueue.first)).length + 1 > cap) {
      return AkStatus.errBuffer; // 事件保持排队
    }
    _writePayload(buffer, jsonEncode(eventQueue.removeAt(0)));
    return AkStatus.event;
  }

  @override
  int cancelOp(Uint8List runtimeId, Uint8List opId) {
    final key = akIdToHex(opId);
    if (_issuedOps.contains(key)) {
      cancelledOps.add(key);
      return AkStatus.ok;
    }
    return AkStatus.errNotFound;
  }

  final Set<String> _issuedOps = {};

  void _writePayload(Uint8List buffer, String payload) {
    final bytes = utf8.encode(payload);
    for (var i = 0; i < bytes.length; i++) {
      buffer[i] = bytes[i];
    }
    buffer[bytes.length] = 0;
  }
}

/// Deterministic [AkSchedCalls] (the six shipped engine calls).
final class FakeSchedCalls implements AkSchedCalls {
  final List<String> startSpecs = [];
  final List<String> enqueueSpecs = [];
  final List<String> policySpecs = [];
  final Map<String, FakeOpScript> taskResults = {};
  final List<Map<String, Object?>> eventQueue = [];
  final List<int> sawCaps = [];
  final Set<String> knownTasks = {};

  String snapshotJson = '{}';

  int minBufferCap = 0;
  int snapshotCode = AkStatus.event;
  int _nextId = 1;

  @override
  int start(String specJson) {
    startSpecs.add(specJson);
    return AkStatus.ok;
  }

  @override
  int enqueue(String specJson, Uint8List taskIdOut) {
    enqueueSpecs.add(specJson);
    final sequence = _nextId;
    _nextId += 1;
    final id = akIdFromHex(sequence.toRadixString(16).padLeft(32, '0'));
    taskIdOut.setAll(0, id);
    knownTasks.add(akIdToHex(id));
    return AkStatus.ok;
  }

  @override
  int setPolicy(String policyJson) {
    policySpecs.add(policyJson);
    return AkStatus.ok;
  }

  @override
  int cancel(Uint8List taskId) => knownTasks.contains(akIdToHex(taskId))
      ? AkStatus.ok
      : AkStatus.errNotFound;

  @override
  int snapshot(String queryJson, Uint8List buffer, int cap) {
    sawCaps.add(cap);
    if (snapshotCode == AkStatus.pending) {
      return AkStatus.pending;
    }
    if (cap < minBufferCap ||
        utf8.encode(snapshotJson).length + 1 > cap) {
      return AkStatus.errBuffer;
    }
    _write(buffer, snapshotJson);
    return AkStatus.event;
  }

  @override
  int pollEvent(Uint8List buffer, int cap) {
    sawCaps.add(cap);
    if (eventQueue.isEmpty) {
      return AkStatus.pending;
    }
    if (cap < minBufferCap ||
        utf8.encode(jsonEncode(eventQueue.first)).length + 1 > cap) {
      return AkStatus.errBuffer;
    }
    _write(buffer, jsonEncode(eventQueue.removeAt(0)));
    return AkStatus.event;
  }

  void _write(Uint8List buffer, String payload) {
    final bytes = utf8.encode(payload);
    for (var i = 0; i < bytes.length; i++) {
      buffer[i] = bytes[i];
    }
    buffer[bytes.length] = 0;
  }
}
