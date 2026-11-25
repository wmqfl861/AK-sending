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
 *   <noBackupFilesDir>/ak-saf-staging/<stageId>/.owner                    本实例的令牌，建目录后最先写
 *   <noBackupFilesDir>/ak-saf-staging/.send-grants                        发送侧自己拿过的读授权（journal）
 *
 * ── 授权：只动发送侧自己加上的那一半，而且要有据可查 ─────────────────────
 *  - pickSendFolder 不持久化任何授权，只借 Activity 的临时授权读出根目录名——
 *    选了却没暂存的选择不会留下任何持久授权；
 *  - stageSafTree 先读授权表，看该 URI 是否已有持久的 READ 授权（同一目录也是接收
 *    保存目录时是 READ+WRITE）：有就原样使用、永不触碰。授权表读不出来时让暂存失败
 *    （grant-lookup）——猜成“没有”就可能把接收侧那份授权的读位拿走再放掉；
 *  - 没有时：先把 URI 记进 journal，再 takePersistableUriPermission(READ)，再复核；
 *  - 本应用的每一次持久授权操作都在同一个工作线程上串行执行：发送侧的读表、take、
 *    release，过期清理，以及接收侧 pickFolder 的 take(READ|WRITE)（经 takeReceiveGrant
 *    转进来）——两次授权操作从不交错；
 *  - journal 里一个 URI 的那一行，在最后一个用着它的暂存任务从在跑的表里去掉时了结
 *    （J15），不论那一次自己有没有 take 过——上一回留下的行也在此时了结，包括排队
 *    之后才被取消的任务；任务结束和过期清理走的是同一段判定（sweepGrantStep）。
 *    了结时重读该 URI 此刻的权限位：只有 READ-only（没有
 *    写位）才释放；带着写位（接收侧 meanwhile 在同一个 URI 上拿了 READ+WRITE，而
 *    Android 持久化的是位的组合）就什么都不放——这份授权现在是接收侧的。两种情况
 *    都把 journal 里那一行划掉（一行不剩时 journal 文件也删掉）；授权表读不出来、
 *    或释放没得到确认，则留着这一行；
 *  - 放完立刻再读一次权限位：写位在、读位没了，说明接收侧就在“读”与“放”的这一瞬
 *    之间升成了 READ+WRITE，而我们刚把它的读位剥掉——把 READ 重新 take 回来（给不
 *    排在本工作线程上串行的 take 兜底）。没 take 成，或 take 完复核还不是 READ+WRITE，
 *    这一行留着，下一次过期清理再试；
 *  - 进程若在 take 与 release 之间被杀，cleanupStaleStaging 对 journal 里不属于任何
 *    在跑暂存的 URI 用同一段逻辑：READ-only 才释放；已经是 READ+WRITE 的不碰授权、
 *    只划掉那一行；不在 journal 里的一概不碰。
 *
 * ── 放行闸门：接收侧的选择与授权了结互不穿插（G15）──────────────────────
 *  Android 7–10 上，一次落在“系统已把接收侧的目录授给本应用”与“接收侧 take 持久
 *  化”之间的 release(READ)，会让那个 take 失败。所以不分 API 版本，只有一条规则：
 *  接收侧的选择先登记、再启动选择器，take 跑过或者没有结果才注销；登记着的时候没
 *  有任何一次了结能获准（行留在 journal 里）；已经获准的那一次先做完，选择器的启
 *  动等它结束再发。判定都是 jobsLock 下很短的状态操作；Binder 调用——主线程上的
 *  launch、工作线程上的 release——都在锁外。
 *
 * ── 实例之外什么都不留 ─────────────────────────────────────────────────
 * 令牌属于 stager 实例，而不是进程（O16）：每个实例建立时铸一枚登记进伴生对象，
 * shutdown 时退掉。每个暂存目录一建起来就先写入本实例的令牌（<id>/.owner，在
 * payload 之外）。过期清理删掉：名字不是 id 的条目、没有完成标记的半截目录（不在
 * 跑的）、以及 .owner 读得出、却不是任何一个活实例令牌的已交付目录——建它的实例
 * 已经退休，它的副本不论多新都是垃圾。旧引擎即使已经销毁，Rust 那边的读也可能还
 * 没停：那一次发送会失败，而它的界面早已不在，这一点是接受的。归属读不出来——
 * lstat 出错、打开或读取抛了、读到的字节比文件持有的少——一律按 UNKNOWN 保留，
 * 日志里只有 errno 名或异常类名。带着活实例令牌的已交付目录从不碰：那个实例的发
 * 送可能还在读它。没有按年龄过期的规则。实例退休时自己再排一次过期清理：别的实
 * 例启动时的那一次可能跑在退休之前。
 *
 * ── 线程与取消 ─────────────────────────────────────────────────────────
 * 通道回调在主线程；主线程上不做任何 ContentResolver 调用，也从不在锁上等。所有
 * 提供器与授权调用、暂存根下的每一次增删都在本类唯一的工作线程上串行执行，暂存、
 * 取消、清理因此不会交错。应答（result.success/error）与 stagingProgress 事件一律
 * 回主线程，每个 MethodChannel.Result 只应答一次。
 * 每个暂存任务自带一个 CancellationSignal，它的查询和打开文件都带着它。cancelStaging
 * 只在调用线程上置取消标志；取消信号、关掉任务打开着的输入流，都在一条专用的小
 * 线程上做（尽力而为）——阻塞中的查询/打开/读取因此能停下来，而主线程不被拖住。
 * 建目录失败按 errno 归类：ENOSPC / EDQUOT 是空间不足（enospc），其余是建不出来。
 *
 * ── 复制的保真边界 ─────────────────────────────────────────────────────
 * 所有文件复制完之后、写完成标记之前，每个计划过的目录（含根）都再查一遍，子项
 * document id 集合不同即失败（source-changed）；每个文件复制完都与已知长度核对，
 * 不同即失败（size-mismatch）。核对不到的只有一种情况：文件内容在长度和成员关系
 * 都不变的情况下被改写——那时复制下来的是读到时的内容。
 *
 * ── 安全删除 ───────────────────────────────────────────────────────────
 * 绝不跟随符号链接：每个条目先经 android.system.Os.lstat 定型（不跟随）——链接和
 * 其它非常规条目（管道、套接字、设备）只删它本身；只有“真实目录”且 canonical 路径
 * 落在被删目录之内才递归进去；其余按文件删。lstat 出错（ENOENT 之外）是单独一种
 * 结果，从不折进任何一种类型：这样的条目留着（T17）。只有普通文件才会被打开：
 * .owner、完成标记、journal 及其重写用的临时文件；journal 的路径上若不是普通文件，
 * 既不打开也不删除，里面的行一律不动。File.isDirectory() 会跟随链接，从不拿它给未
 * 定型的条目分类。暂存根自己不是应用 no-backup 目录里的真实目录时什么都不删：
 * ak-saf-staging 之外的东西永远不碰。
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
import android.os.Looper
import android.os.SystemClock
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.util.Log
import io.flutter.plugin.common.MethodChannel
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.security.SecureRandom
import java.util.Locale
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * 发送侧 SAF 目录暂存器：不碰任何 Android UI，只持有 Context、通道和主线程 Handler。
 * MainActivity 在主线程上调用 [onSendFolderPicked]、[stageSafTree]、[cancelStaging]、
 * [cleanupStagedSend]、[cleanupStaleStaging]、[takeReceiveGrant]、[settleJournal]、
 * [persistedGrants] 与 [shutdown]；接收侧选择的登记（G15）是伴生对象里的
 * [beginReceivePick]、[launchReceivePick]、[endReceivePick]——Activity 还没有 stager
 * 实例时也调得到。
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
        private const val OWNER_NAME = ".owner"
        private const val JOURNAL_NAME = ".send-grants"
        private const val JOURNAL_TEMP_NAME = ".send-grants.tmp"
        private const val STAGE_ID_LENGTH = 32

        /** 条目上限（文件 + 目录）。 */
        private const val MAX_ENTRIES = 100000L

        /** 相对路径上限（UTF-8 字节），与引擎 validate_rel_path 一致。 */
        private const val MAX_REL_PATH_BYTES = 1024

        /** 单个路径分量的上限（UTF-8 字节）：暂存所在文件系统的 NAME_MAX，再长的名字建不出来。 */
        private const val MAX_COMPONENT_BYTES = 255

        /** .owner 只读这么多个字节：令牌连换行是 33 字节，64 封顶，别的东西不读。 */
        private const val OWNER_READ_LIMIT = 64

        /** 空间预检的余量。 */
        private const val FREE_SPACE_MARGIN_BYTES = 16L * 1024 * 1024

        private const val COPY_BUFFER_BYTES = 64 * 1024
        private const val PROGRESS_INTERVAL_MS = 150L

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

        /** 读授权表失败时答给 Activity 的 PlatformException code（persistedGrants）。 */
        private const val CODE_CHANNEL_ERROR = "error"

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

        /**
         * 取消的收尾专用的小线程：取消方只置标志，CancellationSignal.cancel() 和关输入流
         * 都在这里做——主线程从不在输入流的锁上等。守护线程，不持有 Context。
         */
        private val CANCELLER: ExecutorService = Executors.newSingleThreadExecutor { runnable ->
            Thread(runnable, "ak-saf-stager-cancel").apply { isDaemon = true }
        }

        /**
         * 任务表与放行闸门共用的那把锁：下面几样状态只在它之下读写（主线程与工作线程都会来）。
         * 锁里只做很短的状态操作，从不在里面发 Binder 调用。
         */
        private val jobsLock = Any()

        /** 进行中（运行或排队）的暂存任务，按 stageId 索引。 */
        private val activeJobs = HashMap<String, Job>()

        /**
         * 活着的 stager 实例的令牌（O16）：实例建立时加进来，shutdown 时去掉。过期清理只凭它
         * 判断一个已交付的副本还有没有人可能在读——不看“是不是调用清理的那个实例的”。
         */
        private val liveTokens = HashSet<String>()

        /** 登记着的接收侧选择（G15）：同一时刻至多一个，没有就是 null。 */
        private var receivePick: ReceivePick? = null

        /** 本应用有一次授权了结在“已获准”与“做完”之间（释放、重读、恢复 take、判定都在其中）。 */
        private var settlementRunning = false

        /** 给被推迟的 launch 用的主线程 Handler：不依赖任何实例，也不持有 Context。 */
        private val MAIN_HANDLER: Handler by lazy { Handler(Looper.getMainLooper()) }

        // ── 接收侧选择的登记（G15；beginReceivePick 与 launchReceivePick 只在主线程调用，
        //    endReceivePick 主线程与工作线程都会调；这里不做任何 Binder 调用）────────────

        /** 铸一枚实例令牌：16 个随机字节，写成 32 位十六进制。 */
        private fun mintToken(): String {
            val bytes = ByteArray(16)
            SecureRandom().nextBytes(bytes)
            return bytes.joinToString("") { Integer.toHexString(it.toInt() and 0xff).padStart(2, '0') }
        }

        /**
         * 登记一次接收侧选择，返回它的句柄。选择器启动之前就登记：系统只可能在启动之后把
         * 接收侧的目录授给本应用，所以“登记着”就等于“授权可能已经在路上，而 take 还没跑”。
         * 已经有一个登记着时抛 IllegalStateException（Activity 先答 pick_in_progress，不会走到这）。
         */
        fun beginReceivePick(): ReceivePick = synchronized(jobsLock) {
            check(receivePick == null) { "a receive pick is already registered" }
            ReceivePick().also { receivePick = it }
        }

        /**
         * 启动接收侧的选择器。此刻没有授权了结在跑：就在调用线程上直接调 [launch]；有：把它
         * 存进这次选择，等那一次了结结束，由工作线程送回主线程再发（见 [endSettlement]）。
         * [pick] 已经不是登记着的那个、或者已经结束：空操作。launch（startActivity）在锁外。
         */
        fun launchReceivePick(pick: ReceivePick, launch: () -> Unit) {
            val launchNow = synchronized(jobsLock) {
                if (receivePick !== pick || pick.ended) return
                if (settlementRunning) {
                    pick.awaitingLaunch = launch
                    false
                } else {
                    true
                }
            }
            if (launchNow) launch()
        }

        /** 结束一次接收侧选择：它是登记着的那个就去掉登记。不论是不是都标成已结束；重复调用无副作用。 */
        fun endReceivePick(pick: ReceivePick) {
            synchronized(jobsLock) {
                pick.ended = true
                if (receivePick === pick) {
                    receivePick = null
                    // 还留着一次被推迟的 launch：登记都没了，它不再有效
                    pick.awaitingLaunch = null
                }
            }
        }

        // ── 放行闸门，工作线程一侧（只在工作线程调用）───────────────────────────

        /**
         * 一次授权了结的开始：有接收侧选择登记着就不获准——调用方把这一行留着，原路返回；
         * 否则记下“有一次在跑”。从获准到 [endSettlement] 之间，release、重读、恢复 take 和
         * 判定都在这一段里。
         */
        private fun beginSettlement(): Boolean = synchronized(jobsLock) {
            if (receivePick != null) return false
            settlementRunning = true
            true
        }

        /** 一次授权了结的结束（放在 finally 里）：记下“不在跑”，并把一直等着它的那一次 launch 送回主线程。 */
        private fun endSettlement() {
            val deferred: Pair<ReceivePick, () -> Unit>? = synchronized(jobsLock) {
                settlementRunning = false
                val pick = receivePick
                val launch = pick?.awaitingLaunch
                if (pick != null) pick.awaitingLaunch = null
                if (pick == null || launch == null) null else pick to launch
            }
            if (deferred != null) MAIN_HANDLER.post { launchFromMainThread(deferred.first, deferred.second) }
        }

        /**
         * 被推迟的 launch 回到主线程：再走一遍 [launchReceivePick]——登记还在、又没有了结在跑
         * 才真的启动（它判的本就是“此刻能不能启动”）。这一次调用外面没有 Activity 的 try/catch
         * 兜着：抛了只记异常类名，并把这次选择结束掉，不让主线程炸掉。
         */
        private fun launchFromMainThread(pick: ReceivePick, launch: () -> Unit) {
            try {
                launchReceivePick(pick, launch)
            } catch (t: Throwable) {
                Log.w(TAG, "deferred picker launch failed: ${t.javaClass.simpleName}")
                endReceivePick(pick)
            }
        }
    }

    /** 暂存失败：code 是契约里的 PlatformException code，reason 是固定 reason token。 */
    private class StageFailure(val code: String, val reason: String) : Exception(reason)

    private sealed class Outcome {
        class Ok(val result: Any?) : Outcome()
        class Failed(val failure: StageFailure) : Outcome()
    }

    /**
     * 一次接收侧选择的登记（G15）。Activity 只把它当不透明的句柄用：两个字段都只在 jobsLock
     * 之下、由本类读写。
     */
    class ReceivePick internal constructor() {
        /** 推迟到在跑的那次授权了结结束之后才发的 launch；结束那一刻由工作线程取走、送回主线程。 */
        internal var awaitingLaunch: (() -> Unit)? = null

        /** 已经结束（take 跑过了，或者没有结果）：此后对它的操作都是空操作。 */
        internal var ended = false
    }

    private class Job(val stageId: String, val treeUri: Uri, val owner: SafSendStager) {
        /** 本任务的取消信号：它的查询和打开文件都带着它，取消时阻塞中的调用立刻返回。 */
        val signal = CancellationSignal()

        /** 当前打开着的输入流。取消方要把它关掉，同一个流只关一次（见 [closeOpenStream]）。 */
        @Volatile
        private var openStream: InputStream? = null
        private val streamLock = Any()

        // 只在工作线程上读写。
        /** 本任务是否已经建起了自己的 <stageId> 目录：是，失败时才由它负责删掉。 */
        var ownsStageDir: Boolean = false

        @Volatile
        var cancelled: Boolean = false

        fun attachStream(stream: InputStream) {
            openStream = stream
        }

        /** 关这一路输入流的唯一入口：工作线程收尾和取消方都走这里，同一个流只关一次。 */
        fun closeOpenStream() {
            synchronized(streamLock) {
                val stream = openStream ?: return
                openStream = null
                try {
                    stream.close()
                } catch (t: Throwable) {
                    // 尽力而为：关不掉不影响已经写到输出流里的内容。日志里只有异常类名。
                    Log.w(TAG, "closing the staging input stream failed: ${t.javaClass.simpleName}")
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

    /**
     * lstat 定型的结果：不存在 / 普通文件 / 真实目录 / 符号链接 / 其它（管道、套接字、字符与块
     * 设备）。lstat 出错（ENOENT 之外）是单独一种结果——[lstatType] 返回 null——从不折进这
     * 里任何一种。
     */
    private enum class EntryType { MISSING, FILE, DIRECTORY, LINK, OTHER }

    /** 一个已交付暂存目录的归属（O16）：三个回答，不是两个。只有 GONE 才让它过期。 */
    private enum class Ownership { ALIVE, GONE, UNKNOWN }

    /** 这个 id 的名字下已经有东西时怎么处置（O16）。 */
    private enum class IdReuse { PROCEED, IN_USE }

    /** 本实例的令牌（O16）：本实例建的每个暂存目录里都写一份（<id>/.owner）。 */
    private val instanceToken: String = mintToken()

    init {
        // 令牌一建立就登记成活的：本实例的副本从建目录那一刻起受保护，直到 shutdown 退休
        synchronized(jobsLock) { liveTokens.add(instanceToken) }
    }

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
            // 排不进工作线程（不会发生）：它从没跑过。它若是这个 URI 的最后一个使用者，journal
            // 里那一行也由它的注销来了结（J15）
            if (unregisterJob(job)) settleJournal()
            answer.fail(CODE_IO_FAILED, "io")
        }
    }

    /**
     * cancelStaging(stageId)：只在调用线程上置取消标志；取消该任务的 CancellationSignal、
     * 关掉它打开着的输入流，都在专用的小线程上做（主线程不等）。不删任何东西，清理
     * 由正在跑的任务自己完成后再应答。没有匹配的任务时是空操作。
     */
    fun cancelStaging(stageId: String?) {
        if (stageId == null) return
        val job = synchronized(jobsLock) { activeJobs[stageId] } ?: return
        cancelJob(job)
    }

    /**
     * cleanupStagedSend(stageId)：删掉已经交付的暂存副本，答 {removed: true}（已经不在了
     * 也算）或 {removed: false}（有东西删不掉）。调用到达时这个 id 就已登记（在跑或
     * 排队）：立刻答 busy，什么都不排——排在工作线程上的清理会等那次暂存跑完，然后
     * 把它刚交付的副本删掉。
     */
    fun cleanupStagedSend(stageId: String?, result: MethodChannel.Result) {
        val answer = OneShot(result)
        if (stageId == null || !isValidStageId(stageId)) {
            answer.fail(CODE_INVALID_ID, "format")
            return
        }
        val registered = synchronized(jobsLock) { activeJobs.containsKey(stageId) }
        if (registered) {
            answer.fail(CODE_IO_FAILED, "busy")
            return
        }
        runOnWorker(answer) {
            // 调用之后才登记的同 id 任务排在这后面，它什么都还没建；这里照样不动它，答 busy。
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
     * Activity 即将销毁。本实例退休（O16）：它的令牌不再是活的，并且再排一次过期清理——没有
     * 应答要送；别的实例启动时的那一次清理可能跑在退休之前，把本实例的副本当成活的留了下
     * 来。本实例还在跑的暂存全部取消（清理仍在工作线程上完成），之后不再向通道应答或发事件。
     */
    fun shutdown() {
        released = true
        val mine = synchronized(jobsLock) {
            liveTokens.remove(instanceToken)
            activeJobs.values.filter { it.owner === this }
        }
        mine.forEach { cancelJob(it) }
        try {
            WORKER.execute { runCatching { cleanupStale() } }
        } catch (t: Throwable) {
            // 工作线程排不进去（不会发生）：行留在 journal 里，副本留给下一次清理
        }
    }

    /**
     * 接收侧 pickFolder 的 takePersistableUriPermission(READ|WRITE)（批次 Z 复审 N9、G15）：本应用
     * 的每一次持久授权操作都排在本类的工作线程上串行执行，和发送侧的 take/release、过期清理
     * 互不交错。排到工作线程之后：[pick] 还是登记着的那个、也没有结束，才 take（排到之前
     * Activity 被销毁，就什么都不拿）；take 失败只记异常类名，不改变 pickFolder 的应答。然后
     * 把它从登记里去掉、了结 journal（J15），最后回主线程调用 [onDone]。
     *
     * [onDone] 至多跑一次，而且只在主线程上跑：工作线程排不进去时也是先结束这次选择，再回
     * 主线程，从不在调用线程上直接调。
     */
    fun takeReceiveGrant(pick: ReceivePick, treeUri: Uri, onDone: () -> Unit) {
        try {
            WORKER.execute {
                runCatching {
                    val live = synchronized(jobsLock) { receivePick === pick && !pick.ended }
                    if (live) {
                        try {
                            appContext.contentResolver.takePersistableUriPermission(
                                treeUri,
                                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                            )
                        } catch (t: Throwable) {
                            // 不改变返回值：URI 本身当前可用；持久化是否生效由 listSafPersisted 复核。
                            Log.w(TAG, "takePersistableUriPermission(READ|WRITE) failed: ${t.javaClass.simpleName}")
                        }
                    }
                    endReceivePick(pick)
                    releaseJournaledGrants()
                }.onFailure { Log.w(TAG, "receive take wrap-up threw: ${it.javaClass.simpleName}") }
                postToMain { onDone() }
            }
        } catch (t: Throwable) {
            // 工作线程排不进去（不会发生）：授权没拿，这次选择就地结束，应答仍然回主线程
            endReceivePick(pick)
            postToMain { onDone() }
        }
    }

    /**
     * 排一次 journal 的了结（J15），没有应答要送。接收侧的选择没有结果就结束时调用：先前因为
     * 它登记着而留下的行，现在可以了结了。
     */
    fun settleJournal() {
        try {
            WORKER.execute {
                runCatching { releaseJournaledGrants() }
                    .onFailure { Log.w(TAG, "grant settlement threw: ${it.javaClass.simpleName}") }
            }
        } catch (t: Throwable) {
            // 工作线程排不进去（不会发生）：行留在 journal 里，下一次清理再了结
        }
    }

    /**
     * 读一次持久授权表，给 Activity 的只读查询用（G15）。读表是 Binder 调用，放在工作线程上——
     * 和其它每一次授权操作在同一线程上串行，读到的不会是某次 take/release 做到一半的样子。
     * [build] 把表变成应答（也在工作线程上求值，抛了同样要应答），应答回主线程，至多一次。
     * 读表抛了答 result.error("error", "<异常类名>: persistedUriPermissions failed")："error" 是
     * 这件事过去在主线程上抛出时，经通道浮出来的 code。
     */
    fun persistedGrants(result: MethodChannel.Result, build: (List<UriPermission>) -> Any?) {
        val answer = OneShot(result)
        try {
            WORKER.execute {
                val built: Any? = try {
                    build(appContext.contentResolver.persistedUriPermissions)
                } catch (t: Throwable) {
                    Log.w(TAG, "persistedUriPermissions failed: ${t.javaClass.simpleName}")
                    postToMain { answer.fail(CODE_CHANNEL_ERROR, "${t.javaClass.simpleName}: persistedUriPermissions failed") }
                    return@execute
                }
                postToMain { answer.success(built) }
            }
        } catch (t: Throwable) {
            // 工作线程排不进去（不会发生）；实例已经销毁时引擎不在了，什么都不答
            if (!released) answer.fail(CODE_CHANNEL_ERROR, "${t.javaClass.simpleName}: persisted grant lookup not queued")
        }
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
        // 成功时完成标记已经写下，失败时目录已经清掉。先从在跑的表里去掉，再看它是不是这个 URI 的
        // 最后一个使用者：是，才了结 journal 里那一行（J15）——必须先去掉，了结才认得出“没人用了”
        val lastUser = unregisterJob(job)
        runCatching { releaseOwnedGrant(job, lastUser) }
            .onFailure { Log.w(TAG, "grant release after staging threw: ${it.javaClass.simpleName}") }
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
        // 这个 id 的名字下已经有交付过的副本，或者读不出它是什么：拒绝复用，什么都不删（N6、O16）
        refuseReusedId(File(stagingRoot, job.stageId))?.let { throw it }
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
        // 目录一建起来、别的什么都还没放进去，先写下本实例的令牌
        writeOwnerFile(stageCanonical)
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
                        // journal 不是暂存目录：过期清理从不删它，也不把它算进删掉的个数里。它若不是
                        // 普通文件，了结 journal 那一段（下面）会记下类型，里面的行一律不动（T17）
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
     * 过期清理对 journal 里记着、却不属于任何在跑暂存的每一个 URI 了结那份授权（批次 Z
     * 复审 N10）——与发送侧结束时是同一段（见 [releaseJournaledRead]），而且按它此刻的权限位
     * （每个 URI 现读，不靠早先的快照）：
     *  - READ-only（有读位、无写位）→ 释放；放完立刻重读，该把 READ take 回来就 take 回来；
     *    确认之后才划掉这一行；
     *  - 写位在、读位不在 → 上一次放了之后没能把 READ take 回来（那一行因此还留着）：现在再试；
     *  - 已经是 READ+WRITE（接收侧的）→ 授权不碰，只划掉这一行；授权不在了 → 只划掉这一行。
     * 不在 journal 里的授权一概不碰。读不到权限位、或哪一步没得到确认，这一行就留着。
     *
     * 每一个 URI 走的都是那唯一的一条路（[settleJournalLine]），因此都在放行闸门之内（G15）：
     * 有接收侧选择登记着就跳过，行留着，等它的 take 跑过之后、或者它没有结果结束时再了结。
     * journal 不是普通文件时记下类型，里面的行一律不动（T17）。
     */
    private fun releaseJournaledGrants() {
        if (lstatType(stagingRoot()) == EntryType.MISSING) return
        val lines = journalLinesOrNull() ?: return
        if (lines.isEmpty()) return
        val activeUris = synchronized(jobsLock) { activeJobs.values.map { it.treeUri }.toSet() }

        for (line in lines.distinct()) {
            val uri = Uri.parse(line)
            // 在跑的暂存还用着：行和授权都留给它自己处理
            if (uri in activeUris) continue
            settleJournalLine(uri)
        }
    }

    /**
     * 过期判定用到的几样事实（只在工作线程上调用）：名字、是否在跑、条目本身的类型；是真实目录才
     * 看完成标记，交付过的才读归属。读不出来的在纯判定里都是“留着”。
     */
    private fun isStale(child: File, activeIds: Set<String>): Boolean {
        val entryType = lstatType(child)
        val nameIsId = isValidStageId(child.name)
        val markerType = if (entryType == EntryType.DIRECTORY) lstatType(File(child, MARKER_NAME)) else null
        val ownership = if (nameIsId && markerType == EntryType.FILE) readOwnership(child) else null
        return isStaleByRules(
            nameIsId = nameIsId,
            isActive = child.name in activeIds,
            entryType = entryType,
            markerType = markerType,
            ownership = ownership
        )
    }

    /**
     * 过期规则（纯判定；“实例之外什么都不留”）：
     *  - 条目定型失败（lstat 出错，ENOENT 之外）→ 留着：删也无从下手；
     *  - 名字不是 id 格式 → 过期；
     *  - 正在暂存（在跑或排队）的 id → 不碰；
     *  - 不是真实目录（链接、其它非常规条目，或顶着 id 名字的文件）→ 过期，按它自己删——
     *    链接后面的标记保不住链接；
     *  - 完成标记定型失败 → 这个目录留着；
     *  - 没有完成标记，或标记不是普通文件（本应用的 stager 不会这么写）→ 半截目录，过期
     *    （中断的那一次留下的）；
     *  - 交付过的目录，归属是 GONE（.owner 缺失、不是普通文件，或内容不是任何一个活实例的
     *    令牌）→ 过期：建它的实例已经退休，副本不论多新都是垃圾；
     *  - 交付过、归属是 ALIVE → 从不碰：那个实例的发送可能还在读它；UNKNOWN（这次读不出来）
     *    → 留着。一次读不出归属不能变成“不是活实例的”。
     */
    private fun isStaleByRules(
        nameIsId: Boolean,
        isActive: Boolean,
        entryType: EntryType?,
        markerType: EntryType?,
        ownership: Ownership?
    ): Boolean {
        if (entryType == null) return false
        if (!nameIsId) return true
        if (isActive) return false
        if (entryType != EntryType.DIRECTORY) return true
        if (markerType == null) return false
        if (markerType != EntryType.FILE) return true
        return ownership == Ownership.GONE
    }

    /**
     * 读一个已交付暂存目录的归属（O16；只在工作线程上调用）。先定型：.owner 不在，或者不是
     * 普通文件 → GONE；lstat 出错（ENOENT 之外）→ UNKNOWN。是普通文件才打开，至多读
     * [OWNER_READ_LIMIT] 个字节，再拿字节数和 lstat 时的大小、内容一起判定（[ownershipVerdict]）。
     * 日志里只有 errno 名或异常类名，不带 id。
     */
    private fun readOwnership(stageDir: File): Ownership {
        val owner = File(stageDir, OWNER_NAME)
        val stat = try {
            Os.lstat(owner.path)
        } catch (e: ErrnoException) {
            if (e.errno == OsConstants.ENOENT) return Ownership.GONE
            Log.w(TAG, "owner file cannot be typed (${errnoName(e.errno)}); stage directory kept")
            return Ownership.UNKNOWN
        } catch (t: Throwable) {
            Log.w(TAG, "owner file cannot be typed (${t.javaClass.simpleName}); stage directory kept")
            return Ownership.UNKNOWN
        }
        if (!OsConstants.S_ISREG(stat.st_mode)) return Ownership.GONE
        val read: Pair<Int, String> = try {
            FileInputStream(owner).use { input ->
                val buffer = ByteArray(OWNER_READ_LIMIT)
                var count = 0
                while (count < buffer.size) {
                    val got = input.read(buffer, count, buffer.size - count)
                    if (got <= 0) break
                    count += got
                }
                count to String(buffer, 0, count, Charsets.UTF_8)
            }
        } catch (t: Throwable) {
            Log.w(TAG, "owner file cannot be read (${t.javaClass.simpleName}); stage directory kept")
            return Ownership.UNKNOWN
        }
        val live = synchronized(jobsLock) { liveTokens.toSet() }
        return ownershipVerdict(stat.st_size, read.first, read.second, live)
    }

    /**
     * 归属判定（纯判定）。读到的字节数必须正好是 lstat 时文件的大小：少了是短读，多了是文件在
     * lstat 之后变了——都是 UNKNOWN；读全了才拿完整内容去对活实例的令牌。
     */
    private fun ownershipVerdict(size: Long, read: Int, content: String, live: Set<String>): Ownership {
        if (read.toLong() != size) return Ownership.UNKNOWN
        return if (ownerTokenMatches(content, live)) Ownership.ALIVE else Ownership.GONE
    }

    /**
     * .owner 的内容与活实例令牌是否对得上（纯判定）：只认去掉首尾空白后完全相等的那一行；
     * 空白内容什么都不认（令牌实际上总是 32 位十六进制，这里只是不留空档）。
     */
    private fun ownerTokenMatches(ownerContent: String?, tokens: Set<String>): Boolean =
        ownerContent != null && ownerContent.isNotBlank() && ownerContent.trim() in tokens

    /**
     * errno 的名字，给日志用（不带 id、不带路径）。自己列一份：应用私有目录里 lstat、打开、
     * 读取碰得到的就是这些，哪一个 API 版本上都调得到；不在表里的记成 "errno <n>"。
     */
    private fun errnoName(errno: Int): String = when (errno) {
        OsConstants.EACCES -> "EACCES"
        OsConstants.EBUSY -> "EBUSY"
        OsConstants.EFAULT -> "EFAULT"
        OsConstants.EIO -> "EIO"
        OsConstants.ELOOP -> "ELOOP"
        OsConstants.ENAMETOOLONG -> "ENAMETOOLONG"
        OsConstants.ENOENT -> "ENOENT"
        OsConstants.ENOMEM -> "ENOMEM"
        OsConstants.ENOTDIR -> "ENOTDIR"
        OsConstants.EOVERFLOW -> "EOVERFLOW"
        OsConstants.ESTALE -> "ESTALE"
        OsConstants.ETIMEDOUT -> "ETIMEDOUT"
        else -> "errno $errno"
    }

    /** 往刚建好的暂存目录里写 <id>/.owner（本实例的令牌）；写不进去暂存就失败。 */
    private fun writeOwnerFile(stageCanonical: String) {
        val owner = File(stageCanonical, OWNER_NAME)
        checkTarget(owner, stageCanonical)
        try {
            if (!owner.createNewFile()) throw StageFailure(CODE_IO_FAILED, "create")
            FileOutputStream(owner).use { out ->
                out.write((instanceToken + "\n").toByteArray(Charsets.UTF_8))
                out.flush()
                out.fd.sync()
            }
        } catch (t: Throwable) {
            throw stepFailure(t, "create")
        }
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
            // 普通文件、符号链接、其它非常规条目（不管指向哪里）：只删条目本身，从不打开，
            // 链接的目标永远不碰
            EntryType.FILE, EntryType.LINK, EntryType.OTHER -> removeEntry(entry)
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
                    EntryType.FILE, EntryType.LINK, EntryType.OTHER -> if (!removeEntry(child)) complete = false
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
     * 不跟随链接地给条目定型（lstat）。条目不存在 → MISSING；类型读不到（ENOENT 之外的错误）→
     * null，调用方一律按“不可碰”处理。File.isDirectory() 会跟随链接，不用它分类。
     */
    private fun lstatType(file: File): EntryType? = try {
        typeOfMode(Os.lstat(file.path).st_mode)
    } catch (e: ErrnoException) {
        if (e.errno == OsConstants.ENOENT) EntryType.MISSING else null
    }

    /**
     * st_mode → 类型（纯判定）。只有 S_ISREG 才是 FILE：管道、套接字、字符与块设备都是 OTHER，
     * 与链接同样对待——只删它本身，从不打开、从不进入（T17）。
     */
    private fun typeOfMode(mode: Int): EntryType = when {
        OsConstants.S_ISLNK(mode) -> EntryType.LINK
        OsConstants.S_ISDIR(mode) -> EntryType.DIRECTORY
        OsConstants.S_ISREG(mode) -> EntryType.FILE
        else -> EntryType.OTHER
    }

    /** 日志里写的类型名；定型失败（null）记成 untypable。 */
    private fun typeName(type: EntryType?): String = type?.name?.lowercase(Locale.ROOT) ?: "untypable"

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
     * journal 必须是暂存根下的一个普通文件（或还不存在）：是则返回它，否则记一条带类型的日志
     * 返回 null——不打开它，也不删它（T17）。暂存根不可信或建不出来时抛 StageFailure。
     */
    private fun journalFile(): File? {
        trustedStagingRoot(stagingRoot())
        val journal = File(stagingRoot(), JOURNAL_NAME)
        when (val type = lstatType(journal)) {
            EntryType.MISSING, EntryType.FILE -> Unit
            null -> {
                Log.w(TAG, "grant journal cannot be typed; it is left alone")
                return null
            }
            else -> {
                Log.w(TAG, "grant journal is not a regular file (${typeName(type)}); it is left alone")
                return null
            }
        }
        return journal
    }

    /**
     * journal 当前的各行；没有 journal 时是空表。journal 建不出来、读不出来、或者不是普通文件
     * （记下类型）：返回 null——里面的行一律不动。只打开普通文件。
     */
    private fun journalLinesOrNull(): List<String>? {
        val journal: File = try {
            journalFile()
        } catch (e: StageFailure) {
            Log.w(TAG, "grant journal unavailable; journaled grants left alone")
            return null
        } ?: return null
        return try {
            val type = lstatType(journal)
            when (type) {
                EntryType.MISSING -> emptyList()
                EntryType.FILE -> journalLines(journal.readText(Charsets.UTF_8))
                else -> {
                    Log.w(TAG, "grant journal is not a regular file (${typeName(type)}); its lines left alone")
                    null
                }
            }
        } catch (t: Throwable) {
            Log.w(TAG, "grant journal unreadable; journaled grants left alone")
            null
        }
    }

    /**
     * 确保 [directory] 是一个真实目录（不是符号链接）且 canonical 路径正是
     * [expectedCanonical]，返回它的 canonical 路径。不存在就建（必须成功）。
     */
    private fun realDirectory(directory: File, expectedCanonical: String): String {
        when (lstatType(directory)) {
            EntryType.MISSING -> makeDirectory(directory)
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
        makeDirectory(directory)
    }

    /**
     * 建一个目录（父目录已存在）。File.mkdir() 拿不到 errno，所以直接用 Os.mkdir：
     * 失败时按 errno 归类——空间不足（ENOSPC / EDQUOT）是 enospc，其余是建不出来。
     */
    private fun makeDirectory(directory: File) {
        try {
            Os.mkdir(directory.path, OsConstants.S_IRWXU)
        } catch (e: ErrnoException) {
            throw directoryFailure(e.errno)
        } catch (t: Throwable) {
            throw StageFailure(CODE_IO_FAILED, "create")
        }
    }

    /** mkdir 失败的归类（纯判定）：ENOSPC / EDQUOT 是空间不足，其余都算建不出来。 */
    private fun directoryFailure(errno: Int): StageFailure =
        if (errno == OsConstants.ENOSPC || errno == OsConstants.EDQUOT) {
            StageFailure(CODE_NO_SPACE, "enospc")
        } else {
            StageFailure(CODE_IO_FAILED, "create")
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

    /**
     * 这个 id 的名字下已经有东西时拒绝复用（N6、O16；只在工作线程上调用）。不是真实目录（不
     * 存在，或者不是目录的残留）：交给 copyPlan 先删干净再用。是真实目录才要看：交付过（完成
     * 标记是普通文件）——不管它是谁的副本，和读不出结果的，一律 id-in-use，什么都不删。
     */
    private fun refuseReusedId(stageDir: File): StageFailure? {
        if (lstatType(stageDir) != EntryType.DIRECTORY) return null
        val markerType = lstatType(File(stageDir, MARKER_NAME))
        val ownership = if (markerType == EntryType.FILE) null else readOwnership(stageDir)
        return when (idReuseDecision(EntryType.DIRECTORY, markerType, ownership)) {
            IdReuse.PROCEED -> null
            IdReuse.IN_USE -> StageFailure(CODE_IO_FAILED, "id-in-use")
        }
    }

    /**
     * 复用一个 id 的判定（纯判定，O16）：目录不在了（或者名字下不是目录）→ 照常进行；交付
     * 过 → 拒绝；完成标记读不出来、或者（半截目录的）归属读不出来 → 同样拒绝，什么都不删；
     * 其余是半截目录 → 先删干净再用。
     */
    private fun idReuseDecision(
        stageDirType: EntryType?,
        markerType: EntryType?,
        ownership: Ownership?
    ): IdReuse = when {
        stageDirType != EntryType.DIRECTORY -> IdReuse.PROCEED
        markerType == EntryType.FILE -> IdReuse.IN_USE
        markerType == null -> IdReuse.IN_USE
        ownership == Ownership.UNKNOWN -> IdReuse.IN_USE
        else -> IdReuse.PROCEED
    }

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

    /** 读授权表（给放授权与过期清理用）：读不出来返回 null 并记一条日志——不猜。 */
    private fun readGrantsOrNull(): List<UriPermission>? = try {
        appContext.contentResolver.persistedUriPermissions
    } catch (t: Throwable) {
        Log.w(TAG, "persistedUriPermissions unavailable: ${t.javaClass.simpleName}")
        null
    }

    /** 该 EXACT URI 是否带读位（读写都有时也算）。 */
    private fun hasReadBit(grants: List<UriPermission>, treeUri: Uri): Boolean =
        grants.any { it.uri == treeUri && it.isReadPermission }

    /**
     * 释放规则（纯判定）：读位在、写位不在，才放 READ；读位不在没什么可放，写位在就不能碰——
     * 带写位的授权是接收侧的（pickFolder 在同一个 URI 上拿了 READ+WRITE；Android 持久化的
     * 是位的组合，不是引用计数）。
     */
    private fun mustReleaseRead(readBit: Boolean, writeBit: Boolean): Boolean = readBit && !writeBit

    /** 一份 journal 记着的授权，了结时走到了哪一步（纯判定的结果）。 */
    private enum class GrantStep {
        /** 此刻是 READ-only：放掉 READ。 */
        RELEASE,

        /** 读位刚刚被我们从一份 READ+WRITE 授权上剥掉（或上一次没能 take 回来）：把 READ 重新 take 回来。 */
        RESTORE_READ,

        /** 授权已经不在，或带着写位（接收侧的）：journal 里这一行划掉。 */
        DROP_LINE,

        /** 看不出结果（读不到权限位、释放抛了、授权原样还在、重新 take 没成功）：这一行留着，下一次再试。 */
        KEEP_LINE
    }

    /**
     * 放完 READ 之后该怎么做（纯判定）。[readBefore] / [writeBefore] 是放之前现读的权限位，
     * [releaseThrew] 是 releasePersistableUriPermission 有没有抛，后面三个是放完立刻重读的
     * 权限位（读不到时 [bitsAfterKnown] 为 false）。
     *  - 放之前就不是 READ-only：读位不在没什么可放，写位在是接收侧的、不碰——两种情况都
     *    划掉这一行（本来就没放，放完的权限位与此无关）。
     *  - 释放抛了，或放完读不到权限位：这一行留着。
     *  - 放完还是 READ-only：没放掉，这一行留着。
     *  - 放完写位在、读位没了：接收侧就在“读权限位”与“放”的这一瞬之间把授权升成了
     *    READ+WRITE，我们刚把它的读位剥掉——把 READ 重新 take 回来。
     *  - 放完授权整个不在了，或者变成了 READ+WRITE（接收侧在我们放完之后才拿的）：划掉这一行。
     */
    private fun afterReleaseStep(
        readBefore: Boolean,
        writeBefore: Boolean,
        releaseThrew: Boolean,
        bitsAfterKnown: Boolean,
        readAfter: Boolean,
        writeAfter: Boolean
    ): GrantStep {
        if (!mustReleaseRead(readBefore, writeBefore)) return GrantStep.DROP_LINE
        if (releaseThrew || !bitsAfterKnown) return GrantStep.KEEP_LINE
        if (readAfter && !writeAfter) return GrantStep.KEEP_LINE
        if (!readAfter && writeAfter) return GrantStep.RESTORE_READ
        return GrantStep.DROP_LINE
    }

    /**
     * 把 READ 重新 take 回来之后（纯判定）：授权此刻是 READ+WRITE——接收侧那份、读位回来了——
     * 才划掉这一行；其余（读不到、还是只有写位、变回了 READ-only）都留着，下一次再试。
     */
    private fun afterRestoreStep(bitsAfterKnown: Boolean, readAfter: Boolean, writeAfter: Boolean): GrantStep =
        if (bitsAfterKnown && readAfter && writeAfter) GrantStep.DROP_LINE else GrantStep.KEEP_LINE

    /**
     * journal 里一个 URI 的了结的第一步（纯判定；任务结束时和过期清理共用）：此刻是 READ-only
     * → 放 READ；
     * 写位在、读位不在 → 上一次放了之后没能把 READ take 回来（那一行因此还留着），现在再试；
     * 其余（授权不在了，或已经是 READ+WRITE）→ 授权不碰，只划掉这一行。
     */
    private fun sweepGrantStep(readBit: Boolean, writeBit: Boolean): GrantStep = when {
        mustReleaseRead(readBit, writeBit) -> GrantStep.RELEASE
        !readBit && writeBit -> GrantStep.RESTORE_READ
        else -> GrantStep.DROP_LINE
    }

    /** 该 URI 此刻的读位和写位；读不到授权表返回 null（不猜）。 */
    private fun readBits(treeUri: Uri): Pair<Boolean, Boolean>? {
        val grants = readGrantsOrNull() ?: return null
        val mine = grants.filter { it.uri == treeUri }
        return Pair(mine.any { it.isReadPermission }, mine.any { it.isWritePermission })
    }

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
            // take 抛了也不一定没生效：复核一下。读位在了，这一行就留给任务结束时了结（J15）；
            // 授权表读不出来，同样留着；读位不在（授权表也读得到）就把 journal 里那一行撤掉。
            val taken = try {
                hasReadBit(readPersistedGrants(), job.treeUri)
            } catch (e: StageFailure) {
                true
            }
            if (!taken) journalRemove(job.treeUri)
            throw StageFailure(CODE_PERMISSION_DENIED, reason)
        }
        if (!hasReadBit(readPersistedGrants(), job.treeUri)) {
            // 没抛却也没记上：读位不存在，这份授权没拿到
            journalRemove(job.treeUri)
            throw StageFailure(CODE_PERMISSION_DENIED, "no-persisted-grant")
        }
    }

    /**
     * 暂存任务从在跑的表里去掉之后（J15；[lastUser]：此刻没有别的任务用着同一个 URI）：了结
     * journal 里那一行。不论这一次自己有没有 take 过——上一回留在那里的行也是此时了结，包括
     * 排队之后才被取消的任务。走的是唯一的那条路（[settleJournalLine]），不再直接调
     * [releaseJournaledRead]。读不到权限位、释放抛了、或要把 READ take 回来却没 take 成，
     * 这一行就留着，交给下一次了结。
     */
    private fun releaseOwnedGrant(job: Job, lastUser: Boolean) {
        if (!lastUser) return
        settleJournalLine(job.treeUri)
    }

    /**
     * 了结 journal 里 [treeUri] 那一行的唯一一条路：暂存任务结束时（J15）、过期清理、接收侧
     * take 之后和 settleJournal 都走这里。先确认这一行还在 journal 里——不在 journal 里的授权
     * 一概不碰；再过放行闸门（G15），没获准就把这一行留着，原路返回；获准了，按此刻的权限位
     * 走 [sweepGrantStep]：READ-only → 放 READ；写位在、读位不在 → 把 READ take 回来；其余 →
     * 只划掉这一行。读不到权限位，这一行留着。
     */
    private fun settleJournalLine(treeUri: Uri) {
        val journaled = journalLinesOrNull()?.contains(treeUri.toString()) ?: false
        if (!journaled) return
        if (!beginSettlement()) return
        try {
            // 读不到权限位就不知道该做什么：这一行留着
            val bits = readBits(treeUri) ?: return
            when (sweepGrantStep(bits.first, bits.second)) {
                GrantStep.RELEASE -> releaseJournaledRead(treeUri, bits)
                // 上一次放了之后没能把 READ take 回来，这一行才留到现在：现在再试
                GrantStep.RESTORE_READ -> restoreRead(treeUri)
                GrantStep.DROP_LINE -> journalRemove(treeUri)
                GrantStep.KEEP_LINE -> Unit
            }
        } finally {
            endSettlement()
        }
    }

    /**
     * 放掉 [treeUri] 的 READ，并把 journal 里那一行了结（只在工作线程上调用）——发送侧暂存
     * 结束时和过期清理走的是同一段。[before] 是刚刚现读的该 URI 权限位。
     *  1. 放之前就不是 READ-only：不放，直接划掉这一行（见 [afterReleaseStep]）。
     *  2. READ-only：releasePersistableUriPermission(READ)。
     *  3. 放完立刻再读权限位：写位在、读位没了——接收侧就在这一瞬之间把授权升成了
     *     READ+WRITE，我们刚把它的读位剥掉——把 READ 重新 take 回来。那个 take 本来都排在
     *     本工作线程上串行，这一条是给不在这里串行的 take 兜底的。
     *  4. 了结：划掉这一行；或者留着（读不到权限位、释放抛了、重新 take 没成功）。
     * 返回 journal 里这一行是否已经划掉。
     */
    private fun releaseJournaledRead(treeUri: Uri, before: Pair<Boolean, Boolean>?): Boolean {
        // 读不到权限位就不知道该不该放：这一行留着
        val readBefore = before?.first ?: return false
        val writeBefore = before.second
        val released = if (mustReleaseRead(readBefore, writeBefore)) {
            releasePersistableRead(treeUri)
        } else {
            true
        }
        val after = readBits(treeUri)
        return settle(
            afterReleaseStep(
                readBefore = readBefore,
                writeBefore = writeBefore,
                releaseThrew = !released,
                bitsAfterKnown = after != null,
                readAfter = after?.first ?: false,
                writeAfter = after?.second ?: false
            ),
            treeUri
        )
    }

    /** 按判定结果走完这一步；返回 journal 里这一行是否已经划掉。 */
    private fun settle(step: GrantStep, treeUri: Uri): Boolean = when (step) {
        GrantStep.DROP_LINE -> {
            journalRemove(treeUri)
            true
        }
        GrantStep.RESTORE_READ -> restoreRead(treeUri)
        else -> false
    }

    /**
     * 把 READ 重新 take 回来（只在工作线程上调用）：take 抛了（日志里只有异常类名），或者
     * take 完复核发现授权还不是 READ+WRITE，journal 里那一行都留着——下一次过期清理会再试。
     */
    private fun restoreRead(treeUri: Uri): Boolean {
        if (!takePersistedRead(treeUri)) return false
        val after = readBits(treeUri)
        return settle(
            afterRestoreStep(after != null, after?.first ?: false, after?.second ?: false),
            treeUri
        )
    }

    /**
     * 任务结束（或没能排上队）时把它从在跑的表里去掉；只去掉它自己。返回此刻是不是已经没有
     * 任务用着同一个 treeUri——它是最后一个使用者。
     */
    private fun unregisterJob(job: Job): Boolean = synchronized(jobsLock) {
        if (activeJobs[job.stageId] === job) activeJobs.remove(job.stageId)
        activeJobs.values.none { it.treeUri == job.treeUri }
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

    /** 把 [uri] 的 READ 授权重新 take 回来；抛了就是没 take 到（返回 false），日志里只有异常类名。 */
    private fun takePersistedRead(uri: Uri): Boolean = try {
        appContext.contentResolver.takePersistableUriPermission(
            uri,
            Intent.FLAG_GRANT_READ_URI_PERMISSION
        )
        true
    } catch (t: Throwable) {
        Log.w(TAG, "taking the READ grant back failed: ${t.javaClass.simpleName}")
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
            // 只打开普通文件：不存在就是没有行可划；别的类型上面已经挡掉（T17）
            if (lstatType(journal) != EntryType.FILE) return
            val target = treeUri.toString()
            val kept = journalLines(journal.readText(Charsets.UTF_8)).filter { it != target }
            if (kept.isEmpty()) {
                if (!removeEntry(journal)) {
                    Log.w(TAG, "empty grant journal could not be removed")
                }
                return
            }
            val temp = File(journal.parentFile, JOURNAL_TEMP_NAME)
            // 临时文件也只能是普通文件（T17）：这个名字下若是链接、别的非常规条目、目录，或者
            // 定不了型，既不打开它也不删它，这一行留着（过期清理会把这个不是 id 名字的条目按
            // 它自己清掉），绝不写穿链接
            val tempType = lstatType(temp)
            if (tempType != EntryType.MISSING && tempType != EntryType.FILE) {
                Log.w(TAG, "grant journal scratch file is not a regular file (${typeName(tempType)}); line kept")
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

    /**
     * 取消一个任务（只在主线程上调用）：标志在调用线程上立刻生效；CancellationSignal.cancel()
     * 和关输入流交给专用的小线程——主线程从不在输入流的锁上等。
     */
    private fun cancelJob(job: Job) {
        job.cancelled = true
        try {
            CANCELLER.execute {
                try {
                    job.signal.cancel()
                } catch (t: Throwable) {
                    // 尽力而为：标志已经置了，任务在下一次检查时停下来。日志里只有异常类名。
                    Log.w(TAG, "cancelling the staging signal failed: ${t.javaClass.simpleName}")
                }
                job.closeOpenStream()
            }
        } catch (t: Throwable) {
            // 排不进小线程（不会发生）：标志已经置了，任务在下一次检查时停下来
        }
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
