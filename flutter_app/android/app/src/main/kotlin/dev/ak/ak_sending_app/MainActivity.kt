package dev.ak.ak_sending_app

/*
 * dev.ak/storage —— 安卓原生存储通道（P0-06 起步，第 15 轮）
 *
 * 范围：只做“目录能力探测 / 目录选择 / SAF 写路径最小闭环证明”。
 * 不做文件正文传输——正文仍由 Rust 引擎直传（见 docs/STORAGE_AND_PLATFORMS.zh-CN.md
 * 与评审 4.4：默认下载目录 + 用户改选走 SAF；SAF 每次操作经跨进程调用，
 * 十万级小文件会明显变慢，故正文路径的最终选择留给 P0 实测数据）。
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
 *      safPersisted   : Boolean contentResolver.persistedUriPermissions 是否非空
 *
 * 2) pickFolder() -> Map
 *      treeUri : String|null   用户确认的目录 tree URI；取消/失败为 null。
 *                              成功选择后本端调用 takePersistableUriPermission(RW)
 *                              持久化授权（失败仅打日志，不改变返回值——
 *                              授权是否真的持久化由 listSafPersisted() 复核）。
 *      重复调用未完成时返回 PlatformException(pick_in_progress)。
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
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.StatFs
import android.provider.DocumentsContract
import android.util.Log
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
        private const val PICK_FOLDER_KEY = "dev.ak/storage:pickFolder"
        private const val EXTERNAL_STORAGE_AUTHORITY = "com.android.externalstorage.documents"
    }

    /** 进行中的 pickFolder 调用的 result；同一时刻至多一个。 */
    private var pendingPickResult: MethodChannel.Result? = null

    private var folderPicker: ActivityResultLauncher<Uri?>? = null

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
        folderPicker = storageResultRegistry.register(
            PICK_FOLDER_KEY,
            ActivityResultContracts.OpenDocumentTree()
        ) { treeUri -> onFolderPicked(treeUri) }

        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, CHANNEL)
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "getStorageInfo" -> result.success(getStorageInfo())
                    "pickFolder" -> onPickFolder(result)
                    "safWriteTest" -> result.success(
                        safWriteTest(call.argument<String>("treeUri"))
                    )
                    "listSafPersisted" -> result.success(listSafPersisted())
                    else -> result.notImplemented()
                }
            }
    }

    override fun onDestroy() {
        pendingPickResult?.let { pending ->
            runCatching {
                pending.error(
                    "activity_destroyed",
                    "host activity destroyed before folder pick completed",
                    null
                )
            }
        }
        pendingPickResult = null
        runCatching { folderPicker?.unregister() }
        folderPicker = null
        runCatching { engineRef?.plugins?.remove(activityResultRouter::class.java) }
        engineRef = null
        super.onDestroy()
    }

    // ── getStorageInfo ────────────────────────────────────────────────────

    private fun getStorageInfo(): Map<String, Any?> {
        val dir = appDownloadDir()
        val freeBytes = try {
            if (dir.exists()) StatFs(dir.absolutePath).availableBytes else -1L
        } catch (t: Throwable) {
            Log.w(TAG, "StatFs failed for ${dir.absolutePath}: $t")
            -1L
        }
        return mapOf(
            "appDownloadDir" to dir.absolutePath,
            "freeBytes" to freeBytes,
            "safPersisted" to contentResolver.persistedUriPermissions.isNotEmpty()
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

    private fun onPickFolder(result: MethodChannel.Result) {
        val launcher = folderPicker
        if (launcher == null) {
            result.error("unavailable", "folder picker launcher not initialized", null)
            return
        }
        if (pendingPickResult != null) {
            result.error("pick_in_progress", "a folder pick is already in flight", null)
            return
        }
        pendingPickResult = result
        try {
            launcher.launch(initialDownloadDocumentUri())
        } catch (t: Throwable) {
            // 典型场景：Android TV 等没有文档选择器的环境（ActivityNotFoundException）。
            // 不当作崩溃：返回 treeUri=null，让 Dart 侧退回默认目录策略。
            Log.w(TAG, "launch OpenDocumentTree failed: $t")
            pendingPickResult = null
            result.success(mapOf("treeUri" to null as String?))
        }
    }

    private fun onFolderPicked(treeUri: Uri?) {
        val pending = pendingPickResult
        pendingPickResult = null
        if (pending == null) {
            Log.w(TAG, "folder picked but no pending result (treeUri=$treeUri)")
            return
        }
        if (treeUri == null) {
            pending.success(mapOf("treeUri" to null as String?))
            return
        }
        try {
            contentResolver.takePersistableUriPermission(
                treeUri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
            )
        } catch (t: Throwable) {
            // 不改变返回值：URI 本身当前可用；持久化是否生效由 listSafPersisted 复核。
            Log.w(TAG, "takePersistableUriPermission failed for $treeUri: $t")
        }
        pending.success(mapOf("treeUri" to treeUri.toString()))
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
        Log.w(TAG, "build initial Download URI failed: $t")
        null
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
            Log.w(TAG, "safWriteTest failed: $message")
            mapOf("ok" to false, "error" to message)
        } finally {
            // 无论成败都清理探测文件；清理失败只记日志，不掩盖原始结果。
            createdDocUri?.let { doc ->
                runCatching { DocumentsContract.deleteDocument(contentResolver, doc) }
                    .onFailure { Log.w(TAG, "probe cleanup failed for $doc: $it") }
            }
        }
    }

    // ── listSafPersisted ──────────────────────────────────────────────────

    private fun listSafPersisted(): List<Map<String, Any?>> =
        contentResolver.persistedUriPermissions.map { permission ->
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
}
