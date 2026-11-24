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
 *
 * ── 授权：只动发送侧自己加上的那一半 ─────────────────────────────────────
 *  - pickSendFolder 不持久化任何授权，只借 Activity 的临时授权读出根目录名——
 *    选了却没暂存的选择不会留下任何持久授权；
 *  - stageSafTree 先看该 URI 是否已有持久的 READ 授权（同一目录也是接收保存目录时
 *    是 READ+WRITE）：没有才 takePersistableUriPermission(READ) 并复核，并记住是
 *    本次拿的；暂存结束（成功/失败/取消）时也只在“本次拿的”时才释放；
 *  - 进程若在 take 与 release 之间被杀，READ-only 授权留给 cleanupStaleStaging 回收：
 *    只回收“有读位、无写位、且不属于任何在跑暂存”的授权，带写位的一概不动。
 *
 * ── 线程 ───────────────────────────────────────────────────────────────
 * 通道回调在主线程。除两处之外——pickSendFolder 借临时授权读根目录名的那一次查询，
 * 和 stageSafTree 在主线程上做的几次授权调用（persistedUriPermissions /
 * takePersistableUriPermission）——所有提供器访问和暂存根下的每一次增删都在本类
 * 唯一的工作线程上串行执行，暂存、取消、清理因此不会交错。应答
 * （result.success/error）与 stagingProgress 事件一律回主线程，每个
 * MethodChannel.Result 只应答一次。
 *
 * ── 安全删除 ───────────────────────────────────────────────────────────
 * 绝不跟随符号链接：每个条目先经 android.system.Os.lstat 定型（不跟随）——链接只删
 * 链接本身；只有“真实目录”且 canonical 路径落在被删目录之内才递归进去；其余按文件
 * 删。File.isDirectory() 会跟随链接，从不拿它给未定型的条目分类。暂存根自己不是
 * 应用 no-backup 目录里的真实目录时什么都不删：ak-saf-staging 之外的东西永远不碰。
 *
 * ── 错误与日志 ─────────────────────────────────────────────────────────
 * 失败一律 PlatformException(code, 固定 reason token)；message 与日志里都不出现
 * URI、路径、目录或文件名。
 *
 * minSdk 24：不用 java.nio.file，文件操作只用 java.io.File 与 android.system.Os。
 */

