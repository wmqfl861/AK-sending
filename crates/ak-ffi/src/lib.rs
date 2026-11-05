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
//! - `unsafe` exists ONLY in this crate's pointer-shim modules ([`shim`]
//!   and, since wave 23, [`sched_ext`] for the five exports that complete
//!   the frozen v2 surface), each block carrying a SAFETY comment; all
//!   logic lives in safe functions below the shims.
//!
//! # Repository unsafe-code exception
//!
//! Every other crate in this repository is `#![forbid(unsafe_code)]`; this
//! crate is the single, deliberate exception, because a C ABI is exactly
//! the place where raw pointers are the interface. The exception is
//! contained: [`shim`] holds the `unsafe` blocks of the v1 + wave-22
//! exports (pointer dereference, `CStr` reads, buffer writes — each with a
//! `SAFETY:` comment covering non-nullness, lifetime, and UTF-8
//! validation), and [`sched_ext`] (wave 23) holds those of the five
//! ffi-v2 §1 exports that were still missing, under the identical
//! discipline, while [`core`] is
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
//! | `1`  | `ak_poll_progress`: one event written / `ak_job_result`: finished ok / `ak_identity_info`, `ak_jobs_list`, `ak_peer_list`, `ak_peer_add`, `ak_peer_remove`: JSON written |
//! | `2`  | `ak_job_result`: finished with an error (error JSON written) |
//! | `-1` | `AK_ERR_NULL`: a required pointer argument was null |
//! | `-2` | `AK_ERR_INVALID`: malformed JSON, wrong field type, failed validation |
//! | `-3` | `AK_ERR_BUFFER`: caller buffer too small — nothing was consumed, retry with a larger cap (the last-error message names the needed size) |
//! | `-4` | `AK_ERR_NOT_FOUND`: unknown job id / paired fingerprint |
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
//!   "expect": "any",           // REQUIRED: "any" | "book" | 64-hex | 6-digit code
//!   "conflict": "fail",        // optional: "fail" | "rename"
//!   "timeout_secs": 30,        // optional idle budget
//!   "id_dir": "C:/ak/id",      // optional stable identity
//!   "device_name": "phone",    // optional, <= 64 chars
//!   "once_jobs": 1,            // optional, 1..=64; > 1 serves concurrently
//!   "no_internet": false       // optional policy gate
//! }
//! ```
//!
//! `expect` is REQUIRED (breaking change in this revision): a receive
//! spec without it is rejected with `AK_ERR_INVALID` and the message
//! `expect required: any|book|fingerprint|pairing code`. There is no
//! default — `"any"` accepts everyone and must be stated explicitly.
//! Admission policies: `"any"` accepts everyone (dangerous);
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
//! carry control characters — C0, DEL, or C1 (`\n`, `\u0001`, `\u007f`,
//! `\u009b`, ...), raw or escaped, are all refused, mirroring the wire
//! codec's display-string rule exactly. Inputs are capped at 1 MiB,
//! strings at 4096 bytes, nesting at 32.
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
//! # peer book management JSON (`ak_peer_list` / `ak_peer_add` / `ak_peer_remove`)
//!
//! The paired-peer book at `<id_dir>/peers.book` — the same file the CLI's
//! `pair` subcommands and the `expect: "book"` admission policy use — is
//! managed through three calls. `engine` is reserved on all three (like
//! `ak_identity_info`); they answer from disk, never from engine state.
//!
//! `ak_peer_list(e, id_dir, buf, cap)` writes on status `1`: one element
//! per paired peer, sorted by fingerprint. A missing book is `[]`; a
//! corrupt book is `AK_ERR_INVALID` with a "cannot load peer book"
//! message; reads never create anything on disk. Names are reported as
//! stored (a peer the CLI paired without a name has `""`; this API's own
//! add stores "(unnamed)", see below):
//!
//! ```json
//! [{"fingerprint":"<64 hex>","name":"laptop","paired_unix_ms":1730000000000}]
//! ```
//!
//! `ak_peer_add(e, id_dir, fingerprint_hex, name, buf, cap)` pairs one
//! peer (or renames it) and writes `{"peers":N}` with the new peer count
//! on status `1`. Semantics aligned with the CLI's TOFU `pair add`:
//!
//! - `fingerprint_hex` must be exactly 64 hex characters (else `-2`);
//! - `name` is the ABI's one OPTIONAL string argument: null or empty means
//!   "no name given". A NEW peer is then stored as "(unnamed)". Re-adding
//!   an existing fingerprint with a name renames it but KEEPS the original
//!   `paired_unix_ms`; re-adding without a name leaves the record
//!   untouched. The call is therefore idempotent;
//! - names must not contain control characters and are capped at 4096
//!   bytes (the ABI-wide string cap), else `-2` (rejected, never
//!   sanitized);
//! - the book is saved atomically (temp file + rename); a save failure is
//!   `-5`.
//!
//! `ak_peer_remove(e, id_dir, fingerprint_hex, buf, cap)` deletes one
//! paired peer and writes `{"peers":N}`. A fingerprint not in the book
//! (a missing book has no peers, so it is the same case) is
//! `AK_ERR_NOT_FOUND` (`-4`) with a hint in the last-error buffer; a bad
//! fingerprint is `-2` like on add; the save rules match add.
//!
//! On both writers an `AK_ERR_BUFFER` (`-3`) is raised BEFORE anything is
//! persisted — nothing was consumed, retry with a larger cap (the
//! last-error message names the needed size); a null `buf` is `-1`, also
//! checked before any write to disk.
//!
//! # B8 (wave 22): pairing orchestration + scheduler (`ak_pairing_*` / `ak_sched_*`)
//!
//! Contract: `docs/contracts/ffi-v2.md` (C ABI, status codes, `-3`/`-4`
//! semantics, idempotency). One `AkEngine` hosts any number of pairing
//! runtimes (`ak_pairing_start`, keyed by 16-byte `runtime_id`) and at
//! most one scheduler runtime (`ak_sched_start`) that shares the most
//! recent pairing runtime's control client. Every operation is
//! asynchronous: the `*_start` calls only *accept* work (no network wait
//! in a synchronous FFI call) and results arrive by polling.
//!
//! Event/result JSON field names below are the B8 schema fill-in ffi-v2
//! §5 reserves for this implementation ("事件字段在 B8 与 Dart 绑定实现时必须
//! 先回本契约补档") — the contract document still needs the integrator's
//! sign-off before it can be edited (its freeze note requires that), so
//! this header is the normative rendering of this build.
//!
//! `ak_pairing_start` spec (unknown fields ignored):
//!
//! ```json
//! {
//!   "spec_version": 1,
//!   "registry": "127.0.0.1:7101",        // required ip:port of the v2 registry
//!   "registry_id": "<64 hex>",           // required: pinned raw registry public key
//!   "id_dir": "C:/ak/id",                // required: device.key / peers.book / pairing/
//!   "device_name": "phone",              // optional, <= 64 chars
//!   "owner_loop": true,                  // optional (default): answer redeems of my codes
//!   "redeem_window_ms": 90000            // optional, 1000..=119000
//! }
//! ```
//!
//! Returns `AK_OK` + `runtime_id` immediately; the TLS connect completes
//! in the background and surfaces as a `runtime_ready` (or
//! `runtime_failed`) pairing event.
//!
//! Op specs (all parsed and validated BEFORE an op id exists — a `-2`
//! never spawns anything; `request_id` everywhere is the optional 32-hex
//! idempotency key of ffi-v2 §4):
//!
//! ```json
//! // ak_pairing_create_start
//! {"spec_version":1,"request_id":"<32hex>","ttl_secs":0}
//! // ak_pairing_redeem_start
//! {"spec_version":1,"request_id":"<32hex>","invite_code":"AKI1-XXXX-..."}
//! // ak_pairing_list_start
//! {"spec_version":1,"cursor":"<32hex>","limit":32,"state_filter":"active"}
//! // ak_pairing_revoke_start
//! {"spec_version":1,"request_id":"<32hex>","code_id":"<32hex>"}
//! ```
//!
//! `ak_pairing_op_result` (`1` done JSON / `0` running / `2` failed JSON /
//! `-3` buffer — the stored result is NOT consumed, so a create retry
//! re-reads the SAME code, never generates a second one / `-4` unknown
//! runtime or op):
//!
//! ```json
//! {"op":"create","code":"AKI1-...","hint":"AKI1-****-...","code_id":"<32hex>","expires_ms":0,"revision":1}
//! {"op":"redeem","pairing_id":"<32hex>","code_id":"<32hex>","peer":"<64hex>","peer_device":"<64hex>","book":"C:/ak/id/peers.book"}
//! {"op":"list","next_cursor":"<32hex>","entries":[{"code_id":"<32hex>","state":"active","created_ms":1,"expires_ms":0,"revision":1}]}
//! {"op":"revoke","code_id":"<32hex>","revision":2,"revoked_ms":1730000000000}
//! // failures: {"op":"<name>","error":"message"}
//! ```
//!
//! A redeem op drives the full transcript (challenge → proofs → commit →
//! `peers.book` apply → `PairReady`); on success the peer is durably in
//! the trust book — the file is readable directly or via `ak_peer_list`.
//!
//! `ak_pairing_poll_event` (`1` event / `0` none / `-3` buffer — the event
//! is pushed back, nothing is lost / `-4` unknown runtime). Pairing-plane
//! events (schema B8):
//!
//! ```json
//! {"event":"runtime_ready","runtime":"<32hex>"}
//! {"event":"runtime_failed","runtime":"<32hex>","error":"message"}
//! {"event":"op_done","op_id":"<32hex>","op":"create"}
//! {"event":"op_failed","op_id":"<32hex>","op":"redeem"}
//! ```
//!
//! `ak_pairing_cancel_op` aborts a running op (its result becomes
//! `{"op":"...","error":"cancelled by caller"}`); a finished op is
//! untouched; unknown ids are `-4`.
//!
//! `ak_sched_start` spec (binds to the most recent pairing runtime unless
//! `runtime_id` names one; the runtime must be connected — poll
//! `runtime_ready` first; one scheduler per engine):
//!
//! ```json
//! {
//!   "spec_version": 1,
//!   "runtime_id": "<32hex>",             // optional
//!   "same_code": "serial",               // optional: serial|parallel (default serial)
//!   "cross_code": "parallel",            // optional: serial|parallel (default parallel)
//!   "max_active_tasks_per_code": 4,      // optional, contract ranges
//!   "max_active_code_groups": 4,         // optional
//!   "max_queued_tasks": 256              // optional
//! }
//! ```
//!
//! `ak_sched_enqueue` spec — the pairing relation (code group + verified
//! peer key) resolves from the local pairing journal; the caller cannot
//! override either (ffi-v2 §4). The data endpoint of the paired peer is
//! the caller's to reach in this revision:
//!
//! ```json
//! {
//!   "spec_version": 1,
//!   "request_id": "<32hex>",
//!   "pairing_id": "<32hex>",             // required: applied pairing in this runtime's journal
//!   "source_roots": ["C:/send/file.bin"],
//!   "peer": "127.0.0.1:7100",            // required: the paired peer's data endpoint
//!   "conflict": "fail",                  // optional: "fail" | "rename"
//!   "multistream": false,                // optional
//!   "no_internet": false,                // optional policy gate
//!   "timeout_secs": 30                   // optional idle budget
//! }
//! ```
//!
//! Admitted tasks run REAL `ak-engine` transfers (per-task progress also
//! flows into `ak_poll_progress` with the task id as the `job` field);
//! the wire-level `TaskOpen`/`TaskAdmission` handshake of the on-channel
//! executor is the documented next step (see `src/pairing.rs`).
//!
//! `ak_sched_snapshot` (`1` JSON / `-3` buffer / `-4` no scheduler):
//!
//! ```json
//! {"policy":{"same_code":"serial","cross_code":"parallel","max_active_tasks_per_code":4,
//!   "max_active_code_groups":4,"max_queued_tasks":256},
//!  "policy_revision":1,"draining":false,"active_tasks":2,"queued_tasks":1,
//!  "shutting_down":false,"listener":{"enabled":false,"running":false,
//!   "inbound_submitted":0,"inbound_rejected_queue_full":0},
//!  "tasks":[{"id":"<32hex>","state":"Transferring","group":"<32hex code>",
//!   "registry":"<64hex>","direction":"send","label":"file.bin","attempts":1,
//!   "last_error":null,"cancel_requested":false,"parked":null}]}
//! ```
//!
//! `ak_sched_poll_event` (`1` / `0` / `-3` push-back / `-4`) — transfer-
//! plane events, polled separately from pairing events (ffi-v2 §5):
//!
//! ```json
//! {"event":"task_queued","task":"<32hex>","group":"<32hex>"}
//! {"event":"task_admitted","task":"<32hex>","group":"<32hex>"}
//! {"event":"task_started","task":"<32hex>"}
//! {"event":"task_finished","task":"<32hex>","group":"<32hex>","outcome":"completed"}
//! {"event":"task_cancelled","task":"<32hex>","group":"<32hex>","was_running":false}
//! {"event":"policy_changed","revision":2,"draining":false}
//! {"event":"draining","active_tasks":3}
//! {"event":"backoff_scheduled","task":"<32hex>","attempt":1,"until_ms":1050}
//! {"event":"retrying","task":"<32hex>","attempt":2}
//! {"event":"task_parked","task":"<32hex>","reason":"retry_exhausted"}
//! {"event":"transport_disconnected"}
//! {"event":"transport_reconnected","replayed":0}
//! {"event":"redeem_offered","code_id":"<32hex>","pairing_id":"<32hex>","peer":"<64hex>"}
//! {"event":"pair_aborted","pairing_id":"<32hex>"}
//! {"event":"shutdown_started","mode":"cancel"}
//! {"event":"shutdown_complete","completed":1,"cancelled":0,"timed_out":false}
//! ```
//!
//! `ak_sched_set_policy` takes the same policy fields as
//! `ak_sched_start` (`AK_OK` / `-2` / `-4`). `ak_sched_cancel` takes the
//! 16-byte task id (`AK_OK`; `-4` unknown or already-terminal task).
//!
//! # B8 (wave 23): the five exports completing the frozen 18-symbol surface
//!
//! `sched_ext` adds the ffi-v2 §1 entries wave 22 left missing, under the
//! frozen contract signatures — these KEEP the `runtime_id` parameter the
//! wave-22 `ak_sched_*` exports dropped; on every sched call it must name
//! the pairing runtime the scheduler is BOUND to (else `-4`, never a
//! silent rebind):
//!
//! * `ak_pairing_stop_start(e, runtime_id, op_id_out)` — REAL: stops the
//!   runtime (op/owner-loop aborts, spawned best-effort client close),
//!   flips its phase to a terminal "stopped by caller", emits
//!   `{"event":"runtime_stopped","runtime":"<32hex>"}`, clears the
//!   engine's active-runtime pointer when it named this runtime, and
//!   records the immediately-finished op
//!   `{"op":"stop","runtime":"<32hex>"}` (readable via
//!   `ak_pairing_op_result`; later ops on the runtime fail with
//!   "stopped by caller").
//! * `ak_sched_listen_start(e, runtime_id, spec_json, op_id_out)` — REAL
//!   delegation: the contract's name for the scheduler start. Same policy
//!   spec as `ak_sched_start`; the caller's `runtime_id` is injected when
//!   the spec names none, and a spec naming a different id is `-2`. The
//!   op resolves immediately to
//!   `{"op":"listen_start","runtime":"<32hex>","listener_enabled":false}`
//!   — the resident inbound listener is the documented wave-22 scope cut
//!   (`listener_enabled` forced off until the on-channel receive
//!   executor lands) and is reported, not hidden.
//! * `ak_sched_retry(e, runtime_id, task_id)` — REAL: the session
//!   runtime's parked-task restart (`AK_OK`; `-4` unknown/unbound runtime,
//!   unknown task, or a task that is not parked).
//! * `ak_sched_task_result(e, runtime_id, task_id, buf, cap)` — REAL:
//!   writes the single task's view — the same object shape as the
//!   `ak_sched_snapshot` `tasks[]` entries, terminal states included
//!   (ffi-v2 §5's terminal-state recovery channel):
//!
//! ```json
//! {"id":"<32hex>","state":"Transferring","group":"<32hex code>",
//!  "registry":"<64hex>","direction":"send","label":"file.bin","attempts":1,
//!  "last_error":null,"cancel_requested":false,"parked":null}
//! ```
//!
//! `AK_DONE` when written / `-3` buffer (lossless retry) / `-4` unknown
//! runtime binding or task.
//! * `ak_sched_shutdown_start(e, runtime_id, spec_json, op_id_out)` —
//!   REAL: accepts the shutdown immediately (op id minted, no drain wait
//!   in the synchronous call) and drives it on the embedded runtime. Spec:
//!
//! ```json
//! {"spec_version":1,"mode":"cancel"}   // "drain" | "cancel", default cancel
//! ```
//!
//! The op resolves via `ak_pairing_op_result` to
//! `{"op":"shutdown","mode":"cancel","completed":N,"cancelled":N,
//! "parked":N,"timed_out":false,"listener_stopped":false}` (or
//! `{"op":"shutdown","error":"..."}`); the frozen
//! `shutdown_started`/`shutdown_complete` events keep flowing through
//! `ak_sched_poll_event`. Known limitation: the engine keeps its one
//! scheduler slot for its lifetime — after `shutdown_complete` a restart
//! needs a fresh engine handle.
//!
//! NOT yet wired to any frontend: verified by Rust-side tests that drive
//! the C ABI end-to-end (real loopback transfers through the boundary).

