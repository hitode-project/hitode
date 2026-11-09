package org.hitode.launcher

import android.database.Cursor
import android.database.MatrixCursor
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.provider.DocumentsContract.Root
import android.provider.DocumentsProvider
import android.webkit.MimeTypeMap
import java.io.File
import java.io.FileNotFoundException
import java.io.IOException
import java.nio.file.FileVisitResult
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.SimpleFileVisitor
import java.nio.file.attribute.BasicFileAttributes

/**
 * Drive C: and managed game copies as separate roots of the system Files app.
 * DocumentsUI rejects an empty root ID, so documents are identified by
 * `C:` or `Games:` followed by the path below that root.
 */
class DriveProvider : DocumentsProvider() {
    companion object {
        const val AUTHORITY = BuildConfig.APPLICATION_ID + ".drive"
        const val ROOT = "drive"
        private const val DRIVE = "C:"
        const val GAMES = "Games:"
        private const val IMPORTS = "imports"
        @Volatile var gameRunning = false
        private val ROOT_COLUMNS = arrayOf(
            Root.COLUMN_ROOT_ID, Root.COLUMN_FLAGS, Root.COLUMN_TITLE,
            Root.COLUMN_DOCUMENT_ID, Root.COLUMN_ICON,
        )
        private val DOCUMENT_COLUMNS = arrayOf(
            Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE,
            Document.COLUMN_FLAGS, Document.COLUMN_SIZE, Document.COLUMN_LAST_MODIFIED,
        )
    }

    /**
     * Where `drive.rs` keeps drive C: on Android: the prefix in Tauri's app
     * data directory, which is the app's own data directory.
     */
    private val drive: File
        get() = File(context!!.dataDir, "wine/drive_c")

    private val imports: File
        get() = File(context!!.dataDir, IMPORTS)

    override fun onCreate() = true

    private fun managed(id: String) = id == GAMES || id.startsWith("$GAMES/")
    private fun root(id: String) = if (managed(id)) imports else drive
    private fun rootId(id: String) = if (managed(id)) GAMES else DRIVE
    private fun validImport(file: File) = file.canonicalFile ==
        File(File(context!!.dataDir.canonicalFile, IMPORTS), file.toRelativeString(imports))

    private fun file(id: String): File {
        val root = root(id)
        val key = rootId(id)
        if (id == key) {
            if (key == GAMES && !validImport(root)) throw FileNotFoundException(id)
            return root
        }
        val path = id.removePrefix("$key/")
        val parts = path.split('/')
        if (path == id || parts.any { it.isEmpty() || it == "." || it == ".." }) throw FileNotFoundException(id)
        val file = File(root, path)
        if (managed(id) && (!parts.first().matches(Regex("[A-Za-z0-9_-]+")) ||
                !validImport(file))) throw FileNotFoundException(id)
        return file
    }

    private fun id(rootId: String, file: File) = file.toRelativeString(root(rootId)).let {
        if (it.isEmpty()) rootId else "$rootId/$it"
    }

    private fun writable(id: String, file: File) = file.canonicalFile.startsWith(root(id).canonicalFile) &&
        (!managed(id) || id != GAMES && !gameRunning)

    private fun removable(id: String, file: File): Boolean {
        if (!writable(id, file) || id == DRIVE) return false
        if (!managed(id)) return true
        val depth = id.removePrefix("$GAMES/").count { it == '/' }
        // The import wrapper and its payload root are managed in Hitode.
        return if (file.isDirectory) depth >= 2 else depth >= 1
    }

    private fun mutable(id: String, removing: Boolean = false): File {
        val file = file(id)
        if (!(if (removing) removable(id, file) else writable(id, file))) throw FileNotFoundException(id)
        return file
    }

    /** A name the player gives stays one entry of its directory. */
    private fun child(parent: File, name: String): File {
        if (name.isEmpty() || name == "." || name == ".." || '/' in name) throw FileNotFoundException(name)
        return File(parent, name)
    }

    /** Removes `file` and what it holds; a link is removed, never what it leads to. */
    private fun delete(file: File) {
        Files.walkFileTree(file.toPath(), object : SimpleFileVisitor<Path>() {
            override fun visitFile(path: Path, attributes: BasicFileAttributes): FileVisitResult {
                Files.delete(path)
                return FileVisitResult.CONTINUE
            }

            override fun postVisitDirectory(path: Path, error: IOException?): FileVisitResult {
                if (error != null) throw error
                Files.delete(path)
                return FileVisitResult.CONTINUE
            }
        })
    }

    private fun type(file: File) = if (file.isDirectory) Document.MIME_TYPE_DIR else
        MimeTypeMap.getSingleton().getMimeTypeFromExtension(file.extension.lowercase())
            ?: "application/octet-stream"

