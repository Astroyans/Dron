use std::fs;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct WifiStats {
    pub rssi_dbm: i32,
    pub bitrate_mbps: f32,
    #[allow(dead_code)]
    pub interface: String,
}

impl Default for WifiStats {
    fn default() -> Self {
        Self {
            rssi_dbm: -50,
            bitrate_mbps: 54.0,
            interface: "wlan0".to_string(),
        }
    }
}

/// Lee las estadísticas de Wi-Fi de forma ligera (priorizando /proc/net/wireless sin fork de comandos)
pub fn read_wifi_stats_fast(preferred_iface: Option<&str>) -> WifiStats {
    // 1. Lectura directa de /proc/net/wireless (toma < 10 microsegundos y nunca bloquea)
    if let Ok(content) = fs::read_to_string("/proc/net/wireless") {
        for line in content.lines().skip(2) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((iface_part, stats_part)) = line.split_once(':') {
                let iface = iface_part.trim();
                if let Some(target) = preferred_iface {
                    if iface != target {
                        continue;
                    }
                }

                let tokens: Vec<&str> = stats_part.split_whitespace().collect();
                if tokens.len() >= 3 {
                    let level_str = tokens[2].trim_end_matches('.');
                    let rssi: i32 = level_str.parse().unwrap_or(-50);
                    let rssi_dbm = if rssi > 0 { rssi - 100 } else { rssi };

                    return WifiStats {
                        rssi_dbm,
                        bitrate_mbps: 72.2, // Bitrate estándar 802.11n 2.4GHz
                        interface: iface.to_string(),
                    };
                }
            }
        }
    }

    let iface = preferred_iface.unwrap_or("wlan0");
    WifiStats {
        rssi_dbm: -48,
        bitrate_mbps: 54.0,
        interface: iface.to_string(),
    }
}

/// Consulta completa de Wi-Fi (incluyendo bitrate con `iw`), ejecutada sólo periódicamente en segundo plano
pub fn read_wifi_stats_full(preferred_iface: Option<&str>) -> WifiStats {
    let mut stats = read_wifi_stats_fast(preferred_iface);
    let iface = preferred_iface.unwrap_or("wlan0");

    if let Some(br) = query_bitrate_iw(iface) {
        stats.bitrate_mbps = br;
    }
    stats
}

/// Consulta el bitrate usando `iw` (sólo en hilo secundario)
fn query_bitrate_iw(iface: &str) -> Option<f32> {
    let output = Command::new("iw")
        .args(["dev", iface, "link"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("tx bitrate:") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 3 {
                if let Ok(b) = parts[2].parse::<f32>() {
                    return Some(b);
                }
            }
        }
    }
    None
}
