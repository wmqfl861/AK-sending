/// W7.3: the UI's platform-facts seam. Production reads `dart:io`; widget
/// tests override [androidOverride] to rehearse both answers without a
/// real device (a plain static, reset to null in tearDown).
library;

import 'dart:io' show Platform;

class UiPlatform {
  UiPlatform._();

  /// Test override for the Android fact; null = ask the real platform.
  static bool? androidOverride;

  /// Whether the UI currently runs on Android.
  static bool get isAndroid => androidOverride ?? Platform.isAndroid;
}
