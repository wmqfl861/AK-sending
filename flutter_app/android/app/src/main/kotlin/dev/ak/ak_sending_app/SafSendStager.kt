package dev.ak.ak_sending_app

/*
 * 发送侧 SAF 目录暂存（批次 Z）
 *
 * 引擎只认文件系统路径，而系统选择器交回来的是 SAF 目录树 URI（content://…）。
 * 这里经 ContentResolver 把选中的目录树复制进应用私有存储，保持根目录名、相对
 * 结构、空子目录和零字节文件，再把暂存后的根目录路径交给引擎：
 *
 *   <noBackupFilesDir>/ak-saf-staging/<stageId>/payload/<displayName>/…   暂存后的目录树
 *   <noBackupFilesDir>/ak-saf-staging/<stageId>/.ak-stage-complete        完成标记，最后写
 *   <noBackupFilesDir>/ak-saf-staging/.send-grants                        发送侧自己拿过的读授权（journal）
 *
 * ── 授权：只动发送侧自己加上的那一半，而且要有据可查 ─────────────────────
 *  - pickSendFolder 不持久化任何授权，只借 Activity 的临时授权读出根目录名——
 *    选了却没暂存的选择不会留下任何持久授权；
 *  - stageSafTree 先读授权表，看该 URI 是否已有持久的 READ 授权（同一目录也是接收
 *    保存目录时是 READ+WRITE）：有就原样使用、永不触碰。授权表读不出来时让暂存失败
 *    （grant-lookup）——猜成“没有”就可能把接收侧那份授权的读位拿走再放掉；
 *  - 没有时：先把 URI 记进 journal，再 takePersistableUriPermission(READ)，再复核；
 *    暂存结束（成功/失败/取消）时只释放自己拿的这份，释放得到确认后才从 journal
 *    里划掉（一行不剩时 journal 文件也删掉：它只在有记录时存在）；
 *  - 进程若在 take 与 release 之间被杀，cleanupStaleStaging 只回收“在 journal 里、
 *    READ-only、且不属于任何在跑暂存”的授权；不在 journal 里的一概不碰，带写位的
 *    （接收保存目录）更不碰。
 *
 * ── 线程与取消 ─────────────────────────────────────────────────────────
 * 通道回调在主线程，主线程上不做任何 ContentResolver 调用。所有提供器与授权调用、
 * 暂存根下的每一次增删都在本类唯一的工作线程上串行执行，暂存、取消、清理因此不会
 * 交错。应答（result.success/error）与 stagingProgress 事件一律回主线程，每个
 * MethodChannel.Result 只应答一次。
 * 每个暂存任务自带一个 CancellationSignal，它的查询和打开文件都带着它；cancelStaging
 * 置标志、取消信号、关掉任务打开着的输入流（尽力而为），阻塞中的查询/打开/读取
 * 因此能停下来。
 *
 * ── 复制的保真边界 ─────────────────────────────────────────────────────
 * 所有文件复制完之后、写完成标记之前，每个计划过的目录（含根）都再查一遍，子项
 * document id 集合不同即失败（source-changed）；每个文件复制完都与已知长度核对，
 * 不同即失败（size-mismatch）。核对不到的只有一种情况：文件内容在长度和成员关系
 * 都不变的情况下被改写——那时复制下来的是读到时的内容。
 *
 * ── 安全删除 ───────────────────────────────────────────────────────────
 * 绝不跟随符号链接：每个条目先经 android.system.Os.lstat 定型（不跟随）——链接只删
 * 链接本身；只有“真实目录”且 canonical 路径落在被删目录之内才递归进去；其余按文件
 * 删。File.isDirectory() 会跟随链接，从不拿它给未定型的条目分类。暂存根自己不是
 * 应用 no-backup 目录里的真实目录时什么都不删：ak-saf-staging 之外的东西永远不碰。
 *
 * ── 错误与日志 ─────────────────────────────────────────────────────────
 * 失败一律 PlatformException(code, 固定 reason token)；message 与日志里都不出现
 * URI、路径、目录或文件名。journal 的内容（URI）同样从不进日志。
 *
 * minSdk 24：不用 java.nio.file，文件操作只用 java.io.File 与 android.system.Os。
 */

import android.content.Context
import android.content.Intent
import android.content.UriPermission
import android.database.Cursor
import android.net.Uri
import android.os.CancellationSignal
import android.os.Handler
import android.os.SystemClock
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.util.Log
import io.flutter.plugin.common.MethodChannel
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.util.Locale
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * 发送侧 SAF 目录暂存器：不碰任何 Android UI，只持有 Context、通道和主线程 Handler。
 * MainActivity 在主线程上调用 [onSendFolderPicked]、[stageSafTree]、[cancelStaging]、
 * [cleanupStagedSend]、[cleanupStaleStaging] 与 [shutdown]。
 */