    private fun row(cursor: MatrixCursor, key: String, file: File) {
        var flags = 0
        val id = id(key, file)
        if (writable(id, file)) {
            flags = if (file.isDirectory) Document.FLAG_DIR_SUPPORTS_CREATE else Document.FLAG_SUPPORTS_WRITE
            if (removable(id, file)) flags = flags or Document.FLAG_SUPPORTS_DELETE or Document.FLAG_SUPPORTS_RENAME
        }
        val name = if (id == DRIVE || id == GAMES) key else file.name
        cursor.newRow()
            .add(Document.COLUMN_DOCUMENT_ID, id)
            .add(Document.COLUMN_DISPLAY_NAME, name)
            .add(Document.COLUMN_MIME_TYPE, type(file))
            .add(Document.COLUMN_FLAGS, flags)
            .add(Document.COLUMN_SIZE, file.length())
            .add(Document.COLUMN_LAST_MODIFIED, file.lastModified())
    }

    override fun queryRoots(projection: Array<out String>?): Cursor {
        val cursor = MatrixCursor(projection ?: ROOT_COLUMNS)
        drive.mkdirs()
        cursor.newRow()
            .add(Root.COLUMN_ROOT_ID, ROOT)
            .add(Root.COLUMN_FLAGS, Root.FLAG_SUPPORTS_CREATE or Root.FLAG_SUPPORTS_IS_CHILD)
            .add(Root.COLUMN_TITLE, "Hitode C:")
            .add(Root.COLUMN_DOCUMENT_ID, DRIVE)
            .add(Root.COLUMN_ICON, R.mipmap.ic_launcher)
        imports.mkdirs()
        cursor.newRow()
            .add(Root.COLUMN_ROOT_ID, IMPORTS)
            .add(Root.COLUMN_FLAGS, Root.FLAG_SUPPORTS_IS_CHILD)
            .add(Root.COLUMN_TITLE, "Hitode Games")
            .add(Root.COLUMN_DOCUMENT_ID, GAMES)
            .add(Root.COLUMN_ICON, R.mipmap.ic_launcher)
        return cursor
    }

    override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor {
        val cursor = MatrixCursor(projection ?: DOCUMENT_COLUMNS)
        row(cursor, rootId(documentId), file(documentId))
        return cursor
    }

    override fun queryChildDocuments(
        parentDocumentId: String,
        projection: Array<out String>?,
        sortOrder: String?,
    ): Cursor {
        val cursor = MatrixCursor(projection ?: DOCUMENT_COLUMNS)
        val key = rootId(parentDocumentId)
        file(parentDocumentId).listFiles()?.sortedBy { it.name.lowercase() }?.forEach {
            if (key == DRIVE || (parentDocumentId != GAMES || it.name.matches(Regex("[A-Za-z0-9_-]+")) && it.isDirectory) &&
                validImport(it)) row(cursor, key, it)
        }
        return cursor
    }

    override fun isChildDocument(parentDocumentId: String, documentId: String) =
        documentId.startsWith("$parentDocumentId/")

    override fun findDocumentPath(parentDocumentId: String?, childDocumentId: String): DocumentsContract.Path {
        if (!file(childDocumentId).exists()) throw FileNotFoundException(childDocumentId)
        val root = rootId(childDocumentId)
        val start = parentDocumentId ?: root
        if (start != childDocumentId && !isChildDocument(start, childDocumentId)) {
            throw FileNotFoundException(childDocumentId)
        }
        val path = mutableListOf(start)
        if (start != childDocumentId) {
            for (part in childDocumentId.removePrefix("$start/").split('/')) {
                path.add("${path.last()}/$part")
            }
        }
        val rootKey = if (parentDocumentId != null) null else if (root == GAMES) IMPORTS else ROOT
        return DocumentsContract.Path(rootKey, path)
    }

    override fun getDocumentType(documentId: String) = type(file(documentId))

    override fun openDocument(
        documentId: String,
        mode: String,
        signal: CancellationSignal?,
    ): ParcelFileDescriptor {
        val file = file(documentId)
        val access = ParcelFileDescriptor.parseMode(mode)
        val reading = access == ParcelFileDescriptor.MODE_READ_ONLY
        return ParcelFileDescriptor.open(if (reading) file else mutable(documentId), access)
    }

    override fun createDocument(parentDocumentId: String, mimeType: String, displayName: String): String {
        val parent = mutable(parentDocumentId)
        var target = child(parent, displayName)
        var index = 1
        while (target.exists()) target = File(parent, "$displayName (${index++})")
        val created = if (mimeType == Document.MIME_TYPE_DIR) target.mkdir() else target.createNewFile()
        if (!created) throw FileNotFoundException(displayName)
        return id(rootId(parentDocumentId), target)
    }

    override fun deleteDocument(documentId: String) {
        delete(mutable(documentId, removing = true))
    }

    override fun renameDocument(documentId: String, displayName: String): String {
        val source = mutable(documentId, removing = true)
        val target = child(source.parentFile ?: throw FileNotFoundException(documentId), displayName)
        if ((managed(documentId) && target.exists()) || !source.renameTo(target)) throw FileNotFoundException(documentId)
        return id(rootId(documentId), target)
    }
}
