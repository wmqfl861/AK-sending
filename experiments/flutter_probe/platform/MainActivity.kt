package dev.ak.ak_m0_probe

import android.app.Activity
import android.content.Intent
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.CancellationSignal
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

// Process-wide single worker: activity recreation cannot create unbounded workers.
private object DiskProbe {
    val busy = AtomicBoolean(false)
    private val executor = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())
    private val cancel = AtomicReference<CancellationSignal?>()
    private val descriptor = AtomicReference<ParcelFileDescriptor?>()
    fun stop() {
        cancel.get()?.cancel()
        try { descriptor.getAndSet(null)?.close() } catch (_: Exception) { }
    }
    fun run(context: Context, method: String, result: MethodChannel.Result) {
        if (!busy.compareAndSet(false, true)) { result.success("busy"); return }
        val replied = AtomicBoolean(false)
        val signal = CancellationSignal()
        cancel.set(signal)
        val deadline = Runnable {
            stop()
            if (replied.compareAndSet(false, true)) result.success("timeout-restart-required")
        }
        main.postDelayed(deadline, 10000)
        executor.execute {
            var created: Uri? = null
            var outcome = "failed"
            val resolver = context.contentResolver
            try {
                val prefs = context.getSharedPreferences("m0-directory", Context.MODE_PRIVATE)
                val saved = prefs.getString("uri", null)
                if (method == "forget") {
                    prefs.edit().remove("uri").commit()
                    if (saved != null) {
                        try { resolver.releasePersistableUriPermission(Uri.parse(saved), 3) }
                        catch (_: SecurityException) { }
                    }
                    outcome = "forgotten"
                } else if (saved == null) {
                    outcome = "select-required"
                } else {
                    val tree = Uri.parse(saved)
                    val permitted = resolver.persistedUriPermissions.any {
                        it.uri == tree && it.isReadPermission && it.isWritePermission
                    }
                    if (!permitted) {
                        outcome = "permission-revoked"
                    } else {
                        signal.throwIfCanceled()
                        val parent = DocumentsContract.buildDocumentUriUsingTree(tree,
                            DocumentsContract.getTreeDocumentId(tree))
                        created = DocumentsContract.createDocument(resolver, parent,
                            "application/octet-stream", "ak-m0-probe-${UUID.randomUUID()}.tmp")
                        val child = created ?: throw IllegalStateException()
                        signal.throwIfCanceled()
                        val data = ByteArray(64) { 0x42 }
                        val output = resolver.openFileDescriptor(child, "w", signal)
                            ?: throw IllegalStateException()
                        descriptor.set(output)
                        ParcelFileDescriptor.AutoCloseOutputStream(output).use { it.write(data) }
                        descriptor.set(null)
                        signal.throwIfCanceled()
                        val input = resolver.openFileDescriptor(child, "r", signal)
                            ?: throw IllegalStateException()
                        descriptor.set(input)
                        val received = ByteArray(65)
                        var count = 0
                        ParcelFileDescriptor.AutoCloseInputStream(input).use { stream ->
                            while (count < received.size) {
                                val n = stream.read(received, count, received.size - count)
                                if (n < 0) break
                                if (n == 0) throw IllegalStateException()
                                count += n
                                signal.throwIfCanceled()
                            }
                        }
                        descriptor.set(null)
                        signal.throwIfCanceled()
                        outcome = if (count == 64 && received.take(64).all { it == 0x42.toByte() })
                            "synthetic-file-verified" else "verification-failed"
                    }
                }
            } catch (_: Exception) { outcome = "permission-or-provider-failed" }
            finally {
                try { descriptor.getAndSet(null)?.close() } catch (_: Exception) { }
                // Only delete the document created by this invocation, never a chosen directory.
                if (created != null) {
                    try {
                        if (!DocumentsContract.deleteDocument(resolver, created!!)) outcome = "cleanup-failed"
                    } catch (_: Exception) { outcome = "cleanup-failed" }
                }
                cancel.compareAndSet(signal, null)
                main.post {
                    main.removeCallbacks(deadline)
                    busy.set(false)
                    if (replied.compareAndSet(false, true)) result.success(outcome)
                }
            }
        }
    }
}

class MainActivity : FlutterActivity() {
    private var picker: MethodChannel.Result? = null
    private var permission: MethodChannel.Result? = null
    private val localNetwork = "android.permission.ACCESS_LOCAL_NETWORK"
    override fun configureFlutterEngine(engine: FlutterEngine) {
        super.configureFlutterEngine(engine)
        MethodChannel(engine.dartExecutor.binaryMessenger, "ak.m0/risk").setMethodCallHandler { call, result ->
            if (picker != null || permission != null || DiskProbe.busy.get()) {
                result.success("busy")
            } else when (call.method) {
                "network" -> {
                    if (Build.VERSION.SDK_INT < 37) result.success("not-required")
                    else if (checkSelfPermission(localNetwork) == PackageManager.PERMISSION_GRANTED)
                        result.success("granted")
                    else {
                        permission = result
                        try { requestPermissions(arrayOf(localNetwork), 4702) }
                        catch (_: Exception) { permission = null; result.success("denied") }
                    }
                }
                "choose" -> {
                    picker = result
                    try {
                        startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
                            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                                Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
                        }, 4701)
                    } catch (_: Exception) { picker = null; result.success("picker-unavailable") }
                }
                "test", "forget" -> DiskProbe.run(applicationContext, call.method, result)
                else -> result.notImplemented()
            }
        }
    }
    override fun onRequestPermissionsResult(code: Int, names: Array<out String>, grants: IntArray) {
        super.onRequestPermissionsResult(code, names, grants)
        if (code == 4702) {
            val result = permission; permission = null
            result?.success(if (grants.isNotEmpty() && grants[0] == PackageManager.PERMISSION_GRANTED)
                "granted" else "denied")
        }
    }
    override fun onActivityResult(code: Int, status: Int, data: Intent?) {
        super.onActivityResult(code, status, data)
        if (code != 4701) return
        val result = picker ?: return; picker = null
        val uri = data?.data
        if (status != Activity.RESULT_OK || uri == null) { result.success("selection-cancelled"); return }
        try {
            val flags = data.flags and 3
            if (flags != 3) throw SecurityException()
            contentResolver.takePersistableUriPermission(uri, flags)
            val prefs = getSharedPreferences("m0-directory", Context.MODE_PRIVATE)
            val old = prefs.getString("uri", null)
            if (!prefs.edit().putString("uri", uri.toString()).commit()) throw IllegalStateException()
            if (old != null && old != uri.toString()) {
                try { contentResolver.releasePersistableUriPermission(Uri.parse(old), 3) }
                catch (_: SecurityException) { }
            }
            result.success("directory-selected")
        } catch (_: Exception) { result.success("permission-denied") }
    }
    override fun onStop() { DiskProbe.stop(); super.onStop() }
    override fun onDestroy() {
        DiskProbe.stop()
        picker?.success("activity-destroyed"); picker = null
        permission?.success("activity-destroyed"); permission = null
        super.onDestroy()
    }
}
