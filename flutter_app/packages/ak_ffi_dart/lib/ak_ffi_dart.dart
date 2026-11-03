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
export 'src/model/progress_events.dart';
export 'src/model/report.dart';
export 'src/model/display.dart';
export 'src/model/accept_policy.dart';
export 'src/state/job_controller.dart';
