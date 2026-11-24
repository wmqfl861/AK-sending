/// Batch Z: the app-side dialogs of the Android folder SEND path, and the
/// copy behind the one that reports a refusal.
///
/// On Android the system folder picker hands out a `content://` tree URI
/// the engine cannot read, so the gateway stages the folder first over
/// `dev.ak/storage` (`pickSendFolder`, then `stageSafTree`) and shows:
///
/// * [SafSendProgressDialog] while the staging runs. It leaves only
///   together with the staging call's answer: 「取消」 — or the system
///   back gesture — asks the platform to stop, and the answer is what
///   proves that nothing is written any more;
/// * [SafSendStagingErrorDialog] when an attempt was refused: ONE sentence
///   from [safSendStagingErrorBody] — the cause, then what to do;
/// * [SafSendFolderUnsupportedDialog] when the build has the folder send
///   switched off.
///
/// Copy per 审核 §4.5: the title states the result, the body gives the
/// cause and what to do, every button is the action it performs.
library;

import 'package:flutter/foundation.dart' show ValueListenable, clampDouble;
import 'package:flutter/material.dart';

import 'simple_transfer_service.dart' show formatBytes;

/// One `stagingProgress` event of the staging the dialog follows.
class SafSendStagingProgress {
  const SafSendStagingProgress({
    required this.copying,
    required this.filesCopied,
    required this.bytesCopied,
    required this.totalBytesEstimate,
  });

  /// Copy phase; false = the tree is still being enumerated.
  final bool copying;

  final int filesCopied;
  final int bytesCopied;

  /// Estimated total bytes, `-1` while unknown: a file whose size the
  /// provider did not report keeps the bar indeterminate.
  final int totalBytesEstimate;
}

/// 「正在准备文件夹」 — a linear bar and one line of text while the
/// platform copies the picked tree.
///
/// The dialog is modal on purpose: the barrier does not dismiss, and the
/// system back gesture means 「取消」. Both leave it up until the staging
/// call has answered — closing it earlier would promise a stop that has
/// not happened yet.
class SafSendProgressDialog extends StatefulWidget {
  const SafSendProgressDialog({
    super.key,
    required this.progress,
    required this.onCancel,
  });

  /// The staging's latest event; null until the first one.
  final ValueListenable<SafSendStagingProgress?> progress;

  /// Asks the platform to stop the staging. Invoked at most once.
  final VoidCallback onCancel;

  @override
  State<SafSendProgressDialog> createState() => _SafSendProgressDialogState();
}

class _SafSendProgressDialogState extends State<SafSendProgressDialog> {
  bool _cancelling = false;

  void _cancel() {
    if (_cancelling) {
      return;
    }
    setState(() => _cancelling = true);
    widget.onCancel();
  }

  @override
  Widget build(BuildContext context) {
    return ValueListenableBuilder<SafSendStagingProgress?>(
      valueListenable: widget.progress,
      builder: (context, progress, _) => PopScope(
        canPop: false,
        onPopInvokedWithResult: (didPop, result) {
          if (!didPop) {
            _cancel(); // the system back gesture means 「取消」
          }
        },
        child: AlertDialog(
          key: const ValueKey('staging_progress_dialog'),
          title: const Text('正在准备文件夹'),
          content: SizedBox(
            width: double.maxFinite,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  _cancelling ? '正在取消…' : _progressText(progress),
                  key: const ValueKey('staging_progress_text'),
                ),
                const SizedBox(height: 16),
                LinearProgressIndicator(value: _barValue(progress)),
              ],
            ),
          ),
          actions: [
            TextButton(
              key: const ValueKey('staging_cancel_button'),
              onPressed: _cancelling ? null : _cancel,
              child: const Text('取消'),
            ),
          ],
        ),
      ),
    );
  }

  static String _progressText(SafSendStagingProgress? progress) {
    if (progress == null || !progress.copying) {
      return '正在读取文件夹内容…';
    }
    return '已复制 ${progress.filesCopied} 个文件 · '
        '${formatBytes(progress.bytesCopied)}';
  }

  /// Determinate only in the copy phase, and only with a positive
  /// estimate — the enumeration has no total to show.
  static double? _barValue(SafSendStagingProgress? progress) {
    if (progress == null ||
        !progress.copying ||
        progress.totalBytesEstimate <= 0) {
      return null;
    }
    return clampDouble(
      progress.bytesCopied / progress.totalBytesEstimate,
      0,
      1,
    );
  }
}

/// 「无法发送此文件夹」 — why the attempt was refused, and whether to
/// start over. Answers true for 「重新选择」, false for 「取消」 or a
/// dismissed barrier.
class SafSendStagingErrorDialog extends StatelessWidget {
  const SafSendStagingErrorDialog({
    super.key,
    required this.code,
    required this.token,
  });

  /// The refusal's `PlatformException` code and reason token — what
  /// [safSendStagingErrorBody] turns into the body.
  final String code;
  final String token;

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      key: const ValueKey('staging_error_dialog'),
      title: const Text(
        '无法发送此文件夹',
        key: ValueKey('staging_error_title'),
      ),
      content: Text(
        safSendStagingErrorBody(code, token),
        key: const ValueKey('staging_error_body'),
      ),
      actions: [
        TextButton(
          key: const ValueKey('staging_error_cancel_button'),
          onPressed: () => Navigator.of(context).pop(false),
          child: const Text('取消'),
        ),
        FilledButton(
          key: const ValueKey('staging_error_retry_button'),
          onPressed: () => Navigator.of(context).pop(true),
          child: const Text('重新选择'),
        ),
      ],
    );
  }
}

