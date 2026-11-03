//! C ABI over `ak-engine` for future UI frontends (Flutter via FFI, or any
//! native embedding). Contract draft (frozen by the ak-ffi agent):
//!
//! - Handle-based, no globals: `ak_engine_new` owns an embedded tokio
//!   runtime; every other call takes the handle.
//! - All strings cross the boundary as UTF-8 `*const c_char` in, and
//!   caller-provided buffers out (with capacity); no allocation is handed
//!   to the caller that the caller must free except via explicit APIs.
//! - Progress is pulled (poll) as small JSON lines, so any language loop
//!   can render it; errors are status codes plus a message buffer.
//! - `unsafe` exists ONLY in this crate's shim, each block carrying a
//!   SAFETY comment; all logic lives in safe functions below the shim.
//!
//! # Repository unsafe-code exception
//!
//! Every other crate in this repository is `#![forbid(unsafe_code)]`; this
//! crate is the single, deliberate exception, because a C ABI is exactly
//! the place where raw pointers are the interface. The exception is
//! contained: [`shim`] holds all `unsafe` blocks (pointer dereference,
//! `CStr` reads, buffer writes — each with a `SAFETY:` comment covering
//! non-nullness, lifetime, and UTF-8 validation), while [`core`] is
//! compiled with `#![forbid(unsafe_code)]` and owns every line of logic:
//! the engine handle and job table, JSON parsing/serialization, spec
//! construction, and progress/report rendering. Panics never cross the
//! boundary: each entry point wraps its body in a catch-all that turns a
//! panic into an `AK_ERR_INTERNAL` status plus message.
//!
//! # Status codes
//!
//! | code | meaning |
//! |------|---------|
//! | `0`  | ok (`ak_poll_progress`: no event pending; `ak_job_result`: still running) |
//! | `1`  | `ak_poll_progress`: one event written / `ak_job_result`: finished ok / `ak_identity_info` + `ak_jobs_list`: JSON written |
//! | `2`  | `ak_job_result`: finished with an error (error JSON written) |
//! | `-1` | `AK_ERR_NULL`: a required pointer argument was null |
//! | `-2` | `AK_ERR_INVALID`: malformed JSON, wrong field type, failed validation |
//! | `-3` | `AK_ERR_BUFFER`: caller buffer too small — nothing was consumed, retry with a larger cap (the last-error message names the needed size) |
//! | `-4` | `AK_ERR_NOT_FOUND`: unknown job id |
//! | `-5` | `AK_ERR_INTERNAL`: lock poisoning, runtime failure, panic |
//!
//! # spec JSON schemas
//!
//! Send (`ak_send_start`), unknown fields ignored:
//!
//! ```json
//! {
//!   "source_roots": ["C:/pick/dir", "C:/pick/file.txt"],
//!   "peer": "127.0.0.1:7100",
//!   "fp_hex": "<receiver fingerprint, 64 hex chars>",
//!   "chunk_kb": 64,          // optional, 1..=8192, default 1024
//!   "multistream": false,    // optional, needs peer capability
//!   "no_internet": false,    // optional policy gate (loopback/LAN unaffected)
//!   "timeout_secs": 30,      // optional idle budget, 1..=86400
//!   "conflict": "fail",      // optional: "fail" | "rename"
//!   "device_name": "laptop", // optional, <= 64 chars
//!   "id_dir": "C:/ak/id"     // optional; <id_dir>/device.key, else ephemeral
//! }
//! ```
//!
//! With an `id_dir` the job persists its checkpoint record under
//! `<id_dir>/checkpoints` (the ak-cli convention, visible to
//! `ak_jobs_list`); without one the record goes to an ephemeral per-job
//! temp directory and nothing persists.
//!
//! Receive (`ak_receive_start`), unknown fields ignored:
//!
//! ```json
//! {
//!   "out_dir": "C:/inbox",     // required, created when missing
//!   "port": 7100,              // optional, default 7100
//!   "expect": "any",           // optional: "any" | "book" | 64-hex | 6-digit code
//!   "conflict": "fail",        // optional: "fail" | "rename"
//!   "timeout_secs": 30,        // optional idle budget
//!   "id_dir": "C:/ak/id",      // optional stable identity
//!   "device_name": "phone",    // optional, <= 64 chars
//!   "once_jobs": 1,            // optional, 1..=64; > 1 serves concurrently
//!   "no_internet": false       // optional policy gate
//! }
//! ```
//!
//! `expect` admission policies: `"any"` accepts everyone (dangerous);
//! a 64-hex fingerprint pins one peer; a 6-digit pairing code (with or
//! without the dash) accepts only the device whose code matches; `"book"`
//! accepts only fingerprints listed in `<id_dir>/peers.book` — the book is
//! validated at start (missing, empty, or corrupt → `AK_ERR_INVALID`
//! telling the caller to pair first, exactly like the CLI) and re-read on
//! every connection, so pairing edits apply without restarting a
//! concurrent receive. `"book"` requires a non-empty `id_dir`.
//!
//! Parser policy (hand-written, no serde): unknown fields are ignored for
//! forward compatibility; wrong types are errors; string values never
//! carry control characters — raw or escaped (`\n`, `\u0001`, ...) are
//! both refused, mirroring the wire codec's two-layer injection defense.
//! Inputs are capped at 1 MiB, strings at 4096 bytes, nesting at 32.
//!
//! # progress event lines (`ak_poll_progress`)
//!
//! One compact JSON object per call, lowercase keys, never a raw newline:
//!
//! ```json
//! {"job":"<32-hex job id>","event":"connecting"}
//! {"job":"...","event":"listening","addr":"127.0.0.1:52341"}   // receive, once_jobs=1
//! {"job":"...","event":"connected","peer":"<64-hex fingerprint>"}
//! {"job":"...","event":"offered","files":3,"bytes":123456}
//! {"job":"...","event":"accepted","files":3,"bytes":123456}
//! {"job":"...","event":"manifest_page_sent","page":0}
//! {"job":"...","event":"manifest_received"}
//! {"job":"...","event":"resuming","chunks":12}
//! {"job":"...","event":"file_started","file":0,"path":"photos/img.bin","bytes":65536}
//! {"job":"...","event":"chunk_sent","file":0,"chunk":3}
//! {"job":"...","event":"chunk_received","file":0,"chunk":3}
//! {"job":"...","event":"bytes_acked","file":0,"bytes":65536}
//! {"job":"...","event":"paused"} / {"job":"...","event":"resumed"}
//! {"job":"...","event":"file_verified","file":0}
//! {"job":"...","event":"file_committed","file":0,"path":"photos/img.bin"}
//! {"job":"...","event":"verifying"}
//! {"job":"...","event":"final_ack","verified":true}
//! {"job":"...","event":"done"}
//! ```
//!
//! With `once_jobs > 1` every receive line also carries `"conn":<index>`
//! after `"job"`. The `"job"` field is the opaque FFI job id — correlate
//! it with the id from `ak_*_start`, not with the wire-protocol job id
//! (which appears as `"wire_job"` in the result).
//!
//! # result JSON (`ak_job_result`)
//!
//! Success (status `1`):
//!
//! ```json
//! {"job":"<ffi job id>","wire_job":"<protocol job id>","files":3,"bytes":123456,
//!  "duration_ms":812,"resumed":0,"peer":"<64-hex>","roots":["dir","extra"]}
//! ```
//!
//! Failure (status `2`): `{"job":"...","error":"message"}` (plus
//! `"jobs"/"ok"/"failed"/"errors"` for a concurrent receive).
//!
//! # identity info JSON (`ak_identity_info`)
//!
//! Written on status `1`; the directory and `device.key` are created when
//! missing (`created:true`, exactly once):
//!
//! ```json
//! {"device_id":"<public key, 64 hex>","fingerprint":"<64 hex>",
//!  "pairing_code":"123-456","peers":2,"created":false}
//! ```
//!
//! `peers` counts `<id_dir>/peers.book` entries. A missing book is zero; a
//! CORRUPT book also reports zero while the call still succeeds — the
//! last-error buffer then carries a note, so read it right after the call.
//!
//! # jobs list JSON (`ak_jobs_list`)
//!
//! Written on status `1`: one array element per parsable checkpoint record
//! under `<id_dir>/checkpoints` (sorted by job id). A missing directory is
//! `[]`. The whole array is rendered before any write, so `AK_ERR_BUFFER`
//! is cleanly retryable with a larger cap:
//!
//! ```json
//! [{"id":"<32-hex wire job id>","direction":"send"|"receive",
//!   "state":"completed","files":3,"bytes":123456,
//!   "updated_unix_ms":1730000000000,"peer":"<64 hex or empty>","root":"C:/in"}]
//! ```
//!
//! `id` is the wire job id (the `wire_job` field of result JSONs); `peer`
//! is empty when a record has no peer; `root` is the source root (send) or
//! destination (receive).
//!
//! NOT yet wired to any frontend: verified by Rust-side tests that drive
//! the C ABI end-to-end (real loopback transfers through the boundary).

mod core;
mod shim;

pub use crate::core::{
    AkEngine, AK_DONE, AK_ERR_BUFFER, AK_ERR_INTERNAL, AK_ERR_INVALID, AK_ERR_NOT_FOUND,
    AK_ERR_NULL, AK_EVENT, AK_FAILED, AK_OK, AK_PENDING,
};
pub use crate::shim::{
    ak_engine_free, ak_engine_last_error, ak_engine_new, ak_identity_info, ak_job_cancel,
    ak_job_result, ak_jobs_list, ak_poll_progress, ak_receive_start, ak_send_start, ak_version,
};
