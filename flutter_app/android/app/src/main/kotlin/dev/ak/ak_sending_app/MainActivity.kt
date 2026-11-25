package dev.ak.ak_sending_app

/*
 * dev.ak/storage —— 安卓原生存储通道（P0-06 起步，第 15 轮）
 *
 * 范围：只做“目录能力探测 / 目录选择 / SAF 写路径最小闭环证明”。
 * 不做文件正文传输——正文仍由 Rust 引擎直传（见 docs/STORAGE_AND_PLATFORMS.zh-CN.md
 * 与评审 4.4：默认下载目录 + 用户改选走 SAF；SAF 每次操作经跨进程调用，
 * 十万级小文件会明显变慢，故正文路径的最终选择留给 P0 实测数据）。
 *
 * 批次 Z 起增加发送侧目录暂存（方法 6–10，实现在 SafSendStager.kt）：SAF 目录树先
 * 复制进应用私有存储，引擎仍只读文件系统路径。接收侧的应答形状不变；批次 Z 第 5 修
 * （G15/M）起 pickFolder 的 take 和授权表的读取都经 SafSendStager，主线程不再碰授权表。
 *
 * ── Channel 方法与返回 schema（MethodChannel "dev.ak/storage"）──────────────
 *
 * 1) getStorageInfo() -> Map
 *      appDownloadDir : String  应用专属 Download 绝对路径
 *                        （/storage/emulated/0/Android/data/<pkg>/files/Download，
 *                         getExternalFilesDir(DIRECTORY_DOWNLOADS)，探测时确保目录存在；
 *                         外部存储不可用时回退 filesDir/Download）
 *      freeBytes      : Long    该目录所在卷的可用字节数（StatFs.availableBytes）；
 *                        取不到时为 -1（不伪造容量，符合存储文档“剩余空间未知时不虚假放行”）
 *      safPersisted   : Boolean 持久授权表（contentResolver.persistedUriPermissions）
 *                        是否非空。批次 Z 第 5 修（M）起这张表只由 SafSendStager 在工作
 *                        线程上读、回主线程应答；目录与 freeBytes 仍照旧先算好。
 *                        stager 不在时答 PlatformException(unavailable)，读表失败答
 *                        PlatformException(error)。
 *
 * 2) pickFolder() -> Map
 *      treeUri : String|null   用户确认的目录 tree URI；取消/失败为 null。
 *                              成功选择后本端调用 takePersistableUriPermission(RW)
 *                              持久化授权（失败仅打日志，不改变返回值——
 *                              授权是否真的持久化由 listSafPersisted() 复核）。
 *                              批次 Z 复审 N9 起这次 take 排在 SafSendStager 的工作
 *                              线程上执行（本应用的持久授权操作全在那里串行），
 *                              应答等它做完再回。
 *                              批次 Z 第 5 修（G15 放行闸门）：请求先在 SafSendStager
 *                              登记、再打开选择器，登记留到 take 跑完（或没拿到结果就
 *                              结束）为止；登记期间本应用不放掉任何授权——Android 7–10
 *                              上落在授权与 take 之间的一次 release 会让 take 失败。
 *                              正好有放行在收尾时，打开选择器的那一步等它结束再执行。
 *      launcher 或 stager 不在时返回 PlatformException(unavailable)。一次请求从头到尾
 *      （打开选择器、等结果、take 排队或执行）还没结束时再调，返回
 *      PlatformException(pick_in_progress)；Activity 在此期间销毁，返回
 *      PlatformException(activity_destroyed)。
 *      选择器经 androidx.activity 的 ActivityResultContracts.OpenDocumentTree
 *      （ACTION_OPEN_DOCUMENT_TREE，EXTRA_INITIAL_URI 尽力定位到主卷 Download；
 *       系统说明该初值是 best-effort，提供器可能忽略）。
 *
 * 3) safWriteTest(treeUri: String) -> Map
 *      ok    : Boolean
 *      error : String|null   失败原因（异常类名 + 消息）
 *      在 tree 根创建 ak-probe-<ts>.txt（1 字节 "A"）-> 经 ContentResolver 写入 ->
 *      读回校验 -> 删除。这是 SAF 写路径（DocumentsContract.createDocument +
 *      openOutputStream/openInputStream + deleteDocument）的最小闭环证明。
 *
 * 4) listSafPersisted() -> List<Map>
 *      每项 { uri: String, read: Bool, write: Bool, tree: Bool, persistedTimeMs: Long }
 *      persistedTimeMs 在 API<30 上为 -1；tree 由 DocumentsContract.isTreeUri 判定。
 *      批次 Z 第 5 修（M）起授权表只由 SafSendStager 在工作线程上读、回主线程应答；
 *      stager 不在时答 PlatformException(unavailable)，读表失败答 PlatformException(error)。
 *
 * 5) deviceName() -> String|null
 *      本机自报设备名（X5，批次 X 交接 §4.5：仅展示用，不是身份证明）。
 *      Build.MODEL 优先，为空回退 Build.MANUFACTURER，均 trim；
 *      两侧皆空返回 null（Dart 侧随之走引擎默认名 'ak-task'）。
 *
 * 6) pickSendFolder() -> Map                                         （批次 Z：发送侧选目录）
 *      {status:"selected", treeUri, displayName} | {status:"cancelled"}
 *      与 pickFolder 各用一个 launcher 和一个 pending result，互不影响。
 *      只校验 authority，并借 Activity 的临时授权读出根目录名——不持久化任何授权。
 *      这几个发送侧方法在主线程上不做任何 ContentResolver 调用：根目录名在工作
 *      线程上查，应答从那里回主线程。
 *      选择器打不开（ActivityNotFoundException 等）或 launcher 不可用时答 cancelled。
 *
 * 7) stageSafTree(treeUri: String, stageId: String) -> Map
 *      {status:"ok", stageId, path, displayName,
 *       fileCount: Long, byteCount: Long, emptyDirectoryCount: Long}
 *      path 是暂存后的根目录路径（<noBackupFilesDir>/ak-saf-staging/<stageId>/
 *      payload/<displayName>）。失败一律 PlatformException(code, 固定 reason token)，
 *      进度经同一通道的 stagingProgress 事件回传，应答在工作线程完全停下之后。
 *      读授权：先看该 URI 是否已有持久的 READ 授权（有就原样用、不碰）；没有才
 *      先记进 ak-saf-staging/.send-grants 再 take(READ)，结束时只放掉自己拿的那份。
 *      复制完逐个目录复核成员关系（source-changed）、逐个文件核对已知长度
 *      （size-mismatch），最后才写完成标记。
 *
 * 8) cancelStaging(stageId: String) -> null
 *      置取消标志、取消任务的 CancellationSignal、关掉它打开着的输入流；不删任何
 *      东西，清理由在跑的暂存任务自己完成后再应答。
 *
 * 9) cleanupStagedSend(stageId: String) -> Map
 *      {removed: Boolean}   true = 已经不在了（或刚删掉），false = 有东西删不掉。
 *      这个 id 的暂存还在跑时答 PlatformException(STAGE_IO_FAILED, busy)，不删任何东西。
 *
 * 10) cleanupStaleStaging() -> Map
 *      {removed: Long}   清掉以前各次运行留下的暂存目录，以及 .send-grants 里
 *      记着、不属于任何在跑暂存的 READ-only 授权。
 *
 * ── ActivityResult 路由说明 ─────────────────────────────────────────────
 * FlutterActivity 继承的是 android.app.Activity（非 ComponentActivity），
 * 不能直接用 registerForActivityResult。此处自建 ActivityResultRegistry，
 * onLaunch 仍以 Activity.startActivityForResult 发起（未弃用的框架 API），
 * 结果经 Flutter v2 embedding 的 ActivityPluginBinding.addActivityResultListener
 * （临时插件 ActivityResultRouter 注册到 flutterEngine.plugins）回到本 Activity，
 * 再 registry.dispatchResult 走标准 androidx ActivityResult 回调。
 * 全程没有 override Activity.onActivityResult。
 *
 * ── 集成者实测步骤（本轮只保证 Kotlin 编译通过；真机/模拟器实测由集成者执行）──
 * 1) 构建 & 启动：
 *      cd flutter_app && flutter build apk --debug
 *      adb install -r build/app/outputs/flutter-apk/app-debug.apk
 *      adb shell am start -n dev.ak.ak_sending_app/.MainActivity
 * 2) 触发方式：在 Flutter 侧临时加一段调试代码（勿提交，测完删除），例如挂到
 *    某个临时按钮或 initState 的 Future.microtask 里：
 *
 *      const ch = MethodChannel('dev.ak/storage');
 *      // ignore: avoid_print
 *      print(await ch.invokeMethod('getStorageInfo'));
 *      // => {appDownloadDir: /storage/emulated/0/Android/data/dev.ak.ak_sending_app/files/Download,
 *      //     freeBytes: <卷剩余字节>, safPersisted: false}
 *
 *      final pick = await ch.invokeMethod<Map<dynamic, dynamic>>('pickFolder');
 *      print(pick);                    // 选目录 => {treeUri: content://...}
 *      //                                    取消   => {treeUri: null}
 *      final uri = pick?['treeUri'] as String?;
 *      if (uri != null) {
 *        print(await ch.invokeMethod('safWriteTest', {'treeUri': uri}));
 *        // => {ok: true, error: null}
 *        print(await ch.invokeMethod('listSafPersisted'));
 *        // => [{uri: content://..., read: true, write: true, tree: true, persistedTimeMs: ...}]
 *      }
 *      // 反例：await ch.invokeMethod('safWriteTest', {'treeUri': 'content://bad'})
 *      //    => {ok: false, error: 'IllegalArgumentException: Invalid URI: ...'}
 *      // 反例：选择器未关闭时再次 pickFolder => PlatformException(pick_in_progress)
 * 3) 日志：adb logcat -s AKStorage（本文件统一 TAG=AKStorage）
 *
 * ── 已知限制 ───────────────────────────────────────────────────────────
 *  - Android TV 的精简 UI 可能没有系统文件选择器（ACTION_OPEN_DOCUMENT_TREE
 *    无 Activity 可处理）：pickFolder 会以 {treeUri: null} 返回（launcher.launch
 *    抛 ActivityNotFoundException 时同样回 null），届时只能走应用专属目录 /
 *    MediaStore.Downloads 路线。
 *  - SAF 授权在多用户/重启后可能被系统收回（persistedUriPermissions 上限等），
 *    业务侧使用前应以 listSafPersisted + safWriteTest 复核。
 *  - safWriteTest 在主线程做少量 Binder IPC（1 字节写/读/删），探测用途可接受；
 *    正式批量传输不经过此通道。
 */

