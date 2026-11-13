package org.hitode.storage

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.CancellationSignal
import android.os.Environment
import android.os.SystemClock
import android.os.storage.StorageManager
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.provider.Settings
import androidx.activity.result.ActivityResult
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.FileProvider
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.nio.file.Files
import kotlin.concurrent.thread

@InvokeArg
class FolderArgs {
    lateinit var uri: String
}

@InvokeArg
class RequestStorageArgs {
    var open: Boolean = false
}

@InvokeArg
class CopyArgs {
    lateinit var id: String
    lateinit var uri: String
    lateinit var dest: String
    var progress: Channel? = null
}

@InvokeArg
class ExportArgs {
    lateinit var source: String
    lateinit var name: String
}

@InvokeArg
class PickFileArgs {
    lateinit var dest: String
}

@InvokeArg
class ShareFileArgs {
    lateinit var path: String
}

@InvokeArg
class CancelArgs {
    lateinit var id: String
}

private class CopyJob(val id: String) {
    val signal = CancellationSignal()
    @Volatile var stream: InputStream? = null
    var bytes = 0L
    var lastProgress = 0L

    fun cancel() {
        signal.cancel()
        try { stream?.close() } catch (_: IOException) { }
    }
}

private data class Entry(val id: String, val name: String, val directory: Boolean, val size: Long?)

@TauriPlugin
class StoragePlugin(private val activity: Activity) : Plugin(activity) {
    private val resolver = activity.contentResolver
    @Volatile private var copying: CopyJob? = null
    private var pendingCancel: String? = null

    @Command
    fun shareFile(invoke: Invoke) {
        activity.runOnUiThread {
            try {
                val file = File(invoke.parseArgs(ShareFileArgs::class.java).path)
                val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", file)
                val intent = Intent(Intent.ACTION_SEND).apply {
                    type = "application/json"
                    putExtra(Intent.EXTRA_STREAM, uri)
                    clipData = ClipData.newRawUri(file.name, uri)
                    addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                }
                activity.startActivity(Intent.createChooser(intent, null))
                invoke.resolve()
            } catch (error: Exception) {
                invoke.reject(error.message ?: "Could not share report")
            }
        }
    }

    private fun supported() = Build.VERSION.SDK_INT >= 30

    private fun granted() = supported() && Environment.isExternalStorageManager()

    private fun storageRoots(): List<StorageRoot> {
        if (!supported()) return emptyList()
        val roots = activity.getSystemService(StorageManager::class.java)
            ?.storageVolumes
            ?.mapNotNull { volume ->
                val mounted = volume.state == Environment.MEDIA_MOUNTED ||
                    volume.state == Environment.MEDIA_MOUNTED_READ_ONLY
                if (!mounted) return@mapNotNull null
                val dir = volume.directory ?: return@mapNotNull null
                val id = if (volume.isPrimary) "primary" else volume.uuid ?: return@mapNotNull null
                StorageRoot(id, dir, writable = volume.state == Environment.MEDIA_MOUNTED)
            }
            .orEmpty()
        return if (roots.any { it.id == "primary" }) roots
        else roots + StorageRoot("primary", Environment.getExternalStorageDirectory())
    }

    @Command
    fun storageStatus(invoke: Invoke) {
        invoke.resolve(JSObject().put("granted", granted()).put("supported", supported()))
    }

