/// W3 (V-N4): the error-copy mapping — a PURE function family (no Flutter
/// imports, so plain `dart test` can drive it) that turns any error object
/// thrown by the transfer stack plus a SCENE into the four copy lines the
/// failure cards render:
///
/// * 标题（结果）— what happened, one short phrase;
/// * 原因 — one sentence, plain language, never blaming the user;
/// * 建议 — one actionable sentence (a verb, not 确定/知道了);
/// * 详情原文 — the untouched original text (`AkFfiException(...)` etc.),
///   shown ONLY inside the expandable 「详情」 with a copy button.
///
/// Classification is honest by ERROR CODE FIRST (ffi-v2 §3: `-2` invalid
/// input, …) and message vocabulary second (the engine's fixed error
/// strings, e.g. `stream failed: connection lost`). Idle expiry
/// (`idle_expired`) and connection interruption (`connection lost` …, plus
/// an op-level `idle timeout: no frame …` — a transfer that stopped
/// carrying frames, not the pairing idle budget) are DISTINCT kinds — V-N4
/// showed the idle copy on a connection-lost failure; the mapping layer
/// now keeps them apart so the caller can display each honestly (W1 owns
/// the engine-side truth).
///
/// Copy follows 审核 §4.3's 失败与恢复 table (five situations + the
/// unknown fallback 「出了点问题」) and §4.5 (对方 not 对端, actions as
/// buttons, no raw text outside 详情).
///
/// Batch X2 tightened three spots (W 复核 §2): W-N9 — the pairing/join
/// invalidCode copy lists EVERY cause the engine's unified refusal covers
/// (typo/expired/revoked/used, owner offline, both sides picking the same
/// direction); W-N10 — the vocabulary knows the engine's real STORAGE_FULL
/// texts (copied from the Rust constants, provenance in the classifier)
/// and the sending scene gets its own copy (the PEER's device is full,
/// re-pair then resume); W-N4 — status -2 claims invalidCode ONLY on the
/// pairing/join scenes, the transfer scenes go through the vocabulary and
/// the fallback.
library;

import 'dart:async' show TimeoutException;

/// Where the failure surfaced — picks the wording nuances (who can act).
enum TransferErrorScene {
  /// The pairing flows (create/join) on the home page. Today the home
  /// page passes THIS one scene for both flows, and an invalidCode-shaped
  /// failure there is a join-flow phenomenon (the create flow's failures
  /// are never code-shaped), so the join copy below also serves it.
  pairing,

  /// The JOIN flow specifically — the user typed someone else's code
  /// (W-N9). Callers that know the flow kind (home's `_FlowFailedView`
  /// already has `isJoin`) can pass this to be precise; every kind's copy
  /// treats it like [pairing] except where a real distinction exists.
  joining,

  /// A send failed on the connection detail page.
  sending,

  /// A receive failed (the banner).
  receiving,

  /// The settings page's 「导出连接诊断」 failed.
  diagnosticsExport,
}

/// The classified cause. Public so callers can branch on it (e.g. the
/// detail page shows the idle copy only for [idleExpired]).
enum TransferErrorKind {
  /// 码无效或过期 — the code is unknown/used-up/expired (`unknown
  /// task code`, the service's own readable variants; `-2` ONLY on the
  /// pairing/join scenes, W-N4).
  invalidCode,

  /// 两端无法直连 — no candidate accepted the connection / connect
  /// failed (the §4.3 direct-unavailable row).
  directUnavailable,

  /// 传输中断 — the connection broke (connection lost / stream failed /
  /// peer vanished / an op-level `idle timeout: no frame …` …). Resume
  /// from the checkpoint on re-send.
  interrupted,

  /// 配对闲置过久 — the engine's pairing idle budget expired (the
  /// `idle_expired` event reason). A LOCAL time-driven terminal, distinct
  /// from [interrupted].
  idleExpired,

  /// 对方拒绝 — the receiving user declined (DECLINED semantics).
  peerDeclined,

  /// 保存位置不可写 — storage full / storage error. Receiving means MY
  /// save folder; sending means the PEER's device is full (W-N10: the
  /// copy differs by scene).
  saveDirNotWritable,

  /// 长时间无进展 — a stall budget ran out (发送/接收/加入超时,
  /// `TimeoutException`).
  stalled,

  /// Anything else — the 「出了点问题」 fallback.
  unknown,
}

