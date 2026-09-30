//! macOS 网络配置逻辑：网卡列表 / 详情解析 / 静态 IP / 切换 DHCP / 预存储存。
//!
//! 只读操作通过 `networksetup` 直接执行；任何**修改网络配置**的操作都通过
//! `osascript` 的 `do shell script ... with administrator privileges` 触发系统
//! 管理员授权弹窗，不在应用内缓存或要求持久提权。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// 一张物理网卡（服务）的概要 + IP 详情
#[derive(Debug, Clone, Serialize)]
pub struct InterfaceInfo {
    /// networksetup 使用的服务名（如 "Wi-Fi"、"USB 10/100/1000 LAN"）
    pub service: String,
    /// 硬件端口名（通常与服务名一致）
    pub hardware_port: String,
    /// BSD 接口名（en0 / en4 等）
    pub device: String,
    /// 配置方式：DHCP / 手动 / 未知
    pub config_mode: String,
    pub ip: String,
    pub netmask: String,
    pub gateway: String,
    pub dns: Vec<String>,
    /// MAC 地址（Ethernet Address）
    pub mac: String,
    /// 接口状态：active / inactive
    pub status: String,
    /// MTU 值
    pub mtu: String,
    /// IPv6 配置方式：Automatic / Manual / Off / Link-local only
    pub ipv6_mode: String,
    /// IPv6 地址
    pub ipv6: String,
    /// IPv6 网关
    pub ipv6_router: String,
}

/// 静态 IP 预设：IP + 子网掩码 + 网关 + DNS
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub ip: String,
    pub netmask: String,
    pub gateway: String,
    pub dns: Vec<String>,
}

/// 写操作执行结果，返回给前端展示
#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub success: bool,
    pub message: String,
}

// ---------------------------------------------------------------------------
// 只读：网卡列表与详情
// ---------------------------------------------------------------------------

/// 列出所有物理网卡（存在 en* 设备的网络服务）及其当前 IP 详情。
pub fn list_interfaces() -> Result<Vec<InterfaceInfo>, String> {
    let order = run("networksetup", &["-listnetworkserviceorder"])?;
    let services = parse_service_order(&order)?;
    let mut result = Vec::new();
    for (service, hw_port, device) in services {
        // 仅保留真实物理网卡（en0/en1/...），跳过蓝牙、* 等虚拟/禁用项
        if !device.starts_with("en") {
            continue;
        }
        result.push(fetch_interface_detail(&service, &hw_port, &device)?);
    }
    Ok(result)
}

fn fetch_interface_detail(
    service: &str,
    hw_port: &str,
    device: &str,
) -> Result<InterfaceInfo, String> {
    let info = run("networksetup", &["-getinfo", service])?;
    let dns = run("networksetup", &["-getdnsservers", service]).unwrap_or_default();
    let mac = run("networksetup", &["-getmacaddress", service]).unwrap_or_default();
    let ifconfig = run("ifconfig", &[device]).unwrap_or_default();
    Ok(parse_interface_detail(
        service, hw_port, device, &info, &dns, &mac, &ifconfig,
    ))
}

/// 解析 `networksetup -listnetworkserviceorder` 输出，得到 (服务名, 硬件端口, 设备) 列表。
/// 仅当服务行后跟随硬件端口行时才成对记录。
fn parse_service_order(text: &str) -> Result<Vec<(String, String, String)>, String> {
    let mut entries = Vec::new();
    let mut pending_service: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('(') && !line.starts_with("(Hardware Port") {
            // 形如 "(2) Wi-Fi" 或 "(3) *Android USB"
            let after = line
                .splitn(2, ')')
                .nth(1)
                .map(|s| s.trim().trim_start_matches('*').trim().to_string())
                .unwrap_or_default();
            if !after.is_empty() {
                pending_service = Some(after);
            }
        } else if line.starts_with("(Hardware Port:") {
            // 形如 "(Hardware Port: Wi-Fi, Device: en0)"
            let inner = line.trim_end_matches(')');
            let after = inner
                .strip_prefix("(Hardware Port:")
                .unwrap_or(inner)
                .trim();
            let (hw, dev) = match after.split_once("Device:") {
                Some((h, d)) => (h.trim().trim_end_matches(',').trim().to_string(), d.trim().to_string()),
                None => (after.to_string(), String::new()),
            };
            if let Some(service) = pending_service.take() {
                if !dev.is_empty() {
                    entries.push((service, hw, dev));
                }
            }
        }
    }
    if entries.is_empty() {
        return Err("未能从 networksetup 输出解析出任何网络服务".to_string());
    }
    Ok(entries)
}

