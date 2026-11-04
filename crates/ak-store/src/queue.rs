//! Scheduler queue persistence (pairing-code plan B.4 / B.6.1 / B.6.2).
//!
//! Persisted queue entries are what makes a resident daemon's task list
//! survive a restart (B.6.1: default persistence; `--no-persist-queue` is a
//! runtime mode, not a store mode). Each entry carries the task's identity
//! and scheduling channel, never file paths: source/target references stay
//! with the engine in the user's private identity directory and are not
//! duplicated here.
//!
//! **AKREC2 domain isolation** (B.6.2): a task instance is a random,
//! persistent `transfer_id`; the engine derives its checkpoint
//! `record_id` under the new `AKREC2` domain (`transfer_id` plus all
//! existing identity dimensions) and stores the mapping here. Queue files
//! are named `q-<transfer hex>.akjob`: the `q-` prefix keeps them in a
//! distinct name domain from the checkpoint store's `<hex>.akjob` files,
//! so this store may share the checkpoint directory — checkpoint
//! `list()`/migration only accept stems of exactly 32 hex chars and skip
//! `q-` names — though a dedicated subdirectory (e.g. `<data>/queue/`) is
//! the recommended layout and avoids the migration pass counting `q-`
//! files as skipped records.
//!
//! File format (version header `AKQJOB1`, one record per file, LF line
//! endings, fixed field order, nothing after the last line):
//!
//! ```text
//! AKQJOB1
//! task <32 hex>       transfer_id (task instance identity)
//! record <32 hex>     checkpoint record id (AKREC2 domain)
//! code <32 hex | ->   invite code id (scheduling group key)
//! pairing <32 hex | -> pairing transaction the task belongs to
//! peer <64 hex | ->   peer fingerprint (manual transfers may not know it)
//! dir send|receive
//! seq <u64>           enqueue sequence (FIFO order across restarts)
//! policy <u64>        policy revision at enqueue time
//! state queued|active|done|cancelled
//! err <u16 | ->       last stable error code
//! enqueued <unix ms u64>
//! updated <unix ms u64>
//! ```
//!
//! Semantics:
//!
//! - **Atomic updates**: every write goes through the same
//!   temp+fsync+rename discipline as the other stores in this crate.
//! - **Fail-closed listing**: a `q-*.akjob` file that cannot be strictly
//!   parsed — corrupt, truncated, bad hex, a malformed `q-` name, or a
//!   name/content mismatch — makes `list`/`recovery_plan` return `Err`.
//!   Silently dropping a queued task would turn corruption into data loss.
//! - **Idempotent enqueue**: re-enqueueing the same `task_id` with the
//!   same identity returns the stored record without rewriting; any
//!   differing identity field is rejected. A *new* task whose `record_id`
//!   is already claimed by another live (`queued`/`active`) task is
//!   rejected — one live instance per checkpoint record, the AKREC2
//!   isolation rule. Terminal entries never block reuse of their record.
//! - **Single-writer assumption**: `enqueue` assigns `seq` as
//!   `max(existing) + 1`; the plan's scheduler actor (B.4) is the single
//!   writer, so no concurrent-enqueue race exists by construction.
//! - **Recovery** (B.6.1): `recovery_plan` returns non-terminal entries in
//!   FIFO order — previously `queued` entries resume as queued, previously
//!   `active` entries were mid-flight and resume through their checkpoint
//!   (the runtime maps them to its waiting/recover states); terminal
//!   entries are reported separately and are never revived.
//! - **Transitions**: `queued -> active`, `active -> queued` (requeue),
//!   and either to `done`/`cancelled`. `done` and `cancelled` are
//!   terminal; a same-state transition is an idempotent no-op.
//! - **Completion ordering** (PROTOCOL): the engine may only transition to
//!   `done` strictly after final verification and checkpoint commit, the
//!   same rule the checkpoint store documents for its saves.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ak_identity::Fingerprint;

use crate::pairing_journal::{
    atomic_write, hex_decode_fixed, hex_encode, hex_field, hex_field_opt, parse_num, StrictLines,
};
use crate::{invalid_data, now_unix_ms, Direction};

