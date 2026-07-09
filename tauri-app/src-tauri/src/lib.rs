mod commands;
mod events;
mod ids;
mod network;
mod state;
mod views;

const DEFAULT_TTL: u8 = 8;
const DISCOVERY_PORT: u16 = 37020;

pub fn run() {
    tauri::Builder::default()
        .manage(state::AppState::default())
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
        ])
        .run(tauri::generate_context!())
        .expect("failed to run LAN Mesh Tauri app");
}
