mod cache;
mod camera;
mod cli;
mod commands;
mod library;
mod photos;
mod progress;
mod transfer;
mod types;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = commands::AppState::new(app.handle());
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_cli_info,
            commands::get_settings,
            commands::save_settings,
            commands::get_session_status,
            commands::login,
            commands::logout,
            commands::list_dir,
            commands::create_folder,
            commands::upload_paths,
            commands::photos_access_status,
            commands::photos_request_access,
            commands::photos_list,
            commands::photos_albums,
            commands::photos_thumbnail,
            commands::photos_cancel_export,
            commands::upload_photos,
            commands::library_status,
            commands::library_sync,
            commands::library_reconcile,
            commands::library_forget,
            commands::library_index_manifests,
            commands::library_match_photos,
            commands::library_check_paths,
            commands::download_entry,
            commands::delete_entries,
            commands::get_quota,
            commands::check_cli_update,
            commands::install_cli_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