/// The four rendered copy lines of one failure.
class TransferErrorView {
  const TransferErrorView({
    required this.kind,
    required this.title,
    required this.reason,
    required this.suggestion,
    required this.detail,
  });

  final TransferErrorKind kind;

  /// 标题：说结果，一句短语。
  final String title;

  /// 原因：一句人话，不责备用户。
  final String reason;

  /// 建议：一句可执行的动作。
  final String suggestion;

  /// 详情原文：错误码与原始消息，只进可展开的「详情」。
  final String detail;
}

/// The fallback title every unknown error shares (审核 §4.3).
const String unknownErrorTitle = '出了点问题';

/// The fallback suggestion (审核 §4.3): retry, then export diagnostics.
const String unknownErrorSuggestion = '重试；仍不行请导出连接诊断。';

/// The idle-expiry status copy the connection detail page shows ONLY for
/// idle-class endings (J2 wording, kept verbatim).
const String idleExpiryTitle = '配对已闲置过久';
const String idleExpirySuggestion = '请重新配对。';

/// The interruption copy (W1/W3.3): a connection that broke — as opposed
/// to an idle expiry.
const String interruptedTitle = '连接已中断';
const String interruptedSuggestion = '重新发送，会从断点续传';

/// The one-line status the connection detail page shows for a dropped
/// (non-idle) connection: the interruption title plus the resume hint.
const String interruptedStatusLine = '连接已中断，重新发送会从断点续传。';

/// Maps [error] (ANY object — `AkFfiException`, `TimeoutException`,
/// `StateError`, a plain service-authored String …) in [scene] to the
/// four copy lines. Pure: same input, same output, no side effects.
TransferErrorView transferErrorView(Object error, TransferErrorScene scene) {
  final kind = classifyTransferError(error, scene);
  return viewForKind(kind, scene, detail: detailTextOf(error));
}

/// The copy table lookup for a known [kind] (the detail line defaults to
/// empty — callers that own an original text pass it in).
TransferErrorView viewForKind(
  TransferErrorKind kind,
  TransferErrorScene scene, {
  String detail = '',
}) {
  final copy = switch (kind) {
    TransferErrorKind.invalidCode => switch (scene) {
        // W-N9（批次 X2）：加入场景的三层文案。引擎的统一拒绝文本
        // `unknown task code (or expired, revoked, owner offline)`
        // （crates/ak-session/src/pairing/task_net.rs:651；同向加入在
        // 提交前得到的也是这个统一应答）出于防枚举不区分原因，文案必须
        // 把可能性列全：输错、过期、已撤销、已使用、对方不在线、双方
        // 同向。`pairing` 与 `joining` 同文案：首页两条配对流共用
        // pairing 场景（见 enum 注释），而发起流的失败不会是码形问题。
        TransferErrorScene.pairing ||
        TransferErrorScene.joining => const (
              '这个码用不了',
              '配对码可能输错、已过期、已撤销或已使用，也可能是对方不在线，'
                  '或双方选了同一个方向。',
              '请让对方保持应用打开并停留在前台，确认一方发送、一方接收，'
                  '再核对配对码。需要时请对方重新生成。',
            ),
        // W-N4（批次 X2）：-2 不再把发送/接收场景带到这里；传输场景里
        // 命中词表的失效码（例如重发时配对已过期）是传输失败——重新
        // 配对，而不是让用户去“输入新的码”。
        _ => const (
            '这个码用不了',
            '配对码已过期或已被使用，本次传输没有完成。',
            '请重新配对后再试。',
          ),
      },
    TransferErrorKind.directUnavailable => const (
          '无法直接连接到对方',
          '两台设备的网络都不允许外部直接连入。连同一个 Wi-Fi 通常就能解决。'
              '文件不会经过服务器中转。',
          '请再试一次；仍不行时查看连接帮助。',
        ),
    TransferErrorKind.interrupted => switch (scene) {
          TransferErrorScene.receiving => const (
            '传输中断了',
            '与对方的连接断开了（例如对方应用在后台太久）。',
            '请让对方重新发送，会从断点继续。',
          ),
          TransferErrorScene.pairing ||
          TransferErrorScene.joining => const (
            interruptedTitle,
            '与对方的连接断开了。',
            '请重新配对；重新发送会从断点续传。',
          ),
          _ => const (
            '传输中断了',
            '连接断开了（例如应用在后台太久）。',
            '重新发送，会从断点续传。',
          ),
        },
    TransferErrorKind.idleExpired => const (
          idleExpiryTitle,
          '配对后长时间没有传输，本次配对已自动结束。',
          idleExpirySuggestion,
        ),
    TransferErrorKind.peerDeclined => const (
          '对方没有接受',
          '对方拒绝了这次传输，文件没有发出。',
          '可与对方确认后重新发送。',
        ),
    TransferErrorKind.saveDirNotWritable => switch (scene) {
        // W-N10（批次 X2）：发送场景的专用文案——空间不足的是对方的
        // 设备（接收方以 STORAGE_FULL 错误帧报告，发送方引擎把它渲染
        // 成 `peer sent error STORAGE_FULL: …`）。连接已随之结束，必须
        // 重新配对再发送；已有进度从断点续传。
        TransferErrorScene.sending => const (
            '对方的设备空间不足',
            '对方设备的剩余空间不够，本次传输没有完成。',
            '请对方清理空间后重新配对再发送，已有进度会从断点续传。',
          ),
        // 接收（与其余场景）：空间不足的是本机的保存文件夹。
        _ => const (
            '无法保存到所选文件夹',
            '保存位置不可写，或剩余空间不够。',
            '请在「设置」里更换保存文件夹后重试。',
          ),
      },
    TransferErrorKind.stalled => switch (scene) {
          TransferErrorScene.receiving => const (
            '接收长时间没有进展',
            '接收过程中长时间没有数据流动，已停止。',
            '请让对方重新发送，会从断点继续。',
          ),
          TransferErrorScene.pairing ||
          TransferErrorScene.joining => const (
            '配对超时',
            '配对长时间没有完成。',
            '请重试。',
          ),
          _ => const (
            '传输长时间没有进展',
            '传输过程中长时间没有数据流动，已停止。',
            '重新发送，会从断点续传。',
          ),
        },
    TransferErrorKind.unknown => (
          unknownErrorTitle,
          '出现了意料之外的错误。',
          // diagnosticsExport: restarting would CLEAR the in-process
          // diagnostics buffer the user is trying to export — point at
          // 「详情」+copy instead of a restart.
          scene == TransferErrorScene.diagnosticsExport
              ? '请再试一次。如果仍无法导出，请展开「详情」并复制错误信息。'
              : unknownErrorSuggestion,
        ),
  };
  return TransferErrorView(
    kind: kind,
    title: copy.$1,
    reason: copy.$2,
    suggestion: copy.$3,
    detail: detail,
  );
}

