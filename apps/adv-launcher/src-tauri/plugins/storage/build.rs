fn main() {
    tauri_plugin::Builder::new(&[
        "storage_status",
        "request_storage",
        "pick_folder",
        "folder_size",
        "copy_folder",
        "cancel_copy",
    ])
    .android_path("android")
    .ios_path("ios")
    .build();
}