import android.content.Context
import android.content.Intent
import android.database.Cursor
import android.net.Uri
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
        private const val STAGE_ID_LENGTH = 32

        /** 条目上限（文件 + 目录）。 */
        private const val MAX_ENTRIES = 100000L

        /** 相对路径上限（UTF-8 字节），与引擎 validate_rel_path 一致。 */
        private const val MAX_REL_PATH_BYTES = 1024

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

        /**
         * 全进程唯一的工作线程：暂存根下的每次增删和（除 pickSendFolder 那一次之外的）
         * 每次提供器访问都在这里串行执行。守护线程，不持有 Context，Activity 重建后
         * 被取消任务的清理仍在这里跑完。
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

    private class Job(
        val stageId: String,
        val treeUri: Uri,
        val owner: SafSendStager
    ) {
        /** 本次调用是否自己 take 了 READ 授权：是，结束时才释放。只在主线程上、排队之前定下来。 */
        var tookGrant: Boolean = false

        @Volatile
        var cancelled: Boolean = false
    }

    /** 枚举用的一帧：一个待展开的目录及其相对所选目录的路径（所选目录自己是空串）。 */
    private class Frame(val docId: String, val relPath: String)

    private class PlanFile(val relPath: String, val docUri: Uri)

    private class Plan(val rootName: String) {
        /** 子目录的相对路径（不含根本身），父目录总在子目录之前。 */
        val directories = ArrayList<String>()
        val files = ArrayList<PlanFile>()
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

    // ── 通道入口（一律在主线程调用）────────────────────────────────────────

    /**
     * pickSendFolder 的结果（OpenDocumentTree 回调）。不持久化任何授权：只校验
     * authority，并借 Activity 的临时授权读出根目录名。
     */
    fun onSendFolderPicked(treeUri: Uri?, result: MethodChannel.Result) {
        val answer = OneShot(result)
        try {
            if (treeUri == null) {
                answer.success(mapOf("status" to "cancelled"))
                return
            }
            if (!isTrustedAuthority(treeUri)) {
                answer.fail(CODE_PROVIDER_UNTRUSTED, "authority")
                return
            }
            val displayName = checkedRootName(treeUri)
            answer.success(
                mapOf(
                    "status" to "selected",
                    "treeUri" to treeUri.toString(),
                    "displayName" to displayName
                )
            )
        } catch (t: Throwable) {
            // 选择结果回调里绝不抛异常：失败一律转成契约里的 PlatformException。
            val failure = toFailure(t)
            logFailure(failure, t)
            answer.fail(failure.code, failure.reason)
        }
    }

    /**
     * stageSafTree(treeUri, stageId)。校验与授权在主线程做完，目录树的枚举与复制在
     * 工作线程上跑；应答在工作线程完全停下之后才回到主线程。
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

        // 先登记再拿授权：过期清理按“在跑的任务”判断一份授权归谁，拿授权与登记之间不能有空档。
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
        if (!acquireReadGrant(job, answer)) {
            // 已经应答，自己拿的授权也已放掉
            unregisterJob(job)
            return
        }

        synchronized(jobsLock) {
            // 另一个不同 id 的暂存还在跑：先取消它，新任务排在它的清理之后
            activeJobs.values.filter { it !== job }.forEach { it.cancelled = true }
        }
        try {
            WORKER.execute { runJob(job, answer) }
        } catch (t: Throwable) {
            unregisterJob(job)
            releaseGrantIfTaken(job)
            answer.fail(CODE_IO_FAILED, "io")
        }
    }

    /**
     * cancelStaging(stageId)：只置取消标志，不删任何东西——清理由正在跑的任务自己
     * 完成后再应答。没有匹配的任务时是空操作。
     */
    fun cancelStaging(stageId: String?) {
        if (stageId == null) return
        synchronized(jobsLock) { activeJobs[stageId]?.cancelled = true }
    }

    /**
     * cleanupStagedSend(stageId)：删掉已经交付的暂存副本，幂等（已经不在了也答 null）。
     */
    fun cleanupStagedSend(stageId: String?, result: MethodChannel.Result) {
        val answer = OneShot(result)
        if (stageId == null || !isValidStageId(stageId)) {
            answer.fail(CODE_INVALID_ID, "format")
            return
        }
        // 排在工作线程上正在跑的任务之后：轮到它时，那个任务的目录要么已经交付、
        // 要么已被它自己清掉。
        runOnWorker(answer) {
            if (!deleteStagedEntry(File(stagingRoot(), stageId))) {
                Log.w(TAG, "staged send directory could not be removed completely")
            }
            null
        }
    }

    /**
     * cleanupStaleStaging()：清掉以前各次运行留下的东西，答 {"removed": n}。
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
        synchronized(jobsLock) {
            activeJobs.values.filter { it.owner === this }.forEach { it.cancelled = true }
        }
    }

    // ── 工作线程：一次暂存 ─────────────────────────────────────────────────

    private fun runJob(job: Job, answer: OneShot) {
        val outcome: Outcome = try {
            Outcome.Ok(stage(job))
        } catch (t: Throwable) {
            Outcome.Failed(toFailure(t))
        }
        if (outcome is Outcome.Failed) {
            // 失败或取消：先把整个 <stageId> 目录删掉，最后才应答
            if (!deleteStagedEntry(File(stagingRoot(), job.stageId))) {
                Log.w(TAG, "stage directory left behind after a failed staging")
            }
        }
        releaseGrantIfTaken(job)
        unregisterJob(job)
        postToMain { deliver(answer, outcome) }
    }

    /** 枚举 → 复制 → 完成标记；返回 ok map。 */
    private fun stage(job: Job): Map<String, Any?> {
        // 排队期间就已经被取消的任务：什么都不读、什么都不建
        checkCancelled(job)
        val stagingRoot = stagingRoot()
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
        val rootName = checkedRootName(treeUri)
        val plan = Plan(rootName)

        val rootDocId = try {
            DocumentsContract.getTreeDocumentId(treeUri)
        } catch (t: Throwable) {
            // 不是 tree URI
            throw queryFailure(t)
        }
        val seenDocIds = HashSet<String>()
        seenDocIds.add(rootDocId)
        val pending = ArrayDeque<Frame>()
        pending.addLast(Frame(rootDocId, ""))

        while (pending.isNotEmpty()) {
            checkCancelled(job)
            val frame = pending.removeLast()
            var childCount = 0L
            val siblingKeys = HashSet<String>()
            val cursor = queryChildren(treeUri, frame.docId)
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

        if (mimeType == Document.MIME_TYPE_DIR) {
            plan.directories.add(relPath)
            pending.addLast(Frame(docId, relPath))
        } else {
            if (mimeType.isNullOrBlank()) throw StageFailure(CODE_UNSUPPORTED_DOCUMENT, "mime")
            plan.files.add(
                PlanFile(relPath, DocumentsContract.buildDocumentUriUsingTree(treeUri, docId))
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
     * 第二阶段：建目录、逐个文件复制，最后写完成标记。目标一律从暂存目录的 canonical
     * 路径拼出来：之后 absolutePath 与 canonicalPath 一旦不同，只可能是暂存目录之下有
     * 符号链接挡路。
     */
    private fun copyPlan(job: Job, stagingRoot: File, plan: Plan): CopyResult {
        val stageDir = File(stagingRoot, job.stageId)
        val noBackupCanonical = canonicalPathOf(appContext.noBackupFilesDir)
            ?: throw StageFailure(CODE_IO_FAILED, "containment")
        val rootCanonical = realDirectory(
            stagingRoot, noBackupCanonical + File.separator + STAGING_DIR_NAME
        )
        // 同一 id 的残留（上次中断留下的）必须先删干净：暂存目录只能是从零建起来的
        if (!deleteStagedEntry(stageDir)) throw StageFailure(CODE_IO_FAILED, "io")
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
            checkCancelled(job)
            val target = File(payloadDir, joinPaths(plan.rootName, file.relPath))
            checkTarget(target, stageCanonical)
            try {
                if (!target.createNewFile()) throw StageFailure(CODE_IO_FAILED, "create")
            } catch (t: Throwable) {
                throw stepFailure(t, "create")
            }
            val input = openInput(file.docUri)
            try {
                input.use { source ->
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
                }
            } catch (t: Throwable) {
                throw stepFailure(t, "read")
            }
            progress.filesCopied++
        }
        emitProgress(
            job, PHASE_COPY, plan.entryCount,
            progress.filesCopied, progress.bytesCopied, plan.totalEstimate
        )

        // 完成标记最后写：此刻所有文件都已关闭
        val marker = File(stageCanonical, MARKER_NAME)
        checkTarget(marker, stageCanonical)
        try {
            if (!marker.createNewFile()) throw StageFailure(CODE_IO_FAILED, "create")
        } catch (t: Throwable) {
            throw stepFailure(t, "create")
        }
        return CopyResult(File(payloadDir, plan.rootName).absolutePath, progress.bytesCopied)
    }

    /** 64 KiB 一块地复制；块与块之间、每个条目之间都看一眼取消标志。 */
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

    // ── 过期清理（工作线程）─────────────────────────────────────────────────

    /** cleanupStaleStaging 的工作线程部分：返回删掉的暂存根子项个数。 */
    private fun cleanupStale(): Long {
        var removed = 0L
        val stagingRoot = stagingRoot()
        when (lstatType(stagingRoot)) {
            EntryType.MISSING -> Unit   // 还没有任何暂存：无事可做
            EntryType.DIRECTORY -> {
                // 快照取在任务真正执行的时候：此刻工作线程上没有别的任务在改暂存根，
                // 表里有的只是正在跑或排在后面的任务。
                val activeIds = synchronized(jobsLock) { activeJobs.keys.toSet() }
                val children = stagingRoot.listFiles()
                if (children != null) {
                    for (child in children) {
                        if (!isStale(child, activeIds)) continue
                        if (deleteStagedEntry(child)) removed++
                    }
                }
            }
            else -> Log.w(TAG, "staging root is not a real directory; stale entries left alone")
        }
        releaseOrphanReadGrants()
        return removed
    }

    /**
     * 回收 READ-only 的持久授权（有读位、无写位）里不属于任何在跑暂存的那部分——以前
     * 某次进程中断留下的。接收保存目录的授权带写位，永远不动。
     *
     * 先读授权表、后取在跑任务的快照：暂存任务都是先登记再拿授权，所以快照之后才
     * 登记的任务，它拿的授权不会出现在先读到的表里；快照时已登记的任务则在表里被跳过。
     */
    private fun releaseOrphanReadGrants() {
        val persisted = try {
            appContext.contentResolver.persistedUriPermissions
        } catch (t: Throwable) {
            Log.w(TAG, "persistedUriPermissions unavailable; stale grants left alone")
            return
        }
        val activeUris = synchronized(jobsLock) { activeJobs.values.map { it.treeUri }.toSet() }
        for (permission in persisted) {
            if (!permission.isReadPermission || permission.isWritePermission) continue
            if (permission.uri in activeUris) continue
            releaseReadGrant(permission.uri)
        }
    }

    /** 过期规则：名字不是 id 格式 → 删；正在暂存的 id → 留；没有完成标记 → 删；标记超过 24 小时 → 删。 */
    private fun isStale(child: File, activeIds: Set<String>): Boolean {
        val id = child.name
        if (!isValidStageId(id)) return true
        if (id in activeIds) return false
        val marker = File(child, MARKER_NAME)
        val freshMarker = lstatType(marker) == EntryType.FILE &&
            System.currentTimeMillis() - marker.lastModified() < MARKER_MAX_AGE_MS
        return !freshMarker
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

    // ── 授权 ──────────────────────────────────────────────────────────────

    /** 该 EXACT URI 是否已有持久的 READ 授权（读写都有时也算）。 */
    private fun hasReadGrant(treeUri: Uri): Boolean = try {
        appContext.contentResolver.persistedUriPermissions.any { permission ->
            permission.uri == treeUri && permission.isReadPermission
        }
    } catch (t: Throwable) {
        false
    }

    /**
     * 授权：该 URI 已有持久的 READ 授权（同一目录也是接收保存目录时是 READ+WRITE）就
     * 原样使用、永不触碰；没有才自己 take READ 并复核。失败时已经用 [answer] 应答，
     * 自己拿的那份也已放掉，返回 false。
     */
    private fun acquireReadGrant(job: Job, answer: OneShot): Boolean {
        if (hasReadGrant(job.treeUri)) return true
        try {
            appContext.contentResolver.takePersistableUriPermission(
                job.treeUri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION
            )
        } catch (t: Throwable) {
            // 典型场景：没有可持久化的授权（Activity 的临时授权已失效），或持久化
            // 条数到了系统上限。
            Log.w(TAG, "takePersistableUriPermission(READ) failed: ${t.javaClass.simpleName}")
            answer.fail(CODE_PERMISSION_DENIED, grantFailureReason(t))
            return false
        }
        job.tookGrant = true
        if (hasReadGrant(job.treeUri)) return true
        // 复核不到：自己刚 take 的那份先放掉，再应答
        job.tookGrant = false
        releaseReadGrant(job.treeUri)
        answer.fail(CODE_PERMISSION_DENIED, "no-persisted-grant")
        return false
    }

    /** 结束时只释放本次调用自己 take 的那份 READ 授权。 */
    private fun releaseGrantIfTaken(job: Job) {
        if (job.tookGrant) releaseReadGrant(job.treeUri)
    }

    /** 任务结束（或没能排上队）时把它从在跑的表里去掉；只去掉它自己。 */
    private fun unregisterJob(job: Job) {
        synchronized(jobsLock) {
            if (activeJobs[job.stageId] === job) activeJobs.remove(job.stageId)
        }
    }

    private fun releaseReadGrant(uri: Uri) {
        try {
            appContext.contentResolver.releasePersistableUriPermission(
                uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION
            )
        } catch (t: Throwable) {
            // 放不掉的留给 cleanupStaleStaging；日志里不带 URI
            Log.w(TAG, "releasePersistableUriPermission failed: ${t.javaClass.simpleName}")
        }
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

    // ── 提供器查询 ────────────────────────────────────────────────────────

    /** 接收端拒绝的授权错误 → provider-security，其余查询失败 → query。 */
    private fun queryFailure(t: Throwable): StageFailure = when (t) {
        is StageFailure -> t
        is SecurityException -> StageFailure(CODE_PERMISSION_DENIED, "provider-security")
        else -> StageFailure(CODE_ENUMERATION_FAILED, "query")
    }

    /**
     * 所选目录根的显示名。提供器拒绝 → provider-security，其它查询失败 → query。
     * pickSendFolder 那一次在主线程（借 Activity 的临时授权），其余在工作线程。
     */
    private fun queryDisplayName(treeUri: Uri): String? = try {
        val rootDocId = DocumentsContract.getTreeDocumentId(treeUri)
        val rootUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, rootDocId)
        appContext.contentResolver.query(
            rootUri, arrayOf(Document.COLUMN_DISPLAY_NAME), null, null, null
        )?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(0) else null
        }
    } catch (t: Throwable) {
        throw queryFailure(t)
    }

    /** 某个目录的子项查询；失败直接抛 StageFailure。 */
    private fun queryChildren(treeUri: Uri, parentDocId: String): Cursor? = try {
        appContext.contentResolver.query(
            DocumentsContract.buildChildDocumentsUriUsingTree(treeUri, parentDocId),
            CHILD_PROJECTION,
            null,
            null,
            null
        )
    } catch (t: Throwable) {
        throw queryFailure(t)
    }

    private fun openInput(docUri: Uri): InputStream {
        val stream = try {
            appContext.contentResolver.openInputStream(docUri)
        } catch (t: Throwable) {
            throw stepFailure(t, "open")
        }
        if (stream == null) throw StageFailure(CODE_IO_FAILED, "open")
        return stream
    }

    // ── 名字规则 ──────────────────────────────────────────────────────────

    /**
     * 根目录显示名：读出并按名字规则校验，不可用直接抛 StageFailure。
     * 原始名字里的 `/` → separator，`.ak-staging` → receiver-reserved，其余违例一律
     * root-name。
     */
    private fun checkedRootName(treeUri: Uri): String {
        val name = queryDisplayName(treeUri)
        if (name != null && name.contains('/')) throw StageFailure(CODE_NAME_CONFLICT, "separator")
        if (name != null && asciiEqualsIgnoreCase(name, RECEIVER_RESERVED_NAME)) {
            throw StageFailure(CODE_NAME_CONFLICT, "receiver-reserved")
        }
        if (name == null || name.isBlank() || componentNameIssue(name) != null) {
            throw StageFailure(CODE_UNSUPPORTED_DOCUMENT, "root-name")
        }
        return name
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
        return null
    }

    /** C0（U+0000–001F）、DEL（U+007F）、C1（U+0080–009F）。 */
    private fun isControlChar(ch: Char): Boolean =
        ch <= '\u001F' || ch == '\u007F' || (ch >= '\u0080' && ch <= '\u009F')

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

    /** 复制阶段各步的失败：磁盘满 → enospc，提供器拒绝 → provider-security，其余按所在的那一步。 */
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

    /** 因果链上任何一层带着 ENOSPC / "No space left" 就算磁盘满。 */
    private fun isNoSpace(t: Throwable): Boolean {
        var current: Throwable? = t
        var depth = 0
        while (current != null && depth < 16) {
            val message = current.message
            if (message != null && (message.contains("ENOSPC") || message.contains("No space left"))) {
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
