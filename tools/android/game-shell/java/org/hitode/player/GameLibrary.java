package org.hitode.player;

import android.content.ContentResolver;
import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.os.CancellationSignal;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import android.util.AtomicFile;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.text.Normalizer;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.HashSet;
import java.util.Locale;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicBoolean;
import org.json.JSONArray;
import org.json.JSONObject;

/** Private, read-only game copies and a separate writable data directory. */
final class GameLibrary {
    private static final long MAX_BYTES = 8L * 1024 * 1024 * 1024;
    private static final int MAX_ENTRIES = 25000;
    private static final int MAX_DEPTH = 48;
    private final Context context;
    private final File games;
    private final File data;
    private final AtomicFile index;
    private final ArrayList<Entry> entries = new ArrayList<>();
    private volatile CancellationSignal importSignal;
    private volatile InputStream importStream;

    static final class Entry {
        final String id, title, executable;
        final int codepage;
        Entry(String id, String title, String executable, int codepage) {
            this.id = id; this.title = title; this.executable = executable; this.codepage = codepage;
        }
        JSONObject json() throws Exception {
            return new JSONObject().put("id", id).put("title", title).put("exe", executable).put("codepage", codepage);
        }
    }
    static final class Imported {
        final String id, title;
        final ArrayList<String> executables;
        Imported(String id, String title, ArrayList<String> executables) {
            this.id = id; this.title = title; this.executables = executables;
        }
    }
    interface Progress { void update(int count, long bytes, String name); }

    GameLibrary(Context context) throws Exception {
        this.context = context.getApplicationContext();
        games = new File(context.getFilesDir(), "games");
        data = new File(context.getFilesDir(), "data");
        if ((!games.isDirectory() && !games.mkdirs()) || (!data.isDirectory() && !data.mkdirs()))
            throw new IOException("無法建立遊戲庫");
        index = new AtomicFile(new File(context.getFilesDir(), "library.json"));
        if (index.getBaseFile().isFile()) {
            try (FileInputStream stream = index.openRead()) {
                byte[] bytes = readSmall(stream, 2 * 1024 * 1024);
                JSONArray array = new JSONObject(new String(bytes, StandardCharsets.UTF_8)).getJSONArray("games");
                for (int i = 0; i < array.length(); ++i) {
                    JSONObject item = array.getJSONObject(i);
                    String id = item.getString("id");
                    if (!id.matches("[a-f0-9-]{36}")) throw new IOException("遊戲庫資料無效");
                    Entry entry = new Entry(id, item.getString("title"), item.getString("exe"), item.getInt("codepage"));
                    if (entry.codepage != 932 && entry.codepage != 936) throw new IOException("文字編碼無效");
                    File executable = checkedFile(new File(games, id), entry.executable);
                    if (executable.isFile()) entries.add(entry);
                }
            }
        }
        // Only this app creates staging folders. Interrupted imports are never library entries.
        HashSet<String> registered = new HashSet<>();
        for (Entry entry : entries) registered.add(entry.id);
        File[] leftovers = games.listFiles();
        if (leftovers != null) for (File file : leftovers)
            if (file.getName().matches("[a-f0-9-]{36}\\.staging")
                || (file.getName().matches("[a-f0-9-]{36}") && !registered.contains(file.getName()))) deletePrivate(file);
    }
    synchronized ArrayList<Entry> entries() { return new ArrayList<>(entries); }
    File executable(Entry entry) throws IOException { return checkedFile(new File(games, entry.id), entry.executable); }
    File dataDirectory(Entry entry) throws IOException {
        File directory = new File(data, entry.id);
        if (!directory.isDirectory() && !directory.mkdirs()) throw new IOException("無法建立遊戲存檔資料夾");
        return directory;
    }
    synchronized Entry add(Imported imported, String executable, int codepage) throws Exception {
        if (!imported.executables.contains(executable) || (codepage != 932 && codepage != 936))
            throw new IOException("無效的遊戲選擇");
        File exe = checkedFile(new File(games, imported.id), executable);
        if (!exe.isFile()) throw new IOException("找不到遊戲主程式");
        Entry entry = new Entry(imported.id, imported.title, executable, codepage);
        entries.add(entry);
        try { save(); } catch (Exception error) { entries.remove(entry); throw error; }
        return entry;
    }
    synchronized void remove(Entry entry) throws Exception {
        int position = entries.indexOf(entry);
        if (position < 0) return;
        entries.remove(position);
        try { save(); } catch (Exception error) { entries.add(position, entry); throw error; }
        deletePrivate(new File(games, entry.id));
        // Retain independent save data when a game is removed from the library.
    }
    void discard(Imported imported) throws IOException { deletePrivate(new File(games, imported.id)); }
    private void save() throws Exception {
        JSONArray array = new JSONArray();
        for (Entry entry : entries) array.put(entry.json());
        byte[] content = new JSONObject().put("schema", 1).put("games", array).toString(2).getBytes(StandardCharsets.UTF_8);
        FileOutputStream stream = null;
        try { stream = index.startWrite(); stream.write(content); index.finishWrite(stream); }
        catch (Exception error) { if (stream != null) index.failWrite(stream); throw error; }
    }

