/// Pure-Dart surface over the ak-ffi C ABI (see docs/adr/003 and
/// crates/ak-ffi/src/lib.rs for the frozen contract).
///
/// - `AkEngineDart` / `AkFfiBindings` / spec builders: talk dart:ffi to
///   ak_ffi.dll (or the platform equivalent).
/// - `ProgressEvent` / `TransferReport` / `JobController`: FFI-free model and
///   state, unit-testable without the native library.
///
/// The future Flutter app imports only this library.
library;

export 'src/bindings.dart';
export 'src/engine.dart';
export 'src/ffi_json.dart';
export 'src/identity.dart';
export 'src/peers.dart';
export 'src/pairing_ffi.dart';
export 'src/sched_ffi.dart';
export 'src/model/progress_events.dart';
export 'src/model/report.dart';
export 'src/model/display.dart';
export 'src/model/accept_policy.dart';
export 'src/model/invite.dart';
export 'src/model/pairing_events.dart';
export 'src/model/sched_policy.dart';
export 'src/model/sched_events.dart';
export 'src/state/job_controller.dart';