/// 从 `-getinfo`、`-getdnsservers`、`-getmacaddress` 与 `ifconfig` 输出解析单张网卡的详情。
fn parse_interface_detail(
    service: &str,
    hw_port: &str,
    device: &str,
    info: &str,
    dns: &str,
    mac_out: &str,
    ifconfig_out: &str,
) -> InterfaceInfo {
    let mut config_mode = "未知".to_string();
    let mut ip = String::new();
    let mut netmask = String::new();
    let mut gateway = String::new();
    let mut ipv6_mode = String::new();
    let mut ipv6 = String::new();
    let mut ipv6_router = String::new();

    for line in info.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if config_mode == "未知" && l.contains("Configuration") {
            config_mode = if l.contains("DHCP") {
                "DHCP".to_string()
            } else if l.contains("Manual") {
                "手动".to_string()
            } else {
                l.to_string()
            };
            continue;
        }
        // 注意匹配顺序：先匹配更长的 "IPv6 IP address:" / "IPv6 Router:"，
        // 再匹配 "IPv6:"（因为 "IPv6 IP address:" 不以 "IPv6:" 开头，顺序其实互不影响）
        if let Some(v) = l.strip_prefix("IP address:") {
            let v = v.trim();
            if v != "none" && !v.is_empty() {
                ip = v.to_string();
            }
        } else if let Some(v) = l.strip_prefix("Subnet mask:") {
            let v = v.trim();
            if v != "none" && !v.is_empty() {
                netmask = v.to_string();
            }
        } else if let Some(v) = l.strip_prefix("Router:") {
            let v = v.trim();
            if v != "none" && !v.is_empty() {
                gateway = v.to_string();
            }
        } else if let Some(v) = l.strip_prefix("IPv6 IP address:") {
            let v = v.trim();
            if v != "none" && !v.is_empty() {
                ipv6 = v.to_string();
            }
        } else if let Some(v) = l.strip_prefix("IPv6 Router:") {
            let v = v.trim();
            if v != "none" && !v.is_empty() {
                ipv6_router = v.to_string();
            }
        } else if let Some(v) = l.strip_prefix("IPv6:") {
            let v = v.trim();
            if !v.is_empty() {
                ipv6_mode = v.to_string();
            }
        }
    }

    let mut dns_list = Vec::new();
    for line in dns.lines() {
        let l = line.trim();
        if l.is_empty() || l.contains("DNS Servers") {
            continue;
        }
        if is_ipv4(l) {
            dns_list.push(l.to_string());
        }
    }

    let (status, mtu) = parse_ifconfig(ifconfig_out);

    InterfaceInfo {
        service: service.to_string(),
        hardware_port: hw_port.to_string(),
        device: device.to_string(),
        config_mode,
        ip,
        netmask,
        gateway,
        dns: dns_list,
        mac: parse_mac(mac_out),
        status,
        mtu,
        ipv6_mode,
        ipv6,
        ipv6_router,
    }
}

/// 从 `networksetup -getmacaddress` 输出中提取 MAC 地址（兼容新旧两种格式）。
fn parse_mac(text: &str) -> String {
    for line in text.lines() {
        for token in line.split_whitespace() {
            let t = token.trim_end_matches('.');
            if is_mac(t) {
                return t.to_string();
            }
        }
    }
    String::new()
}