import android.content.Intent
import android.content.UriPermission
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.Handler
import android.os.Looper
import android.os.StatFs
import android.provider.DocumentsContract
import android.util.Log
import android.view.WindowManager
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.ActivityResultRegistry
import androidx.activity.result.contract.ActivityResultContract
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.app.ActivityOptionsCompat
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.plugins.FlutterPlugin
import io.flutter.embedding.engine.plugins.activity.ActivityAware
import io.flutter.embedding.engine.plugins.activity.ActivityPluginBinding
import io.flutter.plugin.common.MethodChannel
import io.flutter.plugin.common.PluginRegistry
import java.io.File

class MainActivity : FlutterActivity() {

    companion object {
        private const val TAG = "AKStorage"
        private const val CHANNEL = "dev.ak/storage"
        private const val SCREEN_CHANNEL = "dev.ak/screen"
        private const val PICK_FOLDER_KEY = "dev.ak/storage:pickFolder"
        private const val PICK_SEND_FOLDER_KEY = "dev.ak/storage:pickSendFolder"
        private const val EXTERNAL_STORAGE_AUTHORITY = "com.android.externalstorage.documents"
    }

    /** 一次 pickFolder 请求（批次 Z 第 5 修，G15）：登记句柄、要应答的 result、是否已结束。 */
    private class PickFolderRequest(
        val pick: SafSendStager.ReceivePick,
        val result: MethodChannel.Result
    ) {
        /** 置上之后谁都不能再拿这个请求应答。 */
        var finished = false
    }