mod core;
mod pairing;
mod sched_ext;
mod shim;

pub use crate::core::{
    AkEngine, AK_DONE, AK_ERR_BUFFER, AK_ERR_INTERNAL, AK_ERR_INVALID, AK_ERR_NOT_FOUND,
    AK_ERR_NULL, AK_EVENT, AK_FAILED, AK_OK, AK_PENDING,
};
pub use crate::sched_ext::{
    ak_pairing_stop_start, ak_sched_listen_start, ak_sched_retry, ak_sched_shutdown_start,
    ak_sched_task_result,
};
pub use crate::shim::{
    ak_engine_free, ak_engine_last_error, ak_engine_new, ak_identity_info, ak_job_cancel,
    ak_job_result, ak_jobs_list, ak_pairing_cancel_op, ak_pairing_create_start,
    ak_pairing_list_start, ak_pairing_op_result, ak_pairing_poll_event, ak_pairing_redeem_start,
    ak_pairing_revoke_start, ak_pairing_start, ak_peer_add, ak_peer_list, ak_peer_remove,
    ak_poll_progress, ak_receive_start, ak_sched_cancel, ak_sched_enqueue, ak_sched_poll_event,
    ak_sched_set_policy, ak_sched_snapshot, ak_sched_start, ak_send_start, ak_version,
};
