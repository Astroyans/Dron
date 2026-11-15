use anyhow::{anyhow, Result};
use protocolo_comun::{
    current_time_ms, decode_udp, encode_udp, AckMsg, CommandMsg, CommandType, StatusReportMsg,
    StatusRequestMsg, UdpPacket,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket;

/// Envía un comando crítico (START / STOP) con reintentos automáticos cada 200 ms hasta recibir ACK
pub async fn send_command_reliable(
    drone_ip: &str,
    udp_port: u16,
    cmd_type: CommandType,
    max_retries: u32,
    retry_interval_ms: u64,
) -> Result<AckMsg> {
    let target_addr: SocketAddr = format!("{}:{}", drone_ip, udp_port).parse()?;
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let cmd_id = current_time_ms();

    let cmd_packet = UdpPacket::Command(CommandMsg {
        cmd_id,
        command: cmd_type,
        timestamp_ms: current_time_ms(),
    });

    let encoded = encode_udp(&cmd_packet)?;
    let mut recv_buf = vec![0u8; 4096];

    let cmd_name = match cmd_type {
        CommandType::Start => "START",
        CommandType::Stop => "STOP",
    };

    println!(
        "📡 Enviando comando \x1b[1;36m{}\x1b[0m hacia {} (ID #{})",
        cmd_name, target_addr, cmd_id
    );

    for attempt in 1..=max_retries {
        // Enviar datagrama UDP
        socket.send_to(&encoded, target_addr).await?;

        // Esperar ACK con timeout
        let wait_dur = Duration::from_millis(retry_interval_ms);
        let start_wait = tokio::time::Instant::now();

        while start_wait.elapsed() < wait_dur {
            let remaining = wait_dur.saturating_sub(start_wait.elapsed());
            if remaining.is_zero() {
                break;
            }

            match tokio::time::timeout(remaining, socket.recv_from(&mut recv_buf)).await {
                Ok(Ok((len, _from))) => {
                    if let Ok(UdpPacket::Ack(ack)) = decode_udp(&recv_buf[..len]) {
                        if ack.cmd_id == cmd_id {
                            return Ok(ack);
                        }
                    }
                }
                _ => break,
            }
        }

        println!(
            "  ⏳ Intento {}/{} sin respuesta. Reintentando en {} ms...",
            attempt, max_retries, retry_interval_ms
        );
    }

    Err(anyhow!(
        "Tiempo de espera agotado tras {} intentos sin confirmación del dron",
        max_retries
    ))
}

/// Consulta la telemetría actual del nodo dron
pub async fn request_status(drone_ip: &str, udp_port: u16) -> Result<StatusReportMsg> {
    let target_addr: SocketAddr = format!("{}:{}", drone_ip, udp_port).parse()?;
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let req_id = current_time_ms();

    let req_packet = UdpPacket::StatusRequest(StatusRequestMsg { req_id });
    let encoded = encode_udp(&req_packet)?;
    socket.send_to(&encoded, target_addr).await?;

    let mut recv_buf = vec![0u8; 4096];
    let timeout_dur = Duration::from_millis(1500);

    match tokio::time::timeout(timeout_dur, socket.recv_from(&mut recv_buf)).await {
        Ok(Ok((len, _from))) => {
            if let Ok(UdpPacket::StatusReport(rep)) = decode_udp(&recv_buf[..len]) {
                return Ok(rep);
            }
            Err(anyhow!("Respuesta inesperada recibida"))
        }
        _ => Err(anyhow!("Timeout consultando estado del dron")),
    }
}

/// Formatea e imprime en consola la telemetría del dron
pub fn print_status_report(rep: &StatusReportMsg) {
    let state_str = match rep.state {
        protocolo_comun::ProcessState::Idle => "\x1b[34mIDLE (En espera)\x1b[0m",
        protocolo_comun::ProcessState::Capturando => "\x1b[32;1mCAPTURANDO (Activo)\x1b[0m",
        protocolo_comun::ProcessState::Error => "\x1b[31;1mERROR\x1b[0m",
    };

    let hours = rep.elapsed_secs / 3600;
    let mins = (rep.elapsed_secs % 3600) / 60;
    let secs = rep.elapsed_secs % 60;
    let time_fmt = format!("{:02}:{:02}:{:02}", hours, mins, secs);

    let free_gb = rep.disk_free_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

    println!("---------------------------------------------------------------");
    println!("🚁 TELEMETRÍA DEL DRON (Intel RealSense D415)");
    println!("---------------------------------------------------------------");
    println!("  Estado del Proceso:   {}", state_str);
    println!("  PID del Script:       {}", rep.pid.map(|p| p.to_string()).unwrap_or_else(|| "N/A".to_string()));
    println!("  Tiempo en Ejecución:  {}", time_fmt);
    println!("  Tomas Realizadas:     {} carpetas (captura_*)", rep.captures_count);
    println!("  Espacio Libre MicroSD:{:.2} GB", free_gb);
    println!("  Señal Wi-Fi (RSSI):   {} dBm", rep.rssi_dbm);
    if let Some(ref err) = rep.last_error {
        println!("  \x1b[31mÚltimo Error Registrado:\x1b[0m\n{}", err);
    }
    println!("---------------------------------------------------------------");
}
