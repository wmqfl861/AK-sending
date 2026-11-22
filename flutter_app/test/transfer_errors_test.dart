/// W3 (V-N4) table-driven unit tests for the error-copy mapping
/// (`lib/ui/service/transfer_errors.dart`): every 审核 §4.3 失败与恢复
/// situation + the unknown fallback, the idle-vs-interrupted separation,
/// and the four output lines (title/reason/suggestion/detail-verbatim).
///
/// Batch X2 additions (W 复核 §2): W-N9 — the pairing/join invalidCode
/// copy lists every cause the engine's unified refusal covers; W-N10 —
/// the storage vocabulary and copy tests use the engine's REAL texts
/// (copied from the Rust constants/Display impls, provenance noted per
/// constant — never self-invented strings); W-N4 — status -2 claims
/// invalidCode only on the pairing/join scenes, table-driven across the
/// scenes.
///
/// Plain `flutter test` (the mapping has no Flutter imports; the FFI
/// exception is mimicked by a structurally identical private class so
/// the mapping's dynamic `code`/`message` reads are exercised without
/// importing the FFI package).
library;

import 'dart:async' show TimeoutException;

import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/ui/service/transfer_errors.dart';

/// Shape-compatible stand-in for `AkFfiException` (`code` + `message`).
class _FakeAkFfiException implements Exception {
  const _FakeAkFfiException(this.code, this.message);

  final int code;
  final String message;

  @override
  String toString() => 'AkFfiException(AK_FAILED $code): $message';
}

/// An error with NO `code`/`message` members (the dynamic-read misses).
class _OpaqueError implements Exception {}

/// The V2-d raw shape (AUD-04): connection lost AFTER a successful
/// connect — must classify as an interruption, never idle.
const _v2dConnectionLost = _FakeAkFfiException(
  2,
  'transfer failed after connecting via 10.0.0.9:52330: '
      'stream failed: connection lost',
);

/// W-N10: the engine's REAL storage-full texts, copied from the Rust
/// sources (not invented). What the SENDER engine renders when the
/// receiver reports Error{StorageFull} — every Error frame maps to
/// `EngineError::PeerError { code, message }`
/// (crates/ak-engine/src/handshake.rs / sender.rs) whose Display is
/// `peer sent error {code}: {message}`
/// (crates/ak-engine/src/lib.rs:1036), with `ErrorCode::StorageFull`
/// displaying as `STORAGE_FULL` (crates/ak-core/src/error.rs:66).
///
/// Precheck refusal — the receiver's headroom check fails BEFORE any
/// part file exists; message from crates/ak-engine/src/receiver.rs:955.
const _peerStorageFullPrecheck = _FakeAkFfiException(
  2,
  'peer sent error STORAGE_FULL: insufficient free space at destination',
);

/// Mid-transfer refusal — the disk filled during the chunk phase; the
/// frame's fixed text is STORAGE_FULL_MESSAGE
/// (crates/ak-engine/src/recv_single.rs:18).
const _peerStorageFullMidTransfer = _FakeAkFfiException(
  2,
  'peer sent error STORAGE_FULL: receiver ran out of disk space during '
      'the transfer',
);

/// What the RECEIVER's own engine reports locally when its destination
/// is too small — `EngineError::StorageFull`'s Display
/// (crates/ak-engine/src/lib.rs:1046:
/// `destination storage full: {available} bytes available, {needed}
/// needed`).
const _localStorageFull = _FakeAkFfiException(
  2,
  'destination storage full: 1024 bytes available, 8192 needed',
);

/// The engine's unified join refusal (anti-enumeration: one answer for
/// unknown/expired/revoked codes, an offline owner, and a same-direction
/// join before commit) — crates/ak-session/src/pairing/task_net.rs:651.
const _unifiedRefusal = _FakeAkFfiException(
  2,
  'unknown task code (or expired, revoked, owner offline)',
);

