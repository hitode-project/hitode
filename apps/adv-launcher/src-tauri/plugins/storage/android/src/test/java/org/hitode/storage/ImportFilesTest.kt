package org.hitode.storage

import java.io.File
import java.io.IOException
import java.nio.file.Files
import org.junit.Assert.*
import org.junit.Test

class ImportFilesTest {
    private val roots = listOf(
        StorageRoot("primary", File("/storage/emulated/0")),
        StorageRoot("9C33-6BBD", File("/storage/9C33-6BBD")),
        StorageRoot("RO00-1111", File("/storage/RO00-1111"), writable = false),
    )

    // spec: AND-APP-9
    @Test fun externalVolumesRequirePermissionAndStayUnderRoot() {
        assertEquals(
            File("/storage/emulated/0/Games/Novel"),
            storagePath("primary:Games/Novel", true, roots),
        )
        assertEquals(
            File("/storage/emulated/0"),
            storagePath("primary:", true, roots),
        )
        assertEquals(
            File("/storage/emulated/0/Documents/Games"),
            storagePath("home:Games", true, roots),
        )
        assertEquals(
            File("/storage/9C33-6BBD/Games/Novel"),
            storagePath("9C33-6BBD:Games/Novel", true, roots),
        )
        assertEquals(
            File("/storage/9C33-6BBD/Games/Novel"),
            storagePath("9c33-6bbd:Games/Novel", true, roots),
        )
        assertEquals(
            File("/storage/9C33-6BBD"),
            storagePath("9C33-6BBD:", true, roots),
        )
        assertNull(storagePath("primary:Games", false, roots))
        assertNull(storagePath("9C33-6BBD:Games", false, roots))
        assertNull(storagePath("ABCD-1234:Games", true, roots))
        assertNull(storagePath("primary:../private", true, roots))
        assertNull(storagePath("9C33-6BBD:../../data", true, roots))
        assertNull(storagePath("primary:/etc/passwd", true, roots))
        assertNull(storagePath("RO00-1111:Games/Novel", true, roots, requireWritable = true))
        assertEquals(
            File("/storage/RO00-1111/disc.iso"),
            storagePath("RO00-1111:disc.iso", true, roots, requireWritable = false),
        )
    }

    // spec: AND-APP-9
    @Test fun folderAndFileResolutionCheckMountedVolumeState() {
        val temp = Files.createTempDirectory("volume-test").toFile()
        try {
            val sdRoot = File(temp, "9C33-6BBD").apply { mkdir() }
            val gameDir = File(sdRoot, "Games/Novel").apply { mkdirs() }
            val discFile = File(sdRoot, "disc.iso").apply { writeText("iso") }

            val mounted = listOf(
                StorageRoot("9C33-6BBD", sdRoot, writable = true),
                StorageRoot("RO-CARD", sdRoot, writable = false),
            )

            assertEquals(gameDir.path, writableFolderPath("9C33-6BBD:Games/Novel", true, mounted))
            assertNull(writableFolderPath("9C33-6BBD:Missing", true, mounted))
            assertNull(writableFolderPath("9C33-6BBD:disc.iso", true, mounted))
            assertNull(writableFolderPath("RO-CARD:Games/Novel", true, mounted))
            assertEquals(discFile.path, readableFilePath("RO-CARD:disc.iso", true, mounted))
        } finally {
            temp.deleteRecursively()
        }
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