/// Task instance identity: the random, persistent `transfer_id` (B.6.2).
/// Same instance across retries; new instance for "send another copy".
pub type TaskIdBytes = [u8; 16];
/// Checkpoint record the task resumes: the engine's AKREC2-domain
/// `record_id` derived from `transfer_id` + content/peer/direction/root.
pub type RecordIdBytes = [u8; 16];
/// Invite code id; the scheduling group key (one queue group per code).
pub type CodeIdBytes = [u8; 16];
/// Pairing transaction the task was admitted under.
pub type PairingIdBytes = [u8; 16];

/// Persisted lifecycle state of a queued task. Coarse on purpose: the
/// runtime's fine-grained states (waiting-direct, scanning, paused, ...)
/// are in-memory only and re-derived on recovery from these four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// Waiting to be scheduled; never ran or was requeued.
    Queued,
    /// Mid-flight when last persisted; on recovery the runtime resumes it
    /// through its checkpoint, not from scratch.
    Active,
    /// Terminal success; persisted strictly after final verification and
    /// checkpoint commit (PROTOCOL rule).
    Done,
    /// Terminal cancellation; never revived by recovery.
    Cancelled,
}

impl TaskState {
    /// Stable disk word.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Active => "active",
            TaskState::Done => "done",
            TaskState::Cancelled => "cancelled",
        }
    }

    /// Terminal states never transition again.
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskState::Done | TaskState::Cancelled)
    }

    fn parse(word: &str) -> io::Result<Self> {
        match word {
            "queued" => Ok(TaskState::Queued),
            "active" => Ok(TaskState::Active),
            "done" => Ok(TaskState::Done),
            "cancelled" => Ok(TaskState::Cancelled),
            other => Err(invalid_data(format!("bad queue state '{other}'"))),
        }
    }
}

/// Which transitions the store accepts; a same-state transition is an
/// idempotent no-op handled by the caller, not by this predicate.
fn transition_allowed(from: TaskState, to: TaskState) -> bool {
    matches!(
        (from, to),
        (TaskState::Queued, TaskState::Active)
            | (TaskState::Queued, TaskState::Done)
            | (TaskState::Queued, TaskState::Cancelled)
            | (TaskState::Active, TaskState::Queued)
            | (TaskState::Active, TaskState::Done)
            | (TaskState::Active, TaskState::Cancelled)
    )
}

/// Everything needed to enqueue a task. The identity fields
/// (`task_id`/`record_id`/channel) are immutable for the entry's life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueTaskInit {
    pub task_id: TaskIdBytes,
    pub record_id: RecordIdBytes,
    /// Scheduling group key; `None` for manual fingerprint transfers.
    pub code_id: Option<CodeIdBytes>,
    /// Pairing the task was admitted under, once known.
    pub pairing_id: Option<PairingIdBytes>,
    pub peer_fingerprint: Option<Fingerprint>,
    pub direction: Direction,
    /// Policy revision at enqueue time (context; not part of the
    /// idempotency identity — a replay keeps the stored value).
    pub policy_revision: u64,
}

/// One persisted queue entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedTask {
    pub task_id: TaskIdBytes,
    pub record_id: RecordIdBytes,
    pub code_id: Option<CodeIdBytes>,
    pub pairing_id: Option<PairingIdBytes>,
    pub peer_fingerprint: Option<Fingerprint>,
    pub direction: Direction,
    /// Monotonic enqueue order; the FIFO key across restarts.
    pub enqueue_sequence: u64,
    pub policy_revision: u64,
    pub state: TaskState,
    /// Last stable error code (B.6.1 `last_stable_error_code`).
    pub last_error_code: Option<u16>,
    pub enqueued_unix_ms: u64,
    pub updated_unix_ms: u64,
}

impl QueuedTask {
    /// True when `init` carries exactly this entry's identity fields (the
    /// idempotent-replay test; state, sequence, timestamps, error code,
    /// and policy revision are not compared).
    fn matches_init(&self, init: &QueueTaskInit) -> bool {
        self.task_id == init.task_id
            && self.record_id == init.record_id
            && self.code_id == init.code_id
            && self.pairing_id == init.pairing_id
            && self.peer_fingerprint == init.peer_fingerprint
            && self.direction == init.direction
    }
}

