/// W3 (V-N4) table-driven unit tests for the error-copy mapping
/// (`lib/ui/service/transfer_errors.dart`): every 审核 §4.3 失败与恢复
/// situation + the unknown fallback, the idle-vs-interrupted separation,
/// and the four output lines (title/reason/suggestion/detail-verbatim).
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

void main() {
  group('classifyTransferError: the audit §4.3 table + fallback', () {
    // (label, error, expected kind) — one row per 失败与恢复 situation,
    // the idle/interrupted pair, and the unknown fallback. NOT const:
    // TimeoutException/StateError have non-const constructors.
    final table = <(String, Object, TransferErrorKind)>[
      (
        '码无效或过期：engine unknown task code',
        _FakeAkFfiException(2, 'unknown task code "AKI1-ABCD-…"'),
        TransferErrorKind.invalidCode,
      ),
      (
        '码无效或过期：service-readable 任务码无效',
        '任务码无效或已被使用，请确认后重试。',
        TransferErrorKind.invalidCode,
      ),
      (
        '码无效或过期：配对码不存在（fake/service 文案）',
        '配对码不存在或已失效，请向对方确认最新配对码。',
        TransferErrorKind.invalidCode,
      ),
      (
        '码无效或过期：FFI -2 AK_ERR_INVALID',
        _FakeAkFfiException(-2, 'join spec validation failed'),
        TransferErrorKind.invalidCode,
      ),
      (
        '两端无法直连：no candidate accepted',
        _FakeAkFfiException(
            2, 'no candidate accepted the connection (10.0.0.9: timeout)'),
        TransferErrorKind.directUnavailable,
      ),
      (
        '两端无法直连：connect failed（连超时也是无法直连）',
        '无法建立直连：连接超时',
        TransferErrorKind.directUnavailable,
      ),
      (
        '传输中断：V2-d 原文（connect 之后 connection lost）',
        _v2dConnectionLost,
        TransferErrorKind.interrupted,
      ),
      (
        '传输中断：peer vanished（checkpoint kept）',
        _FakeAkFfiException(2, 'peer vanished mid-transfer (checkpoint kept)'),
        TransferErrorKind.interrupted,
      ),
      (
        '闲置过期：engine idle_expired',
        _FakeAkFfiException(2, 'pairing ended: idle_expired'),
        TransferErrorKind.idleExpired,
      ),
      (
        '传输中断：EngineError::IdleTimeout（发送/接收 op 的空闲超时是'
        '传输中断，不是配对闲置）',
        _FakeAkFfiException(2, 'idle timeout: no frame within 10s'),
        TransferErrorKind.interrupted,
      ),
      (
        '对方拒绝：DECLINED',
        '对方拒绝接收文件（DECLINED）',
        TransferErrorKind.peerDeclined,
      ),
      (
        '对方拒绝：engine declined by the receiving user',
        _FakeAkFfiException(2, 'declined by the receiving user: job 7'),
        TransferErrorKind.peerDeclined,
      ),
      (
        '保存位置不可写：destination storage full',
        _FakeAkFfiException(
          2,
          'destination storage full: 1024 bytes available, 8192 needed',
        ),
        TransferErrorKind.saveDirNotWritable,
      ),
      (
        '保存位置不可写：磁盘空间不足（服务层文案）',
        '磁盘空间不足（STORAGE_FULL）',
        TransferErrorKind.saveDirNotWritable,
      ),
      (
        '长时间无进展：TimeoutException 类型',
        TimeoutException('发送超时（150 秒无进展）', Duration(seconds: 150)),
        TransferErrorKind.stalled,
      ),
      (
        '长时间无进展：加入超时',
        '加入超时：请确认任务码后重试。',
        TransferErrorKind.stalled,
      ),
      (
        '未知错误兜底：identity failure',
        StateError('身份认证失败'),
        TransferErrorKind.unknown,
      ),
      (
        '未知错误兜底：读权限问题不属于保存位置类',
        StateError('没有文件读取权限（permission denied）'),
        TransferErrorKind.unknown,
      ),
      (
        '未知错误兜底：对端身份被替换不是拒绝',
        StateError('对端身份已被替换，拒绝连接'),
        TransferErrorKind.unknown,
      ),
    ];

    for (final (label, error, expected) in table) {
      test(label, () {
        expect(classifyTransferError(error), expected,
            reason: '输入：$error');
      });
    }
  });

  group('transferErrorView: four output lines per situation', () {
    test('码无效或过期 (pairing) → 审核 §4.3 的标题与建议', () {
      final view = transferErrorView(
        _FakeAkFfiException(2, 'unknown task code "AKI1-XXXX"'),
        TransferErrorScene.pairing,
      );
      expect(view.title, '这个码用不了');
      expect(view.reason, contains('过期'));
      expect(view.suggestion, contains('重新生成'));
      // 详情原文：错误码 + 原文，一字不动。
      expect(view.detail,
          'AkFfiException(AK_FAILED 2): unknown task code "AKI1-XXXX"');
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
      final view = transferErrorView(_v2dConnectionLost,
          TransferErrorScene.sending);
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
      final lost = transferErrorView(_v2dConnectionLost,
          TransferErrorScene.pairing);
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

    test('保存位置不可写 (receiving) → 更改文件夹', () {
      final view = transferErrorView(
        _FakeAkFfiException(
            2, 'destination storage full: 0 bytes available, 9 needed'),
        TransferErrorScene.receiving,
      );
      expect(view.title, '无法保存到所选文件夹');
      expect(view.suggestion, contains('更换保存文件夹'));
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