    Imported importTree(Uri tree, AtomicBoolean cancelled, Progress progress) throws Exception {
        CancellationSignal signal = new CancellationSignal();
        importSignal = signal;
        if (cancelled.get()) signal.cancel();
        String rootId = DocumentsContract.getTreeDocumentId(tree);
        Uri root = DocumentsContract.buildDocumentUriUsingTree(tree, rootId);
        Document rootDoc = queryDocument(root, signal);
        if (!DocumentsContract.Document.MIME_TYPE_DIR.equals(rootDoc.mime)) throw new IOException("請選擇遊戲資料夾");
        validateName(rootDoc.name);
        String id = UUID.randomUUID().toString();
        File staging = new File(games, id + ".staging");
        File destination = new File(games, id);
        if (!staging.mkdir()) throw new IOException("無法建立匯入資料夾");
        CopyState state = new CopyState(cancelled, signal, progress, Math.min(MAX_BYTES, Math.max(0, games.getUsableSpace() - 256L * 1024 * 1024)));
        try {
            state.documentIds.add(rootId);
            copyDirectory(tree, rootId, staging, "", 0, state);
            state.checkCancelled();
            if (state.executables.isEmpty()) throw new IOException("資料夾內沒有找到 .exe 遊戲主程式");
            Collections.sort(state.executables, String.CASE_INSENSITIVE_ORDER);
            // Same-parent rename publishes only the complete copy.
            if (!staging.renameTo(destination)) throw new IOException("無法完成遊戲匯入");
            return new Imported(id, rootDoc.name, state.executables);
        } catch (Exception error) {
            deletePrivate(staging);
            throw error;
        } finally { importStream = null; importSignal = null; }
    }
    void cancelImport() {
        CancellationSignal signal = importSignal;
        if (signal != null) signal.cancel();
        InputStream stream = importStream;
        if (stream != null) try { stream.close(); } catch (IOException ignored) { }
    }
    private void copyDirectory(Uri tree, String parentId, File directory, String relative, int depth, CopyState state) throws Exception {
        state.checkCancelled();
        if (depth > MAX_DEPTH) throw new IOException("資料夾層級過深");
        HashSet<String> names = new HashSet<>();
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parentId);
        String[] columns = {DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE, DocumentsContract.Document.COLUMN_FLAGS, DocumentsContract.Document.COLUMN_SIZE};
        try (Cursor cursor = context.getContentResolver().query(children, columns, null, null, null, state.signal)) {
            if (cursor == null) throw new IOException("無法讀取遊戲資料夾");
            while (cursor.moveToNext()) {
                state.checkCancelled();
                if (++state.count > MAX_ENTRIES) throw new IOException("遊戲包含過多檔案（上限 25,000）");
                String docId = cursor.getString(0), name = cursor.getString(1), mime = cursor.getString(2);
                long flags = cursor.getLong(3), declaredSize = cursor.isNull(4) ? -1 : cursor.getLong(4);
                validateName(name);
                String folded = Normalizer.normalize(name, Normalizer.Form.NFKC).toLowerCase(Locale.ROOT).toUpperCase(Locale.ROOT);
                if (!names.add(folded)) throw new IOException("同一資料夾含有重複檔名：" + name);
                if (docId == null || !state.documentIds.add(docId)) throw new IOException("資料夾含有重複連結或循環");
                if ((flags & DocumentsContract.Document.FLAG_VIRTUAL_DOCUMENT) != 0 || "inode/symlink".equals(mime))
                    throw new IOException("不支援虛擬檔案或符號連結：" + name);
                File target = checkedFile(directory, name);
                String childRelative = relative.isEmpty() ? name : relative + "/" + name;
                if (DocumentsContract.Document.MIME_TYPE_DIR.equals(mime)) {
                    if (!target.mkdir()) throw new IOException("無法建立資料夾：" + name);
                    copyDirectory(tree, docId, target, childRelative, depth + 1, state);
                } else {
                    if (declaredSize > state.limit - state.bytes) throw new IOException("儲存空間不足，或遊戲超過 8 GB");
                    Uri document = DocumentsContract.buildDocumentUriUsingTree(tree, docId);
                    ParcelFileDescriptor descriptor = context.getContentResolver().openFileDescriptor(document, "r", state.signal);
                    if (descriptor == null) throw new IOException("無法開啟檔案：" + name);
                    try (InputStream input = new ParcelFileDescriptor.AutoCloseInputStream(descriptor);
                         FileOutputStream output = new FileOutputStream(target)) {
                        importStream = input;
                        byte[] buffer = new byte[128 * 1024];
                        int length;
                        while ((length = input.read(buffer)) != -1) {
                            state.checkCancelled();
                            state.bytes += length;
                            if (state.bytes > state.limit) throw new IOException("儲存空間不足，或遊戲超過 8 GB");
                            output.write(buffer, 0, length);
                            state.report(name);
                        }
                        output.getFD().sync();
                    } finally { importStream = null; }
                    if (name.toLowerCase(Locale.ROOT).endsWith(".exe")) state.executables.add(childRelative);
                }
                state.report(name);
            }
        }
    }
    private Document queryDocument(Uri uri, CancellationSignal signal) throws IOException {
        String[] columns = {DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_MIME_TYPE};
        try (Cursor cursor = context.getContentResolver().query(uri, columns, null, null, null, signal)) {
            if (cursor == null || !cursor.moveToFirst()) throw new IOException("無法讀取選取的資料夾");
            return new Document(cursor.getString(0), cursor.getString(1));
        }
    }
    private static final class Document {
        final String name, mime;
        Document(String name, String mime) { this.name = name; this.mime = mime; }
    }
    private static final class CopyState {
        final AtomicBoolean cancelled;
        final CancellationSignal signal;
        final Progress progress;
        final long limit;
        final HashSet<String> documentIds = new HashSet<>();
        final ArrayList<String> executables = new ArrayList<>();
        long bytes, lastReport;
        int count;
        CopyState(AtomicBoolean cancelled, CancellationSignal signal, Progress progress, long limit) { this.cancelled = cancelled; this.signal = signal; this.progress = progress; this.limit = limit; }
        void checkCancelled() throws IOException { if (cancelled.get() || signal.isCanceled() || Thread.currentThread().isInterrupted()) throw new IOException("已取消匯入"); }
        void report(String name) { long now = android.os.SystemClock.uptimeMillis(); if (now - lastReport > 150) { lastReport = now; progress.update(count, bytes, name); } }
    }
    private static void validateName(String name) throws IOException {
        if (name == null || name.isEmpty() || name.equals(".") || name.equals("..") || name.length() > 240
            || name.endsWith(".") || name.endsWith(" ") || name.indexOf('/') >= 0 || name.indexOf('\\') >= 0
            || name.matches(".*[<>:\"|?*\\p{Cntrl}].*")) throw new IOException("檔名無效：" + name);
        String stem = name.split("\\.", 2)[0].toUpperCase(Locale.ROOT);
        if (stem.matches("CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9]")) throw new IOException("不支援的檔名：" + name);
    }
    private static File checkedFile(File root, String relative) throws IOException {
        if (relative == null || relative.isEmpty() || new File(relative).isAbsolute()) throw new IOException("檔案路徑無效");
        String[] components = relative.replace('\\', '/').split("/", -1);
        for (String component : components) validateName(component);
        File file = new File(root, relative);
        String parent = root.getCanonicalPath() + File.separator;
        if (!file.getCanonicalPath().startsWith(parent)) throw new IOException("檔案超出遊戲資料夾");
        return file;
    }
    private void deletePrivate(File file) throws IOException {
        String path = file.getCanonicalPath();
        if (!path.startsWith(games.getCanonicalPath() + File.separator)) throw new IOException("清理路徑無效");
        if (Files.isSymbolicLink(file.toPath())) { if (!file.delete()) throw new IOException("無法清理連結"); return; }
        File[] children = file.listFiles();
        if (children != null) for (File child : children) deletePrivate(child);
        if (file.exists() && !file.delete()) throw new IOException("無法清理暫存檔案");
    }
    private static byte[] readSmall(InputStream input, int maximum) throws IOException {
        java.io.ByteArrayOutputStream output = new java.io.ByteArrayOutputStream();
        byte[] bytes = new byte[8192]; int length;
        while ((length = input.read(bytes)) != -1) { if (output.size() + length > maximum) throw new IOException("遊戲庫資料過大"); output.write(bytes, 0, length); }
        return output.toByteArray();
    }
}
