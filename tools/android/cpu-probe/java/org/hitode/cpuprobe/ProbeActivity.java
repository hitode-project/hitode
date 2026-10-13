package org.hitode.cpuprobe;

import android.app.Activity;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Process;
import android.util.Log;
import android.view.WindowInsets;
import android.widget.ScrollView;
import android.widget.TextView;
import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;
import java.util.concurrent.atomic.AtomicBoolean;
import org.json.JSONArray;
import org.json.JSONObject;

/** A bounded native probe. No game files, permission requests or network use. */
public final class ProbeActivity extends Activity {
    private static final String TAG = "AdvCpuProbe";
    private static final AtomicBoolean STARTED = new AtomicBoolean();
    private static volatile String latest = "Starting interpreter and Cranelift JIT probe...";
    private static volatile boolean finished;
    private final Handler handler = new Handler(Looper.getMainLooper());
    private TextView output;
    private File report;

    private static native String nativeRun(String privateFilesDirectory, String deviceJson);

    @Override public void onCreate(Bundle saved) {
        super.onCreate(saved);
        report = new File(getFilesDir(), "cpu-probe.json");
        output = new TextView(this);
        output.setTextSize(16);
        output.setPadding(24, 24, 24, 24);
        output.setText(latest);
        output.setTextIsSelectable(true);
        ScrollView scroll = new ScrollView(this);
        scroll.addView(output);
        setContentView(scroll);
        if (Build.VERSION.SDK_INT >= 30) {
            scroll.setOnApplyWindowInsetsListener((view, insets) -> {
                android.graphics.Insets safe = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                view.setPadding(safe.left, safe.top, safe.right, safe.bottom);
                return insets;
            });
        }
        if (STARTED.compareAndSet(false, true)) {
            new Thread(this::runProbe, "adv-cpu-probe").start();
        }
        handler.post(refresh);
    }

    private final Runnable refresh = new Runnable() {
        @Override public void run() {
            if (!finished && report.isFile()) {
                try {
                    latest = describe(new JSONObject(new String(Files.readAllBytes(report.toPath()), StandardCharsets.UTF_8)));
                } catch (Exception error) {
                    Log.w(TAG, "Reading progress", error);
                }
            }
            output.setText(latest + "\n\nPrivate report:\n" + report.getAbsolutePath());
            if (!finished) handler.postDelayed(this, 500);
        }
    };

    private void runProbe() {
        try {
            JSONObject device = new JSONObject();
            device.put("manufacturer", Build.MANUFACTURER);
            device.put("model", Build.MODEL);
            device.put("device", Build.DEVICE);
            device.put("hardware", Build.HARDWARE);
            device.put("android_release", Build.VERSION.RELEASE);
            device.put("api", Build.VERSION.SDK_INT);
            device.put("security_patch", Build.VERSION.SECURITY_PATCH);
            device.put("supported_abis", new JSONArray(Build.SUPPORTED_ABIS));
            device.put("pid", Process.myPid());
            if (Build.VERSION.SDK_INT >= 31) device.put("soc_model", Build.SOC_MODEL);
            latest = "Loading adv-cpu native library...";
            System.loadLibrary("adv_android_probe");
            latest = "Executing interpreter and Cranelift JIT probe...";
            String text = nativeRun(getFilesDir().getAbsolutePath(), device.toString());
            if (text == null) throw new IllegalStateException("JNI returned no report");
            JSONObject result = new JSONObject(text);
            // Native normally publishes every stage atomically. This also saves
            // a JNI-boundary failure that occurred before native report setup.
            writeReport(text);
            latest = describe(result);
            Log.i(TAG, "Application probe complete: " + result.optString("status"));
        } catch (Throwable error) {
            Log.e(TAG, "Application probe failed", error);
            latest = "FAILED\n" + error;
            try {
                JSONObject failure = new JSONObject();
                failure.put("schema", 1);
                failure.put("status", "failed");
                failure.put("path", "java_or_jni_boundary");
                failure.put("error", error.toString());
                writeReport(failure.toString(2));
            } catch (Exception reportError) {
                Log.e(TAG, "Saving failure report", reportError);
            }
        } finally {
            finished = true;
            handler.post(refresh);
        }
    }

    private void writeReport(String text) throws Exception {
        File temporary = new File(getFilesDir(), "cpu-probe.java.tmp");
        try (FileOutputStream stream = new FileOutputStream(temporary)) {
            stream.write(text.getBytes(StandardCharsets.UTF_8));
            stream.getFD().sync();
        }
        Files.move(temporary.toPath(), report.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
    }

    private static String describe(JSONObject report) {
        StringBuilder text = new StringBuilder("ADV CPU application probe\n");
        text.append("Status: ").append(report.optString("status", "unknown")).append('\n');
        JSONObject host = report.optJSONObject("host");
        if (host != null) {
            text.append("Host page size: ").append(host.optLong("page_size")).append('\n');
            text.append("SELinux: ").append(host.optString("selinux_context")).append('\n');
        }
        if (!report.isNull("running_path")) text.append("Running: ").append(report.optString("running_path")).append('\n');
        JSONArray results = report.optJSONArray("results");
        if (results != null) for (int i = 0; i < results.length(); i++) {
            JSONObject result = results.optJSONObject(i);
            if (result == null) continue;
            text.append('\n').append(result.optString("workload")).append(" / ")
                .append(result.optString("path")).append(": ").append(result.optString("status"));
            JSONObject details = result.optJSONObject("details");
            if (details != null && "jit".equals(result.optString("path"))) {
                text.append("\nnative entries=").append(details.optLong("native_entries"))
                    .append(", blocks=").append(details.optLong("native_blocks"))
                    .append(", bytes=").append(details.optLong("native_code_bytes"))
                    .append(", linked=").append(details.optLong("linked_entries"));
            }
            if (result.has("error")) text.append("\n").append(result.optString("error"));
        }
        if (report.has("error")) text.append("\n").append(report.optString("error"));
        return text.toString();
    }

    @Override protected void onDestroy() {
        handler.removeCallbacks(refresh);
        super.onDestroy();
    }
}
