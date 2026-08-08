mod commands;
mod events;
mod groups;
mod ids;
mod network;
mod notifications;
mod persistence;
mod state;
mod transfers;
mod tray;
mod updates;
mod views;

const DEFAULT_TTL: u8 = 8;
const DISCOVERY_PORT: u16 = 37020;

pub fn run() {
    tauri::Builder::default()
        .manage(updates::PendingUpdate::default())
        .manage(updates::UpdateSettings::default())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::create_group,
            commands::discover_relays,
            commands::join_group,
            commands::close_session,
            commands::list_saved_groups,
            commands::activate_saved_group,
            commands::retry_saved_group,
            commands::delete_saved_group,
            commands::send_group_text,
            commands::send_direct_text,
            commands::announce_nickname,
            commands::send_file,
            commands::resume_file_transfer,
            commands::request_file_resume,
            commands::get_members,
            commands::get_connection_status,
            commands::list_network_interfaces,
            commands::probe_relay_addr,
            commands::pick_file,
            commands::save_temp_file,
            commands::save_file_as,
            commands::app_version,
            commands::share_retained_update_package,
            commands::install_received_update_package,
            tray::set_close_to_tray,
            tray::open_main_window,
            tray::is_main_window_visible,
            notifications::set_notifications_enabled,
            updates::check_update,
            updates::install_update,
            updates::set_retain_installer,
            updates::get_retained_update_package,
        ])
        .setup(|app| {
            use tauri::Manager;

            let groups_path = app
                .path()
                .app_local_data_dir()
                .map_err(|err| -> Box<dyn std::error::Error> { Box::new(err) })?
                .join("groups.json");
            app.manage(state::AppState::new(persistence::GroupStore::load(
                groups_path,
            )));
            notifications::setup(app);
            tray::setup(app)?;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(groups::restore_saved_groups(handle));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run LAN Mesh Tauri app");
}