class SafSendStager(
    context: Context,
    private val channel: MethodChannel,
    private val mainHandler: Handler
) {

    companion object {
        private const val TAG = "AKStorage"

        private const val STAGING_DIR_NAME = "ak-saf-staging"
        private const val PAYLOAD_DIR_NAME = "payload"
        private const val MARKER_NAME = ".ak-stage-complete"
        private const val JOURNAL_NAME = ".send-grants"
        private const val JOURNAL_TEMP_NAME = ".send-grants.tmp"
        private const val STAGE_ID_LENGTH = 32

        /** 条目上限（文件 + 目录）。 */
        private const val MAX_ENTRIES = 100000L

        /** 相对路径上限（UTF-8 字节），与引擎 validate_rel_path 一致。 */
        private const val MAX_REL_PATH_BYTES = 1024

        /** 单个路径分量的上限（UTF-8 字节）：暂存所在文件系统的 NAME_MAX，再长的名字建不出来。 */
        private const val MAX_COMPONENT_BYTES = 255

        /** 空间预检的余量。 */
        private const val FREE_SPACE_MARGIN_BYTES = 16L * 1024 * 1024

        private const val COPY_BUFFER_BYTES = 64 * 1024
        private const val PROGRESS_INTERVAL_MS = 150L
        private const val MARKER_MAX_AGE_MS = 24L * 60 * 60 * 1000

        private const val PHASE_ENUMERATE = "enumerate"
        private const val PHASE_COPY = "copy"

        private const val CODE_PERMISSION_DENIED = "SAF_PERMISSION_DENIED"
        private const val CODE_PROVIDER_UNTRUSTED = "SAF_PROVIDER_UNTRUSTED"
        private const val CODE_ENUMERATION_FAILED = "SAF_ENUMERATION_FAILED"
        private const val CODE_UNSUPPORTED_DOCUMENT = "SAF_UNSUPPORTED_DOCUMENT"
        private const val CODE_NAME_CONFLICT = "STAGE_NAME_CONFLICT"
        private const val CODE_NO_SPACE = "STAGE_NO_SPACE"
        private const val CODE_IO_FAILED = "STAGE_IO_FAILED"
        private const val CODE_CANCELLED = "STAGE_CANCELLED"
        private const val CODE_INVALID_ID = "STAGE_INVALID_ID"

        /** 接收端 ak-storage 自己的暂存命名空间：带这个名字的分量它拒收（ASCII 不分大小写）。 */
        private const val RECEIVER_RESERVED_NAME = ".ak-staging"

        /** 允许的 SAF 提供器：主共享存储与下载目录。 */
        private val TRUSTED_AUTHORITIES = setOf(
            "com.android.externalstorage.documents",
            "com.android.providers.downloads.documents"
        )

        /** 引擎 validate_rel_path 拒绝的 Windows 保留设备名 stem（ASCII 不分大小写）。 */
        private val RESERVED_STEMS: Set<String> = setOf("CON", "PRN", "AUX", "NUL") +
            (1..9).flatMap { listOf("COM$it", "LPT$it") }

        private val CHILD_PROJECTION = arrayOf(
            Document.COLUMN_DOCUMENT_ID,
            Document.COLUMN_DISPLAY_NAME,
            Document.COLUMN_MIME_TYPE,
            Document.COLUMN_FLAGS,
            Document.COLUMN_SIZE
        )

        /** 复核成员关系用：只取子项的 document id。 */
        private val CHILD_ID_PROJECTION = arrayOf(Document.COLUMN_DOCUMENT_ID)

        /** 磁盘满的标志：ENOSPC，以及同样按空间不足处理的 EDQUOT（errno 122）。 */
        private val NO_SPACE_MARKERS = listOf("ENOSPC", "No space left", "EDQUOT", "Disk quota exceeded")

        /**
         * 全进程唯一的工作线程：每一次提供器/授权调用和暂存根下的每次增删都在这里串行
         * 执行。守护线程，不持有 Context，Activity 重建后被取消任务的清理仍在这里跑完。
         */
        private val WORKER: ExecutorService = Executors.newSingleThreadExecutor { runnable ->
            Thread(runnable, "ak-saf-stager").apply { isDaemon = true }
        }

        /** 进行中（运行或排队）的暂存任务，按 stageId 索引；jobsLock 之下主线程与工作线程都会访问。 */
        private val jobsLock = Any()
        private val activeJobs = HashMap<String, Job>()
    }

    /** 暂存失败：code 是契约里的 PlatformException code，reason 是固定 reason token。 */
    private class StageFailure(val code: String, val reason: String) : Exception(reason)

    private sealed class Outcome {
        class Ok(val result: Any?) : Outcome()
        class Failed(val failure: StageFailure) : Outcome()
    }

    private class Job(val stageId: String, val treeUri: Uri, val owner: SafSendStager) {
        /** 本任务的取消信号：它的查询和打开文件都带着它，取消时阻塞中的调用立刻返回。 */
        val signal = CancellationSignal()

        /** 当前打开着的输入流。取消方要把它关掉，同一个流只关一次（见 [closeOpenStream]）。 */
        @Volatile
        private var openStream: InputStream? = null
        private val streamLock = Any()

        // 以下两项只在工作线程上读写。
        /** 本次调用是否自己 take 了 READ 授权：是，结束时才释放。 */
        var tookGrant: Boolean = false

        /** 本任务是否已经建起了自己的 <stageId> 目录：是，失败时才由它负责删掉。 */
        var ownsStageDir: Boolean = false

        @Volatile
        var cancelled: Boolean = false

        fun attachStream(stream: InputStream) {
            openStream = stream
        }

        /** 关这一路输入流的唯一入口：工作线程收尾和取消方都走这里。 */
        fun closeOpenStream() {
            synchronized(streamLock) {
                val stream = openStream ?: return
                openStream = null
                try {
                    stream.close()
                } catch (t: Throwable) {
                    // 尽力而为：关不掉不影响已经写到输出流里的内容
                }
            }
        }
    }

    /** 枚举用的一帧：一个待展开的目录、它的相对路径（所选目录自己是空串）和它子项的 document id。 */
    private class Frame(val docId: String, val relPath: String) {
        val childIds = HashSet<String>()
    }

    private class PlanFile(
        val relPath: String,
        val docUri: Uri,
        /** COLUMN_SIZE；未知为 -1。 */
        val size: Long
    )

    /** 一个计划过的目录（含根）及其子项 document id 集合：复制完之后要逐个复核。 */
    private class PlanDirectory(val docId: String, val childIds: Set<String>)

    private class Plan(val rootName: String) {
        /** 子目录的相对路径（不含根本身），父目录总在子目录之前。 */
        val directories = ArrayList<String>()
        val files = ArrayList<PlanFile>()

        /** 每个目录（根本身是第一个）及其子项 document id 集合。 */
        val memberships = ArrayList<PlanDirectory>()
        var entryCount = 0L
        var emptyDirectoryCount = 0L
        var knownBytes = 0L
        var knownSizeFiles = 0L

        val fileCount: Long
            get() = files.size.toLong()

        /** 已知大小之和；只要有任何一个文件大小未知就是 -1（进度不确定）。 */
        val totalEstimate: Long
            get() = if (knownSizeFiles == fileCount) knownBytes else -1L
    }

    private class CopyProgress {
        var filesCopied = 0L
        var bytesCopied = 0L
        var lastEmitAt = 0L
    }

    private class CopyResult(val stagedRootPath: String, val bytesCopied: Long)

    /** lstat 定型的结果：不存在 / 普通文件 / 真实目录 / 符号链接。 */
    private enum class EntryType { MISSING, FILE, DIRECTORY, LINK }

    private val appContext = context.applicationContext

    /** onDestroy 之后置位：Activity 与引擎都不在了，不再向通道应答或发事件。 */
    @Volatile
    private var released = false

    // ── 通道入口（一律在主线程调用；这里不做任何 ContentResolver 调用）──────────

    /**
     * pickSendFolder 的结果（OpenDocumentTree 回调）。不持久化任何授权：只校验
     * authority，并借 Activity 的临时授权读出根目录名——那一次查询在工作线程上做，
     * 应答也从那里回主线程。
     */
    fun onSendFolderPicked(treeUri: Uri?, result: MethodChannel.Result) {
        val answer = OneShot(result)
        if (treeUri == null) {
            answer.success(mapOf("status" to "cancelled"))
            return
        }
        if (!isTrustedAuthority(treeUri)) {
            answer.fail(CODE_PROVIDER_UNTRUSTED, "authority")
            return
        }
        runOnWorker(answer) {
            val displayName = checkedRootName(treeUri, null)
            mapOf(
                "status" to "selected",
                "treeUri" to treeUri.toString(),
                "displayName" to displayName
            )
        }
    }

    /**
     * stageSafTree(treeUri, stageId)。主线程上只做不需要提供器的校验；读授权、枚举、
     * 复制都在工作线程上跑，应答在工作线程完全停下之后才回到主线程。
     */
    fun stageSafTree(treeUriString: String?, stageId: String?, result: MethodChannel.Result) {
        val answer = OneShot(result)
        if (stageId == null || !isValidStageId(stageId)) {
            answer.fail(CODE_INVALID_ID, "format")
            return
        }
        val treeUri = if (treeUriString == null) null else Uri.parse(treeUriString)
        if (treeUri == null || !isTrustedAuthority(treeUri)) {
            answer.fail(CODE_PROVIDER_UNTRUSTED, "authority")
            return
        }

        // 先登记：拿授权、枚举、复制都还在后面，“同一个 id 还在跑”从这一刻起就成立。
        val job = Job(stageId, treeUri, this)
        val busy = synchronized(jobsLock) {
            if (activeJobs.containsKey(stageId)) {
                true
            } else {
                activeJobs[stageId] = job
                false
            }
        }
        if (busy) {
            // 同一个 id 还在跑（或还在排队）
            answer.fail(CODE_IO_FAILED, "busy")
            return
        }
        val running = synchronized(jobsLock) { activeJobs.values.filter { it !== job } }
        // 另一个不同 id 的暂存还在跑：先取消它，新任务排在它的清理之后
        running.forEach { cancelJob(it) }
        try {
            WORKER.execute { runJob(job, answer) }
        } catch (t: Throwable) {
            unregisterJob(job)
            answer.fail(CODE_IO_FAILED, "io")
        }
    }

    /**
     * cancelStaging(stageId)：置取消标志、取消该任务的 CancellationSignal、关掉它打开着
     * 的输入流——不删任何东西，清理由正在跑的任务自己完成后再应答。没有匹配的任务时
     * 是空操作。
     */
    fun cancelStaging(stageId: String?) {
        if (stageId == null) return
        val job = synchronized(jobsLock) { activeJobs[stageId] } ?: return
        cancelJob(job)
    }

    /**
     * cleanupStagedSend(stageId)：删掉已经交付的暂存副本，答 {removed: true}（已经不在了
     * 也算）或 {removed: false}（有东西删不掉）。这个 id 的暂存还在跑（或还在排队）时
     * 答 busy，什么都不删。
     */
    fun cleanupStagedSend(stageId: String?, result: MethodChannel.Result) {
        val answer = OneShot(result)
        if (stageId == null || !isValidStageId(stageId)) {
            answer.fail(CODE_INVALID_ID, "format")
            return
        }
        runOnWorker(answer) {
            // 轮到它时才知道这个 id 是否还在暂存：此时工作线程上没有别的任务，表里有的
            // 只可能是排在后面的任务（它什么都还没建）。
            val activeIds = synchronized(jobsLock) { activeJobs.keys.toSet() }
            if (stageId in activeIds) throw StageFailure(CODE_IO_FAILED, "busy")
            mapOf("removed" to deleteStagedEntry(File(stagingRoot(), stageId)))
        }
    }

    /**
     * cleanupStaleStaging()：清掉以前各次运行留下的东西——过期的暂存目录，以及
     * journal 里记着、却不属于任何在跑暂存的 READ-only 授权；答 {"removed": n}。
     */
    fun cleanupStaleStaging(result: MethodChannel.Result) {
        runOnWorker(answer = OneShot(result)) { mapOf("removed" to cleanupStale()) }
    }

    /**
     * Activity 即将销毁：本实例还在跑的暂存全部取消（清理仍在工作线程上完成），
     * 之后不再向通道应答或发事件。
     */
    fun shutdown() {
        released = true
        val mine = synchronized(jobsLock) { activeJobs.values.filter { it.owner === this } }
        mine.forEach { cancelJob(it) }
    }

    // ── 工作线程：一次暂存 ─────────────────────────────────────────────────

    private fun runJob(job: Job, answer: OneShot) {
        val outcome: Outcome = try {
            Outcome.Ok(stage(job))
        } catch (t: Throwable) {
            // 被取消的任务无论停在哪一步、以什么异常停下来，对外的答案都是“已取消”
            Outcome.Failed(
                if (job.cancelled) StageFailure(CODE_CANCELLED, "user") else toFailure(t)
            )
        }
        val reported = if (outcome is Outcome.Failed) reportableFailure(job, outcome.failure) else outcome
        // 成功时完成标记已经写下，失败时目录已经清掉：此刻才放掉自己拿的授权
        runCatching { releaseOwnedGrant(job) }
            .onFailure { Log.w(TAG, "grant release after staging threw: ${it.javaClass.simpleName}") }
        unregisterJob(job)
        postToMain { deliver(answer, reported) }
    }

    /**
     * 失败或取消之后：只删本任务自己建起来的目录。删不干净时不拿原来的失败交差，
     * 如实答 leftover——留下的东西下一次启动时由 cleanupStaleStaging 清。
     */
    private fun reportableFailure(job: Job, failure: StageFailure): Outcome {
        if (!job.ownsStageDir) return Outcome.Failed(failure)
        val gone = runCatching { deleteStagedEntry(File(stagingRoot(), job.stageId)) }
            .onFailure { Log.w(TAG, "stage directory cleanup threw: ${it.javaClass.simpleName}") }
            .getOrDefault(false)
        return Outcome.Failed(if (gone) failure else StageFailure(CODE_IO_FAILED, "leftover"))
    }

    /** 拒绝复用 → 读授权 → 枚举 → 复制并复核（完成标记最后写）；返回 ok map。自己拿的授权由 runJob 在这之后放掉。 */
    private fun stage(job: Job): Map<String, Any?> {
        // 排队期间就已经被取消的任务：什么都不读、什么都不建
        checkCancelled(job)
        val stagingRoot = stagingRoot()
        // 这个 id 若已经带着有效的完成标记（交付过的副本），拒绝复用，什么都不删
        if (isDeliveredStageDir(File(stagingRoot, job.stageId))) {
            throw StageFailure(CODE_IO_FAILED, "id-in-use")
        }
        acquireReadGrant(job)
        val plan = enumerateTree(job)
        emitProgress(job, PHASE_ENUMERATE, plan.entryCount, 0L, 0L, plan.totalEstimate)
        val copied = copyPlan(job, stagingRoot, plan)
        return mapOf(
            "status" to "ok",
            "stageId" to job.stageId,
            "path" to copied.stagedRootPath,
            "byteCount" to copied.bytesCopied,
            "displayName" to plan.rootName,
            "fileCount" to plan.fileCount,
            "emptyDirectoryCount" to plan.emptyDirectoryCount
        )
    }

    /**
     * 第一阶段：读根目录名，再按显式栈做迭代式深度优先遍历（不递归），把整棵树收进
     * [Plan]。任何一个条目有问题都让整次暂存失败——不跳过，也不改名。
     */
    private fun enumerateTree(job: Job): Plan {
        val treeUri = job.treeUri
        val rootName = checkedRootName(treeUri, job.signal)
        val plan = Plan(rootName)

        val rootDocId = treeDocumentId(treeUri)
        val seenDocIds = HashSet<String>()
        seenDocIds.add(rootDocId)
        val pending = ArrayDeque<Frame>()
        pending.addLast(Frame(rootDocId, ""))

        while (pending.isNotEmpty()) {
            checkCancelled(job)
            val frame = pending.removeLast()
            var childCount = 0L
            val siblingKeys = HashSet<String>()
            val cursor = queryChildren(treeUri, frame.docId, job.signal, CHILD_PROJECTION)
                ?: throw StageFailure(CODE_ENUMERATION_FAILED, "query")
            cursor.use { c ->
                while (c.moveToNext()) {
                    checkCancelled(job)
                    childCount++
                    try {
                        acceptEntry(plan, treeUri, frame, rootName, seenDocIds, siblingKeys, pending, c)
                    } catch (t: Throwable) {
                        throw queryFailure(t)
                    }
                }
            }
            // 记住这个目录（根本身是第一个）的子项 document id 集合，复制完之后要复核
            plan.memberships.add(PlanDirectory(frame.docId, frame.childIds.toSet()))
            if (frame.relPath.isNotEmpty() && childCount == 0L) plan.emptyDirectoryCount++
        }

        if (plan.entryCount == 0L) {
            // 引擎发不了完全空的根目录
            throw StageFailure(CODE_UNSUPPORTED_DOCUMENT, "empty-root")
        }

        // 空间预检：noBackupFilesDir 与暂存根在同一卷上；已知大小只是下界
        val usable = appContext.noBackupFilesDir.usableSpace
        if (usable - FREE_SPACE_MARGIN_BYTES < plan.knownBytes) {
            throw StageFailure(CODE_NO_SPACE, "no-space")
        }
        return plan
    }

    /** 处理子项查询里的一行；目录推入 [pending] 等待展开。 */
    private fun acceptEntry(
        plan: Plan,
        treeUri: Uri,
        frame: Frame,
        rootName: String,
        seenDocIds: MutableSet<String>,
        siblingKeys: MutableSet<String>,
        pending: ArrayDeque<Frame>,
        cursor: Cursor
    ) {
        val docId = cursor.getString(0)
        val displayName = cursor.getString(1).orEmpty()
        val mimeType = cursor.getString(2)
        val flags = cursor.getLong(3)
        val size = if (cursor.isNull(4) || cursor.getLong(4) < 0L) -1L else cursor.getLong(4)

        if (plan.entryCount >= MAX_ENTRIES) {
            throw StageFailure(CODE_ENUMERATION_FAILED, "too-many-entries")
        }
        plan.entryCount++
        if (docId.isNullOrEmpty()) throw StageFailure(CODE_ENUMERATION_FAILED, "query")
        if (!seenDocIds.add(docId)) throw StageFailure(CODE_ENUMERATION_FAILED, "duplicate-id")
        if ((flags and Document.FLAG_VIRTUAL_DOCUMENT.toLong()) != 0L) {
            throw StageFailure(CODE_UNSUPPORTED_DOCUMENT, "virtual-document")
        }

        val nameIssue = componentNameIssue(displayName)
        if (nameIssue != null) throw StageFailure(CODE_NAME_CONFLICT, nameIssue)
        // 同一级里不许有大小写不敏感的重名
        if (!siblingKeys.add(displayName.lowercase(Locale.ROOT))) {
            throw StageFailure(CODE_NAME_CONFLICT, "duplicate")
        }

        val relPath = joinPaths(frame.relPath, displayName)
        if (isTooLong(relPath) || isTooLong("$rootName/$relPath")) {
            throw StageFailure(CODE_NAME_CONFLICT, "too-long")
        }

        frame.childIds.add(docId)
        if (mimeType == Document.MIME_TYPE_DIR) {
            plan.directories.add(relPath)
            pending.addLast(Frame(docId, relPath))
        } else {
            if (mimeType.isNullOrBlank()) throw StageFailure(CODE_UNSUPPORTED_DOCUMENT, "mime")
            plan.files.add(
                PlanFile(relPath, DocumentsContract.buildDocumentUriUsingTree(treeUri, docId), size)
            )
            if (size >= 0L) {
                if (size > Long.MAX_VALUE - plan.knownBytes) {
                    throw StageFailure(CODE_ENUMERATION_FAILED, "overflow")
                }
                plan.knownBytes += size
                plan.knownSizeFiles++
            }
        }
    }

    /**
     * 第二阶段：建目录、逐个文件复制（每个文件都与已知长度核对），然后复核每个目录的
     * 成员关系，最后写完成标记。目标一律从暂存目录的 canonical 路径拼出来：之后
     * absolutePath 与 canonicalPath 一旦不同，只可能是暂存目录之下有符号链接挡路。
     */
    private fun copyPlan(job: Job, stagingRoot: File, plan: Plan): CopyResult {
        val stageDir = File(stagingRoot, job.stageId)
        val rootCanonical = trustedStagingRoot(stagingRoot)
        // 同一 id 的残留（没有完成标记的上次中断）必须先删干净：暂存目录只能是从零建起来的
        if (!deleteStagedEntry(stageDir)) throw StageFailure(CODE_IO_FAILED, "io")
        // 从这里起，这个名字下的任何东西都是本任务建的：失败时由它负责删掉
        job.ownsStageDir = true
        val stageCanonical = realDirectory(
            stageDir, rootCanonical + File.separator + job.stageId
        )
        val payloadCanonical = realDirectory(
            File(stageCanonical, PAYLOAD_DIR_NAME),
            stageCanonical + File.separator + PAYLOAD_DIR_NAME
        )
        val payloadDir = File(payloadCanonical)

        createDirectory(File(payloadDir, plan.rootName), stageCanonical)
        for (relPath in plan.directories) {
            checkCancelled(job)
            createDirectory(File(payloadDir, joinPaths(plan.rootName, relPath)), stageCanonical)
        }

        val progress = CopyProgress()
        val buffer = ByteArray(COPY_BUFFER_BYTES)
        emitProgress(job, PHASE_COPY, plan.entryCount, 0L, 0L, plan.totalEstimate)
        for (file in plan.files) {
            copyFile(
                job, file, File(payloadDir, joinPaths(plan.rootName, file.relPath)),
                stageCanonical, buffer, progress, plan
            )
        }
        emitProgress(
            job, PHASE_COPY, plan.entryCount,
            progress.filesCopied, progress.bytesCopied, plan.totalEstimate
        )

        // 所有文件都复制完并关闭了：先复核成员关系，再写完成标记
        verifyMembership(job, plan)
        // 取消若是在复核期间到的：标记还没写，这次暂存就按取消处理
        checkCancelled(job)
        val marker = File(stageCanonical, MARKER_NAME)
        checkTarget(marker, stageCanonical)
        try {
            if (!marker.createNewFile()) throw StageFailure(CODE_IO_FAILED, "create")
        } catch (t: Throwable) {
            throw stepFailure(t, "create")
        }
        return CopyResult(File(payloadDir, plan.rootName).absolutePath, progress.bytesCopied)
    }

    /** 复制一个文件，复制完与已知长度核对。 */
    private fun copyFile(
        job: Job,
        file: PlanFile,
        target: File,
        stageCanonical: String,
        buffer: ByteArray,
        progress: CopyProgress,
        plan: Plan
    ) {
        checkCancelled(job)
        checkTarget(target, stageCanonical)
        try {
            if (!target.createNewFile()) throw StageFailure(CODE_IO_FAILED, "create")
        } catch (t: Throwable) {
            throw stepFailure(t, "create")
        }
        val bytesBefore = progress.bytesCopied
        val source = openInput(job, file.docUri)
        val copied: Long = try {
            val output = try {
                FileOutputStream(target)
            } catch (t: Throwable) {
                throw stepFailure(t, "create")
            }
            try {
                output.use { sink ->
                    copyLoop(job, source, sink, buffer, progress, plan)
                }
            } catch (t: Throwable) {
                throw stepFailure(t, "write")
            }
            progress.bytesCopied - bytesBefore
        } catch (t: Throwable) {
            throw stepFailure(t, "read")
        } finally {
            job.closeOpenStream()
        }
        progress.filesCopied++
        // 已知长度与实际读到的字节数不一致：源文件在两步之间变了，整次暂存失败
        if (sizeMismatch(file.size, copied)) {
            throw StageFailure(CODE_IO_FAILED, "size-mismatch")
        }
    }

    /** 64 KiB 一块地复制；块与块之间都看一眼取消标志。 */
    private fun copyLoop(
        job: Job,
        source: InputStream,
        sink: OutputStream,
        buffer: ByteArray,
        progress: CopyProgress,
        plan: Plan
    ) {
        while (true) {
            checkCancelled(job)
            val read = try {
                source.read(buffer)
            } catch (t: Throwable) {
                throw stepFailure(t, "read")
            }
            if (read < 0) break
            try {
                sink.write(buffer, 0, read)
            } catch (t: Throwable) {
                throw stepFailure(t, "write")
            }
            if (progress.bytesCopied > Long.MAX_VALUE - read) {
                throw StageFailure(CODE_IO_FAILED, "io")
            }
            progress.bytesCopied += read
            val now = SystemClock.uptimeMillis()
            if (now - progress.lastEmitAt >= PROGRESS_INTERVAL_MS) {
                progress.lastEmitAt = now
                emitProgress(
                    job, PHASE_COPY, plan.entryCount,
                    progress.filesCopied, progress.bytesCopied, plan.totalEstimate
                )
            }
        }
    }

    /**
     * 所有文件都复制完之后、写完成标记之前，把每个计划过的目录（含根）再查一遍：子项
     * document id 集合只要和计划时不同就以 source-changed 让整次暂存失败。
     *
     * 保真边界：这里核对成员关系，copyFile 核对已知长度；一个文件的内容如果在长度和
     * 成员关系都不变的情况下被改写，复制下来的是读到时的内容，这里发现不了。
     */
    private fun verifyMembership(job: Job, plan: Plan) {
        for (directory in plan.memberships) {
            checkCancelled(job)
            val now = queryChildIds(job, directory.docId)
            if (membershipChanged(directory.childIds, now)) {
                throw StageFailure(CODE_IO_FAILED, "source-changed")
            }
        }
    }

    // ── 过期清理（工作线程）─────────────────────────────────────────────────

    /** cleanupStaleStaging 的工作线程部分：返回删掉的暂存根子项个数。 */
    private fun cleanupStale(): Long {
        var removed = 0L
        val stagingRoot = stagingRoot()
        when (lstatType(stagingRoot)) {
            EntryType.MISSING -> Unit   // 还没有任何暂存：无事可做
            EntryType.DIRECTORY -> {
                // 快照取在任务真正执行的时候：此刻工作线程上没有别的任务在改暂存根，
                // 表里有的只是排在后面的任务。
                val activeIds = synchronized(jobsLock) { activeJobs.keys.toSet() }
                val children = stagingRoot.listFiles()
                if (children != null) {
                    for (child in children) {
                        // journal 不是暂存目录：过期清理不碰它
                        if (child.name == JOURNAL_NAME) continue
                        if (!isStale(child, activeIds)) continue
                        if (deleteStagedEntry(child)) removed++
                    }
                }
            }
            else -> Log.w(TAG, "staging root is not a real directory; stale entries left alone")
        }
        releaseJournaledGrants()
        return removed
    }

    /**
     * 回收 journal 里记着、却不属于任何在跑暂存的 READ-only 授权（有读位、无写位）——
     * 以前某次进程中断留下的。不在 journal 里的授权一概不碰；带写位的（接收保存目录）
     * 更不碰。放掉的（以及已经无授权可放的）才从 journal 里划掉。
     */
    private fun releaseJournaledGrants() {
        val stagingRoot = stagingRoot()
        if (lstatType(stagingRoot) == EntryType.MISSING) return
        val journal: File? = try {
            journalFile()
        } catch (e: StageFailure) {
            null
        }
        if (journal == null) {
            Log.w(TAG, "grant journal unavailable; journaled grants left alone")
            return
        }
        if (lstatType(journal) == EntryType.MISSING) return
        val lines = try {
            journalLines(journal.readText(Charsets.UTF_8))
        } catch (t: Throwable) {
            Log.w(TAG, "grant journal unreadable; journaled grants left alone")
            return
        }
        if (lines.isEmpty()) return
        val grants = try {
            appContext.contentResolver.persistedUriPermissions
        } catch (t: Throwable) {
            // 读不到授权表就不知道放掉的是谁的：什么都不碰
            Log.w(TAG, "persistedUriPermissions unavailable; journaled grants left alone")
            return
        }
        val activeUris = synchronized(jobsLock) { activeJobs.values.map { it.treeUri }.toSet() }

        val attempted = ArrayList<Uri>()
        val droppable = ArrayList<Uri>()
        for (line in lines.distinct()) {
            val uri = Uri.parse(line)
            if (uri in activeUris) continue   // 在跑的暂存还用着：行和授权都留给它自己处理
            if (isReadOnlyGrant(grants, uri)) {
                // 放不掉：这一行留着，下一次再试
                if (releasePersistableRead(uri)) attempted.add(uri)
            } else {
                // 没有 READ-only 授权可放（当时没 take 成功，或这份授权已经带着写位）
                droppable.add(uri)
            }
        }
        if (attempted.isNotEmpty()) {
            // 复核：确认不再带读位才划掉
            val after = try {
                appContext.contentResolver.persistedUriPermissions
            } catch (t: Throwable) {
                Log.w(TAG, "persistedUriPermissions unavailable; released grants stay journaled")
                return
            }
            for (uri in attempted) {
                if (!hasReadBit(after, uri)) droppable.add(uri)
            }
        }
        for (uri in droppable) journalRemove(uri)
    }

    /** 过期判定用到的三样东西：名字、是否在跑、条目本身的类型与标记的新旧。 */
    private fun isStale(child: File, activeIds: Set<String>): Boolean {
        val entryType = lstatType(child)
        val markerIsFresh = entryType == EntryType.DIRECTORY && hasFreshMarker(child)
        return isStaleByRules(
            nameIsId = isValidStageId(child.name),
            isActive = child.name in activeIds,
            entryType = entryType,
            markerIsFresh = markerIsFresh
        )
    }

    /**
     * 过期规则（纯判定）：名字不是 id 格式 → 过期；正在暂存的 id → 留；不是真实目录
     * （链接，或顶着 id 名字的文件）→ 过期，按它自己删——链接后面的新标记保不住链接；
     * 没有新鲜的完成标记 → 过期。
     */
    private fun isStaleByRules(
        nameIsId: Boolean,
        isActive: Boolean,
        entryType: EntryType?,
        markerIsFresh: Boolean
    ): Boolean {
        if (!nameIsId) return true
        if (isActive) return false
        if (entryType != EntryType.DIRECTORY) return true
        return !markerIsFresh
    }

    /** 完成标记是一个真实文件，而且不超过 24 小时。 */
    private fun hasFreshMarker(stageDir: File): Boolean {
        val marker = File(stageDir, MARKER_NAME)
        return lstatType(marker) == EntryType.FILE &&
            System.currentTimeMillis() - marker.lastModified() < MARKER_MAX_AGE_MS
    }

    // ── 安全删除（只在工作线程上调用）───────────────────────────────────────

    /**
     * 删除暂存根的直接子项 [entry]，绝不跟随符号链接；返回是否已经删干净。
     * 每个条目先经 lstat 定型：链接只删链接本身；只有真实目录且 canonical 路径确实在
     * 暂存根之下才进入；其余按文件删。暂存根自己不是应用 no-backup 目录里的真实目录
     * 时什么都不删。
     */
    private fun deleteStagedEntry(entry: File): Boolean {
        val stagingRoot = stagingRoot()
        val rootType = lstatType(stagingRoot)
        if (rootType == EntryType.MISSING) return true
        val noBackupCanonical = canonicalPathOf(appContext.noBackupFilesDir)
        val rootCanonical = if (rootType == EntryType.DIRECTORY) canonicalPathOf(stagingRoot) else null
        if (noBackupCanonical == null || rootCanonical == null ||
            rootCanonical != noBackupCanonical + File.separator + STAGING_DIR_NAME
        ) {
            Log.w(TAG, "staging root is not a real directory in app storage; nothing deleted")
            return false
        }
        return when (lstatType(entry)) {
            EntryType.MISSING -> true
            EntryType.DIRECTORY -> {
                val topCanonical = canonicalPathOf(entry)
                if (topCanonical != rootCanonical + File.separator + entry.name) {
                    Log.w(TAG, "staging entry does not lie inside the staging root; nothing deleted")
                    false
                } else {
                    deleteRealDirectory(entry, topCanonical)
                }
            }
            // 文件、符号链接（不管指向哪里）：只删条目本身，链接的目标永远不碰
            EntryType.FILE, EntryType.LINK -> removeEntry(entry)
            null -> {
                Log.w(TAG, "staging entry cannot be typed; nothing deleted")
                false
            }
        }
    }

    /**
     * 删除真实目录 [top]（canonical 路径 [topCanonical]）及其内容，自下而上，不用递归。
     * 子项里只有“真实目录，且 canonical 路径正是上级目录下的这个条目”的才被进入。
     */
    private fun deleteRealDirectory(top: File, topCanonical: String): Boolean {
        var complete = true
        val directories = ArrayList<File>()
        val pending = ArrayDeque<Pair<File, String>>()
        pending.addLast(top to topCanonical)
        directories.add(top)
        while (pending.isNotEmpty()) {
            val (directory, directoryCanonical) = pending.removeLast()
            val children = directory.listFiles()
            if (children == null) {
                complete = false
                continue
            }
            for (child in children) {
                when (lstatType(child)) {
                    EntryType.MISSING -> Unit
                    EntryType.DIRECTORY -> {
                        val childCanonical = canonicalPathOf(child)
                        if (childCanonical != null &&
                            childCanonical == directoryCanonical + File.separator + child.name &&
                            childCanonical.startsWith(topCanonical + File.separator)
                        ) {
                            pending.addLast(child to childCanonical)
                            directories.add(child)
                        } else if (!removeEntry(child)) {
                            // 不在本次要删的目录里：不进去，只按文件删（非空就删不掉）
                            complete = false
                        }
                    }
                    EntryType.FILE, EntryType.LINK -> if (!removeEntry(child)) complete = false
                    null -> complete = false
                }
            }
        }
        // 发现顺序里父目录总在子目录之前：倒过来删，子目录先空
        for (index in directories.indices.reversed()) {
            if (!removeEntry(directories[index])) complete = false
        }
        return complete
    }

    /**
     * 不跟随链接地给条目定型（lstat）。条目不存在 → MISSING；类型读不到 → null，
     * 调用方一律按“不可碰”处理。File.isDirectory() 会跟随链接，不用它分类。
     */
    private fun lstatType(file: File): EntryType? = try {
        val mode = Os.lstat(file.path).st_mode
        when {
            OsConstants.S_ISLNK(mode) -> EntryType.LINK
            OsConstants.S_ISDIR(mode) -> EntryType.DIRECTORY
            else -> EntryType.FILE
        }
    } catch (e: ErrnoException) {
        if (e.errno == OsConstants.ENOENT) EntryType.MISSING else null
    }

    /**
     * 删掉条目本身（remove(3)：普通文件与符号链接走 unlink，目录走 rmdir——两者都不
     * 跟随符号链接）。只在 lstat 定型之后调用：文件和链接删的就是这一个名字，真实
     * 目录要空了才删得掉。
     */
    private fun removeEntry(file: File): Boolean = try {
        Os.remove(file.path)
        true
    } catch (e: ErrnoException) {
        false
    }

    // ── 暂存根下的建立与校验（只在工作线程上调用）────────────────────────────

    /** 暂存根 <noBackupFilesDir>/ak-saf-staging；首次访问会建 no_backup 目录，所以只在工作线程上取。 */
    private fun stagingRoot(): File = File(appContext.noBackupFilesDir, STAGING_DIR_NAME)

    /**
     * 暂存根必须是一个真实目录（不是符号链接），canonical 路径正是
     * <noBackupFilesDir>/ak-saf-staging；不存在就建。返回它的 canonical 路径。
     */
    private fun trustedStagingRoot(stagingRoot: File): String {
        val noBackupCanonical = canonicalPathOf(appContext.noBackupFilesDir)
            ?: throw StageFailure(CODE_IO_FAILED, "containment")
        return realDirectory(
            stagingRoot, noBackupCanonical + File.separator + STAGING_DIR_NAME
        )
    }

    /**
     * journal 必须是暂存根下的一个普通文件（或还不存在）：是则返回它，否则记一条日志
     * 返回 null。暂存根不可信或建不出来时抛 StageFailure。
     */
    private fun journalFile(): File? {
        trustedStagingRoot(stagingRoot())
        val journal = File(stagingRoot(), JOURNAL_NAME)
        val type = lstatType(journal)
        if (type != EntryType.MISSING && type != EntryType.FILE) {
            Log.w(TAG, "grant journal is not a regular file; it is left alone")
            return null
        }
        return journal
    }

    /**
     * 确保 [directory] 是一个真实目录（不是符号链接）且 canonical 路径正是
     * [expectedCanonical]，返回它的 canonical 路径。不存在就用 mkdir() 建（必须成功）。
     */
    private fun realDirectory(directory: File, expectedCanonical: String): String {
        when (lstatType(directory)) {
            EntryType.MISSING -> try {
                if (!directory.mkdir()) throw StageFailure(CODE_IO_FAILED, "create")
            } catch (t: Throwable) {
                throw stepFailure(t, "create")
            }
            EntryType.DIRECTORY -> Unit
            else -> throw StageFailure(CODE_IO_FAILED, "containment")
        }
        val canonical = canonicalPathOf(directory)
        if (canonical == null || canonical != expectedCanonical) {
            throw StageFailure(CODE_IO_FAILED, "containment")
        }
        return canonical
    }

    /** 在暂存目录（canonical 路径 [stageCanonical]）之下建一个目录：必须落在它里面、路径上不能有符号链接。 */
    private fun createDirectory(directory: File, stageCanonical: String) {
        checkTarget(directory, stageCanonical)
        try {
            if (!directory.mkdir()) throw StageFailure(CODE_IO_FAILED, "create")
        } catch (t: Throwable) {
            throw stepFailure(t, "create")
        }
    }

    /**
     * 每个目标的 canonicalPath 必须以“暂存目录的 canonicalPath + 分隔符”开头；目标的
     * absolutePath 与 canonicalPath 不同（路径上有符号链接挡路）直接失败。
     */
    private fun checkTarget(target: File, stageCanonical: String) {
        val canonical = canonicalPathOf(target)
            ?: throw StageFailure(CODE_IO_FAILED, "containment")
        if (!canonical.startsWith(stageCanonical + File.separator)) {
            throw StageFailure(CODE_IO_FAILED, "containment")
        }
        if (canonical != target.absolutePath) throw StageFailure(CODE_IO_FAILED, "symlink")
    }

    private fun canonicalPathOf(file: File): String? = try {
        file.canonicalPath
    } catch (e: IOException) {
        null
    }

    /** 这个 id 的暂存目录里已经有有效的完成标记（一个真实文件）：它交付过，不能悄悄复用。 */
    private fun isDeliveredStageDir(stageDir: File): Boolean =
        isDeliveredByRules(lstatType(stageDir), lstatType(File(stageDir, MARKER_NAME)))

    /** 交付过的判定（纯判定）：暂存目录是真实目录，且完成标记是真实文件。 */
    private fun isDeliveredByRules(stageDirType: EntryType?, markerType: EntryType?): Boolean =
        stageDirType == EntryType.DIRECTORY && markerType == EntryType.FILE

    // ── 读授权与 journal ───────────────────────────────────────────────────

    /**
     * 读授权表。读不出来不是“没有授权”，而是让暂存失败（grant-lookup）：猜成“没有”
     * 就可能把接收侧那份授权的读位拿走再放掉。
     */
    private fun readPersistedGrants(): List<UriPermission> = try {
        appContext.contentResolver.persistedUriPermissions
    } catch (t: Throwable) {
        throw grantLookupFailure(t)
    }

    private fun grantLookupFailure(t: Throwable): StageFailure {
        Log.w(TAG, "persistedUriPermissions failed: ${t.javaClass.simpleName}")
        return StageFailure(CODE_PERMISSION_DENIED, "grant-lookup")
    }

    /** 该 EXACT URI 是否带读位（读写都有时也算）。 */
    private fun hasReadBit(grants: List<UriPermission>, treeUri: Uri): Boolean =
        grants.any { it.uri == treeUri && it.isReadPermission }

    /** 该 EXACT URI 是否只有读授权（有读位、无写位）：只有这种才可能是发送侧自己拿的。 */
    private fun isReadOnlyGrant(grants: List<UriPermission>, treeUri: Uri): Boolean =
        grants.any { it.uri == treeUri && it.isReadPermission && !it.isWritePermission }

    /**
     * 授权：该 URI 已有持久的 READ 授权（同一目录也是接收保存目录时是 READ+WRITE）就
     * 原样使用、永不触碰；没有才自己 take READ 并复核。任何一步不成立都以 StageFailure
     * 让暂存失败。
     */
    private fun acquireReadGrant(job: Job) {
        if (hasReadBit(readPersistedGrants(), job.treeUri)) return
        // 先记 journal 再 take：进程若在 take 之后被杀，清理时只认 journal 里记着的授权。
        // journal 写不进去就不拿授权。
        journalAppend(job.treeUri)
        try {
            appContext.contentResolver.takePersistableUriPermission(
                job.treeUri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION
            )
        } catch (t: Throwable) {
            // 典型场景：没有可持久化的授权（Activity 的临时授权已失效），或持久化
            // 条数到了系统上限。
            Log.w(TAG, "takePersistableUriPermission(READ) failed: ${t.javaClass.simpleName}")
            val reason = grantFailureReason(t)
            // take 抛了也不一定没生效：复核一下。真拿到了读位就当自己拿的，由失败路径
            // 放掉；没拿到（授权表也读得到）就把 journal 里那一行撤掉。
            val taken = try {
                hasReadBit(readPersistedGrants(), job.treeUri)
            } catch (e: StageFailure) {
                true
            }
            job.tookGrant = taken
            if (!taken) journalRemove(job.treeUri)
            throw StageFailure(CODE_PERMISSION_DENIED, reason)
        }
        job.tookGrant = true
        if (!hasReadBit(readPersistedGrants(), job.treeUri)) {
            // 没抛却也没记上：读位不存在，这份授权没拿到
            job.tookGrant = false
            journalRemove(job.treeUri)
            throw StageFailure(CODE_PERMISSION_DENIED, "no-persisted-grant")
        }
    }

    /**
     * 释放本次调用自己 take 的那份 READ 授权。只有释放得到了确认——没有抛异常，而且
     * 复核时该 URI 已经不再带读位——才从 journal 里划掉这一行；复核不了就宁可留着。
     */
    private fun releaseOwnedGrant(job: Job) {
        if (!job.tookGrant) return
        if (!releasePersistableRead(job.treeUri)) return
        val stillHeld = try {
            hasReadBit(readPersistedGrants(), job.treeUri)
        } catch (e: StageFailure) {
            true
        }
        if (!stillHeld) journalRemove(job.treeUri)
    }

    /** 任务结束（或没能排上队）时把它从在跑的表里去掉；只去掉它自己。 */
    private fun unregisterJob(job: Job) {
        synchronized(jobsLock) {
            if (activeJobs[job.stageId] === job) activeJobs.remove(job.stageId)
        }
    }

    /** 放掉 [uri] 的 READ 授权；抛了就是没放掉（返回 false），日志里不带 URI。 */
    private fun releasePersistableRead(uri: Uri): Boolean = try {
        appContext.contentResolver.releasePersistableUriPermission(
            uri,
            Intent.FLAG_GRANT_READ_URI_PERMISSION
        )
        true
    } catch (t: Throwable) {
        Log.w(TAG, "releasePersistableUriPermission failed: ${t.javaClass.simpleName}")
        false
    }

    /**
     * takePersistableUriPermission 失败的原因：系统提示持久化条数到了上限时是
     * grant-limit，其余是 no-persisted-grant。
     */
    private fun grantFailureReason(t: Throwable): String {
        val message = t.message.orEmpty()
        // “没有可持久化的授权”这条消息里带着 URI，路径里可能碰巧含 limit/full 之类的词：先认它
        if (message.contains("No persistable permission grants found", ignoreCase = true)) {
            return "no-persisted-grant"
        }
        val atLimit = listOf("limit", "full", "no more").any { message.contains(it, ignoreCase = true) }
        return if (atLimit) "grant-limit" else "no-persisted-grant"
    }

    /**
     * 把 [treeUri] 记进 journal——必须在 take 之前。写不进去就让暂存失败：没有 journal
     * 就不拿授权，否则进程被杀后没人能证明这份授权是发送侧拿的。
     */
    private fun journalAppend(treeUri: Uri) {
        val journal = journalFile() ?: throw StageFailure(CODE_IO_FAILED, "containment")
        try {
            FileOutputStream(journal, true).use { out ->
                out.write(journalLine(treeUri).toByteArray(Charsets.UTF_8))
                out.flush()
                out.fd.sync()
            }
        } catch (t: Throwable) {
            throw stepFailure(t, "write")
        }
    }

    /**
     * 从 journal 里划掉 [treeUri] 的那一行——只在释放得到确认之后调用。还有别的行时整份
     * 重写：先写临时文件再改名顶替，中途被杀也丢不掉别的行；一行不剩时连文件一起删掉，
     * 暂存根里不留空壳。这里从不抛：出了错只是留着这一行。
     */
    private fun journalRemove(treeUri: Uri) {
        try {
            val journal = journalFile() ?: return
            if (lstatType(journal) == EntryType.MISSING) return
            val target = treeUri.toString()
            val kept = journalLines(journal.readText(Charsets.UTF_8)).filter { it != target }
            if (kept.isEmpty()) {
                if (!removeEntry(journal)) {
                    Log.w(TAG, "empty grant journal could not be removed")
                }
                return
            }
            val temp = File(journal.parentFile, JOURNAL_TEMP_NAME)
            // 临时文件名若被别的东西占着：先按它自己删掉，绝不写穿链接
            val tempType = lstatType(temp)
            if (tempType != EntryType.MISSING && tempType != EntryType.FILE && !removeEntry(temp)) {
                Log.w(TAG, "grant journal scratch file cannot be cleared; line kept")
                return
            }
            FileOutputStream(temp).use { out ->
                out.write(journalText(kept).toByteArray(Charsets.UTF_8))
                out.flush()
                out.fd.sync()
            }
            if (!temp.renameTo(journal)) {
                removeEntry(temp)
                Log.w(TAG, "grant journal could not be rewritten; line kept")
            }
        } catch (t: Throwable) {
            Log.w(TAG, "grant journal could not be rewritten: ${t.javaClass.simpleName}; line kept")
        }
    }

    private fun journalLine(treeUri: Uri): String = treeUri.toString() + "\n"

    /** journal 的内容：一行一个 URI，空行不算。 */
    private fun journalLines(text: String): List<String> =
        text.split('\n').map { it.trimEnd('\r') }.filter { it.isNotEmpty() }

    private fun journalText(lines: List<String>): String =
        if (lines.isEmpty()) "" else lines.joinToString("\n") + "\n"

    // ── 提供器查询（只在工作线程上调用）──────────────────────────────────────

    /** 提供器拒绝的授权错误 → provider-security，其余查询失败 → query。 */
    private fun queryFailure(t: Throwable): StageFailure = when (t) {
        is StageFailure -> t
        is SecurityException -> StageFailure(CODE_PERMISSION_DENIED, "provider-security")
        else -> StageFailure(CODE_ENUMERATION_FAILED, "query")
    }

    /**
     * 所选目录根的显示名。提供器拒绝 → provider-security，其它查询失败 → query。
     * pickSendFolder 那一次借的是 Activity 的临时授权。
     */
    private fun queryDisplayName(treeUri: Uri, signal: CancellationSignal?): String? = try {
        val rootUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, treeDocumentId(treeUri))
        appContext.contentResolver.query(
            rootUri, arrayOf(Document.COLUMN_DISPLAY_NAME), null, null, null, signal
        )?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(0) else null
        }
    } catch (t: Throwable) {
        throw queryFailure(t)
    }

    private fun treeDocumentId(treeUri: Uri): String = try {
        DocumentsContract.getTreeDocumentId(treeUri)
    } catch (t: Throwable) {
        // 不是 tree URI
        throw queryFailure(t)
    }

    /** 某个目录的子项查询（带着任务的取消信号）；失败直接抛 StageFailure。 */
    private fun queryChildren(
        treeUri: Uri,
        parentDocId: String,
        signal: CancellationSignal?,
        projection: Array<String>
    ): Cursor? = try {
        appContext.contentResolver.query(
            DocumentsContract.buildChildDocumentsUriUsingTree(treeUri, parentDocId),
            projection,
            null,
            null,
            null,
            signal
        )
    } catch (t: Throwable) {
        throw queryFailure(t)
    }

    /** 复核用：某个目录此刻的子项 document id 集合。 */
    private fun queryChildIds(job: Job, parentDocId: String): Set<String> {
        val ids = HashSet<String>()
        val cursor = queryChildren(job.treeUri, parentDocId, job.signal, CHILD_ID_PROJECTION)
            ?: throw StageFailure(CODE_ENUMERATION_FAILED, "query")
        try {
            cursor.use { c ->
                while (c.moveToNext()) {
                    val docId = c.getString(0)
                    if (docId.isNullOrEmpty()) throw StageFailure(CODE_ENUMERATION_FAILED, "query")
                    ids.add(docId)
                    // 计划里的条目总数不超过上限：此刻一个目录里的 id 已经比它还多，源一定变了
                    if (ids.size > MAX_ENTRIES) throw StageFailure(CODE_IO_FAILED, "source-changed")
                }
            }
        } catch (t: Throwable) {
            throw queryFailure(t)
        }
        return ids
    }

    /** 成员关系是否变了：子项 document id 集合逐个相同才算没变。 */
    private fun membershipChanged(planned: Set<String>, now: Set<String>): Boolean = planned != now

    /**
     * 经任务的取消信号打开文档，把流记在任务上（取消时可以从外面把它关掉）。
     * 拿不到 → open；提供器拒绝 → provider-security。
     */
    private fun openInput(job: Job, docUri: Uri): InputStream {
        val descriptor = try {
            appContext.contentResolver.openAssetFileDescriptor(docUri, "r", job.signal)
        } catch (t: Throwable) {
            throw stepFailure(t, "open")
        } ?: throw StageFailure(CODE_IO_FAILED, "open")
        val stream: InputStream? = try {
            descriptor.createInputStream()
        } catch (t: Throwable) {
            // 流没建出来：描述符别漏掉
            runCatching { descriptor.close() }
            throw stepFailure(t, "open")
        }
        if (stream == null) {
            runCatching { descriptor.close() }
            throw StageFailure(CODE_IO_FAILED, "open")
        }
        job.attachStream(stream)
        return stream
    }

    // ── 名字规则 ──────────────────────────────────────────────────────────

    /**
     * 根目录显示名：读出并按名字规则校验，不可用直接抛 StageFailure。
     */
    private fun checkedRootName(treeUri: Uri, signal: CancellationSignal?): String {
        val name = queryDisplayName(treeUri, signal)
        rootNameIssue(name)?.let { throw it }
        return name ?: throw StageFailure(CODE_UNSUPPORTED_DOCUMENT, "root-name")
    }

    /**
     * 根目录名的规则（纯判定），不可用时返回失败：原始名字里的 `/` → separator，
     * `.ak-staging` → receiver-reserved，超过单个分量的字节上限 → too-long，其余违例
     * （空、控制字符、`\`、`:`、保留设备名、以点或空格结尾……）一律 root-name。
     */
    private fun rootNameIssue(name: String?): StageFailure? {
        if (name == null || name.isBlank()) return StageFailure(CODE_UNSUPPORTED_DOCUMENT, "root-name")
        if (name.contains('/')) return StageFailure(CODE_NAME_CONFLICT, "separator")
        if (asciiEqualsIgnoreCase(name, RECEIVER_RESERVED_NAME)) {
            return StageFailure(CODE_NAME_CONFLICT, "receiver-reserved")
        }
        if (componentByteLength(name) > MAX_COMPONENT_BYTES) {
            return StageFailure(CODE_NAME_CONFLICT, "too-long")
        }
        return componentNameIssue(name)?.let { StageFailure(CODE_UNSUPPORTED_DOCUMENT, "root-name") }
    }

    /**
     * 一个路径分量的名字规则（镜像引擎 validate_rel_path，按 Unicode 字符而不是字节
     * 判断）。返回违例的 reason token，合法时返回 null。
     */
    private fun componentNameIssue(name: String?): String? {
        // 原始显示名里的 `/`：引擎会把 a/b 当成合法的两级路径，所以必须当成一个分量拒绝
        if (name != null && name.contains('/')) return "separator"
        if (name.isNullOrEmpty() || name == "." || name == "..") return "dot-component"
        for (ch in name) {
            if (isControlChar(ch)) return "control-char"
            if (ch == '\\' || ch == ':') return "separator"
        }
        if (asciiEqualsIgnoreCase(name, RECEIVER_RESERVED_NAME)) return "receiver-reserved"
        if (asciiUpper(name.substringBefore('.')) in RESERVED_STEMS) return "reserved-stem"
        if (name.endsWith(' ') || name.endsWith('.')) return "trailing-dot-or-space"
        // 暂存所在文件系统建不出超过 NAME_MAX 字节的名字：提前拒掉，而不是留到复制时莫名失败
        if (componentByteLength(name) > MAX_COMPONENT_BYTES) return "too-long"
        return null
    }

    /** C0（U+0000–001F）、DEL（U+007F）、C1（U+0080–009F）。 */
    private fun isControlChar(ch: Char): Boolean =
        ch <= '\u001F' || ch == '\u007F' || (ch >= '\u0080' && ch <= '\u009F')

    private fun componentByteLength(name: String): Int = name.toByteArray(Charsets.UTF_8).size

    /** stageId：Dart 生成的 32 位小写十六进制，等价于 ^[0-9a-f]{32}$（整串匹配）。 */
    private fun isValidStageId(stageId: String?): Boolean {
        if (stageId == null || stageId.length != STAGE_ID_LENGTH) return false
        return stageId.all { it in '0'..'9' || it in 'a'..'f' }
    }

    private fun isTrustedAuthority(uri: Uri): Boolean =
        uri.scheme == "content" && uri.authority in TRUSTED_AUTHORITIES

    private fun isTooLong(text: String): Boolean =
        text.toByteArray(Charsets.UTF_8).size > MAX_REL_PATH_BYTES

    private fun joinPaths(base: String, child: String): String =
        if (base.isEmpty()) child else "$base/$child"

    /** 只折 ASCII 字母（与引擎的 to_ascii_uppercase 一致），不做 Unicode 大小写折叠。 */
    private fun asciiUpper(text: String): String {
        val builder = StringBuilder(text.length)
        for (ch in text) builder.append(if (ch in 'a'..'z') ch.uppercaseChar() else ch)
        return builder.toString()
    }

    /** 只折 ASCII 字母的不分大小写比较。 */
    private fun asciiEqualsIgnoreCase(a: String, b: String): Boolean {
        if (a.length != b.length) return false
        return a.indices.all { asciiLower(a[it]) == asciiLower(b[it]) }
    }

    private fun asciiLower(ch: Char): Char = if (ch in 'A'..'Z') ch.lowercaseChar() else ch

    // ── 失败归类与应答 ────────────────────────────────────────────────────

    private fun checkCancelled(job: Job) {
        if (job.cancelled) throw StageFailure(CODE_CANCELLED, "user")
    }

    /** 取消一个任务（只在主线程上调用）：置标志、取消它的 CancellationSignal、关掉它打开着的输入流。 */
    private fun cancelJob(job: Job) {
        job.cancelled = true
        try {
            job.signal.cancel()
        } catch (t: Throwable) {
            // 尽力而为：标志已经置了，任务在下一次检查时停下来
        }
        job.closeOpenStream()
    }

    /** 已知长度（≥ 0）与实际复制的字节数不一致；长度未知的文件（-1）无从比较。 */
    private fun sizeMismatch(knownSize: Long, copied: Long): Boolean =
        knownSize >= 0L && copied != knownSize

    /** 复制阶段各步的失败：空间不足 → enospc，提供器拒绝 → provider-security，其余按所在的那一步。 */
    private fun stepFailure(t: Throwable, ioReason: String): StageFailure = when {
        t is StageFailure -> t
        t is SecurityException -> StageFailure(CODE_PERMISSION_DENIED, "provider-security")
        t is IOException && isNoSpace(t) -> StageFailure(CODE_NO_SPACE, "enospc")
        else -> StageFailure(CODE_IO_FAILED, ioReason)
    }

    private fun toFailure(t: Throwable): StageFailure = when {
        t is StageFailure -> t
        t is SecurityException -> StageFailure(CODE_PERMISSION_DENIED, "provider-security")
        t is IOException && isNoSpace(t) -> StageFailure(CODE_NO_SPACE, "enospc")
        else -> StageFailure(CODE_IO_FAILED, "io")
    }

    /**
     * 因果链上任何一层表示空间不足就算：ENOSPC / "No space left"，以及同样按空间不足
     * 处理的 EDQUOT / "Disk quota exceeded"（errno 122）。
     */
    private fun isNoSpace(t: Throwable): Boolean {
        var current: Throwable? = t
        var depth = 0
        while (current != null && depth < 16) {
            if (current is ErrnoException &&
                (current.errno == OsConstants.ENOSPC || current.errno == OsConstants.EDQUOT)
            ) {
                return true
            }
            val message = current.message
            if (message != null && NO_SPACE_MARKERS.any { message.contains(it, ignoreCase = true) }) {
                return true
            }
            current = current.cause
            depth++
        }
        return false
    }

    private fun logFailure(failure: StageFailure, t: Throwable? = null) {
        if (failure.code == CODE_CANCELLED) return
        val origin = if (t == null || t === failure) "" else " (${t.javaClass.simpleName})"
        Log.w(TAG, "send staging failed: ${failure.code}/${failure.reason}$origin")
    }

    private fun emitProgress(
        job: Job,
        phase: String,
        entriesSeen: Long,
        filesCopied: Long,
        bytesCopied: Long,
        totalBytesEstimate: Long
    ) {
        postToMain {
            channel.invokeMethod(
                "stagingProgress",
                mapOf(
                    "stageId" to job.stageId,
                    "phase" to phase,
                    "entriesSeen" to entriesSeen,
                    "filesCopied" to filesCopied,
                    "bytesCopied" to bytesCopied,
                    "totalBytesEstimate" to totalBytesEstimate
                )
            )
        }
    }

    /** 在工作线程上跑 [block]，把它的返回值（或失败）回主线程应答。 */
    private fun runOnWorker(answer: OneShot, block: () -> Any?) {
        try {
            WORKER.execute {
                val outcome: Outcome = try {
                    Outcome.Ok(block())
                } catch (t: Throwable) {
                    Outcome.Failed(toFailure(t))
                }
                postToMain { deliver(answer, outcome) }
            }
        } catch (t: Throwable) {
            answer.fail(CODE_IO_FAILED, "io")
        }
    }

    private fun deliver(answer: OneShot, outcome: Outcome) {
        when (outcome) {
            is Outcome.Ok -> answer.success(outcome.result)
            is Outcome.Failed -> {
                logFailure(outcome.failure)
                answer.fail(outcome.failure.code, outcome.failure.reason)
            }
        }
    }

    /** 回主线程；Activity 已销毁时引擎不在了，什么都不发。 */
    private fun postToMain(block: () -> Unit) {
        mainHandler.post {
            if (released) return@post
            runCatching(block)
        }
    }

    /** 只应答一次的 MethodChannel.Result；它的方法只在主线程上调用。 */
    private class OneShot(private val result: MethodChannel.Result) : MethodChannel.Result {
        private var done = false

        /** 以 PlatformException(code, reason token) 应答失败。 */
        fun fail(code: String, reason: String) {
            error(code, reason, null)
        }

        override fun success(result: Any?) {
            if (done) return
            done = true
            this.result.success(result)
        }

        override fun error(code: String, message: String?, details: Any?) {
            if (done) return
            done = true
            this.result.error(code, message, details)
        }

        override fun notImplemented() {
            if (done) return
            done = true
            result.notImplemented()
        }
    }
}
