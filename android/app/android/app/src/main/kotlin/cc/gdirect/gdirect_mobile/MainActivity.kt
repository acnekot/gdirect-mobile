package cc.gdirect.gdirect_mobile

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ContentValues
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Color
import android.graphics.pdf.PdfRenderer
import android.media.MediaScannerConnection
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import java.io.File
import java.util.concurrent.Executors

/** Bridges the few Android features the Dart side needs. */
class MainActivity : FlutterActivity() {
    private val worker = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())
    private var channel: MethodChannel? = null

    // Documents handed to the engine as open descriptors, by descriptor number.
    private val descriptors = HashMap<Int, ParcelFileDescriptor>()
    private var pendingPick: MethodChannel.Result? = null
    private val shared = ArrayList<Map<String, Any?>>()

    // Networks that are not a VPN, newest last.
    private val underlying = LinkedHashSet<Network>()
    private var bypassVpn = false
    private var bound: Network? = null
    private var watching = false

    private var pdf: PdfRenderer? = null
    private var pdfPath: String? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        val channel = MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "cc.gdirect/native")
        this.channel = channel
        channel.setMethodCallHandler { call, result -> handle(call, result) }
        collectShared(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        if (collectShared(intent)) channel?.invokeMethod("shared", null)
    }

    /** Runs slow work off the main thread and answers on it. */
    private fun background(result: MethodChannel.Result, code: String, work: () -> Any?) {
        worker.execute {
            try {
                val value = work()
                main.post { result.success(value) }
            } catch (e: Exception) {
                main.post { result.error(code, e.message ?: e.javaClass.simpleName, null) }
            }
        }
    }

    private fun handle(call: MethodCall, result: MethodChannel.Result) {
        when (call.method) {
            "saveToDownloads" -> {
                val path = call.argument<String>("path")
                val name = call.argument<String>("name")
                val subdir = call.argument<String>("subdir")
                if (path == null || name == null) {
                    result.error("args", "path and name are required", null)
                } else {
                    background(result, "save") { saveToDownloads(File(path), name, subdir) }
                }
            }
            "downloadsDir" -> {
                // From Android 11 an app may create files in Download/ by path.
                if (Build.VERSION.SDK_INT >= 30) {
                    val dir = File(
                        Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS),
                        "gdirect"
                    )
                    result.success(dir.absolutePath)
                } else {
                    result.success(null)
                }
            }
            "scanFile" -> {
                val path = call.argument<String>("path")
                if (path == null) {
                    result.error("args", "path is required", null)
                } else {
                    MediaScannerConnection.scanFile(this, arrayOf(path), null) { _, uri ->
                        main.post {
                            if (uri != null) result.success(uri.toString())
                            else result.error("scan", "the file could not be added to Downloads", null)
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
                    .putExtra("progress", call.argument<Int>("progress") ?: -1)
                try {
                    if (on) startForegroundService(intent) else stopService(intent)
                } catch (e: Exception) {
                    // Starting from the background can be refused; transfers go on.
                }
                result.success(null)
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
            "bypassVpn" -> {
                bypassVpn = call.argument<Boolean>("on") ?: false
                watchNetworks()
                applyBinding()
                result.success(networkState())
            }
            "networkState" -> {
                watchNetworks()
                result.success(networkState())
            }
            "pickFiles" -> pick(result, Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                type = "*/*"
                putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
            }, PICK_FILES)
            "pickFolder" -> pick(result, Intent(Intent.ACTION_OPEN_DOCUMENT_TREE), PICK_FOLDER)
            "openFd" -> {
                val uri = call.argument<String>("uri")
                if (uri == null) {
                    result.error("args", "uri is required", null)
                } else {
                    background(result, "open") {
                        val pfd = contentResolver.openFileDescriptor(Uri.parse(uri), "r")
                            ?: throw IllegalStateException("the file cannot be opened")
                        synchronized(descriptors) { descriptors[pfd.fd] = pfd }
                        pfd.fd
                    }
                }
            }
            "closeFd" -> {
                val fd = call.argument<Int>("fd")
                val pfd = synchronized(descriptors) { descriptors.remove(fd) }
                try {
                    pfd?.close()
                } catch (e: Exception) {
                    // Already gone.
                }
                result.success(null)
            }
            "readText" -> {
                val uri = call.argument<String>("uri")
                if (uri == null) {
                    result.error("args", "uri is required", null)
                } else {
                    background(result, "read") {
                        val stream = contentResolver.openInputStream(Uri.parse(uri))
                            ?: throw IllegalStateException("the file cannot be opened")
                        stream.use { input ->
                            // A settings file, not a book: stop at 256 KB.
                            val buffer = ByteArray(256 * 1024)
                            var filled = 0
                            while (filled < buffer.size) {
                                val n = input.read(buffer, filled, buffer.size - filled)
                                if (n < 0) break
                                filled += n
                            }
                            String(buffer, 0, filled, Charsets.UTF_8)
                        }
                    }
                }
            }
            "takeShared" -> {
                val items = ArrayList(shared)
                shared.clear()
                result.success(items)
            }
            "pdfPages" -> {
                val path = call.argument<String>("path")
                if (path == null) result.error("args", "path is required", null)
                else background(result, "pdf") { openPdf(path).pageCount }
            }
            "pdfPage" -> {
                val path = call.argument<String>("path")
                val index = call.argument<Int>("index") ?: 0
                val width = (call.argument<Int>("width") ?: 1080).coerceIn(200, 2400)
                if (path == null) result.error("args", "path is required", null)
                else background(result, "pdf") { renderPdfPage(path, index, width) }
            }
            "pdfClose" -> {
                worker.execute { closePdf() }
                result.success(null)
            }
            else -> result.notImplemented()
        }
    }

    // ---- downloads ---------------------------------------------------------

    /** Copies a finished download into the public Downloads/gdirect folder. */
    private fun saveToDownloads(source: File, name: String, subdir: String?): String {
        val mime = MimeTypeMap.getSingleton()
            .getMimeTypeFromExtension(source.extension.lowercase())
            ?: "application/octet-stream"
        val folder = listOf("Download", "gdirect")
            .plus(subdir.orEmpty().split('/').filter { it.isNotEmpty() && it != "." && it != ".." })
            .joinToString("/")
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, name)
            put(MediaStore.Downloads.MIME_TYPE, mime)
            put(MediaStore.Downloads.RELATIVE_PATH, folder)
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

    // ---- picking and sharing ----------------------------------------------

    private fun pick(result: MethodChannel.Result, intent: Intent, code: Int) {
        pendingPick?.success(null)
        pendingPick = result
        try {
            startActivityForResult(intent, code)
        } catch (e: ActivityNotFoundException) {
            pendingPick = null
            result.error("pick", "no file picker is installed", null)
        }
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (requestCode != PICK_FILES && requestCode != PICK_FOLDER) {
            super.onActivityResult(requestCode, resultCode, data)
            return
        }
        val result = pendingPick ?: return
        pendingPick = null
        if (resultCode != Activity.RESULT_OK || data == null) {
            result.success(null)
            return
        }
        if (requestCode == PICK_FILES) {
            val uris = ArrayList<Uri>()
            data.clipData?.let { clip -> for (i in 0 until clip.itemCount) uris.add(clip.getItemAt(i).uri) }
            if (uris.isEmpty()) data.data?.let { uris.add(it) }
            background(result, "pick") {
                uris.map { uri ->
                    keep(uri)
                    describe(uri)
                }
            }
        } else {
            val tree = data.data
            if (tree == null) {
                result.success(null)
            } else {
                background(result, "pick") {
                    keep(tree)
                    walk(tree)
                }
            }
        }
    }

    /** Keeps read access to a picked document across restarts, when offered. */
    private fun keep(uri: Uri) {
        try {
            contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        } catch (e: Exception) {
            // Not every provider offers lasting access; this run still has it.
        }
    }

    private fun describe(uri: Uri): Map<String, Any?> {
        var name: String? = null
        var size: Long? = null
        try {
            contentResolver.query(uri, null, null, null, null)?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val n = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    val s = cursor.getColumnIndex(OpenableColumns.SIZE)
                    if (n >= 0 && !cursor.isNull(n)) name = cursor.getString(n)
                    if (s >= 0 && !cursor.isNull(s)) size = cursor.getLong(s)
                }
            }
        } catch (e: Exception) {
            // Fall back to the address below.
        }
        return mapOf(
            "uri" to uri.toString(),
            "name" to (name ?: uri.lastPathSegment?.substringAfterLast('/') ?: "file"),
            "size" to size,
        )
    }

    /** Lists every file below a picked folder with its path inside it. */
    private fun walk(tree: Uri): Map<String, Any?> {
        val files = ArrayList<Map<String, Any?>>()
        val rootId = DocumentsContract.getTreeDocumentId(tree)
        var rootName = "folder"
        contentResolver.query(
            DocumentsContract.buildDocumentUriUsingTree(tree, rootId),
            arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME), null, null, null
        )?.use { if (it.moveToFirst() && !it.isNull(0)) rootName = it.getString(0) }
        val pending = ArrayDeque<Pair<String, String>>()
        pending.add(rootId to "")
        while (pending.isNotEmpty()) {
            val (id, path) = pending.removeFirst()
            contentResolver.query(
                DocumentsContract.buildChildDocumentsUriUsingTree(tree, id),
                arrayOf(
                    DocumentsContract.Document.COLUMN_DOCUMENT_ID,
                    DocumentsContract.Document.COLUMN_DISPLAY_NAME,
                    DocumentsContract.Document.COLUMN_MIME_TYPE,
                    DocumentsContract.Document.COLUMN_SIZE,
                ),
                null, null, null
            )?.use { cursor ->
                while (cursor.moveToNext()) {
                    val child = cursor.getString(0)
                    val name = cursor.getString(1) ?: continue
                    if (cursor.getString(2) == DocumentsContract.Document.MIME_TYPE_DIR) {
                        pending.add(child to if (path.isEmpty()) name else "$path/$name")
                    } else {
                        if (files.size >= MAX_FOLDER_FILES) {
                            throw IllegalStateException("this folder has more than $MAX_FOLDER_FILES files")
                        }
                        files.add(
                            mapOf(
                                "uri" to DocumentsContract.buildDocumentUriUsingTree(tree, child).toString(),
                                "name" to name,
                                "path" to path,
                                "size" to if (cursor.isNull(3)) null else cursor.getLong(3),
                            )
                        )
                    }
                }
            }
        }
        return mapOf("name" to rootName, "files" to files)
    }

    /** Remembers files another app shared with us; true when there are some. */
    @Suppress("DEPRECATION")
    private fun collectShared(intent: Intent?): Boolean {
        if (intent == null) return false
        val uris = when (intent.action) {
            Intent.ACTION_SEND -> listOfNotNull(intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM))
            Intent.ACTION_SEND_MULTIPLE ->
                intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM).orEmpty()
            else -> emptyList()
        }
        if (uris.isEmpty()) return false
        for (uri in uris) shared.add(describe(uri))
        // Handled: a rotation must not deliver the same files again.
        intent.action = Intent.ACTION_MAIN
        intent.removeExtra(Intent.EXTRA_STREAM)
        return true
    }

    // ---- networks ----------------------------------------------------------

    private fun connectivity() = getSystemService(ConnectivityManager::class.java)

    private fun watchNetworks() {
        if (watching) return
        watching = true
        val cm = connectivity()
        // The default request excludes VPNs: these are the real networks.
        val request = NetworkRequest.Builder()
            .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .build()
        cm.registerNetworkCallback(request, object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) = changed { underlying.add(network) }
            override fun onLost(network: Network) = changed { underlying.remove(network) }
        })
        // The default network tells us when a VPN comes up or goes away.
        cm.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) = changed {}
            override fun onLost(network: Network) = changed {}
            override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) = changed {}
        })
    }

    private fun changed(update: () -> Unit) {
        main.post {
            update()
            val before = networkState()
            applyBinding()
            val after = networkState()
            if (before != after || lastReported != after) {
                lastReported = after
                channel?.invokeMethod("network", after)
            }
        }
    }

    private var lastReported: Map<String, Any?>? = null

    private fun vpnActive(): Boolean {
        val cm = connectivity()
        val caps = cm.getNetworkCapabilities(cm.activeNetwork ?: return false) ?: return false
        return caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)
    }

    /** The real network transfers should use: Wi-Fi first, newest first. */
    private fun preferred(): Network? {
        val cm = connectivity()
        val live = underlying.toList().reversed()
        return live.firstOrNull {
            cm.getNetworkCapabilities(it)?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true
        } ?: live.firstOrNull()
    }

    /**
     * While a VPN is up and bypassing is wanted, sends this app's connections
     * over the real network instead; otherwise leaves routing to the system.
     */
    private fun applyBinding() {
        val target = if (bypassVpn && vpnActive()) preferred() else null
        if (target == bound) return
        bound = if (connectivity().bindProcessToNetwork(target)) target else null
    }

    private fun networkState(): Map<String, Any?> {
        val cm = connectivity()
        val current = preferred()
        val caps = current?.let { cm.getNetworkCapabilities(it) }
        val kind = when {
            caps == null -> "none"
            caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> "wifi"
            caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> "cellular"
            caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> "ethernet"
            else -> "other"
        }
        return mapOf(
            // Changes whenever the device moves to another network.
            "id" to (current?.toString() ?: ""),
            "kind" to kind,
            "vpn" to vpnActive(),
            "bypassing" to (bound != null),
        )
    }

    // ---- PDF ---------------------------------------------------------------

    private fun openPdf(path: String): PdfRenderer {
        pdf?.let { if (pdfPath == path) return it }
        closePdf()
        val renderer = PdfRenderer(ParcelFileDescriptor.open(File(path), ParcelFileDescriptor.MODE_READ_ONLY))
        pdf = renderer
        pdfPath = path
        return renderer
    }

    private fun closePdf() {
        try {
            pdf?.close()
        } catch (e: Exception) {
            // Nothing to release.
        }
        pdf = null
        pdfPath = null
    }

    /** Renders one page to a PNG in the cache and returns its path. */
    private fun renderPdfPage(path: String, index: Int, width: Int): String {
        val renderer = openPdf(path)
        val dir = File(cacheDir, "pdf").apply { mkdirs() }
        val out = File(dir, "${path.hashCode()}-$index-$width.png")
        if (out.exists() && out.length() > 0) return out.absolutePath
        renderer.openPage(index).use { page ->
            val height = (width.toLong() * page.height / page.width).toInt().coerceIn(1, 8000)
            val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
            bitmap.eraseColor(Color.WHITE)
            page.render(bitmap, null, null, PdfRenderer.Page.RENDER_MODE_FOR_DISPLAY)
            out.outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
            bitmap.recycle()
        }
        return out.absolutePath
    }

    companion object {
        private const val PICK_FILES = 4101
        private const val PICK_FOLDER = 4102
        private const val MAX_FOLDER_FILES = 2000
    }
}
