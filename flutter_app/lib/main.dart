/// AK-sending Flutter desktop app entry (wave 11).
///
/// The shell (MaterialApp + NavigationRail) and all pages live in
/// `lib/ui/**` (parallel UI agent); this file only constructs the real
/// transfer service and hands it over:
///
/// ```text
/// main.dart ─▶ FfiTransferService (lib/services) ─▶ ui AkSendingApp
/// ```
library;

import 'package:flutter/material.dart';

import 'services/ffi_transfer_service.dart';
import 'ui/app.dart' show AkSendingApp;

void main() {
  // FfiTransferService() never throws: a missing ak_ffi.dll degrades to
  // status=error with the probe candidates on the version card.
  runApp(AkSendingApp(service: FfiTransferService()));
}