/// Restart recovery view (B.6.1): `resume` holds every non-terminal entry
/// in enqueue (FIFO) order — previously `queued` entries resume as queued,
/// previously `active` entries resume through their checkpoint — and
/// `finished` holds terminal entries for history and queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPlan {
    pub resume: Vec<QueuedTask>,
    pub finished: Vec<QueuedTask>,
}

/// Directory of `q-<transfer hex>.akjob` files.
pub struct TaskQueueStore {
    dir: PathBuf,
}

const HEADER: &str = "AKQJOB1";
const EXTENSION: &str = "akjob";
const NAME_PREFIX: &str = "q-";

impl TaskQueueStore {
    /// Open (creating if needed) the queue directory. A dedicated
    /// subdirectory is recommended (see the module docs on sharing the
    /// checkpoint directory).
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    fn path_for(&self, task_id: &TaskIdBytes) -> PathBuf {
        self.dir
            .join(format!("{NAME_PREFIX}{}.{EXTENSION}", hex_encode(task_id)))
    }

    /// Enqueue a new task in `Queued` state, assigning the next enqueue
    /// sequence. Idempotent: the same `task_id` with identical identity
    /// fields returns the stored record unchanged (no rewrite, sequence
    /// kept); differing fields are rejected. A new task cannot claim a
    /// `record_id` already held by another live task (AKREC2 isolation).
    pub fn enqueue(&self, init: &QueueTaskInit) -> io::Result<QueuedTask> {
        if let Some(existing) = self.load(&init.task_id)? {
            if existing.matches_init(init) {
                return Ok(existing);
            }
            return Err(invalid_data(format!(
                "task {} already enqueued with different identity fields",
                hex_encode(&init.task_id)
            )));
        }
        let all = self.list()?;
        let max_seq = all
            .iter()
            .map(|task| task.enqueue_sequence)
            .max()
            .unwrap_or(0);
        for task in &all {
            if task.record_id == init.record_id
                && task.task_id != init.task_id
                && !task.state.is_terminal()
            {
                return Err(invalid_data(format!(
                    "record {} is already claimed by live task {}",
                    hex_encode(&task.record_id),
                    hex_encode(&task.task_id)
                )));
            }
        }
        let now = now_unix_ms();
        let task = QueuedTask {
            task_id: init.task_id,
            record_id: init.record_id,
            code_id: init.code_id,
            pairing_id: init.pairing_id,
            peer_fingerprint: init.peer_fingerprint,
            direction: init.direction,
            enqueue_sequence: max_seq + 1,
            policy_revision: init.policy_revision,
            state: TaskState::Queued,
            last_error_code: None,
            enqueued_unix_ms: now,
            updated_unix_ms: now,
        };
        self.persist(&task)?;
        Ok(task)
    }

    /// Strict load: an absent task is `None`; a present-but-corrupt file
    /// is `Err`.
    pub fn load(&self, task_id: &TaskIdBytes) -> io::Result<Option<QueuedTask>> {
        let path = self.path_for(task_id);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let task = decode_task(&text)?;
        if &task.task_id != task_id {
            return Err(invalid_data("queue task id does not match its file name"));
        }
        Ok(Some(task))
    }