void main() {
  group('classifyTransferError: the audit §4.3 table + fallback', () {
    // (label, error, scene, expected kind) — one row per 失败与恢复
    // situation, the idle/interrupted pair, and the unknown fallback.
    // NOT const: TimeoutException/StateError have non-const constructors.
    final table =
        <(String, Object, TransferErrorScene, TransferErrorKind)>[
      (
        '码无效或过期：engine 统一拒绝文本（task_net.rs:651）',
        _unifiedRefusal,
        TransferErrorScene.pairing,
        TransferErrorKind.invalidCode,
      ),
      (
        '码无效或过期：service-readable 任务码无效',
        '任务码无效或已被使用，请确认后重试。',
        TransferErrorScene.pairing,
        TransferErrorKind.invalidCode,
      ),
      (
        '码无效或过期：配对码不存在（fake/service 文案）',
        '配对码不存在或已失效，请向对方确认最新配对码。',
        TransferErrorScene.joining,
        TransferErrorKind.invalidCode,
      ),
      (
        'W-N4：FFI -2 在加入场景（pairing）→ invalidCode',
        _FakeAkFfiException(-2, 'join spec validation failed'),
        TransferErrorScene.pairing,
        TransferErrorKind.invalidCode,
      ),
      (
        'W-N4：FFI -2 在 joining 场景同样 → invalidCode',
        _FakeAkFfiException(-2, 'join spec validation failed'),
        TransferErrorScene.joining,
        TransferErrorKind.invalidCode,
      ),
      (
        'W-N4：FFI -2 在发送场景走词表与兜底 → unknown',
        _FakeAkFfiException(-2, 'send spec validation failed'),
        TransferErrorScene.sending,
        TransferErrorKind.unknown,
      ),
      (
        'W-N4：FFI -2 在接收场景走词表与兜底 → unknown',
        _FakeAkFfiException(-2, 'recv spec validation failed'),
        TransferErrorScene.receiving,
        TransferErrorKind.unknown,
      ),
      (
        'W-N4：-2 的消息命中词表时由词表决定（发送场景的统一拒绝文本'
        '仍是 invalidCode）',
        _FakeAkFfiException(
            -2, 'unknown task code (or expired, revoked, owner offline)'),
        TransferErrorScene.sending,
        TransferErrorKind.invalidCode,
      ),
      (
        '两端无法直连：no candidate accepted',
        _FakeAkFfiException(
            2, 'no candidate accepted the connection (10.0.0.9: timeout)'),
        TransferErrorScene.pairing,
        TransferErrorKind.directUnavailable,
      ),
      (
        '两端无法直连：connect failed（连超时也是无法直连）',
        '无法建立直连：连接超时',
        TransferErrorScene.pairing,
        TransferErrorKind.directUnavailable,
      ),
      (
        '传输中断：V2-d 原文（connect 之后 connection lost）',
        _v2dConnectionLost,
        TransferErrorScene.sending,
        TransferErrorKind.interrupted,
      ),
      (
        '传输中断：peer vanished（checkpoint kept）',
        _FakeAkFfiException(2, 'peer vanished mid-transfer (checkpoint kept)'),
        TransferErrorScene.sending,
        TransferErrorKind.interrupted,
      ),
      (
        '闲置过期：engine idle_expired',
        _FakeAkFfiException(2, 'pairing ended: idle_expired'),
        TransferErrorScene.pairing,
        TransferErrorKind.idleExpired,
      ),
      (
        '传输中断：EngineError::IdleTimeout（发送/接收 op 的空闲超时是'
        '传输中断，不是配对闲置）',
        _FakeAkFfiException(2, 'idle timeout: no frame within 10s'),
        TransferErrorScene.sending,
        TransferErrorKind.interrupted,
      ),
      (
        '对方拒绝：DECLINED',
        '对方拒绝接收文件（DECLINED）',
        TransferErrorScene.sending,
        TransferErrorKind.peerDeclined,
      ),
      (
        '对方拒绝：engine declined by the receiving user',
        _FakeAkFfiException(2, 'declined by the receiving user: job 7'),
        TransferErrorScene.sending,
        TransferErrorKind.peerDeclined,
      ),
      (
        'W-N10：接收方预检的真实文本（receiver.rs:955）→ 保存位置类',
        _peerStorageFullPrecheck,
        TransferErrorScene.sending,
        TransferErrorKind.saveDirNotWritable,
      ),
      (
        'W-N10：传输中途 STORAGE_FULL 帧的真实文本（recv_single.rs:18）'
        '→ 保存位置类',
        _peerStorageFullMidTransfer,
        TransferErrorScene.sending,
        TransferErrorKind.saveDirNotWritable,
      ),
      (
        '保存位置不可写：接收方本地 Display（lib.rs:1046）',
        _localStorageFull,
        TransferErrorScene.receiving,
        TransferErrorKind.saveDirNotWritable,
      ),
      (
        '长时间无进展：TimeoutException 类型',
        TimeoutException('发送超时（150 秒无进展）', Duration(seconds: 150)),
        TransferErrorScene.sending,
        TransferErrorKind.stalled,
      ),
      (
        '长时间无进展：加入超时',
        '加入超时：请确认任务码后重试。',
        TransferErrorScene.pairing,
        TransferErrorKind.stalled,
      ),
      (
        '未知错误兜底：identity failure',
        StateError('身份认证失败'),
        TransferErrorScene.sending,
        TransferErrorKind.unknown,
      ),
      (
        '未知错误兜底：读权限问题不属于保存位置类',
        StateError('没有文件读取权限（permission denied）'),
        TransferErrorScene.sending,
        TransferErrorKind.unknown,
      ),
      (
        '未知错误兜底：对端身份被替换不是拒绝',
        StateError('对端身份已被替换，拒绝连接'),
        TransferErrorScene.receiving,
        TransferErrorKind.unknown,
      ),
    ];

    for (final (label, error, scene, expected) in table) {
      test(label, () {
        expect(classifyTransferError(error, scene), expected,
            reason: '输入（$scene）：$error');
      });
    }
  });

  group('transferErrorView: four output lines per situation', () {
    test('W-N9：加入场景的 invalidCode 三层文案列全原因（pairing 与 '
        'joining 同文案）', () {
      const reason = '配对码可能输错、已过期、已撤销或已使用，也可能是对方'
          '不在线，或双方选了同一个方向。';
      const suggestion = '请让对方保持应用打开并停留在前台，确认一方发送、'
          '一方接收，再核对配对码。需要时请对方重新生成。';
      for (final scene in [
        TransferErrorScene.pairing,
        TransferErrorScene.joining,
      ]) {
        final view = transferErrorView(_unifiedRefusal, scene);
        expect(view.title, '这个码用不了', reason: '$scene');
        expect(view.reason, reason, reason: '$scene');
        expect(view.suggestion, suggestion, reason: '$scene');
        // 三层信息都要在：原因层的前台/同向线索、建议层的重新生成。
        expect(view.reason, contains('对方不在线'), reason: '$scene');
        expect(view.reason, contains('同一个方向'), reason: '$scene');
        expect(view.suggestion, contains('前台'), reason: '$scene');
        expect(view.suggestion, contains('重新生成'), reason: '$scene');
      }
      // 详情原文：错误码 + 原文，一字不动。
      expect(
        transferErrorView(_unifiedRefusal, TransferErrorScene.joining).detail,
        'AkFfiException(AK_FAILED 2): '
            'unknown task code (or expired, revoked, owner offline)',
      );
    });

    test('W-N4：状态码 -2 三个场景的标题（表驱动）', () {
      final table = <(TransferErrorScene, String)>[
        (TransferErrorScene.pairing, '这个码用不了'),
        (TransferErrorScene.joining, '这个码用不了'),
        (TransferErrorScene.sending, '出了点问题'),
        (TransferErrorScene.receiving, '出了点问题'),
      ];
      for (final (scene, expectedTitle) in table) {
        final view = transferErrorView(
            _FakeAkFfiException(-2, 'join spec validation failed'), scene);
        expect(view.title, expectedTitle, reason: '$scene');
      }
    });

    test('W-N4：传输场景命中词表的失效码 → 重新配对，不再叫用户输新码', () {
      final view = transferErrorView(
          _FakeAkFfiException(
              -2, 'unknown task code (or expired, revoked, owner offline)'),
          TransferErrorScene.sending);
      expect(view.kind, TransferErrorKind.invalidCode);
      expect(view.title, '这个码用不了');
      expect(view.suggestion, '请重新配对后再试。');
      expect(view.suggestion, isNot(contains('输入新的码')));
    });

    test('两端无法直连 → 不经过服务器中转的承诺 + 再试一次', () {
      final view = transferErrorView(
        _FakeAkFfiException(
            2, 'no candidate accepted the connection (timeout)'),
        TransferErrorScene.pairing,
      );
      expect(view.title, '无法直接连接到对方');
      expect(view.reason, contains('Wi-Fi'));
      expect(view.reason, contains('不会经过服务器中转'));
      expect(view.suggestion, contains('再试一次'));
    });

    test('传输中断 (sending) → 断点续传建议', () {
      final view = transferErrorView(
          _v2dConnectionLost, TransferErrorScene.sending);
      expect(view.title, '传输中断了');
      expect(view.suggestion, contains('断点续传'));
      expect(view.detail, contains('AK_FAILED'));
      expect(view.detail, contains('connection lost'));
    });

    test('传输中断 (receiving) → 请对方重发', () {
      final view = transferErrorView(
          StateError('连接已断开，接收已中断。'), TransferErrorScene.receiving);
      expect(view.title, '传输中断了');
      expect(view.suggestion, contains('对方重新发送'));
    });

    test('发送 op 的 idle timeout 是传输中断，不是配对闲置（codex 裁决）', () {
      final view = transferErrorView(
        _FakeAkFfiException(
            2, 'idle timeout: no frame within 10s (checkpoint kept)'),
        TransferErrorScene.sending,
      );
      expect(view.kind, TransferErrorKind.interrupted);
      expect(view.title, '传输中断了');
      expect(view.suggestion, contains('断点续传'));
      // 不是配对闲置文案。
      expect(view.title, isNot(contains('闲置')));
    });

    test('闲置过期与连接中断的文案分开（V-N4 的核心）', () {
      final idle = transferErrorView(
        _FakeAkFfiException(2, 'pairing ended: idle_expired'),
        TransferErrorScene.pairing,
      );
      final lost = transferErrorView(
          _v2dConnectionLost, TransferErrorScene.pairing);
      expect(idle.title, '配对已闲置过久');
      expect(idle.kind, TransferErrorKind.idleExpired);
      expect(lost.title, '连接已中断');
      expect(lost.kind, TransferErrorKind.interrupted);
      // 互相不含对方的词。
      expect(idle.title, isNot(contains('中断')));
      expect(lost.title, isNot(contains('闲置')));
    });

    test('对方拒绝 (sending)', () {
      final view = transferErrorView(
          StateError('对方拒绝接收文件（DECLINED）'),
          TransferErrorScene.sending);
      expect(view.title, '对方没有接受');
      expect(view.suggestion, contains('重新发送'));
    });

    test('W-N10：发送场景的 STORAGE_FULL 真实文本（预检与传输中途各一句）'
        '→ 对方的设备空间不足', () {
      for (final error in [
        _peerStorageFullPrecheck,
        _peerStorageFullMidTransfer,
      ]) {
        final view = transferErrorView(error, TransferErrorScene.sending);
        expect(view.kind, TransferErrorKind.saveDirNotWritable,
            reason: '$error');
        expect(view.title, '对方的设备空间不足', reason: '$error');
        expect(view.reason, contains('剩余空间不够'), reason: '$error');
        expect(view.suggestion, contains('清理空间'), reason: '$error');
        expect(view.suggestion, contains('重新配对'), reason: '$error');
        expect(view.suggestion, contains('断点续传'), reason: '$error');
        // 发送场景不得套用接收场景的“更换保存文件夹”。
        expect(view.title, isNot('无法保存到所选文件夹'), reason: '$error');
        expect(view.suggestion, isNot(contains('更换保存文件夹')),
            reason: '$error');
      }
    });

    test('W-N10：接收场景维持“无法保存到所选文件夹……更换保存文件夹”', () {
      // 接收方本地引擎的 Display（lib.rs:1046）。
      final view =
          transferErrorView(_localStorageFull, TransferErrorScene.receiving);
      expect(view.kind, TransferErrorKind.saveDirNotWritable);
      expect(view.title, '无法保存到所选文件夹');
      expect(view.suggestion, contains('更换保存文件夹'));
      // 同一句引擎文本在发送场景给发送文案——文案随场景分开，不随文本变。
      final peer = transferErrorView(
          _peerStorageFullPrecheck, TransferErrorScene.receiving);
      expect(peer.title, '无法保存到所选文件夹');
    });

    test('未知错误兜底 → 出了点问题 + 导出连接诊断（传输场景）', () {
      final view = transferErrorView(
          StateError('身份认证失败'), TransferErrorScene.sending);
      expect(view.title, unknownErrorTitle);
      expect(view.title, '出了点问题');
      expect(view.suggestion, '重试；仍不行请导出连接诊断。');
      expect(view.detail, contains('身份认证失败'));
    });

    test('未知错误兜底（导出诊断场景）→ 展开详情复制错误信息', () {
      final view = transferErrorView(
          StateError('disk full'), TransferErrorScene.diagnosticsExport);
      expect(view.title, '出了点问题');
      expect(view.suggestion, contains('详情'));
      expect(view.suggestion, contains('复制'));
      // 重启会清空进程内诊断缓冲，不得再建议重启；也不建议再导出诊断。
      expect(view.suggestion, isNot(contains('重启')));
      expect(view.suggestion, isNot(contains('导出连接诊断')));
    });

    test('长时间无进展 (sending) → 断点续传', () {
      final view = transferErrorView(
        TimeoutException('发送超时（150 秒无进展）'),
        TransferErrorScene.sending,
      );
      expect(view.kind, TransferErrorKind.stalled);
      expect(view.title, '传输长时间没有进展');
      expect(view.suggestion, contains('断点续传'));
    });
  });

  group('detail line carries the ORIGINAL text', () {
    test('String 错误的原文就是它自己', () {
      expect(detailTextOf('任务码无效或已被使用，请确认后重试。'),
          '任务码无效或已被使用，请确认后重试。');
    });

    test('异常的原文含错误码（toString）', () {
      expect(detailTextOf(const _FakeAkFfiException(2, 'boom')),
          'AkFfiException(AK_FAILED 2): boom');
    });

    test('message 读取：无 message 成员时为空串、code 为 null', () {
      expect(messageTextOf(_OpaqueError()), '');
      expect(errorCodeOf(_OpaqueError()), isNull);
      expect(errorCodeOf('plain string'), isNull);
      // StateError CARRIES a message member — the mapping reads it.
      expect(messageTextOf(StateError('身份认证失败')), '身份认证失败');
    });
  });
}
