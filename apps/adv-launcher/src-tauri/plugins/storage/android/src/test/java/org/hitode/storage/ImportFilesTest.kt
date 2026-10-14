package org.hitode.storage

import java.io.File
import java.io.IOException
import java.nio.file.Files
import org.junit.Assert.*
import org.junit.Test

class ImportFilesTest {
    // spec: AND-APP-9
    @Test fun primaryStorageRequiresPermissionAndStaysUnderRoot() {
        assertEquals("/storage/emulated/0/Games/Novel", primaryPath("primary:Games/Novel", true))
        assertEquals("/storage/emulated/0", primaryPath("primary:", true))
        assertNull(primaryPath("primary:Games", false))
        assertNull(primaryPath("ABCD-1234:Games", true))
        assertNull(primaryPath("primary:../private", true))
    }

    // spec: AND-APP-9
    @Test fun failedCopyLeavesNeitherPartialDestinationNorStaging() {
        val parent = Files.createTempDirectory("import-test").toFile()
        try {
            val source = File(parent, "source").apply { mkdir() }
            File(source, "game.dat").writeText("source")
            val dest = File(parent, "imported")
            try {
                publishFolder(dest) { staging ->
                    File(staging, "partial.dat").writeText("partial")
                    throw IOException("copy failed")
                }
                fail("Copy must fail")
            } catch (error: IOException) {
                assertEquals("copy failed", error.message)
            }
            assertEquals(listOf("source"), parent.list()!!.toList())
            assertEquals("source", File(source, "game.dat").readText())
        } finally {
            parent.deleteRecursively()
        }
    }

    // spec: AND-APP-9
    @Test fun cancelledCopyIsNotPublishedAndCompleteCopyIsPublished() {
        val parent = Files.createTempDirectory("import-test").toFile()
        try {
            val dest = File(parent, "imported")
            try {
                publishFolder(dest) { staging ->
                    File(staging, "game.dat").writeText("partial")
                    throw java.util.concurrent.CancellationException("cancelled")
                }
                fail("Copy must be cancelled")
            } catch (_: java.util.concurrent.CancellationException) {
                assertTrue(parent.list()!!.isEmpty())
            }
            publishFolder(dest) { staging ->
                assertFalse(dest.exists())
                File(staging, "game.dat").writeText("complete")
            }
            assertEquals("complete", File(dest, "game.dat").readText())
            assertEquals(listOf("imported"), parent.list()!!.toList())
        } finally {
            parent.deleteRecursively()
        }
    }
}