    /**
     * 进行中的 pickFolder 请求；同一时刻至多一个。字段里留着的永远是“已登记、未结束”的
     * 请求——选择器打开着、打开那一步还在等放行、take 排队或执行中都算，结束时才移出去。
     * 所以它在，再调 pickFolder 就答 pick_in_progress。
     */
    private var pendingPickRequest: PickFolderRequest? = null

    private var folderPicker: ActivityResultLauncher<Uri?>? = null

    /** 进行中的 pickSendFolder 调用的 result（批次 Z）；与上面两个成员完全独立。 */
    private var pendingSendPickResult: MethodChannel.Result? = null

    private var sendFolderPicker: ActivityResultLauncher<Uri?>? = null

    /** 发送侧目录暂存（批次 Z）：枚举、复制、取消与清理都在它里面。 */
    private var sendStager: SafSendStager? = null

    private var engineRef: FlutterEngine? = null

    /**
     * 自建 ActivityResultRegistry：FlutterActivity 不是 ComponentActivity，
     * 拿不到系统 registry，但 androidx 的 register/contract/lancer 照常可用。
     * onLaunch 里的 startActivityForResult 正是 ComponentActivity 内部的做法。
     */
    private val storageResultRegistry = object : ActivityResultRegistry() {
        override fun <I, O> onLaunch(
            requestCode: Int,
            contract: ActivityResultContract<I, O>,
            input: I,
            options: ActivityOptionsCompat?
        ) {
            val intent = contract.createIntent(this@MainActivity, input)
            val launchOptions = options?.toBundle()
            if (launchOptions != null) {
                this@MainActivity.startActivityForResult(intent, requestCode, launchOptions)
            } else {
                this@MainActivity.startActivityForResult(intent, requestCode)
            }
        }
    }