    /// All entries in enqueue (FIFO) order, **fail-closed**: any `q-*.akjob`
    /// file that cannot be strictly parsed — including a malformed `q-`
    /// name — is an error, never a silently dropped task. Checkpoint
    /// files, `.tmp` residues, and unrelated names are ignored.
    pub fn list(&self) -> io::Result<Vec<QueuedTask>> {
        let mut tasks = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            // A non-UTF-8 name cannot start with the ASCII "q-".
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(rest) = name.strip_prefix(NAME_PREFIX) else {
                continue; // not the queue's name domain
            };
            if rest.ends_with(".tmp") {
                continue; // interrupted atomic write
            }
            let stem = rest
                .strip_suffix(&format!(".{EXTENSION}"))
                .ok_or_else(|| invalid_data(format!("malformed queue file name '{name}'")))?;
            let name_id = hex_decode_fixed::<16>(stem)
                .ok_or_else(|| invalid_data(format!("malformed queue file name '{name}'")))?;
            let text = fs::read_to_string(&path)?;
            let task = decode_task(&text)?;
            if task.task_id != name_id {
                return Err(invalid_data(format!(
                    "queue task id does not match its file name '{name}'"
                )));
            }
            tasks.push(task);
        }
        tasks.sort_by_key(|task| (task.enqueue_sequence, task.task_id));
        Ok(tasks)
    }

    /// All entries (any state) referencing one checkpoint `record_id`, in
    /// enqueue order — the restart mapping from a checkpoint record found
    /// on disk back to the task instance that owns it. Empty for an
    /// orphaned or legacy record.
    pub fn find_by_record(&self, record_id: &RecordIdBytes) -> io::Result<Vec<QueuedTask>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|task| &task.record_id == record_id)
            .collect())
    }

    /// Apply a state transition atomically. A same-state transition is an
    /// idempotent no-op returning the stored record (no rewrite). An
    /// illegal transition — anything out of a terminal state, or an
    /// unlisted pair — is rejected and leaves the file untouched. Missing
    /// task: `NotFound`.
    pub fn transition(
        &self,
        task_id: &TaskIdBytes,
        new_state: TaskState,
    ) -> io::Result<QueuedTask> {
        let mut task = self.required(task_id)?;
        if task.state == new_state {
            return Ok(task);
        }
        if !transition_allowed(task.state, new_state) {
            return Err(invalid_data(format!(
                "illegal queue transition {} -> {} for task {}",
                task.state.as_str(),
                new_state.as_str(),
                hex_encode(task_id)
            )));
        }
        task.state = new_state;
        task.updated_unix_ms = now_unix_ms();
        self.persist(&task)?;
        Ok(task)
    }

    /// Record or clear the last stable error code (atomic rewrite).
    /// Setting the value already stored is a no-op. Missing task:
    /// `NotFound`.
    pub fn update_error(&self, task_id: &TaskIdBytes, code: Option<u16>) -> io::Result<QueuedTask> {
        let mut task = self.required(task_id)?;
        if task.last_error_code == code {
            return Ok(task);
        }
        task.last_error_code = code;
        task.updated_unix_ms = now_unix_ms();
        self.persist(&task)?;
        Ok(task)
    }

    /// Restart view; see [`RecoveryPlan`]. Fail-closed like [`list`].
    pub fn recovery_plan(&self) -> io::Result<RecoveryPlan> {
        let mut plan = RecoveryPlan {
            resume: Vec::new(),
            finished: Vec::new(),
        };
        for task in self.list()? {
            if task.state.is_terminal() {
                plan.finished.push(task);
            } else {
                plan.resume.push(task);
            }
        }
        Ok(plan)
    }

    /// Delete one entry (pruning terminal history; the caller's policy).
    /// Returns false if absent.
    pub fn remove(&self, task_id: &TaskIdBytes) -> io::Result<bool> {
        match fs::remove_file(self.path_for(task_id)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn required(&self, task_id: &TaskIdBytes) -> io::Result<QueuedTask> {
        self.load(task_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("queue has no task {}", hex_encode(task_id)),
            )
        })
    }

    /// Encode and atomically replace the entry's file.
    fn persist(&self, task: &QueuedTask) -> io::Result<()> {
        atomic_write(
            &self.dir,
            &format!("{NAME_PREFIX}{}", hex_encode(&task.task_id)),
            EXTENSION,
            encode_task(task).as_bytes(),
        )
    }
}

fn encode_task(task: &QueuedTask) -> String {
    let mut out = String::with_capacity(320);
    out.push_str(HEADER);
    out.push('\n');
    out.push_str("task ");
    out.push_str(&hex_encode(&task.task_id));
    out.push('\n');
    out.push_str("record ");
    out.push_str(&hex_encode(&task.record_id));
    out.push('\n');
    out.push_str("code ");
    push_opt_hex(&mut out, task.code_id.as_ref());
    out.push('\n');
    out.push_str("pairing ");
    push_opt_hex(&mut out, task.pairing_id.as_ref());
    out.push('\n');
    out.push_str("peer ");
    match &task.peer_fingerprint {
        Some(fp) => out.push_str(&hex_encode(fp.as_bytes())),
        None => out.push('-'),
    }
    out.push('\n');
    out.push_str("dir ");
    out.push_str(direction_word(task.direction));
    out.push('\n');
    out.push_str("seq ");
    out.push_str(&task.enqueue_sequence.to_string());
    out.push('\n');
    out.push_str("policy ");
    out.push_str(&task.policy_revision.to_string());
    out.push('\n');
    out.push_str("state ");
    out.push_str(task.state.as_str());
    out.push('\n');
    out.push_str("err ");
    match task.last_error_code {
        Some(code) => out.push_str(&code.to_string()),
        None => out.push('-'),
    }
    out.push('\n');
    out.push_str("enqueued ");
    out.push_str(&task.enqueued_unix_ms.to_string());
    out.push('\n');
    out.push_str("updated ");
    out.push_str(&task.updated_unix_ms.to_string());
    out.push('\n');
    out
}

