fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&[
                "expand_main_window",
                "open_external_url",
                "open_google_flow",
                "close_google_flow",
                "resize_google_flow",
                "remove_google_flow_account",
                "begin_blob_download",
                "write_blob_download_chunk",
                "complete_blob_download",
                "cancel_blob_download",
                "debug_trigger_isolated_dialog",
                "get_device_id",
                "get_license_state",
                "activate_license",
                "validate_license",
                "clear_license_state",
                "load_accounts",
                "save_accounts",
            ]),
        ),
    )
    .expect("failed to run Tauri build script")
}
