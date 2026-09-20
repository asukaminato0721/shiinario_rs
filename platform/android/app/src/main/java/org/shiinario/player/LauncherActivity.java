package org.shiinario.player;

import android.Manifest;
import android.app.Activity;
import android.app.NativeActivity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Environment;
import android.provider.DocumentsContract;
import android.provider.Settings;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;
import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.Locale;

/** Direct directory access, matching the native runtime's archive and save semantics. */
public final class LauncherActivity extends Activity {
    private EditText path;
    private TextView status;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        LinearLayout layout = new LinearLayout(this);
        layout.setOrientation(LinearLayout.VERTICAL);
        int padding = (int) (24 * getResources().getDisplayMetrics().density);
        layout.setPadding(padding, padding, padding, padding);
        TextView title = new TextView(this);
        title.setText("Shiina Rio\nChoose an original game folder. Archives and saves stay in that folder.");
        title.setTextSize(20);
        layout.addView(title);
        path = new EditText(this);
        path.setSingleLine(true);
        path.setHint("/storage/emulated/0/Games/MyGame");
        path.setText(getPreferences(MODE_PRIVATE).getString("game", ""));
        layout.addView(path);
        Button access = new Button(this);
        access.setText("Allow game folder access");
        access.setOnClickListener(view -> requestStorage());
        layout.addView(access);
        Button choose = new Button(this);
        choose.setText("Choose game folder");
        choose.setOnClickListener(view -> startActivityForResult(new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE), 1));
        layout.addView(choose);
        Button start = new Button(this);
        start.setText("Start game");
        start.setOnClickListener(view -> launch());
        layout.addView(start);
        status = new TextView(this);
        status.setTextIsSelectable(true);
        layout.addView(status);
        setContentView(layout);
    }

    private boolean hasStorage() {
        return Build.VERSION.SDK_INT >= 30 ? Environment.isExternalStorageManager()
            : checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED;
    }
    private void requestStorage() {
        if (Build.VERSION.SDK_INT >= 30) {
            startActivity(new Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                Uri.parse("package:" + getPackageName())));
        } else {
            requestPermissions(new String[]{Manifest.permission.READ_EXTERNAL_STORAGE, Manifest.permission.WRITE_EXTERNAL_STORAGE}, 2);
        }
    }
    @Override protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != 1 || result != RESULT_OK || data == null || data.getData() == null) return;
        try {
            Uri uri = data.getData();
            if (!"com.android.externalstorage.documents".equals(uri.getAuthority())) {
                throw new IllegalArgumentException("Choose a folder on local shared storage, or enter its filesystem path.");
            }
            String[] id = DocumentsContract.getTreeDocumentId(uri).split(":", 2);
            File base = "primary".equalsIgnoreCase(id[0]) ? Environment.getExternalStorageDirectory() : new File("/storage", id[0]);
            File selected = id.length == 2 ? new File(base, id[1]) : base;
            path.setText(selected.getCanonicalPath());
        } catch (Exception error) { status.setText(error.getMessage()); }
    }
    private void launch() {
        try {
            if (!hasStorage()) {
                status.setText("Allow game folder access, then tap Start game again.");
                requestStorage();
                return;
            }
            File root = new File(path.getText().toString().trim()).getCanonicalFile();
            File[] files = root.listFiles();
            if (files == null) throw new IllegalArgumentException("Cannot read the selected directory.");
            boolean ini = false, war = false;
            for (File file : files) {
                if (!file.isFile()) continue;
                String name = file.getName().toLowerCase(Locale.ROOT);
                ini |= name.endsWith(".ini");
                war |= name.endsWith(".war");
            }
            if (!ini || !war) throw new IllegalArgumentException("Select the game root containing its INI and WAR files. Keep the original EXE filenames for game detection.");
            Files.write(new File(getFilesDir(), "game-path.txt").toPath(), root.getPath().getBytes(StandardCharsets.UTF_8));
            Files.deleteIfExists(new File(getFilesDir(), "last-error.txt").toPath());
            getPreferences(MODE_PRIVATE).edit().putString("game", root.getPath()).apply();
            status.setText("");
            startActivity(new Intent(this, NativeActivity.class));
        } catch (Exception error) { status.setText(error.getMessage()); }
    }
    @Override protected void onResume() {
        super.onResume();
        File error = new File(getFilesDir(), "last-error.txt");
        if (error.isFile()) {
            try { status.setText(new String(Files.readAllBytes(error.toPath()), StandardCharsets.UTF_8)); }
            catch (Exception failure) { status.setText(failure.getMessage()); }
        }
    }
}
