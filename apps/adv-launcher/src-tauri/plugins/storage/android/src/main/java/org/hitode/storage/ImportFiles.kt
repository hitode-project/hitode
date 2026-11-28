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

/** Resolves a readable file inside Hitode's own DriveProvider roots (`C:` or `Games:`). */
internal fun driveFilePath(documentId: String, dataDir: File): String? {
    val (root, relative) = when {
        documentId.startsWith("C:/") -> File(dataDir, "wine/drive_c") to documentId.removePrefix("C:/")
        documentId.startsWith("Games:/") -> File(dataDir, "imports") to documentId.removePrefix("Games:/")
        else -> return null
    }
    if (relative.isEmpty() || relative.contains('\u0000')) return null
    val parts = relative.split('/')
    if (parts.any { it.isEmpty() || it == "." || it == ".." }) return null
    val base = root.canonicalFile.toPath()
    val target = File(root, relative).canonicalFile
    val targetPath = target.toPath()
    return if (targetPath.startsWith(base) && targetPath != base && target.isFile && target.canRead()) {
        target.path
    } else null
}

/** Extracts a MediaStore `_id` from Downloads (`msf:<id>` / `<id>`) or MediaDocuments IDs. */
internal fun mediaDocumentId(authority: String?, documentId: String): Long? = when (authority) {
    "com.android.providers.downloads.documents" -> when {
        documentId.startsWith("msf:") -> documentId.removePrefix("msf:").toLongOrNull()
        else -> documentId.toLongOrNull()
    }
    "com.android.providers.media.documents" -> {
        val colon = documentId.indexOf(':')
        if (colon > 0) documentId.substring(colon + 1).toLongOrNull() else null
    }
    else -> null
}

internal fun extensionMatches(name: String, allowed: List<String>?): Boolean {
    if (allowed.isNullOrEmpty()) return true
    val dot = name.lastIndexOf('.')
    if (dot <= 0 || dot == name.length - 1) return false
    val ext = name.substring(dot + 1).lowercase()
    return allowed.any { it.equals(ext, ignoreCase = true) }
}

internal fun fallbackDisplayName(uriPath: String?): String? =
    uriPath?.substringAfterLast('/')?.takeIf { it.isNotEmpty() && it != "." && it != ".." }

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

internal fun legacyExternalVolumeRoot(
    appFilesDir: File,
    storageDir: File = File("/storage"),
): StorageRoot? {
    val storageRoot = storageDir.toPath().normalize()
    var current: File? = appFilesDir.toPath().normalize().toFile()
    while (current != null) {
        val parent = current.parentFile
        if (parent != null && parent.toPath().normalize() == storageRoot) {
            val name = current.name
            if (name != "emulated" && name != "self" && name.isNotEmpty()) {
                return StorageRoot(name, current, writable = false)
            }
            return null
        }
        current = parent
    }
    return null
}

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