    /**
     * 临时插件：把系统 activity result 经 embedding 的 v2 控制面送回本 Activity。
     * 只在 onAttachedToActivity 里注册 ActivityResultListener，不提供任何 Dart API。
     */
    private val activityResultRouter = object : FlutterPlugin, ActivityAware,
        PluginRegistry.ActivityResultListener {
        private var binding: ActivityPluginBinding? = null

        override fun onAttachedToEngine(binding: FlutterPlugin.FlutterPluginBinding) = Unit

        override fun onDetachedFromEngine(binding: FlutterPlugin.FlutterPluginBinding) = Unit

        override fun onAttachedToActivity(binding: ActivityPluginBinding) {
            this.binding = binding
            binding.addActivityResultListener(this)
        }

        override fun onDetachedFromActivityForConfigChanges() {
            binding?.removeActivityResultListener(this)
            binding = null
        }

        override fun onReattachedToActivityForConfigChanges(binding: ActivityPluginBinding) {
            this.binding = binding
            binding.addActivityResultListener(this)
        }

        override fun onDetachedFromActivity() {
            binding?.removeActivityResultListener(this)
            binding = null
        }

        override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?): Boolean =
            storageResultRegistry.dispatchResult(requestCode, resultCode, data)
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        engineRef = flutterEngine
        flutterEngine.plugins.add(activityResultRouter)
        // 接收侧选目录（批次 Z 第 5 修，G15）：这个 launcher 只负责打开系统选择器。请求在
        // onPickFolder 里先向 SafSendStager 登记、后 launch——正好有授权放行在收尾时，
        // launch 由 stager 等它结束再执行（晚一些，仍在主线程上）。结果一律回到
        // onFolderPicked，由它或者 take 跑完后的回调结束请求、应答；迟到的结果只丢弃。
        folderPicker = storageResultRegistry.register(
            PICK_FOLDER_KEY,
            ActivityResultContracts.OpenDocumentTree()
        ) { treeUri -> onFolderPicked(treeUri) }

        // 批次 Z：发送侧选目录用另一个 launcher（另一个 key 与 requestCode），
        // 与 pickFolder 的 pending result / 回调互不影响。
        sendFolderPicker = storageResultRegistry.register(
            PICK_SEND_FOLDER_KEY,
            ActivityResultContracts.OpenDocumentTree()
        ) { treeUri -> onSendFolderPicked(treeUri) }

