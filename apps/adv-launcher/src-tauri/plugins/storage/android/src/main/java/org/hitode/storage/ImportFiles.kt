package org.hitode.storage

import java.io.File
import java.io.IOException
import java.nio.file.Files

internal fun primaryPath(documentId: String, granted: Boolean): String? {
    if (!granted || !documentId.startsWith("primary:")) return null
    val root = File("/storage/emulated/0")
    val path = root.toPath().resolve(documentId.removePrefix("primary:")).normalize()
    return if (path.startsWith(root.toPath())) path.toString() else null
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