/// Classifies [error] in [scene] into a [TransferErrorKind]. Error code
/// first (an `AkFfiException`-shaped `code` field), then the message
/// vocabulary (the engine's fixed strings), then the fallback. The scene
/// decides which code facts apply (W-N4: `-2` only claims invalidCode on
/// the pairing/join scenes).
TransferErrorKind classifyTransferError(
    Object error, TransferErrorScene scene) {
  final text = messageTextOf(error).toLowerCase();
  // Error-code facts: the negative FFI statuses (ffi-v2 §3). `-2`
  // (AK_ERR_INVALID) on the pairing path is the used-up/unknown code
  // shape the adapter reports; other negative codes carry their own
  // message vocabulary below.
  final code = errorCodeOf(error);
  if (code != null && code < 0) {
    if (code == -2 &&
        (scene == TransferErrorScene.pairing ||
            scene == TransferErrorScene.joining)) {
      // AK_ERR_INVALID: malformed/invalid input — on the pairing/join
      // scenes that means the task code (spec/JSON validation of the
      // join). W-N4（批次 X2）：发送/接收场景的 -2 是输入拼装/参数
      // 错误，不再是“这个码用不了”——落回下面的词表与兜底。
      return TransferErrorKind.invalidCode;
    }
    // -1/-3/-5 are programming/buffer/internal faults with no user
    // action beyond the fallback; -4 (not found) falls through to the
    // message vocabulary (unknown task code says it better).
  }
  // Idle expiry FIRST: it must never be swallowed by the interruption
  // vocabulary (V-N4) — `idle_expired` (the pairing-idle event reason)
  // and the J2 UI copy. NOTE: `idle timeout: no frame …`
  // (EngineError::IdleTimeout, an op-level idle timeout) is NOT here —
  // during a send/receive op it means the transfer stopped carrying
  // frames, so it belongs to the interruption vocabulary below; the
  // pairing idle budget ends with the `idle_expired` event instead.
  if (text.contains('idle_expired') ||
      text.contains('配对已闲置')) {
    return TransferErrorKind.idleExpired;
  }
  if (text.contains('declined') ||
      text.contains('对方拒绝') ||
      text.contains('peer rejected the offer')) {
    return TransferErrorKind.peerDeclined;
  }
  if (text.contains('unknown task code') ||
      text.contains('任务码无效') ||
      text.contains('配对码不存在') ||
      text.contains('配对码格式不正确') ||
      text.contains('该请求已失效')) {
    return TransferErrorKind.invalidCode;
  }
  if (text.contains('storage full') ||
      // W-N10（批次 X2）：以下三条是引擎真实产生的文本（先在 Rust
      // 里核对过再抄，词表原先认的是 `storage full`（中间空格），这三
      // 句一句都不命中，于是落到兜底“出了点问题”）：
      // - `storage_full` — wire 错误码 ErrorCode::StorageFull 的 Display
      //   （crates/ak-core/src/error.rs:66），发送方引擎把收到的 Error
      //   帧渲染成 `peer sent error STORAGE_FULL: …`
      //   （EngineError::PeerError 的 Display，
      //   crates/ak-engine/src/lib.rs:1036）；
      // - `insufficient free space` — 接收方预检（headroom）不过时
      //   Error 帧的 message（crates/ak-engine/src/receiver.rs:955）；
      // - `ran out of disk space` — 传输中途磁盘写满时 STORAGE_FULL 帧
      //   的固定文本 STORAGE_FULL_MESSAGE
      //   （crates/ak-engine/src/recv_single.rs:18）。
      // `storage full`（空格）则盖住接收方本地的
      // `destination storage full: … bytes available, … needed`
      // （EngineError::StorageFull 的 Display，lib.rs:1046）。
      text.contains('storage_full') ||
      text.contains('insufficient free space') ||
      text.contains('ran out of disk space') ||
      text.contains('storage error') ||
      text.contains('磁盘空间不足') ||
      text.contains('空间不足') ||
      text.contains('无法保存')) {
    return TransferErrorKind.saveDirNotWritable;
  }
  // Direct-unavailable BEFORE interrupted/stalled: a connect-phase
  // failure that happens to mention a timeout is still 「无法直连」.
  if (text.contains('no candidate accepted') ||
      text.contains('connect failed') ||
      text.contains('无法建立直连') ||
      text.contains('连通测试未通过')) {
    return TransferErrorKind.directUnavailable;
  }
  if (text.contains('connection lost') ||
      text.contains('connection reset') ||
      text.contains('connection closed') ||
      text.contains('broken pipe') ||
      text.contains('stream failed') ||
      text.contains('peer vanished') ||
      // An op-level idle timeout (`idle timeout: no frame within 10s …`):
      // no frames flowing means the transfer was interrupted — in the
      // pairing scene the interrupted copy still says 请重新配对.
      text.contains('idle timeout') ||
      text.contains('连接已断开') ||
      text.contains('接收已中断') ||
      text.contains('连接中断') ||
      text.contains('中断了连接') ||
      text.contains('对端中断')) {
    return TransferErrorKind.interrupted;
  }
  if (error is TimeoutException ||
      text.contains('timed out') ||
      text.contains('timeout') ||
      text.contains('超时') ||
      text.contains('无进展')) {
    return TransferErrorKind.stalled;
  }
  return TransferErrorKind.unknown;
}

/// The int status code when [error] carries one (`AkFfiException.code`);
/// null otherwise. Dynamic by design: the mapping layer must not import
/// the FFI package (pure Dart, `dart test` friendly) while still reading
/// the exception's facts.
int? errorCodeOf(Object error) {
  try {
    final dynamic code = (error as dynamic).code;
    if (code is int) {
      return code;
    }
  } on Object {
    // No `code` member (String errors, StateError, …) — not an error.
  }
  return null;
}

/// The exception's own message when it has one (`AkFfiException.message`);
/// a plain String error IS its message; else the empty string.
String messageTextOf(Object error) {
  if (error is String) {
    return error;
  }
  try {
    final dynamic message = (error as dynamic).message;
    if (message is String) {
      return message;
    }
  } on Object {
    // Fall through to the toString shape.
  }
  return '';
}

/// The 详情原文 line: the ORIGINAL text with its error code —
/// `AkFfiException(AK_FAILED 2): …` already renders both; a plain String
/// error (a service-authored reason) is itself; everything else falls
/// back to `toString()`.
String detailTextOf(Object error) {
  if (error is String) {
    return error;
  }
  return '$error';
}