/// `<hex>` or the `-` placeholder for an optional fixed-width id.
fn push_opt_hex(out: &mut String, value: Option<&[u8; 16]>) {
    match value {
        Some(bytes) => out.push_str(&hex_encode(bytes)),
        None => out.push('-'),
    }
}

fn decode_task(text: &str) -> io::Result<QueuedTask> {
    let mut lines = StrictLines::new(text, HEADER)?;
    let task_id = hex_field::<16>(&mut lines, "task")?;
    let record_id = hex_field::<16>(&mut lines, "record")?;
    let code_id = hex_field_opt::<16>(&mut lines, "code")?;
    let pairing_id = hex_field_opt::<16>(&mut lines, "pairing")?;
    let peer_fingerprint = hex_field_opt::<32>(&mut lines, "peer")?.map(Fingerprint::from_bytes);
    let direction = parse_direction(lines.field("dir")?)?;
    let enqueue_sequence = parse_num(Some(lines.field("seq")?), "enqueue sequence", "seq")?;
    let policy_revision = parse_num(Some(lines.field("policy")?), "policy revision", "policy")?;
    let state = TaskState::parse(lines.field("state")?)?;
    let raw_err = lines.field("err")?;
    let last_error_code = if raw_err == "-" {
        None
    } else {
        Some(parse_num(Some(raw_err), "error code", "err")?)
    };
    let enqueued_unix_ms = parse_num(Some(lines.field("enqueued")?), "enqueued time", "enqueued")?;
    let updated_unix_ms = parse_num(Some(lines.field("updated")?), "updated time", "updated")?;
    lines.finish()?;
    Ok(QueuedTask {
        task_id,
        record_id,
        code_id,
        pairing_id,
        peer_fingerprint,
        direction,
        enqueue_sequence,
        policy_revision,
        state,
        last_error_code,
        enqueued_unix_ms,
        updated_unix_ms,
    })
}

fn parse_direction(word: &str) -> io::Result<Direction> {
    match word {
        "send" => Ok(Direction::Send),
        "receive" => Ok(Direction::Receive),
        other => Err(invalid_data(format!("bad direction '{other}'"))),
    }
}

