mod commands;
mod events;
mod ids;
mod network;
mod state;
mod updates;
mod views;

const DEFAULT_TTL: u8 = 8;
const DISCOVERY_PORT: u16 = 37020;

pub fn run() {
    tauri::Builder::default()
        .manage(state::AppState::default())
        .manage(updates::PendingUpdate::default())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::create_group,
            commands::discover_relays,
            commands::join_group,
            commands::close_session,
            commands::send_group_text,
            commands::send_direct_text,
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
            updates::check_update,
            updates::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run LAN Mesh Tauri app");
}