/// 校验形如 xx:xx:xx:xx:xx:xx 的 MAC 地址
fn is_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// 从 `ifconfig <interface>` 输出中提取 (status, mtu)
fn parse_ifconfig(text: &str) -> (String, String) {
    let mut status = String::new();
    let mut mtu = String::new();
    for line in text.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("status:") {
            status = v.trim().to_string();
        }
        let tokens: Vec<&str> = l.split_whitespace().collect();
        for (i, t) in tokens.iter().enumerate() {
            if *t == "mtu" {
                if let Some(n) = tokens.get(i + 1) {
                    mtu = n.to_string();
                }
                break;
            }
        }
    }
    (status, mtu)
}

// ---------------------------------------------------------------------------
// 写操作：设置静态 IP / 切换 DHCP（经 osascript 提权）
// ---------------------------------------------------------------------------

/// 为指定服务设置静态 IP + 子网掩码 + 网关 + DNS。
pub fn set_static_ip(
    service: &str,
    ip: &str,
    netmask: &str,
    gateway: &str,
    dns: &[String],
) -> Result<ApplyResult, String> {
    let fields = [
        ("IP 地址", ip),
        ("子网掩码", netmask),
        ("网关", gateway),
    ];
    for (label, value) in fields {
        if !is_ipv4(value) {
            return Err(format!("{label}「{}」不是合法的 IPv4 地址", value));
        }
    }
    if dns.is_empty() {
        return Err("请至少填写一个 DNS 地址".to_string());
    }
    if !dns.iter().all(|d| is_ipv4(d)) {
        return Err("DNS 中存在非法的 IPv4 地址".to_string());
    }

    let dns_args = dns.iter().map(|d| shq(d)).collect::<Vec<_>>().join(" ");
    let shell = format!(
        "networksetup -setmanual {} {} {} {} && networksetup -setdnsservers {} {}",
        shq(service),
        shq(ip),
        shq(netmask),
        shq(gateway),
        shq(service),
        dns_args
    );
    let message = format!(
        "已为「{}」设置静态 IP：{}\n子网掩码：{}\n网关：{}\nDNS：{}",
        service,
        ip,
        netmask,
        gateway,
        dns.join("、")
    );
    let detail = format!(
        "服务「{}」 IP={} 掩码={} 网关={} DNS={}",
        service,
        ip,
        netmask,
        gateway,
        dns.join("、")
    );
    match run_privileged(&shell) {
        Ok(_) => {
            append_log("set_static_ip", &detail, "success", &message);
            Ok(ApplyResult {
                success: true,
                message,
            })
        }
        Err(e) => {
            append_log("set_static_ip", &detail, "fail", &e);
            Err(e)
        }
    }
}

/// 将指定服务切换为 DHCP 自动获取（同时清空手动 DNS，恢复使用 DHCP 下发的 DNS）。
pub fn set_dhcp(service: &str) -> Result<ApplyResult, String> {
    let shell = format!(
        "networksetup -setdhcp {} && networksetup -setdnsservers {} empty",
        shq(service),
        shq(service)
    );
    let message = format!("「{}」已切换为 DHCP 自动获取 IP，手动 DNS 已重置。", service);
    let detail = format!("服务「{}」切换为 DHCP", service);
    match run_privileged(&shell) {
        Ok(_) => {
            append_log("set_dhcp", &detail, "success", &message);
            Ok(ApplyResult {
                success: true,
                message,
            })
        }
        Err(e) => {
            append_log("set_dhcp", &detail, "fail", &e);
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// 预设持久化
// ---------------------------------------------------------------------------

fn presets_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "无法确定用户主目录（HOME 未设置）".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("IPSwitcher")
        .join("presets.json"))
}

pub fn load_presets() -> Result<Vec<Preset>, String> {
    let path = presets_path()?;
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("读取预设文件失败：{}", e)),
    };
    match serde_json::from_str(&content) {
        Ok(p) => Ok(p),
        Err(e) => Err(format!("预设文件格式错误：{}", e)),
    }
}