        val storageChannel = MethodChannel(flutterEngine.dartExecutor.binaryMessenger, CHANNEL)
        sendStager = SafSendStager(
            applicationContext,
            storageChannel,
            Handler(Looper.getMainLooper())
        )
        storageChannel.setMethodCallHandler { call, result ->
            when (call.method) {
                "getStorageInfo" -> withSendStager(result) {
                    val info = getStorageInfo()
                    // 批次 Z 第 5 修（M）：授权表在工作线程上读，应答由 stager 回主线程。
                    persistedGrants(result) { grants ->
                        info + ("safPersisted" to grants.isNotEmpty())
                    }
                }
                "pickFolder" -> onPickFolder(result)
                "safWriteTest" -> result.success(
                    safWriteTest(call.argument<String>("treeUri"))
                )
                "listSafPersisted" -> withSendStager(result) {
                    persistedGrants(result, ::listSafPersisted)
                }
                "deviceName" -> result.success(deviceName())
                "pickSendFolder" -> onPickSendFolder(result)
                "stageSafTree" -> withSendStager(result) {
                    stageSafTree(
                        call.argument<Any?>("treeUri") as? String,
                        call.argument<Any?>("stageId") as? String,
                        result
                    )
                }
                "cancelStaging" -> withSendStager(result) {
                    cancelStaging(call.argument<Any?>("stageId") as? String)
                    result.success(null)
                }
                "cleanupStagedSend" -> withSendStager(result) {
                    cleanupStagedSend(call.argument<Any?>("stageId") as? String, result)
                }
                "cleanupStaleStaging" -> withSendStager(result) {
                    cleanupStaleStaging(result)
                }
                else -> result.notImplemented()
            }
        }