    @Command
    fun requestStorage(invoke: Invoke) {
        val open = runCatching { invoke.parseArgs(RequestStorageArgs::class.java).open }.getOrDefault(false)
        if (!supported() || (!open && granted())) {
            storageStatus(invoke)
            return
        }
        val appIntent = Intent(
            Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
            Uri.parse("package:${activity.packageName}")
        )
        try {
            try {
                startActivityForResult(invoke, appIntent, "storageResult")
            } catch (_: ActivityNotFoundException) {
                startActivityForResult(
                    invoke,
                    Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION),
                    "storageResult"
                )
            }
        } catch (error: Exception) {
            invoke.reject(error.message)
        }
    }

    @ActivityCallback
    fun storageResult(invoke: Invoke, result: ActivityResult) {
        storageStatus(invoke)
    }

    @Command
    fun pickFolder(invoke: Invoke) {
        try {
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).addFlags(
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
            )
            startActivityForResult(invoke, intent, "folderResult")
        } catch (error: Exception) {
            invoke.reject(error.message)
        }
    }

    @ActivityCallback
    fun folderResult(invoke: Invoke, result: ActivityResult) {
        if (result.resultCode == Activity.RESULT_CANCELED) {
            invoke.resolve(JSObject().put("uri", null))
            return
        }
        try {
            val uri = result.data?.data ?: throw IOException("No folder selected")
            val id = DocumentsContract.getTreeDocumentId(uri)
            if (uri.authority == activity.packageName + ".drive" && (id == "Games:" || id.startsWith("Games:/"))) {
                val root = File(activity.dataDir.canonicalFile, "imports")
                val folder = if (id == "Games:") root else {
                    val parts = id.removePrefix("Games:/").split('/')
                    if (parts.any { it.isEmpty() || it == "." || it == ".." } ||
                        !parts.first().matches(Regex("[A-Za-z0-9_-]+"))) {
                        throw IOException("Invalid Hitode folder")
                    }
                    File(root, parts.joinToString("/"))
                }
                if (folder.canonicalFile != folder || !folder.isDirectory) {
                    throw IOException("Invalid Hitode folder")
                }
                invoke.resolve(JSObject().put("uri", uri.toString()).put("path", folder.path).put("managed", true))
                return
            }
            val flags = result.data!!.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION
            resolver.takePersistableUriPermission(uri, flags)
            val roots = storageRoots()
            val isGranted = granted()
            val path = if (uri.authority == "com.android.externalstorage.documents") {
                writableFolderPath(id, isGranted, roots)
            } else null
            val canGrant = path == null &&
                canGrantDirectAccess(uri.authority, id, isGranted, supported(), roots)
            invoke.resolve(
                JSObject()
                    .put("uri", uri.toString())
                    .put("path", path)
                    .put("canGrant", canGrant)
            )
        } catch (error: Exception) {
            invoke.reject(error.message)
        }
    }

    @Command
    fun resolveFolder(invoke: Invoke) {
        try {
            val uri = Uri.parse(invoke.parseArgs(FolderArgs::class.java).uri)
            val id = DocumentsContract.getTreeDocumentId(uri)
            val path = if (uri.authority == "com.android.externalstorage.documents") {
                writableFolderPath(id, granted(), storageRoots())
            } else null
            invoke.resolve(JSObject().put("path", path))
        } catch (error: Exception) {
            invoke.reject(error.message)
        }
    }

    /** One file: its real path when shared storage is reachable, else a private copy. */
    @Command
    fun pickFile(invoke: Invoke) {
        try {
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT)
                .addCategory(Intent.CATEGORY_OPENABLE)
                .setType("*/*")
            startActivityForResult(invoke, intent, "fileResult")
        } catch (error: Exception) {
            invoke.reject(error.message)
        }
    }

    @ActivityCallback
    fun fileResult(invoke: Invoke, result: ActivityResult) {
        if (result.resultCode == Activity.RESULT_CANCELED) {
            invoke.resolve(JSObject().put("path", null))
            return
        }
        thread(name = "file-copy") {
            try {
                val uri = result.data?.data ?: throw IOException("No file selected")
                val direct = directPath(uri)
                if (direct != null) {
                    invoke.resolve(JSObject().put("path", direct))
                    return@thread
                }
                val dest = File(invoke.parseArgs(PickFileArgs::class.java).dest)
                val name = resolver.query(uri, arrayOf(Document.COLUMN_DISPLAY_NAME), null, null, null)
                    ?.use { if (it.moveToFirst()) it.getString(0) else null }
                    ?: throw IOException("Cannot read the file name")
                // A folder per copy keeps the original name without collisions.
                val folder = Files.createTempDirectory(dest.toPath(), "file-").toFile()
                val file = File(folder, checkedName(name))
                try {
                    val input = resolver.openInputStream(uri) ?: throw IOException("Cannot open file")
                    input.use { file.outputStream().use { output -> it.copyTo(output, 65536) } }
                } catch (error: Exception) {
                    folder.deleteRecursively()
                    throw error
                }
                invoke.resolve(JSObject().put("path", file.path))
            } catch (error: Exception) {
                invoke.reject(error.message)
            }
        }
    }

    /** External-storage volumes and Downloads documents name a real file under all-files access. */
    private fun directPath(uri: Uri): String? {
        if (!granted() || !DocumentsContract.isDocumentUri(activity, uri)) return null
        val id = DocumentsContract.getDocumentId(uri)
        return when (uri.authority) {
            "com.android.externalstorage.documents" -> readableFilePath(id, true, storageRoots())
            "com.android.providers.downloads.documents" -> id.removePrefix("raw:")
                .takeIf { id.startsWith("raw:") }
                ?.takeIf { File(it).let { file -> file.isFile && file.canRead() } }
            else -> null
        }
    }

    private fun entries(tree: Uri, parent: String, signal: CancellationSignal): List<Entry> {
        signal.throwIfCanceled()
        val uri = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parent)
        val columns = arrayOf(Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME,
            Document.COLUMN_MIME_TYPE, Document.COLUMN_SIZE)
        val cursor = resolver.query(uri, columns, null, null, null, signal)
            ?: throw IOException("Cannot read folder")
        return cursor.use {
            val found = mutableListOf<Entry>()
            while (it.moveToNext()) {
                signal.throwIfCanceled()
                found.add(Entry(it.getString(0), it.getString(1),
                    it.getString(2) == Document.MIME_TYPE_DIR, if (it.isNull(3)) null else it.getLong(3)))
            }
            found
        }
    }

    private fun open(tree: Uri, id: String, signal: CancellationSignal): InputStream {
        val uri = DocumentsContract.buildDocumentUriUsingTree(tree, id)
        return resolver.openAssetFileDescriptor(uri, "r", signal)?.createInputStream()
            ?: throw IOException("Cannot open source file")
    }

    private fun size(tree: Uri, id: String, signal: CancellationSignal): Long {
        var bytes = 0L
        for (entry in entries(tree, id, signal)) {
            bytes += if (entry.directory) size(tree, entry.id, signal) else entry.size
                ?: open(tree, entry.id, signal).use { input ->
                    val buffer = ByteArray(65536)
                    var count = 0L
                    while (true) {
                        signal.throwIfCanceled()
                        val n = input.read(buffer)
                        if (n < 0) break
                        count += n
                    }
                    count
                }
        }
        return bytes
    }

    @Command
    fun folderSize(invoke: Invoke) {
        thread(name = "folder-size") {
            try {
                val tree = Uri.parse(invoke.parseArgs(FolderArgs::class.java).uri)
                invoke.resolveObject(size(tree, DocumentsContract.getTreeDocumentId(tree), CancellationSignal()))
            } catch (error: Exception) {
                invoke.reject(error.message)
            }
        }
    }

    private fun checkedName(name: String): String {
        if (name.isEmpty() || name == "." || name == ".." || name.contains('/') || name.contains('\u0000')) {
            throw IOException("Invalid source file name")
        }
        return name
    }

    private fun rootName(tree: Uri, id: String, signal: CancellationSignal): String {
        val uri = DocumentsContract.buildDocumentUriUsingTree(tree, id)
        val cursor = resolver.query(uri, arrayOf(Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE),
            null, null, null, signal) ?: throw IOException("Cannot read selected folder")
        return cursor.use {
            if (!it.moveToFirst() || it.getString(1) != Document.MIME_TYPE_DIR) {
                throw IOException("Select a game folder")
            }
            checkedName(it.getString(0))
        }
    }

    private fun progress(job: CopyJob, channel: Channel?, file: String, force: Boolean = false) {
        val now = SystemClock.elapsedRealtime()
        if (force || now - job.lastProgress >= 100) {
            channel?.send(JSObject().put("bytes", job.bytes).put("file", file))
            job.lastProgress = now
        }
    }

    private fun copy(tree: Uri, id: String, directory: File, job: CopyJob, channel: Channel?) {
        val buffer = ByteArray(65536)
        for (entry in entries(tree, id, job.signal)) {
            job.signal.throwIfCanceled()
            val dest = File(directory, checkedName(entry.name))
            if (entry.directory) {
                if (!dest.mkdir()) throw IOException("Cannot create game subfolder")
                copy(tree, entry.id, dest, job, channel)
            } else {
                if (!dest.createNewFile()) throw IOException("Duplicate source file name")
                open(tree, entry.id, job.signal).use { input ->
                    job.stream = input
                    try {
                        dest.outputStream().use { output ->
                            while (true) {
                                job.signal.throwIfCanceled()
                                val n = input.read(buffer)
                                if (n < 0) break
                                output.write(buffer, 0, n)
                                job.bytes += n
                                progress(job, channel, entry.name)
                            }
                        }
                    } finally {
                        job.stream = null
                    }
                }
            }
        }
    }

    @Command
    fun copyFolder(invoke: Invoke) {
        if (copying != null) {
            invoke.reject("A folder import is already running")
            return
        }
        val args = try { invoke.parseArgs(CopyArgs::class.java) } catch (error: Exception) {
            invoke.reject(error.message)
            return
        }
        val job = CopyJob(args.id)
        if (pendingCancel == args.id) {
            job.cancel()
            pendingCancel = null
        }
        copying = job
        thread(name = "folder-copy") {
            try {
                val tree = Uri.parse(args.uri)
                val id = DocumentsContract.getTreeDocumentId(tree)
                val dest = File(args.dest).canonicalFile
                val name = rootName(tree, id, job.signal)
                progress(job, args.progress, name, true)
                publishFolder(dest) { staging ->
                    val root = File(staging, name)
                    if (!root.mkdir()) throw IOException("Cannot create game folder")
                    copy(tree, id, root, job, args.progress)
                    job.signal.throwIfCanceled()
                }
                progress(job, args.progress, name, true)
                invoke.resolve(JSObject().put("path", File(dest, name).path))
            } catch (error: Exception) {
                invoke.reject(error.message)
            } finally {
                copying = null
            }
        }
    }

    @Command
    fun exportFolder(invoke: Invoke) {
        try {
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            startActivityForResult(invoke, intent, "exportResult")
        } catch (error: Exception) {
            invoke.reject(error.message)
        }
    }

    private fun exportEntries(source: File, parent: Uri) {
        for (file in source.listFiles() ?: throw IOException("Cannot read source folder")) {
            if (Files.isSymbolicLink(file.toPath())) throw IOException("Cannot export symbolic links")
            if (!file.isDirectory && !file.isFile) throw IOException("Cannot export a special file")
            val target = DocumentsContract.createDocument(
                resolver, parent, if (file.isDirectory) Document.MIME_TYPE_DIR else "application/octet-stream", file.name
            ) ?: throw IOException("Cannot create exported entry")
            if (file.isDirectory) exportEntries(file, target)
            else {
                val output = resolver.openOutputStream(target, "w") ?: throw IOException("Cannot write exported file")
                output.use { stream -> file.inputStream().use { it.copyTo(stream) } }
            }
        }
    }

    @ActivityCallback
    fun exportResult(invoke: Invoke, result: ActivityResult) {
        if (result.resultCode == Activity.RESULT_CANCELED) {
            invoke.resolve()
            return
        }
        thread(name = "folder-export") {
            var created: Uri? = null
            try {
                val tree = result.data?.data ?: throw IOException("No folder selected")
                if (tree.authority == activity.packageName + ".drive") {
                    throw IOException("Choose a destination outside Hitode")
                }
                val args = invoke.parseArgs(ExportArgs::class.java)
                val id = DocumentsContract.getTreeDocumentId(tree)
                val parent = DocumentsContract.buildDocumentUriUsingTree(tree, id)
                if (entries(tree, id, CancellationSignal()).any { it.name == args.name }) {
                    throw IOException("Destination already exists")
                }
                val exported = DocumentsContract.createDocument(
                    resolver, parent, Document.MIME_TYPE_DIR, args.name
                ) ?: throw IOException("Cannot create destination folder")
                created = exported
                exportEntries(File(args.source), exported)
                val path = if (exported.authority == "com.android.externalstorage.documents") {
                    writableFolderPath(DocumentsContract.getDocumentId(exported), granted(), storageRoots())
                } else null
                invoke.resolve(JSObject().put("label", path ?: args.name).put("path", path))
            } catch (error: Exception) {
                created?.let { runCatching { DocumentsContract.deleteDocument(resolver, it) } }
                invoke.reject(error.message)
            }
        }
    }

    @Command
    fun cancelCopy(invoke: Invoke) {
        try {
            val id = invoke.parseArgs(CancelArgs::class.java).id
            val job = copying
            if (job?.id == id) job.cancel() else pendingCancel = id
            invoke.resolve()
        } catch (error: Exception) {
            invoke.reject(error.message)
        }
    }

    override fun onDestroy(activity: AppCompatActivity) {
        copying?.cancel()
    }
}
