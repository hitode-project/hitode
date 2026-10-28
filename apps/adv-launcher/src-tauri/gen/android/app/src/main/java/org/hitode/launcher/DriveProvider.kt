package org.hitode.launcher

import android.database.Cursor
import android.database.MatrixCursor
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
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
 * Drive C: of the player's Wine prefix as a root of the system Files app,
 * so players reach what games write outside their folders. A document id is
 * `C:` for the drive and `C:/` plus the path below it: DocumentsUI rejects
 * the empty id of a root document.
 */
class DriveProvider : DocumentsProvider() {
    companion object {
        const val AUTHORITY = "org.hitode.launcher.drive"
        const val ROOT = "drive"
        private const val DRIVE = "C:"
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

    override fun onCreate() = true

    private fun file(id: String): File {
        // Ids name paths below the drive; its links lead into the Wine installation.
        if (id == DRIVE) return drive
        val path = id.removePrefix("$DRIVE/")
        if (path == id || path.split('/').any { it == ".." }) throw FileNotFoundException(id)
        return File(drive, path)
    }

    private fun id(file: File) = file.toRelativeString(drive).let { if (it.isEmpty()) DRIVE else "$DRIVE/$it" }

    /**
     * Whether the player may change `file`: what the drive's links lead to is
     * the Wine installation, which the Files app only shows.
     */
    private fun owned(file: File) = file.canonicalFile.startsWith(drive.canonicalFile)

    private fun mutable(file: File): File {
        if (!owned(file)) throw FileNotFoundException(id(file))
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

    private fun row(cursor: MatrixCursor, file: File) {
        var flags = 0
        if (owned(file)) {
            flags = if (file.isDirectory) Document.FLAG_DIR_SUPPORTS_CREATE else Document.FLAG_SUPPORTS_WRITE
            if (file != drive) flags = flags or Document.FLAG_SUPPORTS_DELETE or Document.FLAG_SUPPORTS_RENAME
        }
        val name = if (file == drive) DRIVE else file.name
        cursor.newRow()
            .add(Document.COLUMN_DOCUMENT_ID, id(file))
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
        return cursor
    }

    override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor {
        val cursor = MatrixCursor(projection ?: DOCUMENT_COLUMNS)
        row(cursor, file(documentId))
        return cursor
    }

    override fun queryChildDocuments(
        parentDocumentId: String,
        projection: Array<out String>?,
        sortOrder: String?,
    ): Cursor {
        val cursor = MatrixCursor(projection ?: DOCUMENT_COLUMNS)
        file(parentDocumentId).listFiles()?.sortedBy { it.name.lowercase() }?.forEach { row(cursor, it) }
        return cursor
    }

    override fun isChildDocument(parentDocumentId: String, documentId: String) =
        documentId.startsWith("$parentDocumentId/")

    override fun getDocumentType(documentId: String) = type(file(documentId))

    override fun openDocument(
        documentId: String,
        mode: String,
        signal: CancellationSignal?,
    ): ParcelFileDescriptor {
        val file = file(documentId)
        val access = ParcelFileDescriptor.parseMode(mode)
        val reading = access == ParcelFileDescriptor.MODE_READ_ONLY
        return ParcelFileDescriptor.open(if (reading) file else mutable(file), access)
    }

    override fun createDocument(parentDocumentId: String, mimeType: String, displayName: String): String {
        val parent = mutable(file(parentDocumentId))
        var target = child(parent, displayName)
        var index = 1
        while (target.exists()) target = File(parent, "$displayName (${index++})")
        val created = if (mimeType == Document.MIME_TYPE_DIR) target.mkdir() else target.createNewFile()
        if (!created) throw FileNotFoundException(displayName)
        return id(target)
    }

    override fun deleteDocument(documentId: String) {
        if (documentId == DRIVE) throw FileNotFoundException(documentId)
        delete(mutable(file(documentId)))
    }

    override fun renameDocument(documentId: String, displayName: String): String {
        val source = mutable(file(documentId))
        val target = child(source.parentFile, displayName)
        if (documentId == DRIVE || !source.renameTo(target)) throw FileNotFoundException(documentId)
        return id(target)
    }
}