        // W7.3（批次 W 交接 §4.7 第 3 条）：传输期间保持屏幕常亮的窄通道。
        // 只有两个方法：on 加 FLAG_KEEP_SCREEN_ON、off 清除之。不加第三方
        // 插件；Dart 侧（ScreenKeepAwake）在任一传输进行中调 on、全部结束
        // 调 off，Windows/Linux 上不调用。重复 on/off 是幂等的窗口 flag
        // 操作；Activity 重建时 flag 随窗口一起消失，Dart 侧的下一次状态
        // 翻转（或 dispose 的兜底 off）重新对齐。
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, SCREEN_CHANNEL)
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "on" -> {
                        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                        result.success(null)
                    }
                    "off" -> {
                        window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }
    }

    override fun onDestroy() {
        // 接收侧请求还没结束（选择器未回、launch 还在等放行、take 排队或执行中）：先撤掉
        // 登记——排队的 take 自己跳过，等着的 launch 不再放行——再答 activity_destroyed。
        pendingPickRequest?.let { request ->
            if (finishPickRequest(request)) {
                SafSendStager.endReceivePick(request.pick)
                answerActivityDestroyed(request.result)
            }
        }
        pendingSendPickResult?.let { pending ->
            runCatching {
                pending.error(
                    "activity_destroyed",
                    "host activity destroyed before send folder pick completed",
                    null
                )
            }
        }
        pendingSendPickResult = null
        // 批次 Z：还在跑的暂存一律取消（清理由任务自己完成），之后不再向通道应答。
        runCatching { sendStager?.shutdown() }
        sendStager = null
        runCatching { folderPicker?.unregister() }
        folderPicker = null
        runCatching { sendFolderPicker?.unregister() }
        sendFolderPicker = null
        runCatching { engineRef?.plugins?.remove(activityResultRouter::class.java) }
        engineRef = null
        super.onDestroy()
    }

    // ── getStorageInfo ────────────────────────────────────────────────────

    /**
     * 目录与 freeBytes 照旧在调用线程上算。safPersisted 不在这里取（批次 Z 第 5 修 M）：
     * 授权表只能经 SafSendStager.persistedGrants 在工作线程上读，由调用处补上这一个值。
     */
    private fun getStorageInfo(): Map<String, Any?> {
        val dir = appDownloadDir()
        val freeBytes = try {
            if (dir.exists()) StatFs(dir.absolutePath).availableBytes else -1L
        } catch (t: Throwable) {
            Log.w(TAG, "StatFs failed: ${t.javaClass.simpleName}")
            -1L
        }
        return mapOf(
            "appDownloadDir" to dir.absolutePath,
            "freeBytes" to freeBytes
        )
    }

    /** 应用专属 Download 目录：外部不可用时回退内部存储并确保目录存在。 */
    private fun appDownloadDir(): File {
        val external = getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS)
        if (external != null) {
            if (!external.exists()) external.mkdirs()
            return external
        }
        val internal = File(filesDir, Environment.DIRECTORY_DOWNLOADS)
        if (!internal.exists()) internal.mkdirs()
        return internal
    }

    // ── pickFolder ────────────────────────────────────────────────────────

    /**
     * 接收侧选目录（批次 Z 第 5 修，G15 放行闸门）。次序不能变：先向 SafSendStager 登记，
     * 再经它 launch——正好有授权放行在收尾时，它把 launch 留到收尾结束之后。登记一直留到
     * take 跑完（或没拿到结果就结束）为止，这期间再调 pickFolder 答 pick_in_progress。
     */
    private fun onPickFolder(result: MethodChannel.Result) {
        val launcher = folderPicker
        if (launcher == null) {
            result.error("unavailable", "folder picker launcher not initialized", null)
            return
        }
        // 字段里有请求就是还在途（选择器打开着、launch 在等放行、take 排队或执行中）。
        if (pendingPickRequest != null) {
            result.error("pick_in_progress", "a folder pick is already in flight", null)
            return
        }
        if (sendStager == null) {
            result.error("unavailable", "send stager not initialized", null)
            return
        }
        val pick = try {
            SafSendStager.beginReceivePick()
        } catch (t: Throwable) {
            // 本文件只在自己的字段空着时登记：登记不上只会是登记处另有一份在（同进程里
            // 另一个 Activity 实例的请求）。按在途处理，不让异常从通道处理器里抛出来。
            Log.w(TAG, "beginReceivePick failed: ${t.javaClass.simpleName}")
            result.error("pick_in_progress", "a folder pick is already in flight", null)
            return
        }
        val request = PickFolderRequest(pick, result)
        pendingPickRequest = request
        val launch = {
            // 请求已经被结束（onDestroy）而 stager 又把这一步送了过来：不再打开选择器。
            if (!request.finished && pendingPickRequest === request) {
                try {
                    launcher.launch(initialDownloadDocumentUri())
                } catch (t: Throwable) {
                    // 典型场景：Android TV 等没有文档选择器的环境（ActivityNotFoundException）。
                    // 不当作崩溃：返回 treeUri=null，让 Dart 侧退回默认目录策略。
                    Log.w(TAG, "launch OpenDocumentTree failed: ${t.javaClass.simpleName}")
                    endPickWithoutResult(request)
                }
            }
        }
        try {
            SafSendStager.launchReceivePick(pick, launch)
        } catch (t: Throwable) {
            // launchReceivePick 自己抛了出来（launcher 那一步的异常在它里面已经收尾过）：
            // 这里兜住，做同样的收尾，应答仍只有一次。
            Log.w(TAG, "launchReceivePick failed: ${t.javaClass.simpleName}")
            endPickWithoutResult(request)
        }
    }

    /**
     * 没拿到结果的收尾：撤销登记、答 {treeUri: null}（与第 5 修之前一样）。
     * 只对还没结束的请求做一次。
     */
    private fun endPickWithoutResult(request: PickFolderRequest) {
        if (!finishPickRequest(request)) return
        SafSendStager.endReceivePick(request.pick)
        request.result.success(mapOf("treeUri" to null as String?))
    }

    /**
     * 结束一个请求并把它移出字段，返回是不是这一处结束的。每个请求只有第一处拿到 true：
     * take 的 onDone、onDestroy 这些路径之中只能有一处应答。
     */
    private fun finishPickRequest(request: PickFolderRequest): Boolean {
        if (request.finished) return false
        request.finished = true
        if (pendingPickRequest === request) pendingPickRequest = null
        return true
    }

    /** 请求随 Activity 一起作废时的应答；引擎可能已经不在了，所以包在 runCatching 里。 */
    private fun answerActivityDestroyed(result: MethodChannel.Result) {
        runCatching {
            result.error(
                "activity_destroyed",
                "host activity destroyed before folder pick completed",
                null
            )
        }
    }

    /** OpenDocumentTree 的结果回到 pickFolder 的调用方；只结束字段里那一个请求。 */
    private fun onFolderPicked(treeUri: Uri?) {
        val request = pendingPickRequest
        if (request == null) {
            // 没有在途的请求（已经由别的路径结束）：丢弃，日志里不带 URI。
            Log.w(TAG, "folder picked but no pending result")
            return
        }
        if (treeUri == null) {
            // 取消：没有授权可拿。登记撤掉之后让 stager 把登记期间搁下的日志行结算掉。
            if (finishPickRequest(request)) {
                SafSendStager.endReceivePick(request.pick)
                sendStager?.settleJournal()
                request.result.success(mapOf("treeUri" to null as String?))
            }
            return
        }
        val stager = sendStager
        if (stager == null) {
            // stager 已随 Activity 释放（不会走到这一步）：授权不拿了，按 Activity 已销毁应答。
            if (finishPickRequest(request)) {
                SafSendStager.endReceivePick(request.pick)
                answerActivityDestroyed(request.result)
            }
            return
        }
        // 批次 Z 复审 N9：takePersistableUriPermission(READ|WRITE) 经 SafSendStager 的工作线程
        // 执行——本应用所有持久授权操作在同一线程上串行，与发送侧的 take/release 互不
        // 交错。应答还是同一个 map，等工作线程做完再回；take 失败只记日志，不改变应答。
        // 登记由工作线程在 take 之后撤掉（G15）：在那之前请求一直留在字段里。
        stager.takeReceiveGrant(request.pick, treeUri) {
            if (finishPickRequest(request)) {
                request.result.success(mapOf("treeUri" to treeUri.toString()))
            }
        }
    }

    /**
     * EXTRA_INITIAL_URI 初值：主外部卷的 Download 目录。
     * ExternalStorageProvider 的 docId 形如 "<root>:<path>"，主卷 root 固定为
     * "primary"。官方文档声明该初值是 best-effort，解析失败时选择器忽略并落到默认位置。
     */
    private fun initialDownloadDocumentUri(): Uri? = try {
        DocumentsContract.buildDocumentUri(
            EXTERNAL_STORAGE_AUTHORITY,
            "primary:" + Environment.DIRECTORY_DOWNLOADS
        )
    } catch (t: Throwable) {
        Log.w(TAG, "build initial Download URI failed: ${t.javaClass.simpleName}")
        null
    }

    // ── pickSendFolder（批次 Z）──────────────────────────────────────────

    /**
     * 发送侧选目录：与 pickFolder 各用各的 launcher、pending result 和回调。
     * 应答只有 {status:"selected"} / {status:"cancelled"} 两种——launcher 不可用、
     * 已有一次在途、或选择器打不开（Android TV 等没有文档选择器的环境，
     * ActivityNotFoundException）都答 cancelled，绝不崩溃。
     */
    private fun onPickSendFolder(result: MethodChannel.Result) {
        val launcher = sendFolderPicker
        if (launcher == null || pendingSendPickResult != null) {
            Log.w(TAG, "pickSendFolder declined (no launcher or already in flight)")
            result.success(mapOf("status" to "cancelled"))
            return
        }
        pendingSendPickResult = result
        try {
            launcher.launch(initialDownloadDocumentUri())
        } catch (t: Throwable) {
            Log.w(TAG, "launch OpenDocumentTree (send) failed: ${t.javaClass.simpleName}")
            pendingSendPickResult = null
            result.success(mapOf("status" to "cancelled"))
        }
    }

    /** OpenDocumentTree 的结果回到 pickSendFolder 的调用方；校验与根目录名都在 stager 里。 */
    private fun onSendFolderPicked(treeUri: Uri?) {
        val pending = pendingSendPickResult
        pendingSendPickResult = null
        if (pending == null) {
            // 没有在途的调用：丢弃（日志里不带 URI）。
            Log.w(TAG, "send folder picked but no pending result")
            return
        }
        val stager = sendStager
        if (stager == null) {
            runCatching {
                pending.error(
                    "activity_destroyed",
                    "host activity destroyed before send folder pick completed",
                    null
                )
            }
            return
        }
        stager.onSendFolderPicked(treeUri, pending)
    }

    /** 批次 Z 各方法的统一入口：stager 只在 Activity 存活期间存在，兜底答 unavailable。 */
    private fun withSendStager(result: MethodChannel.Result, block: SafSendStager.() -> Unit) {
        val stager = sendStager
        if (stager == null) {
            result.error("unavailable", "send stager not initialized", null)
            return
        }
        stager.block()
    }

    // ── safWriteTest ──────────────────────────────────────────────────────

    /**
     * SAF 写路径最小闭环：在 tree 根建 ak-probe-<ts>.txt（1 字节）-> 写 ->
     * 读回校验 -> 删除。所有操作经 DocumentsContract / ContentResolver（跨进程），
     * 返回的 docUri 为准（提供器可能改写显示名）。任何一步失败都会清理已建文件。
     */
    private fun safWriteTest(treeUriString: String?): Map<String, Any?> {
        if (treeUriString.isNullOrBlank()) {
            return mapOf("ok" to false, "error" to "treeUri argument is missing or empty")
        }
        var createdDocUri: Uri? = null
        return try {
            val treeUri = Uri.parse(treeUriString)
            // 非 tree URI 在此抛 IllegalArgumentException
            val rootDocId = DocumentsContract.getTreeDocumentId(treeUri)
            val rootDocUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, rootDocId)

            val probeName = "ak-probe-${System.currentTimeMillis()}.txt"
            createdDocUri = DocumentsContract.createDocument(
                contentResolver, rootDocUri, "text/plain", probeName
            ) ?: throw IllegalStateException("createDocument returned null for $probeName")

            val payload = byteArrayOf(0x41) // 'A'
            contentResolver.openOutputStream(createdDocUri, "w")?.use { stream ->
                stream.write(payload)
                stream.flush()
            } ?: throw IllegalStateException("openOutputStream(w) returned null")
            val readBack = contentResolver.openInputStream(createdDocUri)?.use { it.readBytes() }
                ?: throw IllegalStateException("openInputStream returned null")
            check(readBack contentEquals payload) {
                "read-back mismatch: expected 1 byte 'A', got ${readBack.size} byte(s)"
            }

            mapOf("ok" to true, "error" to null as String?)
        } catch (t: Throwable) {
            val message = "${t.javaClass.simpleName}: ${t.message}"
            // 回给 Dart 的 map 不变；日志只带异常类名（消息里可能带着探测文件的名字）。
            Log.w(TAG, "safWriteTest failed: ${t.javaClass.simpleName}")
            mapOf("ok" to false, "error" to message)
        } finally {
            // 无论成败都清理探测文件；清理失败只记日志，不掩盖原始结果。
            createdDocUri?.let { doc ->
                runCatching { DocumentsContract.deleteDocument(contentResolver, doc) }
                    .onFailure { Log.w(TAG, "probe cleanup failed: ${it.javaClass.simpleName}") }
            }
        }
    }

    // ── listSafPersisted ──────────────────────────────────────────────────

    /**
     * 批次 Z 第 5 修（M）：授权表由 SafSendStager.persistedGrants 在工作线程上读出来再交给
     * 这里整理——主线程不碰 contentResolver.persistedUriPermissions。
     */
    private fun listSafPersisted(grants: List<UriPermission>): List<Map<String, Any?>> =
        grants.map { permission ->
            mapOf(
                "uri" to permission.uri.toString(),
                "read" to permission.isReadPermission,
                "write" to permission.isWritePermission,
                "tree" to DocumentsContract.isTreeUri(permission.uri),
                "persistedTimeMs" to if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    permission.persistedTime
                } else {
                    -1L
                }
            )
        }

    // ── deviceName ────────────────────────────────────────────────────────

    /**
     * X5（批次 X 交接 §4.5 第 5 条）：本机自报设备名，仅展示用，不是身份证明。
     * Build.MODEL 优先，trim 后为空回退 Build.MANUFACTURER；两侧皆空返回
     * null（Dart 侧不传 device_name，引擎用默认名 'ak-task'）。
     */
    private fun deviceName(): String? {
        val model = Build.MODEL.orEmpty().trim()
        if (model.isNotEmpty()) {
            return model
        }
        val manufacturer = Build.MANUFACTURER.orEmpty().trim()
        return manufacturer.ifEmpty { null }
    }
}
