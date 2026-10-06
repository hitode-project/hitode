package org.advhost.player;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.ActivityInfo;
import android.content.res.Configuration;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.RectF;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.net.Uri;
import android.os.Bundle;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.widget.ArrayAdapter;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ProgressBar;
import android.widget.ScrollView;
import android.widget.SeekBar;
import android.widget.Spinner;
import android.widget.TextView;
import java.io.File;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.Locale;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import org.json.JSONArray;
import org.json.JSONObject;

/** SAF library and native Android presentation for the ADV runtime. */
public final class GameActivity extends Activity {
    private static final String TAG = "AdvPlayer";
    private static final int PICK_TREE = 40;
    private static final int BACKGROUND = Color.rgb(16, 20, 28);
    private static final int PANEL = Color.rgb(27, 34, 46);
    private static final int TEXT = Color.rgb(235, 240, 247);
    private static final int MUTED = Color.rgb(166, 180, 199);
    private static final int ACCENT = Color.rgb(119, 192, 255);
    private final Handler main = new Handler(Looper.getMainLooper());
    private final ExecutorService work = Executors.newSingleThreadExecutor();
    private final AtomicInteger generation = new AtomicInteger();
    private final AtomicBoolean importCancelled = new AtomicBoolean();
    private volatile long session;
    private volatile boolean resumed, destroyed;
    private GameLibrary library;
    private Throwable libraryError, nativeError;
    private LinearLayout root;
    private TextView status, importStatus;
    private AlertDialog importDialog, guestDialog;
    private int shownDialog = -1;
    private String shownDialogStructure = "";
    private JSONArray menus = new JSONArray();
    private Button menuButton, fastButton;
    private GameSurface gameSurface;
    private boolean importing, ctrlHeld;
    private GameLibrary.Imported pendingImport;
    private String currentTitle;
    private volatile boolean activeFixture;
    private final HashMap<Integer, View> guestControls = new HashMap<>();
    private AudioManager audioManager;
    private AudioFocusRequest audioRequest;
    private boolean audioFocused;

    private static native void nativeInit(android.content.Context application);
    private static native long nativeStart(String exe, String dataDir, int codepage);
    private static native void nativeCommand(long session, String json);
    private static native String nativePoll(long session);
    private static native long nativeCopyFrame(long session, Bitmap target);
    private static native void nativeStop(long session);

