/// Client UI U1 (2026-11-13): pure display helpers for transfer progress
/// and results — throughput and durations. (The direct-path label needs
/// `dart:io`'s address parser and lives in the services layer:
/// `lib/services/direct_path_label.dart`.)
///
/// Everything here is a pure function of its arguments (no clock, no
/// platform calls), so widget and unit tests pin the exact wording.
library;

import 'simple_transfer_service.dart' show formatBytes;

/// Throughput as `3.2 MB/s` (binary units, the [formatBytes] scale).
String formatSpeed(double bytesPerSecond) {
  if (bytesPerSecond.isNaN || bytesPerSecond <= 0) {
    return '0 B/s';
  }
  return '${formatBytes(bytesPerSecond.round())}/s';
}

/// A short elapsed time: `不到 1 秒`, `45 秒`, `3 分 05 秒`, `1 小时 02 分`.
String formatElapsed(Duration duration) {
  final seconds = duration.inSeconds;
  if (seconds < 1) {
    return '不到 1 秒';
  }
  if (seconds < 60) {
    return '$seconds 秒';
  }
  if (seconds < 3600) {
    final minutes = seconds ~/ 60;
    final rest = (seconds % 60).toString().padLeft(2, '0');
    return '$minutes 分 $rest 秒';
  }
  final hours = seconds ~/ 3600;
  final minutes = ((seconds % 3600) ~/ 60).toString().padLeft(2, '0');
  return '$hours 小时 $minutes 分';
}

/// An approximate time left: `约 8 秒`, `约 3 分钟`, `约 2 小时`. Rounded
/// up so the estimate never promises less than it measured.
String formatRemaining(Duration remaining) {
  final seconds = remaining.inMilliseconds <= 0
      ? 0
      : (remaining.inMilliseconds / 1000).ceil();
  if (seconds < 60) {
    return '约 ${seconds < 1 ? 1 : seconds} 秒';
  }
  if (seconds < 3600) {
    return '约 ${(seconds / 60).ceil()} 分钟';
  }
  return '约 ${(seconds / 3600).ceil()} 小时';
}