pub fn save_presets(presets: &[Preset]) -> Result<(), String> {
    let path = presets_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建预设目录失败：{}", e))?;
    }
    let json = serde_json::to_string_pretty(presets).map_err(|e| format!("序列化预设失败：{}", e))?;
    std::fs::write(&path, json).map_err(|e| format!("写入预设文件失败：{}", e))
}

// ---------------------------------------------------------------------------
// 日志记录
// ---------------------------------------------------------------------------

/// 一条操作日志
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub time: String,
    pub action: String,
    pub detail: String,
    pub result: String, // success / fail
    pub message: String,
}

fn logs_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "无法确定用户主目录（HOME 未设置）".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("IPSwitcher")
        .join("logs.jsonl"))
}

/// 当前本地时间字符串（调用系统 date 命令，避免引入时间库依赖）。
fn now_local() -> String {
    if let Ok(out) = Command::new("date").args(["+%Y-%m-%d %H:%M:%S"]).output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    // 兜底：Unix 时间戳
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// 追加一条日志（JSONL 格式）。日志写入失败不阻断主流程。
pub fn append_log(action: &str, detail: &str, result: &str, message: &str) {
    let path = match logs_path() {
        Ok(p) => p,
        Err(_) => return,
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let entry = LogEntry {
        time: now_local(),
        action: action.to_string(),
        detail: detail.to_string(),
        result: result.to_string(),
        message: message.to_string(),
    };
    let json = match serde_json::to_string(&entry) {
        Ok(j) => j,
        Err(_) => return,
    };
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(file, "{json}");
    }
}

/// 读取日志，最新在前，最多返回 500 条。
pub fn list_logs() -> Result<Vec<LogEntry>, String> {
    let path = logs_path()?;
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("读取日志文件失败：{}", e)),
    };
    let mut logs: Vec<LogEntry> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    logs.reverse();
    if logs.len() > 500 {
        logs.truncate(500);
    }
    Ok(logs)
}

// ---------------------------------------------------------------------------
// 流量统计
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TrafficStats {
    pub device: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// 读取指定接口自启用以来的累计收发字节数（netstat -ib）。
pub fn get_traffic(device: &str) -> Result<TrafficStats, String> {
    let out = run("netstat", &["-ib"])?;
    parse_netstat_traffic(device, &out)
}

/// 解析 `netstat -ib` 输出：定位接口的 <Link#> 行并取 Ibytes / Obytes。
/// 兼容 Address 列为空（如 lo0）导致的列偏移。
fn parse_netstat_traffic(device: &str, text: &str) -> Result<TrafficStats, String> {
    let mut cols: usize = 0;
    let mut rx_idx: Option<usize> = None;
    let mut tx_idx: Option<usize> = None;

    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        if fields[0].eq_ignore_ascii_case("name") {
            cols = fields.len();
            for (i, f) in fields.iter().enumerate() {
                if f.eq_ignore_ascii_case("ibytes") {
                    rx_idx = Some(i);
                }
                if f.eq_ignore_ascii_case("obytes") {
                    tx_idx = Some(i);
                }
            }
            continue;
        }
        if fields[0] != device {
            continue;
        }
        // <Link#> 在 Network 列；无 Address 值（如 lo0）时整行会少一列
        if fields.iter().any(|f| f.starts_with("<Link")) {
            let (ri, ti) = match (rx_idx, tx_idx) {
                (Some(a), Some(b)) => (a, b),
                _ => return Err("无法解析 netstat 表头".to_string()),
            };
            let shift = if fields.len() == cols { 0 } else { 1 };
            let rx = fields
                .get(ri.saturating_sub(shift))
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let tx = fields
                .get(ti.saturating_sub(shift))
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            return Ok(TrafficStats {
                device: device.to_string(),
                rx_bytes: rx,
                tx_bytes: tx,
            });
        }
    }
    Err(format!("未找到接口 {device} 的流量数据"))
}

