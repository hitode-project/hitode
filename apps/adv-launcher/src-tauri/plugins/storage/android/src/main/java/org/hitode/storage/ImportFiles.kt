package org.hitode.storage

import java.io.File
import java.io.IOException
import java.nio.file.Files

internal data class StorageRoot(
    val id: String,
    val directory: File,
    val writable: Boolean = true,
)

internal fun storagePath(
    documentId: String,
    granted: Boolean,
    roots: List<StorageRoot>,
    requireWritable: Boolean = true,
): File? {
    if (!granted) return null
    val colon = documentId.indexOf(':')
    if (colon <= 0) return null
    val rawId = documentId.substring(0, colon)
    val rawRel = documentId.substring(colon + 1)
    val (volumeId, relative) = if (rawId == "home") {
        "primary" to if (rawRel.isEmpty()) "Documents" else "Documents/$rawRel"
    } else {
        rawId to rawRel
    }
    if (relative.startsWith('/') || relative.contains('\u0000')) return null
    val volume = roots.firstOrNull { it.id.equals(volumeId, ignoreCase = true) } ?: return null
    if (requireWritable && !volume.writable) return null
    val root = volume.directory.toPath().normalize()
    val path = root.resolve(relative).normalize()
    return if (path.startsWith(root)) path.toFile() else null
}

internal fun writableFolderPath(
    documentId: String,
    granted: Boolean,
    roots: List<StorageRoot>,
): String? = storagePath(documentId, granted, roots, requireWritable = true)
    ?.takeIf { it.isDirectory && it.canRead() && it.canWrite() }
    ?.path

internal fun readableFilePath(
    documentId: String,
    granted: Boolean,
    roots: List<StorageRoot>,
): String? = storagePath(documentId, granted, roots, requireWritable = false)
    ?.takeIf { it.isFile && it.canRead() }
    ?.path

internal fun canGrantDirectAccess(
    authority: String?,
    documentId: String,
    granted: Boolean,
    supported: Boolean,
    roots: List<StorageRoot>,
): Boolean = !granted &&
    supported &&
    authority == "com.android.externalstorage.documents" &&
    storagePath(documentId, granted = true, roots, requireWritable = true) != null

/** Publish a complete same-parent copy; exceptions leave no partial destination. */
internal fun publishFolder(dest: File, copy: (File) -> Unit) {
    if (dest.exists()) throw IOException("Import destination already exists")
    val parent = dest.parentFile!!
    if (!parent.isDirectory && !parent.mkdirs()) throw IOException("Cannot create import directory")
    val staging = Files.createTempDirectory(parent.toPath(), ".${dest.name}-").toFile()
    try {
        copy(staging)
        if (!staging.renameTo(dest)) throw IOException("Cannot publish imported folder")
    } finally {
        if (staging.exists() && !staging.deleteRecursively()) throw IOException("Cannot remove incomplete import")
    }
}
