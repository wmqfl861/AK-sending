/// AK-sending app entry (wave-28 v1 极简配对 UI).
///
/// The shell (MaterialApp + 连接/设置 navigation) and all pages live in
/// `lib/ui/**`; this file only constructs the transfer service and hands
/// it over:
///
/// ```text
/// main.dart ─▶ SimpleTransferService (lib/ui/service contract)
///                     ▲ implemented by
///             LocalSimpleTransferService   (lib/services/simple_transfer_fake.dart —
///                                           dev/test fake, current)
///             SimpleTransferService(…)     (agent C's real implementation,
///                                           constructor-injected facade — swap
///                                           it in HERE when it lands)
/// ```
library;

import 'package:flutter/material.dart';

import 'services/simple_transfer_fake.dart';
import 'ui/app.dart' show AkSendingApp;

void main() {
  // Wave-28 switch point (the ONLY place the implementation is chosen):
  // replace with agent C's real `SimpleTransferService(facade)` once it
  // lands; the UI and the tests never change.
  final service = LocalSimpleTransferService();
  runApp(AkSendingApp(service: service));
}