    @Override public void onCreate(Bundle saved) {
        super.onCreate(saved);
        getWindow().setStatusBarColor(BACKGROUND);
        getWindow().setNavigationBarColor(BACKGROUND);
        audioManager = (AudioManager)getSystemService(AUDIO_SERVICE);
        audioRequest = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(new AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_GAME)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
            .setOnAudioFocusChangeListener(change -> {
                audioFocused = change == AudioManager.AUDIOFOCUS_GAIN;
                command(object("type", "audio_focus", "active", audioFocused));
                if (!audioFocused) releaseInputs();
            }, main).build();
        try { System.loadLibrary("adv_android"); nativeInit(getApplicationContext()); }
        catch (Throwable error) { nativeError = error; Log.e(TAG, "Native initialization", error); }
        showLibrary();
        work.execute(() -> {
            try { library = new GameLibrary(this); }
            catch (Throwable error) { libraryError = error; Log.e(TAG, "Library", error); }
            main.post(() -> {
                if (destroyed) return;
                showLibrary();
                if ((getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0
                    && "shell-probe".equals(getIntent().getStringExtra("fixture"))) launchFixture();
            });
        });
    }

    private LinearLayout makeRoot() {
        LinearLayout layout = new LinearLayout(this);
        layout.setOrientation(LinearLayout.VERTICAL);
        layout.setBackgroundColor(BACKGROUND);
        layout.setPadding(dp(20), dp(12), dp(20), dp(12));
        layout.setOnApplyWindowInsetsListener((view, insets) -> {
            if (Build.VERSION.SDK_INT >= 30) {
                android.graphics.Insets safe = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                view.setPadding(safe.left + dp(16), safe.top + dp(8), safe.right + dp(16), safe.bottom + dp(8));
            } else view.setPadding(insets.getSystemWindowInsetLeft() + dp(16), insets.getSystemWindowInsetTop() + dp(8), insets.getSystemWindowInsetRight() + dp(16), insets.getSystemWindowInsetBottom() + dp(8));
            return insets;
        });
        setContentView(layout);
        root = layout;
        return layout;
    }
    private TextView text(String content, int size, int color) {
        TextView view = new TextView(this);
        view.setText(content); view.setTextSize(size); view.setTextColor(color);
        view.setPadding(0, dp(6), 0, dp(6));
        return view;
    }
    private Button button(String label, View.OnClickListener listener) {
        Button view = new Button(this);
        view.setText(label); view.setTextColor(TEXT); view.setAllCaps(false);
        view.setMinHeight(dp(48)); view.setOnClickListener(listener);
        view.setBackgroundTintList(android.content.res.ColorStateList.valueOf(PANEL));
        return view;
    }
    private void showLibrary() {
        if (destroyed) return;
        setRequestedOrientation(ActivityInfo.SCREEN_ORIENTATION_PORTRAIT);
        LinearLayout layout = makeRoot();
        layout.addView(text("ADV 遊戲庫", 27, TEXT));
        layout.addView(text("匯入完整遊戲資料夾，開始你的故事。", 15, MUTED));
        Button add = button("＋ 匯入遊戲資料夾", view -> pickGame());
        add.setTextColor(ACCENT); add.setEnabled(library != null && !importing);
        layout.addView(add);
        ScrollView scroll = new ScrollView(this);
        LinearLayout list = new LinearLayout(this); list.setOrientation(LinearLayout.VERTICAL);
        scroll.addView(list);
        layout.addView(scroll, new LinearLayout.LayoutParams(-1, 0, 1));
        if (libraryError != null) list.addView(text("無法讀取遊戲庫\n" + message(libraryError), 15, Color.rgb(255, 160, 160)));
        else if (library == null) list.addView(text("正在載入遊戲庫…", 16, MUTED));
        else {
            ArrayList<GameLibrary.Entry> entries = library.entries();
            if (entries.isEmpty()) {
                TextView empty = text("遊戲庫還是空的\n\n使用上方按鈕選擇含有主程式與素材的資料夾。\n匯入的遊戲副本與存檔會保存在此裝置。", 17, MUTED);
                empty.setPadding(0, dp(40), 0, 0); list.addView(empty);
            }
            for (GameLibrary.Entry entry : entries) {
                LinearLayout row = new LinearLayout(this); row.setOrientation(LinearLayout.VERTICAL);
                row.setPadding(0, dp(10), 0, dp(8));
                Button play = button(entry.title + "  ›", view -> launch(entry));
                play.setEnabled(nativeError == null);
                play.setOnLongClickListener(view -> { entryOptions(entry); return true; });
                row.addView(play);
                row.addView(text(entry.executable + "  ·  " + encodingName(entry.codepage), 12, MUTED));
                list.addView(row);
            }
        }
        if (nativeError != null) layout.addView(text("遊戲播放器無法載入\n" + message(nativeError), 14, Color.rgb(255, 160, 160)));
        layout.addView(text("原始資料夾不會被修改 · 長按遊戲可移除", 12, MUTED));
    }
    private void pickGame() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
        startActivityForResult(intent, PICK_TREE);
    }
    @Override protected void onActivityResult(int request, int result, Intent intent) {
        super.onActivityResult(request, result, intent);
        if (request != PICK_TREE || result != RESULT_OK || intent == null || intent.getData() == null || importing) return;
        Uri tree = intent.getData();
        // The provider grants read access; never request or take write permission.
        try { getContentResolver().takePersistableUriPermission(tree, Intent.FLAG_GRANT_READ_URI_PERMISSION); }
        catch (SecurityException ignored) { /* Immediate copy also works with the temporary read grant. */ }
        importing = true; importCancelled.set(false);
        LinearLayout content = new LinearLayout(this); content.setOrientation(LinearLayout.VERTICAL); content.setPadding(dp(24), dp(8), dp(24), dp(8));
        ProgressBar progress = new ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal); progress.setIndeterminate(true);
        content.addView(progress); importStatus = text("正在準備遊戲資料夾…", 14, TEXT); content.addView(importStatus);
        importDialog = new AlertDialog.Builder(this).setTitle("匯入遊戲").setView(content)
            .setNegativeButton("取消", null).setCancelable(false).create();
        importDialog.setOnShowListener(ignored -> importDialog.getButton(AlertDialog.BUTTON_NEGATIVE).setOnClickListener(view -> {
            importCancelled.set(true); new Thread(() -> library.cancelImport(), "adv-import-cancel").start();
            importStatus.setText("正在取消，請稍候…"); view.setEnabled(false);
        }));
        importDialog.show();
        work.execute(() -> {
            GameLibrary.Imported imported = null; Throwable failure = null;
            try { imported = library.importTree(tree, importCancelled, (count, bytes, name) -> main.post(() -> {
                if (importStatus != null && !importCancelled.get()) importStatus.setText(String.format(Locale.getDefault(), "%s\n%d 個檔案 · %.1f MB", name, count, bytes / 1048576.0));
            })); } catch (Throwable error) { failure = error; Log.w(TAG, "Import", error); }
            try { getContentResolver().releasePersistableUriPermission(tree, Intent.FLAG_GRANT_READ_URI_PERMISSION); } catch (SecurityException ignored) { }
            final GameLibrary.Imported completed = imported; final Throwable error = failure;
            if (destroyed && completed != null) { try { library.discard(completed); } catch (Exception ignored) { } return; }
            main.post(() -> {
                importing = false; importStatus = null;
                if (importDialog != null) { importDialog.dismiss(); importDialog = null; }
                if (destroyed) return;
                showLibrary();
                if (completed != null) chooseExecutable(completed);
                else if (!importCancelled.get()) showError("匯入失敗", message(error));
            });
        });
    }
    private void chooseExecutable(GameLibrary.Imported imported) {
        pendingImport = imported;
        String[] choices = imported.executables.toArray(new String[0]);
        new AlertDialog.Builder(this).setTitle("選擇遊戲主程式").setItems(choices, (dialog, which) -> chooseEncoding(imported, choices[which]))
            .setNegativeButton("取消", (dialog, which) -> discard(imported))
            .setOnCancelListener(dialog -> discard(imported)).show();
    }
    private void chooseEncoding(GameLibrary.Imported imported, String executable) {
        final int[] chosen = {0};
        new AlertDialog.Builder(this).setTitle("遊戲文字編碼")
            .setSingleChoiceItems(new String[]{"日文（CP932）", "簡體中文（CP936）"}, 0, (dialog, which) -> chosen[0] = which)
            .setPositiveButton("加入遊戲庫", (dialog, which) -> work.execute(() -> {
                Throwable failure = null;
                try { library.add(imported, executable, chosen[0] == 0 ? 932 : 936); } catch (Throwable error) { failure = error; }
                final Throwable error = failure;
                main.post(() -> { pendingImport = null; if (!destroyed) { showLibrary(); if (error != null) { showError("無法儲存遊戲", message(error)); discard(imported); } } });
            }))
            .setNegativeButton("取消", (dialog, which) -> discard(imported))
            .setOnCancelListener(dialog -> discard(imported)).show();
    }
    private void discard(GameLibrary.Imported imported) { if (pendingImport == imported) pendingImport = null; work.execute(() -> { try { library.discard(imported); } catch (Exception error) { Log.w(TAG, "Discard", error); } }); }
    private void entryOptions(GameLibrary.Entry entry) {
        new AlertDialog.Builder(this).setTitle(entry.title).setMessage("移除此裝置上的遊戲副本？已建立的存檔會保留。")
            .setPositiveButton("移除遊戲", (dialog, which) -> work.execute(() -> {
                Throwable failure = null; try { library.remove(entry); } catch (Throwable error) { failure = error; }
                final Throwable error = failure; main.post(() -> { if (!destroyed) { showLibrary(); if (error != null) showError("無法移除遊戲", message(error)); } });
            })).setNegativeButton("取消", null).show();
    }
    private void launch(GameLibrary.Entry entry) {
        activeFixture = false;
        try { launchFile(entry.title, library.executable(entry), library.dataDirectory(entry), entry.codepage); }
        catch (Throwable error) { showError("無法啟動遊戲", message(error)); }
    }
    private void launchFixture() {
        File executable = new File(getFilesDir(), "fixtures/shell-probe.exe");
        File data = new File(getFilesDir(), "data/shell-probe");
        if (executable.isFile() && (data.isDirectory() || data.mkdirs())) { activeFixture = true; launchFile("功能檢查", executable, data, 932); }
        else showError("功能檢查", "找不到預先安裝的測試檔案。");
    }
    private void launchFile(String title, File executable, File data, int codepage) {
        if (nativeError != null || session != 0) return;
        final int token = generation.incrementAndGet(); currentTitle = title;
        showGame(); status.setText("正在啟動…");
        work.execute(() -> {
            long handle = 0; Throwable failure = null;
            try { handle = nativeStart(executable.getAbsolutePath(), data.getAbsolutePath(), codepage); if (handle == 0) throw new IllegalStateException("遊戲未能啟動"); }
            catch (Throwable error) { failure = error; Log.e(TAG, "Start", error); }
            final long started = handle; final Throwable error = failure;
            main.post(() -> {
                if (destroyed || token != generation.get()) { if (started != 0) nativeStop(started); return; }
                if (error != null) { stopSession(); showError("無法啟動遊戲", message(error)); return; }
                session = started;
                if (resumed) { requestAudioFocus(); command(object("type", "resume")); }
                else command(object("type", "pause"));
                command(object("type", "audio_focus", "active", audioFocused));
                if (gameSurface != null) gameSurface.wake();
            });
        });
    }
    private void showGame() {
        setRequestedOrientation(ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE);
        LinearLayout layout = makeRoot();
        LinearLayout top = new LinearLayout(this); top.setGravity(Gravity.CENTER_VERTICAL);
        Button back = button("‹ 遊戲庫", view -> confirmStop()); top.addView(back);
        status = text(currentTitle, 13, MUTED); status.setMaxLines(2); status.setPadding(dp(12), 0, dp(8), 0);
        top.addView(status, new LinearLayout.LayoutParams(0, -2, 1));
        menuButton = button("選單", view -> showMenus(menus, "遊戲選單")); menuButton.setEnabled(false); top.addView(menuButton);
        layout.addView(top);
        gameSurface = new GameSurface();
        layout.addView(gameSurface, new LinearLayout.LayoutParams(-1, 0, 1));
        LinearLayout controls = new LinearLayout(this); controls.setGravity(Gravity.CENTER);
        controls.addView(button("確認", view -> tapKey(13)), new LinearLayout.LayoutParams(0, -2, 1));
        controls.addView(button("右鍵", view -> { if (gameSurface != null) gameSurface.rightClick(); }), new LinearLayout.LayoutParams(0, -2, 1));
        controls.addView(button("Esc", view -> tapKey(27)), new LinearLayout.LayoutParams(0, -2, 1));
        fastButton = button("按住快轉", null);
        fastButton.setOnTouchListener((view, event) -> {
            int action = event.getActionMasked();
            if (action == MotionEvent.ACTION_DOWN) { view.setPressed(true); setCtrl(true); return true; }
            if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL || action == MotionEvent.ACTION_OUTSIDE) { view.setPressed(false); setCtrl(false); return true; }
            if (action == MotionEvent.ACTION_MOVE && (event.getX() < 0 || event.getY() < 0 || event.getX() >= view.getWidth() || event.getY() >= view.getHeight())) { view.setPressed(false); setCtrl(false); }
            return true;
        });
        controls.addView(fastButton, new LinearLayout.LayoutParams(0, -2, 1));
        layout.addView(controls);
    }
    private void confirmStop() {
        releaseInputs();
        new AlertDialog.Builder(this).setTitle("回到遊戲庫？").setMessage("請先在遊戲內儲存進度。")
            .setPositiveButton("結束遊戲", (dialog, which) -> stopSession()).setNegativeButton("繼續遊戲", null).show();
    }
    private void stopSession() {
        generation.incrementAndGet(); releaseInputs();
        activeFixture = false;
        long handle = session; session = 0;
        if (gameSurface != null) { gameSurface.close(); gameSurface = null; }
        if (guestDialog != null) { guestDialog.dismiss(); guestDialog = null; }
        shownDialog = -1; shownDialogStructure = ""; guestControls.clear();
        abandonAudioFocus();
        if (handle != 0) nativeStop(handle); // JNI only signals its worker; no UI-thread join.
        showLibrary();
    }
    private void command(JSONObject command) {
        long handle = session;
        if (handle != 0 && command != null) {
            try { nativeCommand(handle, command.toString()); } catch (Throwable error) { Log.w(TAG, "Command", error); }
        }
    }
    private static JSONObject object(Object... pairs) {
        JSONObject object = new JSONObject();
        try { for (int i = 0; i + 1 < pairs.length; i += 2) object.put(String.valueOf(pairs[i]), pairs[i + 1]); }
        catch (Exception impossible) { throw new IllegalArgumentException(impossible); }
        return object;
    }
    private void tapKey(int key) { command(object("type", "key", "key", key, "pressed", true)); command(object("type", "key", "key", key, "pressed", false)); }
    private void setCtrl(boolean held) { if (ctrlHeld == held) return; ctrlHeld = held; command(object("type", "key", "key", 17, "pressed", held)); }
    private void releaseInputs() { setCtrl(false); if (fastButton != null) fastButton.setPressed(false); if (gameSurface != null) gameSurface.releasePointer(); }
    private void requestAudioFocus() { if (audioManager != null) audioFocused = audioManager.requestAudioFocus(audioRequest) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED; }
    private void abandonAudioFocus() { if (audioManager != null) audioManager.abandonAudioFocusRequest(audioRequest); audioFocused = false; }

    private void updateState(long handle, JSONObject state) {
        if (destroyed || session != handle || gameSurface == null) return;
        String stateName = state.optString("status", "running");
        String error = state.isNull("error") ? "" : state.optString("error", "");
        if ("error".equals(stateName) || "exited".equals(stateName) || "stopped".equals(stateName)) {
            stopSession();
            if (!error.isEmpty() || "error".equals(stateName)) showError("遊戲已停止", error.isEmpty() ? "遊戲未能繼續執行，請重新啟動。" : error);
            return;
        }
        status.setText("paused".equals(stateName) ? currentTitle + " · 已暫停" : currentTitle);
        JSONArray nextMenus = state.optJSONArray("menus"); menus = nextMenus == null ? new JSONArray() : nextMenus;
        menuButton.setEnabled(state.optBoolean("menu_enabled", true) && menus.length() > 0);
        JSONArray dialogs = state.optJSONArray("dialogs"); updateDialogs(dialogs == null ? new JSONArray() : dialogs);
        String commandError = state.isNull("command_error") ? "" : state.optString("command_error", "");
        if (!commandError.isEmpty()) status.setText(commandError);
    }
    private String label(String raw) { return raw.replace("&&", "\u0001").replace("&", "").replace("\u0001", "&").replace('\t', ' '); }
    private void showMenus(JSONArray items, String title) {
        releaseInputs();
        LinearLayout list = new LinearLayout(this); list.setOrientation(LinearLayout.VERTICAL); list.setPadding(dp(16), 0, dp(16), dp(8));
        ScrollView scroll = new ScrollView(this); scroll.addView(list);
        AlertDialog dialog = new AlertDialog.Builder(this).setTitle(title).setView(scroll).setNegativeButton("關閉", null).create();
        for (int i = 0; i < items.length(); ++i) {
            JSONObject item = items.optJSONObject(i); if (item == null) continue;
            String caption = label(item.optString("label", ""));
            if (caption.isEmpty()) { View line = new View(this); line.setBackgroundColor(PANEL); list.addView(line, new LinearLayout.LayoutParams(-1, dp(1))); continue; }
            JSONArray children = item.optJSONArray("children");
            Button button = button((item.optBoolean("checked") ? "✓ " : "") + caption + (children != null && children.length() > 0 ? "  ›" : ""), view -> {
                dialog.dismiss();
                if (children != null && children.length() > 0) showMenus(children, caption);
                else command(object("type", "menu", "id", item.optInt("id")));
            });
            button.setEnabled(item.optBoolean("enabled", true)); list.addView(button);
        }
        dialog.show();
    }
    private void updateDialogs(JSONArray dialogs) {
        JSONObject active = null;
        for (int i = 0; i < dialogs.length(); ++i) { JSONObject dialog = dialogs.optJSONObject(i); if (dialog != null && dialog.optBoolean("visible", true)) active = dialog; }
        if (active == null) { if (guestDialog != null) guestDialog.dismiss(); guestDialog = null; shownDialog = -1; shownDialogStructure = ""; return; }
        int id = active.optInt("id");
        // Keep widgets stable while typing or dragging. Native changes replace a dialog when its control topology changes.
        JSONArray controls = active.optJSONArray("controls");
        StringBuilder structure = new StringBuilder(active.optString("title"));
        if (controls != null) for (int i = 0; i < controls.length(); ++i) { JSONObject c = controls.optJSONObject(i); if (c != null) structure.append('|').append(c.optInt("id")).append(':').append(c.optString("class")).append(':').append(c.optBoolean("visible", true)); }
        if (id == shownDialog && structure.toString().equals(shownDialogStructure) && guestDialog != null && guestDialog.isShowing()) {
            if (controls != null) for (int i = 0; i < controls.length(); ++i) { JSONObject c = controls.optJSONObject(i); if (c != null) refreshDialogControl(c); }
            return;
        }
        if (guestDialog != null) guestDialog.dismiss(); releaseInputs();
        guestControls.clear();
        shownDialog = id; shownDialogStructure = structure.toString();
        LinearLayout list = new LinearLayout(this); list.setOrientation(LinearLayout.VERTICAL); list.setPadding(dp(20), dp(4), dp(20), dp(8));
        ScrollView scroll = new ScrollView(this); scroll.addView(list);
        if (controls == null || controls.length() == 0) list.addView(text(active.optString("text", ""), 16, TEXT));
        if (controls != null) for (int i = 0; i < controls.length(); ++i) {
            JSONObject control = controls.optJSONObject(i); if (control == null || !control.optBoolean("visible", true)) continue;
            addDialogControl(list, id, control);
        }
        guestDialog = new AlertDialog.Builder(this).setTitle(active.optString("title", "遊戲對話框")).setView(scroll)
            .setNegativeButton("關閉", (dialog, which) -> command(object("type", "dialog-close", "dialog", id))).create();
        guestDialog.setCanceledOnTouchOutside(false);
        guestDialog.setOnCancelListener(dialog -> command(object("type", "dialog-close", "dialog", id)));
        guestDialog.show();
    }
    private void addDialogControl(LinearLayout list, int dialogId, JSONObject control) {
        int id = control.optInt("id"); String cls = control.optString("class", "").toLowerCase(Locale.ROOT);
        String caption = label(control.optString("text", "")); boolean enabled = control.optBoolean("enabled", true) && control.optBoolean("supported", true);
        View view;
        if (cls.equals("button")) {
            int kind = control.optInt("style") & 15;
            if (kind == 7) view = text(caption, 16, ACCENT);
            else if (kind == 2 || kind == 3 || kind == 4 || kind == 5 || kind == 6 || kind == 9) {
                CheckBox check = new CheckBox(this); check.setText(caption); check.setTextColor(TEXT); check.setChecked(control.optBoolean("checked", control.optInt("check") != 0));
                check.setOnClickListener(v -> command(object("type", "dialog", "dialog", dialogId, "id", id))); view = check;
            } else view = button(caption.isEmpty() ? "確認" : caption, v -> command(object("type", "dialog", "dialog", dialogId, "id", id)));
        } else if (cls.contains("trackbar")) {
            JSONObject range = control.optJSONObject("range"); int minimum = range == null ? 0 : range.optInt("min"), maximum = range == null ? 100 : range.optInt("max", 100);
            SeekBar seek = new SeekBar(this); seek.setMin(minimum); seek.setMax(Math.max(minimum, maximum)); seek.setProgress(control.optInt("value", minimum));
            seek.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
                public void onStartTrackingTouch(SeekBar bar) { }
                public void onStopTrackingTouch(SeekBar bar) { command(object("type", "dialog-track", "dialog", dialogId, "id", id, "value", bar.getProgress())); }
                public void onProgressChanged(SeekBar bar, int value, boolean fromUser) { if (fromUser) command(object("type", "dialog-track", "dialog", dialogId, "id", id, "value", value)); }
            }); view = seek;
        } else if (cls.equals("combobox") || cls.equals("listbox")) {
            JSONArray items = control.optJSONArray("items"); ArrayList<String> labels = new ArrayList<>(); if (items != null) for (int i = 0; i < items.length(); ++i) labels.add(items.optString(i));
            Spinner spinner = new Spinner(this); ArrayAdapter<String> adapter = new ArrayAdapter<>(this, android.R.layout.simple_spinner_item, labels); adapter.setDropDownViewResource(android.R.layout.simple_spinner_dropdown_item); spinner.setAdapter(adapter);
            int selected = control.optInt("selection", -1); if (selected >= 0 && selected < labels.size()) spinner.setSelection(selected);
            spinner.setOnItemSelectedListener(new android.widget.AdapterView.OnItemSelectedListener() {
                boolean first = true;
                public void onNothingSelected(android.widget.AdapterView<?> parent) { }
                public void onItemSelected(android.widget.AdapterView<?> parent, View v, int position, long row) { if (first) { first = false; return; } command(object("type", "dialog-select", "dialog", dialogId, "id", id, "value", position)); }
            }); view = spinner;
        } else if (cls.equals("edit")) {
            EditText edit = new EditText(this); edit.setText(caption); edit.setTextColor(TEXT); edit.setSingleLine((control.optInt("style") & 4) == 0);
            edit.addTextChangedListener(new android.text.TextWatcher() {
                public void beforeTextChanged(CharSequence s, int start, int count, int after) { }
                public void onTextChanged(CharSequence s, int start, int before, int count) { command(object("type", "dialog-text", "dialog", dialogId, "id", id, "text", s.toString())); }
                public void afterTextChanged(android.text.Editable text) { }
            }); view = edit;
        } else view = text(caption, 15, TEXT);
        view.setEnabled(enabled); guestControls.put(id, view); list.addView(view, new LinearLayout.LayoutParams(-1, -2));
    }
    private void refreshDialogControl(JSONObject control) {
        View view = guestControls.get(control.optInt("id")); if (view == null) return;
        view.setEnabled(control.optBoolean("enabled", true) && control.optBoolean("supported", true));
        if (view instanceof CheckBox) ((CheckBox)view).setChecked(control.optBoolean("checked", control.optInt("check") != 0));
        if (view instanceof EditText) {
            String next = control.optString("text", "");
            EditText edit = (EditText)view;
            if (!edit.hasFocus() && !next.contentEquals(edit.getText())) edit.setText(next);
        } else if (view instanceof TextView) ((TextView)view).setText(label(control.optString("text", "")));
        if (view instanceof SeekBar && !view.isPressed()) ((SeekBar)view).setProgress(control.optInt("value"));
    }

    private final class GameSurface extends SurfaceView implements SurfaceHolder.Callback, Runnable {
        private volatile boolean open = true, ready;
        private volatile int frameWidth, frameHeight;
        private final Paint paint = new Paint(Paint.FILTER_BITMAP_FLAG);
        private final Thread renderer;
        private volatile RectF destination = new RectF();
        private boolean pointerDown;
        private int pointerId = -1, lastX, lastY;
        GameSurface() {
            super(GameActivity.this); getHolder().addCallback(this); setFocusable(true);
            // Paint letterboxing on the Surface canvas; a View background obscures the Surface below it.
            renderer = new Thread(this, "adv-frame-presenter"); renderer.start();
        }
        public void surfaceCreated(SurfaceHolder holder) { ready = true; wake(); }
        public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) { ready = true; wake(); }
        public void surfaceDestroyed(SurfaceHolder holder) { ready = false; releasePointer(); }
        void wake() { synchronized (this) { notifyAll(); } }
        void close() { open = false; ready = false; wake(); }
        @Override public void run() {
            Bitmap bitmap = null; long copied = -1, lastState = 0, observedHandle = 0, lastFixtureReport = 0;
            while (open && !destroyed) {
                long handle = session;
                if (!ready || !resumed || handle == 0) { synchronized (this) { try { wait(100); } catch (InterruptedException ignored) { } } continue; }
                try {
                    JSONObject snapshot = new JSONObject(nativePoll(handle)); JSONObject frame = snapshot.optJSONObject("frame");
                    if (handle != observedHandle) { observedHandle = handle; copied = -1; }
                    int width = frame == null ? 0 : frame.optInt("width"), height = frame == null ? 0 : frame.optInt("height");
                    long serial = frame == null ? -1 : frame.optLong("serial", -1);
                    if (width > 0 && height > 0 && width <= 4096 && height <= 4096 && (long)width * height <= 16777216) {
                        if (bitmap == null || bitmap.getWidth() != width || bitmap.getHeight() != height) {
                            if (bitmap != null) bitmap.recycle(); bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888); copied = -1;
                        }
                        if (serial != copied) { long complete = nativeCopyFrame(handle, bitmap); if (complete >= 0) { copied = complete; frameWidth = width; frameHeight = height; } }
                    }
                    Canvas canvas = null;
                    try {
                        if (ready && open && resumed) canvas = getHolder().lockCanvas();
                        if (canvas != null) {
                            canvas.drawColor(Color.BLACK);
                            if (bitmap != null && copied >= 0) {
                                float scale = Math.min(canvas.getWidth() / (float)bitmap.getWidth(), canvas.getHeight() / (float)bitmap.getHeight());
                                float w = bitmap.getWidth() * scale, h = bitmap.getHeight() * scale;
                                RectF rect = new RectF((canvas.getWidth() - w) / 2, (canvas.getHeight() - h) / 2, (canvas.getWidth() + w) / 2, (canvas.getHeight() + h) / 2);
                                destination = rect; canvas.drawBitmap(bitmap, null, rect, paint);
                            }
                        }
                    } finally { if (canvas != null) getHolder().unlockCanvasAndPost(canvas); }
                    long now = android.os.SystemClock.uptimeMillis();
                    if (activeFixture && now - lastFixtureReport >= 500) {
                        lastFixtureReport = now;
                        snapshot.put("presented_serial", copied);
                        writeFixtureReport(snapshot);
                    }
                    if (now - lastState >= 100) { lastState = now; main.post(() -> updateState(handle, snapshot)); }
                } catch (Throwable error) {
                    Log.e(TAG, "Presentation", error);
                    main.post(() -> { if (session == handle && !destroyed) { stopSession(); showError("無法顯示遊戲", message(error)); } });
                    break;
                }
                try { Thread.sleep(16); } catch (InterruptedException ignored) { }
            }
            if (bitmap != null) bitmap.recycle();
        }
        private boolean map(float x, float y) {
            RectF rect = destination;
            if (frameWidth <= 0 || frameHeight <= 0 || rect.width() <= 0 || !rect.contains(x, y)) return false;
            lastX = Math.min(frameWidth - 1, Math.max(0, (int)((x - rect.left) * frameWidth / rect.width())));
            lastY = Math.min(frameHeight - 1, Math.max(0, (int)((y - rect.top) * frameHeight / rect.height()))); return true;
        }
        private void pointer(String action) { command(object("type", "pointer", "action", action, "x", lastX, "y", lastY, "button", 0)); }
        @Override public boolean onTouchEvent(MotionEvent event) {
            int action = event.getActionMasked();
            if (action == MotionEvent.ACTION_DOWN) {
                if (!map(event.getX(), event.getY())) return true;
                pointerId = event.getPointerId(0); pointerDown = true; pointer("down"); return true;
            }
            if (action == MotionEvent.ACTION_MOVE && pointerDown) {
                int index = event.findPointerIndex(pointerId); if (index >= 0 && map(event.getX(index), event.getY(index))) pointer("move"); else releasePointer(); return true;
            }
            if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL || action == MotionEvent.ACTION_OUTSIDE
                || (action == MotionEvent.ACTION_POINTER_UP && event.getPointerId(event.getActionIndex()) == pointerId)) { releasePointer(); if (action == MotionEvent.ACTION_UP) performClick(); return true; }
            return true;
        }
        @Override public boolean performClick() { super.performClick(); return true; }
        void releasePointer() { if (pointerDown) pointer("up"); pointerDown = false; pointerId = -1; }
        void rightClick() { if (frameWidth > 0 && frameHeight > 0) command(object("type", "right-click", "x", lastX, "y", lastY)); }
        void fixtureClick() {
            RectF rect = destination; if (rect.width() <= 0 || rect.height() <= 0) return;
            long time = android.os.SystemClock.uptimeMillis();
            MotionEvent down = MotionEvent.obtain(time, time, MotionEvent.ACTION_DOWN, rect.centerX(), rect.centerY(), 0);
            MotionEvent up = MotionEvent.obtain(time, time + 1, MotionEvent.ACTION_UP, rect.centerX(), rect.centerY(), 0);
            try { onTouchEvent(down); onTouchEvent(up); } finally { down.recycle(); up.recycle(); }
        }
    }

    // Debug-only, fixed private fixture. No intent supplies an executable path or native JSON.
    @Override protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        if ((getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) == 0) return;
        String action = intent.getStringExtra("probe_action");
        if (action == null) {
            if (session == 0 && gameSurface == null && "shell-probe".equals(intent.getStringExtra("fixture"))) launchFixture();
            return;
        }
        if (!activeFixture || session == 0 || gameSurface == null) { Log.w(TAG, "Rejected fixture action outside fixture"); return; }
        if (!action.equals("advance") && !action.equals("pointer") && !action.equals("save") && !action.equals("load")
            && !action.equals("ctrl_down") && !action.equals("ctrl_up") && !action.equals("rotate") && !action.equals("stop")) {
            Log.w(TAG, "Rejected unknown fixture action"); return;
        }
        if (action.equals("stop")) { dispatchFixtureAction(action); return; }
        final long handle = session;
        final int token = generation.get();
        final long deadline = android.os.SystemClock.uptimeMillis() + 2000;
        // Bringing the task forward delivers onNewIntent before onResume. Wait for
        // both lifecycle and the guest worker's resume before sending fenced input.
        main.post(new Runnable() {
            @Override public void run() {
                if (destroyed || !activeFixture || session != handle || generation.get() != token) return;
                boolean ready = false;
                if (resumed) {
                    try { JSONObject state = new JSONObject(nativePoll(handle)); ready = "running".equals(state.optString("status")) && !state.optBoolean("paused", true); }
                    catch (Throwable error) { Log.w(TAG, "Fixture readiness", error); }
                }
                if (ready) { dispatchFixtureAction(action); return; }
                if (android.os.SystemClock.uptimeMillis() < deadline) main.postDelayed(this, 40);
                else Log.w(TAG, "Fixture action timed out waiting for resume: " + action);
            }
        });
    }
    private void dispatchFixtureAction(String action) {
        switch (action) {
            case "advance": tapKey(13); break;
            case "pointer": gameSurface.fixtureClick(); break;
            case "save": case "load": {
                int id = action.equals("save") ? 5001 : 5002;
                if (!hasEnabledMenu(menus, id)) { Log.w(TAG, "Fixture menu unavailable: " + id); return; }
                command(object("type", "menu", "id", id)); break;
            }
            case "ctrl_down": setCtrl(true); break;
            case "ctrl_up": setCtrl(false); break;
            case "rotate": setRequestedOrientation(getResources().getConfiguration().orientation == Configuration.ORIENTATION_LANDSCAPE
                ? ActivityInfo.SCREEN_ORIENTATION_PORTRAIT : ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE); break;
            case "stop": stopSession(); break;
            default: Log.w(TAG, "Rejected unknown fixture action"); return;
        }
        Log.i(TAG, "Fixture action: " + action);
    }
    private static boolean hasEnabledMenu(JSONArray items, int id) {
        for (int i = 0; i < items.length(); ++i) {
            JSONObject item = items.optJSONObject(i); if (item == null || !item.optBoolean("enabled", true)) continue;
            JSONArray children = item.optJSONArray("children");
            if (children != null && hasEnabledMenu(children, id)) return true;
            if (item.optInt("id") == id) return true;
        }
        return false;
    }
    private void writeFixtureReport(JSONObject snapshot) {
        android.util.AtomicFile report = new android.util.AtomicFile(new File(getFilesDir(), "shell-state.json"));
        java.io.FileOutputStream output = null;
        try { output = report.startWrite(); output.write(snapshot.toString().getBytes(java.nio.charset.StandardCharsets.UTF_8)); report.finishWrite(output); }
        catch (Exception error) { if (output != null) report.failWrite(output); Log.w(TAG, "Fixture report", error); }
    }

    @Override public boolean onKeyDown(int keyCode, KeyEvent event) {
        if (session != 0 && keyCode != KeyEvent.KEYCODE_BACK) {
            int key = windowsKey(keyCode); if (key != 0) { command(object("type", "key", "key", key, "pressed", true)); return true; }
        }
        return super.onKeyDown(keyCode, event);
    }
    @Override public boolean onKeyUp(int keyCode, KeyEvent event) {
        if (session != 0 && keyCode != KeyEvent.KEYCODE_BACK) {
            int key = windowsKey(keyCode); if (key != 0) { command(object("type", "key", "key", key, "pressed", false)); return true; }
        }
        return super.onKeyUp(keyCode, event);
    }
    private static int windowsKey(int key) {
        switch (key) { case KeyEvent.KEYCODE_ENTER: case KeyEvent.KEYCODE_DPAD_CENTER: return 13; case KeyEvent.KEYCODE_ESCAPE: return 27;
            case KeyEvent.KEYCODE_CTRL_LEFT: case KeyEvent.KEYCODE_CTRL_RIGHT: return 17; case KeyEvent.KEYCODE_SPACE: return 32;
            case KeyEvent.KEYCODE_DPAD_LEFT: return 37; case KeyEvent.KEYCODE_DPAD_UP: return 38; case KeyEvent.KEYCODE_DPAD_RIGHT: return 39; case KeyEvent.KEYCODE_DPAD_DOWN: return 40; default: return 0; }
    }
    @Override public void onBackPressed() { if (session != 0 || gameSurface != null) confirmStop(); else super.onBackPressed(); }
    @Override public void onWindowFocusChanged(boolean focused) { super.onWindowFocusChanged(focused); if (!focused) { releaseInputs(); for (int key : new int[]{13, 17, 27, 32, 37, 38, 39, 40}) command(object("type", "key", "key", key, "pressed", false)); } }
    @Override protected void onPause() { resumed = false; releaseInputs(); command(object("type", "pause")); command(object("type", "audio_focus", "active", false)); abandonAudioFocus(); super.onPause(); }
    @Override protected void onResume() { super.onResume(); resumed = true; if (session != 0) { requestAudioFocus(); command(object("type", "audio_focus", "active", audioFocused)); command(object("type", "resume")); if (gameSurface != null) gameSurface.wake(); } }
    @Override public void onConfigurationChanged(Configuration configuration) { super.onConfigurationChanged(configuration); releaseInputs(); if (root != null) root.requestApplyInsets(); }
    @Override protected void onDestroy() {
        destroyed = true; generation.incrementAndGet(); importCancelled.set(true); if (importing && library != null) new Thread(() -> library.cancelImport(), "adv-import-cancel").start(); releaseInputs();
        long handle = session; session = 0; if (gameSurface != null) gameSurface.close(); if (handle != 0) nativeStop(handle);
        if (pendingImport != null) {
            GameLibrary.Imported pending = pendingImport; pendingImport = null;
            work.execute(() -> { if (!containsEntry(pending.id)) try { library.discard(pending); } catch (Exception error) { Log.w(TAG, "Discard pending", error); } });
        }
        abandonAudioFocus(); work.shutdown(); super.onDestroy();
    }
    private boolean containsEntry(String id) { for (GameLibrary.Entry entry : library.entries()) if (entry.id.equals(id)) return true; return false; }
    private void showError(String title, String error) { if (!destroyed) new AlertDialog.Builder(this).setTitle(title).setMessage(error).setPositiveButton("知道了", null).show(); }
    private static String message(Throwable error) { if (error == null) return "發生未知錯誤"; return error.getMessage() == null ? error.toString() : error.getMessage(); }
    private static String encodingName(int codepage) { return codepage == 936 ? "簡體中文" : "日文"; }
    private int dp(int value) { return Math.round(value * getResources().getDisplayMetrics().density); }
}