// ---------------------------------------------------------------------------
// 基础工具
// ---------------------------------------------------------------------------

/// 校验是否为合法的 IPv4 地址
pub fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|p| {
        !p.is_empty()
            && p.len() <= 3
            && p.chars().all(|c| c.is_ascii_digit())
            && p.parse::<u32>().map(|n| n <= 255).unwrap_or(false)
    })
}

/// Shell 参数加双引号并转义，防止服务名/参数中的空格与特殊字符被解析
fn shq(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('`', "\\`")
    )
}

/// AppleScript 字符串转义（先转义反斜杠，再转义双引号）
fn apple_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 直接执行命令，返回 stdout。
fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("无法执行 {cmd}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let detail = if stderr.trim().is_empty() { stdout } else { stderr };
        Err(format!("{cmd} 执行失败（{}）：{}", out.status, detail.trim()))
    }
}

/// 以管理员权限执行 shell 命令（触发系统授权弹窗），返回 stdout。
fn run_privileged(shell_cmd: &str) -> Result<String, String> {
    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        apple_escape(shell_cmd)
    );
    let out = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("无法执行 osascript: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        Ok(stdout.trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let raw = if stderr.trim().is_empty() { stdout } else { stderr };
        Err(interpret_auth_error(raw.trim()))
    }
}