fn direction_word(direction: Direction) -> &'static str {
    match direction {
        Direction::Send => "send",
        Direction::Receive => "receive",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CheckpointStore, FileCheckpoint, FileCheckpointStore, JobIdBytes, TaskRecord};
    use std::path::Path;

    fn store(dir: &Path) -> TaskQueueStore {
        TaskQueueStore::open(dir).expect("open store")
    }

    fn init(byte: u8) -> QueueTaskInit {
        QueueTaskInit {
            task_id: [byte; 16],
            record_id: [byte + 100; 16],
            code_id: Some([byte + 1; 16]),
            pairing_id: Some([byte + 2; 16]),
            peer_fingerprint: Some(Fingerprint::from_bytes([byte + 3; 32])),
            direction: Direction::Send,
            policy_revision: 7,
        }
    }

    fn task_path(dir: &Path, task_id: &TaskIdBytes) -> PathBuf {
        dir.join(format!("{NAME_PREFIX}{}.{EXTENSION}", hex_encode(task_id)))
    }

    fn file_bytes(dir: &Path, task_id: &TaskIdBytes) -> Vec<u8> {
        fs::read(task_path(dir, task_id)).expect("read task")
    }

    #[test]
    fn enqueue_load_roundtrip_and_file_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        let task = s.enqueue(&init(7)).expect("enqueue");
        assert_eq!(task.state, TaskState::Queued);
        assert_eq!(task.enqueue_sequence, 1);
        assert_eq!(task.last_error_code, None);
        assert_eq!(task.enqueued_unix_ms, task.updated_unix_ms);
        assert!(
            task_path(dir.path(), &[7; 16]).is_file(),
            "q-<hex>.akjob name"
        );
        assert_eq!(s.load(&[7; 16]).expect("load"), Some(task));
    }

    #[test]
    fn enqueue_is_idempotent_and_sequences_are_monotonic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        let first = s.enqueue(&init(3)).expect("enqueue");
        let before = file_bytes(dir.path(), &[3; 16]);
        let replay = s.enqueue(&init(3)).expect("idempotent replay");
        assert_eq!(replay, first);
        assert_eq!(file_bytes(dir.path(), &[3; 16]), before, "no rewrite");
        let second = s.enqueue(&init(4)).expect("enqueue");
        assert_eq!(second.enqueue_sequence, 2);
        // Sequence keeps counting past terminal entries.
        s.transition(&[3; 16], TaskState::Done).expect("done");
        let third = s.enqueue(&init(5)).expect("enqueue");
        assert_eq!(third.enqueue_sequence, 3);
    }

    #[test]
    fn enqueue_conflict_on_identity_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        s.enqueue(&init(9)).expect("enqueue");
        let before = file_bytes(dir.path(), &[9; 16]);
        let mut other = init(9);
        other.record_id = [0xD2; 16];
        assert!(s.enqueue(&other).is_err(), "record_id change");
        let mut other = init(9);
        other.direction = Direction::Receive;
        assert!(s.enqueue(&other).is_err(), "direction change");
        let mut other = init(9);
        other.peer_fingerprint = None;
        assert!(s.enqueue(&other).is_err(), "peer change");
        assert_eq!(file_bytes(dir.path(), &[9; 16]), before, "entry untouched");
    }

    #[test]
    fn record_id_isolation_between_live_tasks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        let a = s.enqueue(&init(11)).expect("enqueue a");
        let mut b = init(12);
        b.record_id = a.record_id; // same checkpoint record, new instance
        assert!(
            s.enqueue(&b).is_err(),
            "a live task holds the record; AKREC2 isolation"
        );
        // After the holder finishes, a new instance may claim the record.
        s.transition(&[11; 16], TaskState::Done).expect("done");
        let b = s.enqueue(&b).expect("enqueue after terminal");
        assert_eq!(b.record_id, a.record_id);
        let holders = s.find_by_record(&a.record_id).expect("find");
        assert_eq!(holders.len(), 2);
        assert_eq!(holders[0].task_id, [11; 16]);
        assert_eq!(holders[0].state, TaskState::Done);
        assert_eq!(holders[1].task_id, [12; 16]);
        assert_eq!(holders[1].state, TaskState::Queued);
    }

    #[test]
    fn transition_matrix_legal_and_terminal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        s.enqueue(&init(14)).expect("enqueue");
        // queued -> active -> queued (requeue) -> done.
        s.transition(&[14; 16], TaskState::Active).expect("start");
        s.transition(&[14; 16], TaskState::Queued).expect("requeue");
        s.transition(&[14; 16], TaskState::Active).expect("start");
        s.transition(&[14; 16], TaskState::Done).expect("done");
        for target in [TaskState::Queued, TaskState::Active, TaskState::Cancelled] {
            assert!(
                s.transition(&[14; 16], target).is_err(),
                "done is terminal, {target:?} rejected"
            );
        }
        // queued -> cancelled directly.
        s.enqueue(&init(15)).expect("enqueue");
        s.transition(&[15; 16], TaskState::Cancelled)
            .expect("cancel queued");
        assert!(
            s.transition(&[15; 16], TaskState::Active).is_err(),
            "cancelled is terminal"
        );
    }

    #[test]
    fn same_state_transition_is_an_idempotent_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        s.enqueue(&init(17)).expect("enqueue");
        s.transition(&[17; 16], TaskState::Active).expect("start");
        let before = file_bytes(dir.path(), &[17; 16]);
        let replay = s
            .transition(&[17; 16], TaskState::Active)
            .expect("idempotent");
        assert_eq!(replay.state, TaskState::Active);
        assert_eq!(file_bytes(dir.path(), &[17; 16]), before, "no rewrite");
    }

    #[test]
    fn transition_missing_task_is_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        let err = s
            .transition(&[77; 16], TaskState::Active)
            .expect_err("missing");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn error_code_is_set_and_cleared_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        s.enqueue(&init(19)).expect("enqueue");
        let with_err = s.update_error(&[19; 16], Some(0x0503)).expect("set error");
        assert_eq!(with_err.last_error_code, Some(0x0503));
        assert!(with_err.updated_unix_ms >= with_err.enqueued_unix_ms);
        let replay = s.update_error(&[19; 16], Some(0x0503)).expect("idempotent");
        assert_eq!(replay.last_error_code, Some(0x0503));
        let cleared = s.update_error(&[19; 16], None).expect("clear");
        assert_eq!(cleared.last_error_code, None);
        assert_eq!(
            s.load(&[19; 16])
                .expect("load")
                .expect("present")
                .last_error_code,
            None
        );
    }

    #[test]
    fn recovery_plan_partitions_and_orders_fifo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        for byte in [21u8, 22, 23, 24] {
            s.enqueue(&init(byte)).expect("enqueue");
        }
        s.transition(&[22; 16], TaskState::Active).expect("active");
        s.transition(&[23; 16], TaskState::Done).expect("done");
        s.transition(&[24; 16], TaskState::Cancelled)
            .expect("cancelled");

        let plan = s.recovery_plan().expect("plan");
        let ids: Vec<TaskIdBytes> = plan.resume.iter().map(|t| t.task_id).collect();
        assert_eq!(ids, vec![[21; 16], [22; 16]], "queued then active, FIFO");
        assert_eq!(plan.resume[1].state, TaskState::Active);
        let done: Vec<TaskIdBytes> = plan.finished.iter().map(|t| t.task_id).collect();
        assert_eq!(done, vec![[23; 16], [24; 16]]);
    }

    #[test]
    fn find_by_record_ignores_foreign_and_tmp_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        s.enqueue(&init(27)).expect("enqueue");
        // A checkpoint-domain file, a queue temp residue, and junk: none
        // of these are queue records and none may fail the listing.
        fs::write(
            dir.path()
                .join(format!("{}.akjob", hex_encode(&[0xAB; 16]))),
            b"AKJOB1\njob xx\n",
        )
        .expect("checkpoint name");
        fs::write(dir.path().join("q-deadbeef.1.2.akjob.tmp"), b"half").expect("tmp");
        fs::write(dir.path().join("unrelated.txt"), b"junk").expect("junk");

        assert_eq!(s.list().expect("list").len(), 1);
        assert_eq!(
            s.find_by_record(&[0xEE; 16]).expect("find"),
            Vec::new(),
            "unknown record maps to no task"
        );
    }

    #[test]
    fn corrupt_records_fail_closed_not_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        s.enqueue(&init(31)).expect("enqueue");
        let original = String::from_utf8(file_bytes(dir.path(), &[31; 16])).expect("utf8");

        let variants: Vec<(&str, String)> = vec![
            (
                "bad header",
                format!(
                    "AKQJOB2\n{}",
                    original.lines().skip(1).collect::<Vec<_>>().join("\n")
                ),
            ),
            ("unknown line", format!("{original}extra junk\n")),
            (
                "missing state line",
                original
                    .lines()
                    .filter(|l| !l.starts_with("state "))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            (
                "bad state word",
                original.replacen("state queued", "state paused", 1),
            ),
            (
                "bad record hex",
                original.replacen(&hex_encode(&[131; 16]), "zz", 1),
            ),
            ("bad error code", original.replacen("err -", "err 99999", 1)),
            (
                "truncated",
                original
                    .split("updated ")
                    .next()
                    .expect("split")
                    .to_string(),
            ),
        ];
        for (what, text) in variants {
            fs::write(task_path(dir.path(), &[31; 16]), text.as_bytes()).expect("write");
            assert!(s.load(&[31; 16]).is_err(), "load must fail for {what}");
            assert!(s.list().is_err(), "list must fail closed for {what}");
            assert!(
                s.recovery_plan().is_err(),
                "recovery fails closed for {what}"
            );
        }

        // Malformed q- names and name/content mismatches fail closed too.
        fs::write(dir.path().join("q-nothex.akjob"), original.as_bytes()).expect("write");
        assert!(s.list().is_err(), "malformed name must fail closed");
        fs::remove_file(dir.path().join("q-nothex.akjob")).expect("remove");
        fs::write(dir.path().join("q-broken.akjob"), b"AKQJOB1\n").expect("write");
        assert!(s.list().is_err(), "unparsable q- name must fail closed");
        fs::remove_file(dir.path().join("q-broken.akjob")).expect("remove");

        fs::write(task_path(dir.path(), &[88; 16]), original.as_bytes()).expect("write");
        assert!(
            s.load(&[88; 16]).is_err(),
            "name/content mismatch must fail closed"
        );
    }

    #[test]
    fn crash_before_rename_keeps_previous_state_visible() {
        // "Changed half" crash during a transition: the active-state text
        // sits in a temp file, the rename never happened.
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        s.enqueue(&init(33)).expect("enqueue");
        let mut staged = s.load(&[33; 16]).expect("load").expect("present");
        staged.state = TaskState::Active;
        staged.updated_unix_ms += 1;
        let tmp = dir.path().join(format!(
            "{NAME_PREFIX}{}.{}.0.{EXTENSION}.tmp",
            hex_encode(&[33; 16]),
            std::process::id()
        ));
        fs::write(&tmp, encode_task(&staged).as_bytes()).expect("write tmp");

        assert_eq!(
            s.load(&[33; 16]).expect("load").expect("present").state,
            TaskState::Queued
        );
        let active = s
            .transition(&[33; 16], TaskState::Active)
            .expect("replay completes");
        assert_eq!(active.state, TaskState::Active);
    }

    #[test]
    fn crash_in_windows_rename_gap_recovers_by_reenqueue() {
        // "Changed half" crash inside rename_replace: the old final file
        // is gone, the new one was never renamed in. The task is absent
        // and enqueueing the same instance again is allowed.
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        let first = s.enqueue(&init(35)).expect("enqueue");
        fs::remove_file(task_path(dir.path(), &[35; 16])).expect("remove final");
        fs::write(dir.path().join("q-residue.akjob.tmp"), b"AKQJOB1").expect("tmp");

        assert_eq!(s.load(&[35; 16]).expect("load"), None);
        let plan = s.recovery_plan().expect("plan");
        assert_eq!(
            plan,
            RecoveryPlan {
                resume: Vec::new(),
                finished: Vec::new()
            }
        );
        let redo = s.enqueue(&init(35)).expect("re-enqueue");
        assert_eq!(redo.task_id, first.task_id);
        assert_eq!(redo.enqueue_sequence, 1, "fresh history, fresh sequence");
    }

    #[test]
    fn shares_a_directory_with_checkpoints_without_interference() {
        let dir = tempfile::tempdir().expect("tempdir");
        let one_dir = dir.path();
        let checkpoints = FileCheckpointStore::open(one_dir).expect("checkpoint store");
        let queue = store(one_dir);

        let mut record = TaskRecord {
            job_id: JobIdBytes::from([0x4C; 16]),
            direction: Direction::Send,
            root: std::path::PathBuf::from(r"D:\data"),
            peer_fingerprint: None,
            chunk_size: 262_144,
            total_files: 0,
            total_bytes: 0,
            state: "in_progress".to_string(),
            created_unix_ms: 1_730_000_000_000,
            updated_unix_ms: 1_730_000_500_000,
            files: Vec::<FileCheckpoint>::new(),
        };
        checkpoints.save(&record).expect("save checkpoint");
        queue.enqueue(&init(37)).expect("enqueue");

        // Each store sees exactly its own records.
        assert_eq!(checkpoints.list().expect("list").len(), 1);
        assert_eq!(queue.list().expect("list").len(), 1);
        record.state = "done".to_string();
        checkpoints.save(&record).expect("save again");
        assert_eq!(queue.list().expect("list").len(), 1);
    }

    #[test]
    fn remove_reports_presence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        s.enqueue(&init(39)).expect("enqueue");
        assert!(s.remove(&[39; 16]).expect("remove"));
        assert!(!s.remove(&[39; 16]).expect("remove again"));
        assert_eq!(s.load(&[39; 16]).expect("load"), None);
    }
}
