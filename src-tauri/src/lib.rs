mod net;

use net::{InterfaceInfo, Preset};

/// 列出所有物理网卡及其当前 IP 详情
#[tauri::command]
fn list_interfaces() -> Result<Vec<InterfaceInfo>, String> {
    net::list_interfaces()
}

/// 为指定服务设置静态 IP（经管理员授权弹窗执行）
#[tauri::command]
fn set_static_ip(
    service: String,
    ip: String,
    netmask: String,
    gateway: String,
    dns: Vec<String>,
) -> Result<net::ApplyResult, String> {
    net::set_static_ip(&service, &ip, &netmask, &gateway, &dns)
}

/// 将指定服务切换为 DHCP（经管理员授权弹窗执行）
#[tauri::command]
fn set_dhcp(service: String) -> Result<net::ApplyResult, String> {
    net::set_dhcp(&service)
}

#[tauri::command]
fn load_presets() -> Result<Vec<Preset>, String> {
    net::load_presets()
}

#[tauri::command]
fn save_presets(presets: Vec<Preset>) -> Result<(), String> {
    net::save_presets(&presets)
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            list_interfaces,
            set_static_ip,
            set_dhcp,
            load_presets,
            save_presets
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}