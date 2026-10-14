package org.hitode.storage

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.CancellationSignal
import android.os.Environment
import android.os.SystemClock
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.provider.Settings
import androidx.activity.result.ActivityResult
import androidx.appcompat.app.AppCompatActivity
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
import kotlin.concurrent.thread

@InvokeArg
class FolderArgs {
    lateinit var uri: String
}

@InvokeArg
class CopyArgs {
    lateinit var id: String
    lateinit var uri: String
    lateinit var dest: String
    var progress: Channel? = null
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

    private fun granted() = Build.VERSION.SDK_INT >= 30 && Environment.isExternalStorageManager()

    @Command
    fun storageStatus(invoke: Invoke) {
        invoke.resolve(JSObject().put("granted", granted()))
    }

    @Command
    fun requestStorage(invoke: Invoke) {
        if (Build.VERSION.SDK_INT < 30 || granted()) {
            storageStatus(invoke)
            return
        }
        try {
            startActivityForResult(invoke, Intent(
                Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                Uri.parse("package:${activity.packageName}")
            ), "storageResult")
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
            val flags = result.data!!.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION
            resolver.takePersistableUriPermission(uri, flags)
            val path = if (uri.authority == "com.android.externalstorage.documents") {
                primaryPath(DocumentsContract.getTreeDocumentId(uri), granted())
            } else null
            invoke.resolve(JSObject().put("uri", uri.toString()).put("path", path))
        } catch (error: Exception) {
            invoke.reject(error.message)
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
