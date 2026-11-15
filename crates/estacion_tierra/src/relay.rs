use anyhow::Result;
use chrono::Local;
use protocolo_comun::{R3c1Ack, R3u1Header};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

/// Crea un socket UDP de alto rendimiento con búferes de 2 MB y SO_REUSEADDR
fn create_optimized_udp_socket(addr: SocketAddr) -> Result<UdpSocket> {
    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = socket.set_reuse_address(true);
    let _ = socket.set_recv_buffer_size(2 * 1024 * 1024);
    let _ = socket.set_send_buffer_size(2 * 1024 * 1024);
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    let std_sock: std::net::UdpSocket = socket.into();
    Ok(UdpSocket::from_std(std_sock)?)
}

pub async fn run_relay(
    listen_port: u16,
    forward_to: Option<String>,
    _save_dir: Option<PathBuf>,
) -> Result<()> {
    let listen_addr: SocketAddr = format!("0.0.0.0:{}", listen_port).parse()?;
    let rx_sock = Arc::new(create_optimized_udp_socket(listen_addr)?);

    let tx_bind_addr: SocketAddr = "0.0.0.0:0".parse()?;
    let tx_sock = Arc::new(create_optimized_udp_socket(tx_bind_addr)?);

    let forward_target: Option<SocketAddr> = if let Some(ref target) = forward_to {
        Some(target.parse()?)
    } else {
        None
    };

    let running = Arc::new(AtomicBool::new(true));
    let r_sig = running.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        println!("\n⏹ Deteniendo Relay Gateway (Ctrl+C recibido)...");
        r_sig.store(false, Ordering::SeqCst);
    });

    println!("===============================================================================");
    println!("📡 GATEWAY RELAY TOF / RGB-D: RASPBERRY PI -> ARCH LINUX -> WINDOWS");
    println!("   Escuchando flujo ToF/RGB-D del Dron en: 0.0.0.0:{}", listen_port);
    if let Some(target) = forward_target {
        println!("   Retransmitiendo datagramas UDP a Windows en: \x1b[32;1m{}\x1b[0m", target);
    } else {
        println!("   \x1b[33mModo Local Sniffer: Sin retransmisión (especifica --forward-to <IP:8766>)\x1b[0m");
    }
    println!("   Búferes de socket: 2 MB (SO_RCVBUF / SO_SNDBUF) para prevención de drop de kernel");
    println!("   Canal inverso de ACK (R3C1): Habilitado bidireccionalmente hacia el Dron");
    println!("===============================================================================");
    println!(
        "{:<10} {:<18} {:<10} {:<16} {:<14} {:<12}",
        "HORA", "ORIGEN DRON", "PKT/S", "VELOCIDAD", "FRAMES (FPS)", "ACKS (LOSS)"
    );
    println!("{}", "-".repeat(84));

    let last_drone_addr: Arc<RwLock<Option<SocketAddr>>> = Arc::new(RwLock::new(None));
    let total_pkts_fwd = Arc::new(AtomicU64::new(0));
    let total_bytes_fwd = Arc::new(AtomicU64::new(0));
    let total_acks_routed = Arc::new(AtomicU64::new(0));
    let last_ack_loss = Arc::new(AtomicU32::new(0));
    let last_ack_fid = Arc::new(AtomicU32::new(0));

    // ========================================================================
    // TAREA 1: CANAL INVERSO (Windows -> Laptop -> Dron)
    // Escucha datagramas de retorno (ACKs R3C1) en tx_sock y los enruta al dron
    // ========================================================================
    let tx_sock_ack = tx_sock.clone();
    let rx_sock_ack = rx_sock.clone();
    let last_drone_addr_ack = last_drone_addr.clone();
    let total_acks_clone = total_acks_routed.clone();
    let last_ack_loss_clone = last_ack_loss.clone();
    let last_ack_fid_clone = last_ack_fid.clone();
    let running_ack = running.clone();

    tokio::spawn(async move {
        let mut ack_buf = vec![0u8; 512];
        while running_ack.load(Ordering::Relaxed) {
            match tokio::time::timeout(
                Duration::from_millis(200),
                tx_sock_ack.recv_from(&mut ack_buf),
            )
            .await
            {
                Ok(Ok((len, _src))) => {
                    // Si es un paquete de confirmación R3C1, extraer métricas
                    if let Some(ack) = R3c1Ack::parse(&ack_buf[..len]) {
                        last_ack_fid_clone.store(ack.frame_id, Ordering::Relaxed);
                        last_ack_loss_clone.store(ack.loss_x100 as u32, Ordering::Relaxed);
                    }

                    // Enrutar el datagrama ACK de vuelta a la Raspberry Pi
                    let maybe_dst = *last_drone_addr_ack.read().await;
                    if let Some(dst) = maybe_dst {
                        let _ = rx_sock_ack.send_to(&ack_buf[..len], dst).await;
                        total_acks_clone.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => {} // Timeout normal
            }
        }
    });

    // ========================================================================
    // BUCLE PRINCIPAL: CANAL DIRECTO (Dron -> Laptop -> Windows)
    // Recibe fragmentos R3U1, los retransmite inmediatamente y computa métricas
    // ========================================================================
    let mut recv_buf = vec![0u8; 4096];
    let mut frame_chunks_tracker: HashMap<u32, (u16, u16)> = HashMap::new(); // frame_id -> (recibidos, total)
    let mut completed_frames_count: u64 = 0;
    let mut last_completed_frames: u64 = 0;

    let mut last_stat_time = std::time::Instant::now();
    let mut pkts_interval: u64 = 0;
    let mut bytes_interval: u64 = 0;

    while running.load(Ordering::Relaxed) {
        let (len, src_addr) = match tokio::time::timeout(
            Duration::from_millis(250),
            rx_sock.recv_from(&mut recv_buf),
        )
        .await
        {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => break,
            Err(_) => {
                // Timeout sin tráfico entrante, evaluar si toca imprimir reporte
                let now = std::time::Instant::now();
                if now.duration_since(last_stat_time) >= Duration::from_secs(1) {
                    print_stat_row(
                        &*last_drone_addr.read().await,
                        pkts_interval,
                        bytes_interval,
                        now.duration_since(last_stat_time).as_secs_f64(),
                        completed_frames_count,
                        completed_frames_count.saturating_sub(last_completed_frames),
                        total_acks_routed.load(Ordering::Relaxed),
                        last_ack_loss.load(Ordering::Relaxed),
                    );
                    last_stat_time = now;
                    pkts_interval = 0;
                    bytes_interval = 0;
                    last_completed_frames = completed_frames_count;
                }
                continue;
            }
        };

        // Actualizar dirección de origen del dron si cambió
        {
            let mut addr_lock = last_drone_addr.write().await;
            if *addr_lock != Some(src_addr) {
                *addr_lock = Some(src_addr);
            }
        }

        // RETRANSMISIÓN ZERO-COPY INMEDIATA A WINDOWS
        if let Some(target) = forward_target {
            let _ = tx_sock.send_to(&recv_buf[..len], target).await;
        }

        pkts_interval += 1;
        bytes_interval += len as u64;
        total_pkts_fwd.fetch_add(1, Ordering::Relaxed);
        total_bytes_fwd.fetch_add(len as u64, Ordering::Relaxed);

        // Inspección del encabezado R3U1 para seguimiento de integridad de frames
        if let Some(chunk) = R3u1Header::parse(&recv_buf[..len]) {
            let entry = frame_chunks_tracker
                .entry(chunk.frame_id)
                .or_insert((0, chunk.total_chunks));
            entry.0 += 1;
            if entry.0 >= entry.1 {
                completed_frames_count += 1;
                frame_chunks_tracker.remove(&chunk.frame_id);
            }
        }

        // Podar periódicamente frames obsoletos incompletos si se acumulan (> 200)
        if frame_chunks_tracker.len() > 200 {
            frame_chunks_tracker.clear();
        }

        // Reporte periódico cada 1 segundo
        let now = std::time::Instant::now();
        let dt = now.duration_since(last_stat_time);
        if dt >= Duration::from_secs(1) {
            print_stat_row(
                &*last_drone_addr.read().await,
                pkts_interval,
                bytes_interval,
                dt.as_secs_f64(),
                completed_frames_count,
                completed_frames_count.saturating_sub(last_completed_frames),
                total_acks_routed.load(Ordering::Relaxed),
                last_ack_loss.load(Ordering::Relaxed),
            );
            last_stat_time = now;
            pkts_interval = 0;
            bytes_interval = 0;
            last_completed_frames = completed_frames_count;
        }
    }

    println!("\n{}", "=".repeat(84));
    println!("📊 RESUMEN FINAL DE RETRANSMISIÓN RELAY (ARCH LINUX)");
    println!(
        "   Total Paquetes Procesados: {}",
        total_pkts_fwd.load(Ordering::Relaxed)
    );
    println!(
        "   Total Tráfico Reenviado:   {:.2} MB",
        total_bytes_fwd.load(Ordering::Relaxed) as f64 / (1024.0 * 1024.0)
    );
    println!("   Total Frames Reconstruidos: {}", completed_frames_count);
    println!(
        "   Total ACKs Enrutados:       {}",
        total_acks_routed.load(Ordering::Relaxed)
    );
    println!("===============================================================================");

    Ok(())
}

fn print_stat_row(
    drone_addr: &Option<SocketAddr>,
    pkts: u64,
    bytes: u64,
    dt_sec: f64,
    total_frames: u64,
    frames_in_interval: u64,
    total_acks: u64,
    last_loss_x100: u32,
) {
    let time_str = Local::now().format("%H:%M:%S").to_string();
    let src_str = match drone_addr {
        Some(addr) => format!("{}:{}", addr.ip(), addr.port()),
        None => "esperando...".to_string(),
    };

    let pps = if dt_sec > 0.0 {
        pkts as f64 / dt_sec
    } else {
        0.0
    };
    let kb_s = if dt_sec > 0.0 {
        (bytes as f64 / 1024.0) / dt_sec
    } else {
        0.0
    };
    let mbps = (kb_s * 8.0) / 1000.0;
    let fps = if dt_sec > 0.0 {
        frames_in_interval as f64 / dt_sec
    } else {
        0.0
    };
    let loss_pct = last_loss_x100 as f64 / 100.0;

    println!(
        "{:<10} {:<18} {:<10.0} {:<16} #{:<4} ({:<4.1} fps)  {:<4} ({:.1}%)",
        time_str,
        src_str,
        pps,
        format!("{:.1} KB/s ({:.2}M)", kb_s, mbps),
        total_frames,
        fps,
        total_acks,
        loss_pct
    );
}
