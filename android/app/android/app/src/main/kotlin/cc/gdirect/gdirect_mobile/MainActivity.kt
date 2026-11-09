package cc.gdirect.gdirect_mobile

import android.Manifest
import android.content.ActivityNotFoundException
import android.content.ContentValues
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.MediaStore
import android.webkit.MimeTypeMap
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File
import java.util.concurrent.Executors

/** Bridges the few Android features the Dart side needs. */
class MainActivity : FlutterActivity() {
    private val worker = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "cc.gdirect/native")
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "saveToDownloads" -> {
                        val path = call.argument<String>("path")
                        val name = call.argument<String>("name")
                        if (path == null || name == null) {
                            result.error("args", "path and name are required", null)
                        } else {
                            worker.execute {
                                try {
                                    val uri = saveToDownloads(File(path), name)
                                    main.post { result.success(uri) }
                                } catch (e: Exception) {
                                    main.post { result.error("save", e.message, null) }
                                }
                            }
                        }
                    }
                    "openUri" -> {
                        val uri = call.argument<String>("uri")
                        if (uri == null) {
                            result.error("args", "uri is required", null)
                        } else {
                            val mime = call.argument<String>("mime")
                                ?: contentResolver.getType(Uri.parse(uri))
                                ?: "*/*"
                            val intent = Intent(Intent.ACTION_VIEW).apply {
                                setDataAndType(Uri.parse(uri), mime)
                                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                            }
                            try {
                                startActivity(intent)
                                result.success(null)
                            } catch (e: ActivityNotFoundException) {
                                result.error("open", "no app can open this file", null)
                            }
                        }
                    }
                    "keepAlive" -> {
                        val on = call.argument<Boolean>("on") ?: false
                        val intent = Intent(this, KeepAliveService::class.java)
                            .putExtra("text", call.argument<String>("text") ?: "")
                        try {
                            if (on) startForegroundService(intent) else stopService(intent)
                            result.success(null)
                        } catch (e: Exception) {
                            // Starting from the background can be refused; transfers go on.
                            result.success(null)
                        }
                    }
                    "notifyPermission" -> {
                        if (Build.VERSION.SDK_INT >= 33 &&
                            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) !=
                            PackageManager.PERMISSION_GRANTED
                        ) {
                            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
                        }
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }
    }

    /** Copies a finished download into the public Downloads/gdirect folder. */
    private fun saveToDownloads(source: File, name: String): String {
        val mime = MimeTypeMap.getSingleton()
            .getMimeTypeFromExtension(source.extension.lowercase())
            ?: "application/octet-stream"
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, name)
            put(MediaStore.Downloads.MIME_TYPE, mime)
            put(MediaStore.Downloads.RELATIVE_PATH, "Download/gdirect")
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        val resolver = contentResolver
        val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
            ?: throw IllegalStateException("cannot create the file in Downloads")
        try {
            val out = resolver.openOutputStream(uri)
                ?: throw IllegalStateException("cannot write to Downloads")
            out.use { sink -> source.inputStream().use { it.copyTo(sink, 1 shl 20) } }
            val done = ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }
            resolver.update(uri, done, null, null)
        } catch (e: Exception) {
            resolver.delete(uri, null, null)
            throw e
        }
        return uri.toString()
    }
}