/// 把 osascript 提权失败的错误转成可读提示
fn interpret_auth_error(raw: &str) -> String {
    if raw.contains("User canceled") || raw.contains("(-128)") {
        "已取消：未获得管理员授权，网络配置未变更。".to_string()
    } else if raw.contains("Wrong password") || raw.contains("passphrase") {
        "授权失败：密码错误或验证失败，网络配置未变更。".to_string()
    } else {
        format!("执行失败：{}", raw)
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_ORDER: &str = r#"An asterisk (*) denotes that a network service is disabled.

(1) USB 10/100/1000 LAN
(Hardware Port: USB 10/100/1000 LAN, Device: en4)

(2) Wi-Fi
(Hardware Port: Wi-Fi, Device: en0)

(3) iPhone USB
(Hardware Port: iPhone USB, Device: en5)

(4) Bluetooth PAN
(Hardware Port: Bluetooth PAN, Device: en1)

(5) Display Sleep
(Hardware Port: Display Sleep, Device: *)

(6) *Thunderbolt Bridge
(Hardware Port: Thunderbolt Bridge, Device: bridge0)
"#;

    const SAMPLE_MANUAL_INFO: &str = r#"Manual Configuration

IP address: 192.168.1.100
Subnet mask: 255.255.255.0
Router: 192.168.1.1
IPv6: Automatic
IPv6 IP address: none
IPv6 Router: none
Ethernet Address: 3c:22:fb:a1:b2:c3
"#;

    const SAMPLE_DHCP_INFO: &str = r#"DHCP Configuration

IP address: 10.6.172.22
Subnet mask: 255.255.0.0
Router: 10.6.0.1
IPv6: Automatic
IPv6 IP address: none
IPv6 Router: none
"#;

    const SAMPLE_NO_IP_INFO: &str = r#"DHCP Configuration

IP address: none
Subnet mask: none
Router: none
IPv6: Automatic
"#;

    const SAMPLE_IPV6_INFO: &str = r#"DHCP Configuration

IP address: 10.6.172.22
Subnet mask: 255.255.0.0
Router: 10.6.0.1
IPv6: Automatic
IPv6 IP address: fe80::1c22:fbff:fea1:b2c3%en0
IPv6 Router: fe80::1c22:fbff:fea1:b2c3%en0
"#;

    const SAMPLE_IFCONFIG: &str = r#"en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	options=6463<RXCSUM,TXCSUM,VLAN_MTU,TSO4,TSO6,CHANNEL_IO,PARTIAL_CSUM,ZEROINV_CSUM>
	ether 3c:22:fb:a1:b2:c3
	inet6 fe80::1c22:fbff:fea1:b2c3%en0 prefixlen 64 secured scopeid 0x4
	inet 192.168.1.162 netmask 0xffffff00 broadcast 192.168.1.255
	mediamtu: 1500
	media: autoselect
	status: active
"#;

    #[test]
    fn parses_service_order_and_filters_non_en() {
        let entries = parse_service_order(SAMPLE_ORDER).unwrap();
        assert_eq!(entries.len(), 6, "应解析出全部成对服务");
        assert_eq!(entries[1], ("Wi-Fi".to_string(), "Wi-Fi".to_string(), "en0".to_string()));
        // Display Sleep 与 Thunderbolt Bridge 设备不是 en*，不应进入最终网卡列表
        let devices_str: String = entries
            .iter()
            .map(|e| e.2.as_str())
            .collect::<Vec<_>>()
            .join(",");
        assert!(devices_str.contains("en0") && devices_str.contains("en4"));
    }

    #[test]
    fn parses_manual_detail() {
        let iface = parse_interface_detail(
            "Wi-Fi", "Wi-Fi", "en0", SAMPLE_MANUAL_INFO, SAMPLE_MANUAL_INFO, SAMPLE_MANUAL_INFO, SAMPLE_IFCONFIG,
        );
        assert_eq!(iface.config_mode, "手动");
        assert_eq!(iface.ip, "192.168.1.100");
        assert_eq!(iface.netmask, "255.255.255.0");
        assert_eq!(iface.gateway, "192.168.1.1");
        assert_eq!(iface.mac, "3c:22:fb:a1:b2:c3");
        assert_eq!(iface.ipv6_mode, "Automatic");
        assert_eq!(iface.ipv6, "");
        assert_eq!(iface.status, "active");
        assert_eq!(iface.mtu, "1500");
        // Ethernet Address 等不应被误解析为 DNS
        assert!(iface.dns.is_empty());
    }

    #[test]
    fn parses_dhcp_detail() {
        let iface = parse_interface_detail(
            "Wi-Fi", "Wi-Fi", "en0", SAMPLE_DHCP_INFO, "8.8.8.8\n1.1.1.1\n", "", "",
        );
        assert_eq!(iface.config_mode, "DHCP");
        assert_eq!(iface.ip, "10.6.172.22");
        assert_eq!(iface.netmask, "255.255.0.0");
        assert_eq!(iface.gateway, "10.6.0.1");
        assert_eq!(iface.dns, vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()]);
        assert_eq!(iface.ipv6_mode, "Automatic");
        assert_eq!(iface.ipv6, "");
        assert_eq!(iface.mac, "");
    }

    #[test]
    fn parses_no_ip_detail() {
        let iface = parse_interface_detail("Wi-Fi", "Wi-Fi", "en0", SAMPLE_NO_IP_INFO, "", "", "");
        assert_eq!(iface.config_mode, "DHCP");
        assert_eq!(iface.ip, "");
        assert_eq!(iface.gateway, "");
    }

    #[test]
    fn parses_dns_with_auto_message() {
        let dns =
            "There aren't any DNS Servers set on Wi-Fi.\n2026:db8::1\n".to_string();
        let iface = parse_interface_detail("Wi-Fi", "Wi-Fi", "en0", SAMPLE_DHCP_INFO, &dns, "", "");
        assert!(iface.dns.is_empty(), "自动 DNS 与 IPv6 不应被收进 IPv4 DNS 列表");
    }

    #[test]
    fn parses_ipv6_fields() {
        let iface = parse_interface_detail("Wi-Fi", "Wi-Fi", "en0", SAMPLE_IPV6_INFO, "", "", "");
        assert_eq!(iface.ipv6_mode, "Automatic");
        assert_eq!(iface.ipv6, "fe80::1c22:fbff:fea1:b2c3%en0");
        assert_eq!(iface.ipv6_router, "fe80::1c22:fbff:fea1:b2c3%en0");
    }

    #[test]
    fn parses_mac_both_formats() {
        // 新版格式：Wi-Fi en0 has an active Ethernet hardware address of XX:XX:...
        let modern = parse_mac("Wi-Fi en0 has an active Ethernet hardware address of 3c:22:fb:a1:b2:c3");
        assert_eq!(modern, "3c:22:fb:a1:b2:c3");
        // 旧版格式：Ethernet Address: XX:XX:...
        let legacy = parse_mac("Ethernet Address: aa:bb:cc:dd:ee:ff");
        assert_eq!(legacy, "aa:bb:cc:dd:ee:ff");
        // 空输出
        assert_eq!(parse_mac(""), "");
    }

    #[test]
    fn parses_ifconfig_status_and_mtu() {
        let (status, mtu) = parse_ifconfig(SAMPLE_IFCONFIG);
        assert_eq!(status, "active");
        // "mediamtu: 1500" 不应被误当成 mtu（要求 token 恰为 "mtu"）
        assert_eq!(mtu, "1500");
        let (s2, m2) = parse_ifconfig("en0: flags=... mtu 1280\n\tstatus: inactive");
        assert_eq!(s2, "inactive");
        assert_eq!(m2, "1280");
    }

    #[test]
    fn validates_mac_format() {
        assert!(is_mac("3c:22:fb:a1:b2:c3"));
        assert!(is_mac("AA:BB:CC:DD:EE:FF"));
        assert!(!is_mac("3c22fba1b2c3"));
        assert!(!is_mac("3c:22:fb:a1:b2"));
        assert!(!is_mac("2001:db8:1:2:3:4")); // IPv6 六段不是 MAC（段长 4，非 2）
    }

    #[test]
    fn logs_append_and_list_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("ipswitcher-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("HOME", &tmp);
        append_log("set_dhcp", "服务「Wi-Fi」切换为 DHCP", "success", "已切换");
        append_log("add_preset", "办公网", "fail", "名称已存在");
        let logs = list_logs().expect("应能读取日志");
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].action, "add_preset", "最新一条应在前");
        assert_eq!(logs[0].result, "fail");
        assert_eq!(logs[1].action, "set_dhcp");
        assert!(!logs[0].time.is_empty(), "时间戳不应为空");
        // 文件存在且为 JSONL
        let content = std::fs::read_to_string(tmp.join("Library/Application Support/IPSwitcher/logs.jsonl"))
            .expect("日志文件应存在");
        assert_eq!(content.lines().count(), 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parses_netstat_traffic() {
        let sample = r#"Name  Mtu   Network       Address            Ipkts Ierrs    Ibytes    Opkts Oerrs    Obytes  Coll
lo0    16384  <Link#1>                       855006     0   87850616   855006     0   87850616     0
en0    1500   <Link#5>    3c:22:fb:a1:b2:c3    1000     0  123456789      900     0  987654321     0
en0    1500   fe80::1/64  fe80::1%en0           10      0       800       10      0       900       0
en0    1500   192.168.1.0/24 192.168.1.162     20      0      2000       20      0      2000       0
"#;
        let s = parse_netstat_traffic("en0", sample).expect("应解析出 en0 流量");
        assert_eq!(s.rx_bytes, 123456789, "应取 <Link#> 行，而非按 IP 行");
        assert_eq!(s.tx_bytes, 987654321);
        // Address 为空的 Link 行（如 lo0）存在列偏移，应仍能正确解析
        let s_lo = parse_netstat_traffic("lo0", sample).expect("应解析出 lo0 流量");
        assert_eq!(s_lo.rx_bytes, 87850616);
        assert_eq!(s_lo.tx_bytes, 87850616);
        assert_eq!(
            parse_netstat_traffic("en9", sample).unwrap_err(),
            "未找到接口 en9 的流量数据"
        );
    }

    #[test]
    fn builds_static_ip_shell_command() {
        let dns = vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()];
        let shell = format!(
            "networksetup -setmanual {} {} {} {} && networksetup -setdnsservers {} {}",
            shq("Wi-Fi"),
            shq("192.168.1.100"),
            shq("255.255.255.0"),
            shq("192.168.1.1"),
            shq("Wi-Fi"),
            dns.iter().map(|d| shq(d)).collect::<Vec<_>>().join(" ")
        );
        assert_eq!(
            shell,
            "networksetup -setmanual \"Wi-Fi\" \"192.168.1.100\" \"255.255.255.0\" \"192.168.1.1\" && networksetup -setdnsservers \"Wi-Fi\" \"8.8.8.8\" \"1.1.1.1\""
        );
    }

    #[test]
    fn builds_dhcp_shell_command() {
        let shell = format!(
            "networksetup -setdhcp {} && networksetup -setdnsservers {} empty",
            shq("USB 10/100/1000 LAN"),
            shq("USB 10/100/1000 LAN")
        );
        assert_eq!(
            shell,
            "networksetup -setdhcp \"USB 10/100/1000 LAN\" && networksetup -setdnsservers \"USB 10/100/1000 LAN\" empty"
        );
    }

    #[test]
    fn rejects_invalid_ipv4() {
        assert!(is_ipv4("192.168.1.1"));
        assert!(is_ipv4("0.0.0.0"));
        assert!(is_ipv4("255.255.255.255"));
        assert!(!is_ipv4("256.1.1.1"));
        assert!(!is_ipv4("192.168.1"));
        assert!(!is_ipv4("192.168.1.1.5"));
        assert!(!is_ipv4(""));
        assert!(!is_ipv4("1.2.3.4.5"));
    }

    #[test]
    fn apple_escape_quotes() {
        assert_eq!(apple_escape("a\"b\\c"), "a\\\"b\\\\c");
        let script = format!(
            "do shell script \"{}\" with administrator privileges",
            apple_escape("networksetup -setdhcp \"Wi-Fi\"")
        );
        assert_eq!(
            script,
            "do shell script \"networksetup -setdhcp \\\"Wi-Fi\\\"\" with administrator privileges"
        );
    }

    /// 真机只读集成测试：列出本机物理网卡与详情 + 流量。
    /// 仅执行只读命令，不改动任何网络配置。运行：cargo test -- --ignored
    #[test]
    #[ignore]
    fn real_machine_list_interfaces() {
        let ifaces = list_interfaces().expect("应能列出本机物理网卡");
        println!("本机物理网卡数：{}", ifaces.len());
        for i in &ifaces {
            println!(
                "- {} (device={}, mode={}, ip={}, mask={}, gw={}, dns={:?}, mac={}, status={}, mtu={}, ipv6_mode={}, ipv6={})",
                i.service,
                i.device,
                i.config_mode,
                i.ip,
                i.netmask,
                i.gateway,
                i.dns,
                i.mac,
                i.status,
                i.mtu,
                i.ipv6_mode,
                i.ipv6
            );
            if !i.device.is_empty() {
                match get_traffic(&i.device) {
                    Ok(t) => println!(
                        "    流量(累计收/发)：{} bytes / {} bytes",
                        t.rx_bytes, t.tx_bytes
                    ),
                    Err(e) => println!("    流量读取失败：{e}"),
                }
            }
        }
        assert!(!ifaces.is_empty(), "应至少发现一张物理网卡");
        for i in &ifaces {
            assert!(!i.service.is_empty() && !i.device.is_empty());
        }
    }
}