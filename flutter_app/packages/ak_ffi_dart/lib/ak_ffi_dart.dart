/// Pure-Dart surface over the ak-ffi C ABI (see docs/adr/003 and
/// crates/ak-ffi/src/lib.rs for the frozen contract).
///
/// Layout: bindings/ talks dart:ffi to ak_ffi.dll; model/ and state/ are
/// FFI-free and unit-testable; the future Flutter app imports only this.
library;