/// 「暂时无法发送文件夹」 — the build has the Android folder send
/// switched off. Nothing to answer: the pick simply ends cancelled.
class SafSendFolderUnsupportedDialog extends StatelessWidget {
  const SafSendFolderUnsupportedDialog({super.key});

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('暂时无法发送文件夹'),
      content: const Text(
        '这个版本还不支持直接发送文件夹，请只选择文件。',
        key: ValueKey('folder_send_unsupported_body'),
      ),
      actions: [
        FilledButton(
          key: const ValueKey('folder_send_unsupported_ok_button'),
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('关闭'),
        ),
      ],
    );
  }
}

/// The body of [SafSendStagingErrorDialog]: one sentence — the cause,
/// then what to do — for a refused attempt.
///
/// [code] and [token] are the `PlatformException` of `dev.ak/storage`
/// (fixed values: never a URI, a path or a name):
///
/// | code | tokens | body |
/// |---|---|---|
/// | `SAF_PERMISSION_DENIED` | `no-persisted-grant` `grant-limit` `provider-security` `grant-lookup` | no read permission |
/// | `SAF_PROVIDER_UNTRUSTED` | `authority` | not supported from this location |
/// | `SAF_ENUMERATION_FAILED` | `query` `duplicate-id` `overflow` | cannot read everything inside |
/// | | `too-many-entries` | too many items to send at once |
/// | `SAF_UNSUPPORTED_DOCUMENT` | `empty-root` | the folder is empty |
/// | | `root-name` `virtual-document` `mime` | cannot be sent as-is |
/// | `STAGE_NAME_CONFLICT` | `duplicate` `reserved-stem` `trailing-dot-or-space` `control-char` `separator` `dot-component` `too-long` | names that cannot be sent as-is |
/// | | `receiver-reserved` | a name the receiver reserves |
/// | | `duplicate-with-picked` (this side only) | same name as a picked file |
/// | `STAGE_NO_SPACE` | `no-space` `enospc` | not enough storage on this phone |
/// | `STAGE_IO_FAILED` | `source-changed` `size-mismatch` | the folder changed while it was prepared |
/// | | `leftover` | stopped; temporary files are removed at the next start |
/// | `STAGE_IO_FAILED` `STAGE_INVALID_ID`, anything unknown | any other | preparing the folder failed |
///
/// `STAGE_CANCELLED` is not a refusal and never gets here. A cancelled
/// staging that could not remove everything of itself answers
/// `STAGE_IO_FAILED` / `leftover` instead — so this dialog CAN follow a
/// 「取消」, and says why.
String safSendStagingErrorBody(String code, String token) {
  switch (code) {
    case 'SAF_PERMISSION_DENIED':
      return '没有这个文件夹的读取权限，请重新选择文件夹，并在系统提示中允许访问。';
    case 'SAF_PROVIDER_UNTRUSTED':
      return '不支持发送这个位置的文件夹，请改选手机本地存储里的文件夹，'
          '例如「下载」里的。';
    case 'SAF_ENUMERATION_FAILED':
      return token == 'too-many-entries'
          ? '这个文件夹里的项目太多，一次无法全部发送，'
              '请分成几个较小的文件夹分批发送。'
          : '无法完整读取这个文件夹里的内容，请换一个文件夹，'
              '或先把它复制到手机本地存储再发送。';
    case 'SAF_UNSUPPORTED_DOCUMENT':
      return token == 'empty-root'
          ? '这个文件夹是空的，没有可发送的内容，'
              '请改选一个里面有文件的文件夹。'
          : '这个文件夹无法原样发送，请换一个文件夹，或只选择其中的文件。';
    case 'STAGE_NAME_CONFLICT':
      return switch (token) {
        'receiver-reserved' =>
          '文件夹里有接收方保留的名称（.ak-staging，不区分大小写），'
              '无法原样发送，请给它改名，或只发送其余的文件。',
        'duplicate-with-picked' =>
          '这个文件夹与已选的某个文件同名，请单独发送这个文件夹，'
              '或去掉那个文件。',
        _ =>
          '文件夹里有无法原样发送的名称（重名、过长，或含有电脑不支持的'
              '字符和设备名，或以点、空格结尾），'
              '请先整理这些名称，或只发送其中的文件。',
      };
    case 'STAGE_NO_SPACE':
      return '手机存储空间不足，无法复制这个文件夹，'
          '请清理出足够的空间后重试。';
    case 'STAGE_IO_FAILED':
      return switch (token) {
        // Verified after the copy: a directory that gained or lost a
        // child, or a file that did not have the length it announced.
        'source-changed' || 'size-mismatch' =>
          '准备期间文件夹里的内容有变动，无法按原样发送，'
              '请重新选择这个文件夹。',
        // The staging stopped and its directory could not be removed
        // completely — nothing for the user to do, so it says when.
        'leftover' =>
          '准备已中止，部分临时文件未能删除，'
              '会在下次启动时自动清理。',
        _ => '准备文件夹失败，请重新选择这个文件夹。',
      };
    default:
      return '准备文件夹失败，请重新选择这个文件夹。';
  }
}
