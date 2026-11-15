use anyhow::Result;
use chrono::Local;
use protocolo_comun::{
    current_time_ms, decode_udp, encode_udp, evaluar_enlace, LinkQuality, PingMsg,
    UdpPacket,
};
use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;

#[derive(Debug, Clone)]
struct SampleRecord {
    timestamp_iso: String,
    seq_id: u64,
    rtt_ms: f32,
    jitter_ms: f32,
    loss_pct: f32,
    rssi_dbm: i32,
    bitrate_mbps: f32,
    calidad: String,
}

pub async fn run_range_test(
    drone_ip: String,
    udp_port: u16,
    freq_hz: u64,
    window_size: usize,
    output_csv: Option<PathBuf>,
) -> Result<()> {
    let target_addr: SocketAddr = format!("{}:{}", drone_ip, udp_port).parse()?;
    let bind_addr = "0.0.0.0:0";
    let socket = Arc::new(UdpSocket::bind(bind_addr).await?);

    let period_ms = 1000 / freq_hz.max(1);
    let running = Arc::new(AtomicBool::new(true));

    // Capturar Ctrl+C para finalizar y exportar el informe ordenadamente
    let r = running.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        println!("\n\n⏹ Deteniendo prueba de alcance (Ctrl+C recibido)...");
        r.store(false, Ordering::SeqCst);
    });

    println!("===============================================================================");
    println!("📡 FASE 1: PRUEBA DE ENLACE, LATENCIA Y ALCANCE MÁXIMO (RANGE TEST)");
    println!("   Destino (Nodo Dron):    {}", target_addr);
    println!("   Frecuencia de muestreo: {} Hz (cada {} ms)", freq_hz, period_ms);
    println!("   Ventana de análisis:    {} paquetes", window_size);
    println!("   Presiona Ctrl+C para detener y generar el reporte CSV de alcance.");
    println!("===============================================================================");
    println!(
        "{:<8} {:<12} {:<12} {:<12} {:<12} {:<14} {:<18}",
        "SEQ", "RTT (ms)", "JITTER (ms)", "PÉRDIDA", "RSSI (dBm)", "BITRATE", "ESTADO"
    );
    println!("{}", "-".repeat(84));

    let mut seq_id: u64 = 0;
    let mut history_sent: VecDeque<(u64, u64)> = VecDeque::new(); // (seq_id, tx_time_ms)
    let mut history_received: VecDeque<u64> = VecDeque::new();    // seq_id
    let mut all_samples: Vec<SampleRecord> = Vec::new();

    let mut last_rtt: Option<f32> = None;
    let mut current_jitter: f32 = 0.0;
    let mut recv_buf = vec![0u8; 4096];

    while running.load(Ordering::SeqCst) {
        seq_id += 1;
        let tx_time = current_time_ms();
        let ping_pkt = UdpPacket::Ping(PingMsg {
            seq_id,
            timestamp_tx_ms: tx_time,
        });

        if let Ok(bytes) = encode_udp(&ping_pkt) {
            let _ = socket.send_to(&bytes, target_addr).await;
            history_sent.push_back((seq_id, tx_time));
            if history_sent.len() > window_size {
                history_sent.pop_front();
            }
        }

        // Escuchar respuestas durante el periodo actual
        let timeout = Duration::from_millis(period_ms);
        let deadline = tokio::time::Instant::now() + timeout;

        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            match tokio::time::timeout(remaining, socket.recv_from(&mut recv_buf)).await {
                Ok(Ok((len, _from))) => {
                    if let Ok(UdpPacket::Pong(pong)) = decode_udp(&recv_buf[..len]) {
                        history_received.push_back(pong.seq_id);
                        if history_received.len() > window_size {
                            history_received.pop_front();
                        }

                        let now = current_time_ms();
                        let rtt = (now.saturating_sub(pong.timestamp_tx_ms)) as f32;

                        // Cálculo de Jitter RFC 3550
                        if let Some(prev_rtt) = last_rtt {
                            let diff = (rtt - prev_rtt).abs();
                            current_jitter += (diff - current_jitter) / 16.0;
                        }
                        last_rtt = Some(rtt);

                        // Cálculo de pérdida en la ventana deslizante
                        let window_total = history_sent.len();
                        let window_received = history_sent
                            .iter()
                            .filter(|(s, _)| history_received.contains(s))
                            .count();
                        let loss_pct = if window_total > 0 {
                            ((window_total - window_received) as f32 / window_total as f32) * 100.0
                        } else {
                            0.0
                        };

                        let calidad = evaluar_enlace(pong.rssi_dbm, loss_pct);
                        let estado_str = match calidad {
                            LinkQuality::Optimo => "\x1b[32mÓPTIMO\x1b[0m",
                            LinkQuality::Degradado => "\x1b[33mDEGRADADO\x1b[0m",
                            LinkQuality::LimiteAlcance => "\x1b[31;1mLÍMITE DE ALCANCE\x1b[0m",
                        };

                        println!(
                            "{:<8} {:<12.2} {:<12.2} {:<11.1}% {:<12} {:<14.1} {}",
                            format!("#{}", pong.seq_id),
                            rtt,
                            current_jitter,
                            loss_pct,
                            format!("{} dBm", pong.rssi_dbm),
                            format!("{} Mbps", pong.bitrate_mbps),
                            estado_str
                        );

                        all_samples.push(SampleRecord {
                            timestamp_iso: Local::now().to_rfc3339(),
                            seq_id: pong.seq_id,
                            rtt_ms: rtt,
                            jitter_ms: current_jitter,
                            loss_pct,
                            rssi_dbm: pong.rssi_dbm,
                            bitrate_mbps: pong.bitrate_mbps,
                            calidad: format!("{}", calidad),
                        });
                    }
                }
                _ => break,
            }
        }
    }

    // =========================================================================
    // REPORTE FINAL Y EXPORTACIÓN A CSV
    // =========================================================================
    println!("\n{}", "=".repeat(84));
    println!("📊 RESUMEN DE LA PRUEBA DE ALCANCE");
    println!("{}", "=".repeat(84));

    let total_sent = seq_id;
    let total_received = all_samples.len() as u64;
    let global_loss = if total_sent > 0 {
        ((total_sent.saturating_sub(total_received)) as f32 / total_sent as f32) * 100.0
    } else {
        0.0
    };

    let rtt_values: Vec<f32> = all_samples.iter().map(|s| s.rtt_ms).collect();
    let min_rtt = rtt_values.iter().cloned().reduce(f32::min).unwrap_or(0.0);
    let max_rtt = rtt_values.iter().cloned().reduce(f32::max).unwrap_or(0.0);
    let avg_rtt = if !rtt_values.is_empty() {
        rtt_values.iter().sum::<f32>() / rtt_values.len() as f32
    } else {
        0.0
    };

    let rssi_values: Vec<i32> = all_samples.iter().map(|s| s.rssi_dbm).collect();
    let avg_rssi = if !rssi_values.is_empty() {
        rssi_values.iter().sum::<i32>() / rssi_values.len() as i32
    } else {
        0
    };

    println!("  Paquetes transmitidos:  {}", total_sent);
    println!("  Paquetes recibidos:     {}", total_received);
    println!("  Pérdida global:         {:.2}%", global_loss);
    println!("  Latencia RTT Mín/Prom/Máx: {:.2} ms / {:.2} ms / {:.2} ms", min_rtt, avg_rtt, max_rtt);
    println!("  Jitter final:           {:.2} ms", current_jitter);
    println!("  RSSI promedio:          {} dBm", avg_rssi);

    let csv_path = output_csv.unwrap_or_else(|| {
        let date_str = Local::now().format("%Y%m%d_%H%M%S");
        PathBuf::from(format!("test_alcance_{}.csv", date_str))
    });

    println!("\n💾 Guardando reporte en: {}", csv_path.display());
    let mut file = File::create(&csv_path)?;
    writeln!(
        file,
        "timestamp_iso,seq_id,rtt_ms,jitter_ms,loss_pct,rssi_dbm,bitrate_mbps,estado"
    )?;
    for s in all_samples {
        writeln!(
            file,
            "{},{},{:.2},{:.2},{:.2},{},{:.2},{}",
            s.timestamp_iso,
            s.seq_id,
            s.rtt_ms,
            s.jitter_ms,
            s.loss_pct,
            s.rssi_dbm,
            s.bitrate_mbps,
            s.calidad
        )?;
    }
    println!("✅ Reporte CSV generado exitosamente.");

    Ok(())
}
