mod process;
mod sync_server;
mod wifi;

use anyhow::Result;
use clap::Parser;
use process::ProcessManager;
use protocolo_comun::{
    current_time_ms, decode_udp, encode_udp, AckMsg, CommandType, PongMsg,
    UdpPacket, DEFAULT_TCP_SYNC_PORT, DEFAULT_UDP_PORT,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use sync_server::SyncServer;
use tokio::net::UdpSocket;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[derive(Parser, Debug)]
#[command(
    name = "nodo_dron",
    author = "Dron-Link Core Team",
    version = "0.1.0",
    about = "Demonio embebido para Raspberry Pi a bordo del dron (Intel RealSense D415)"
)]
struct Args {
    /// Puerto UDP para telemetría, ping de alcance y comandos
    #[arg(long, default_value_t = DEFAULT_UDP_PORT)]
    udp_port: u16,

    /// Puerto TCP para el servidor de volcado de datos al aterrizar
    #[arg(long, default_value_t = DEFAULT_TCP_SYNC_PORT)]
    tcp_port: u16,

    /// Ruta al script Python de captura de la cámara RealSense D415
    #[arg(long, default_value = "/home/pi/tof/main.py")]
    script_path: PathBuf,

    /// Ruta al script Python de streaming en vivo RealSense D415
    #[arg(long, default_value = "/home/pi/tof/stream_d415.py")]
    stream_script_path: PathBuf,

    /// IP y puerto destino de la laptop Arch Linux para streaming UDP
    #[arg(long, default_value = "192.168.4.1:8766")]
    stream_target: String,

    /// Binario o entorno virtual de Python a utilizar
    #[arg(long, default_value = "python3")]
    python_bin: String,

    /// Directorio donde el script almacena las carpetas captura_*
    #[arg(long, default_value = "/home/pi/tof/capturas_relieve")]
    data_dir: PathBuf,

    /// Nombre de la interfaz Wi-Fi para monitoreo de señal (ej: wlan0, wlan1)
    #[arg(long, default_value = "wlan0")]
    iface: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Inicializar sistema de logs estructurados
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let args = Args::parse();

    info!("=======================================================");
    info!("🚀 Iniciando demonio NODO_DRON en Raspberry Pi");
    info!("   Puerto UDP Comandos/Ping: {}", args.udp_port);
    info!("   Puerto TCP Sync:          {}", args.tcp_port);
    info!("   Script Captura ToF:       {}", args.script_path.display());
    info!("   Script Streaming ToF:     {}", args.stream_script_path.display());
    info!("   Destino Streaming Laptop: {}", args.stream_target);
    info!("   Directorio Datos:         {}", args.data_dir.display());
    info!("   Interfaz Wi-Fi:           {}", args.iface);
    info!("=======================================================");

    let process_mgr = Arc::new(ProcessManager::new(
        args.script_path.clone(),
        args.stream_script_path.clone(),
        args.stream_target.clone(),
        args.python_bin.clone(),
        args.data_dir.clone(),
    ));

    // Caché atómico de estadísticas Wi-Fi para no bloquear el socket UDP con llamadas I/O
    let wifi_cache = Arc::new(std::sync::RwLock::new(wifi::read_wifi_stats_fast(Some(&args.iface))));
    let wifi_cache_updater = Arc::clone(&wifi_cache);
    let iface_name = args.iface.clone();

    // Tarea en segundo plano para refrescar estadísticas Wi-Fi periódicamente sin frenar el Ping
    tokio::spawn(async move {
        let mut loop_count = 0u64;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            loop_count += 1;
            let iface = iface_name.clone();

            let stats = if loop_count % 4 == 0 {
                // Cada 2 segundos consultar bitrate con iw en hilo de bloqueo separado
                tokio::task::spawn_blocking(move || wifi::read_wifi_stats_full(Some(&iface)))
                    .await
                    .unwrap_or_default()
            } else {
                // Cada 500 ms lectura ultraligera de procfs
                wifi::read_wifi_stats_fast(Some(&iface))
            };

            if let Ok(mut w) = wifi_cache_updater.write() {
                *w = stats;
            }
        }
    });

    // Iniciar servidor TCP de sincronización en segundo plano
    let sync_srv = Arc::new(SyncServer::new(args.tcp_port, args.data_dir.clone()));
    tokio::spawn(async move {
        if let Err(e) = sync_srv.run().await {
            error!("Error fatal en Servidor TCP de Sincronización: {}", e);
        }
    });

    // Enlazar socket UDP para escuchar comandos y pruebas de alcance
    let udp_addr = format!("0.0.0.0:{}", args.udp_port);
    let socket = Arc::new(UdpSocket::bind(&udp_addr).await?);
    info!("Receptor UDP escuchando en {}", udp_addr);

    let mut buf = vec![0u8; 64 * 1024];

    loop {
        let (len, src_addr) = match socket.recv_from(&mut buf).await {
            Ok(res) => res,
            Err(e) => {
                warn!("Error recibiendo datagrama UDP: {}", e);
                continue;
            }
        };

        let packet = match decode_udp(&buf[..len]) {
            Ok(p) => p,
            Err(e) => {
                warn!("Datagrama UDP no reconocido desde {}: {}", src_addr, e);
                continue;
            }
        };

        handle_packet(
            packet,
            src_addr,
            &socket,
            &process_mgr,
            &wifi_cache,
        )
        .await;
    }
}

async fn handle_packet(
    packet: UdpPacket,
    src_addr: SocketAddr,
    socket: &Arc<UdpSocket>,
    process_mgr: &Arc<ProcessManager>,
    wifi_cache: &Arc<std::sync::RwLock<wifi::WifiStats>>,
) {
    match packet {
        UdpPacket::Ping(ping) => {
            // FASE 1: Responder inmediatamente al PING desde la memoria caché (latencia < 0.1 ms)
            let stats = wifi_cache.read().map(|s| s.clone()).unwrap_or_default();
            let pong = PongMsg {
                seq_id: ping.seq_id,
                timestamp_tx_ms: ping.timestamp_tx_ms,
                timestamp_rx_ms: current_time_ms(),
                rssi_dbm: stats.rssi_dbm,
                bitrate_mbps: stats.bitrate_mbps,
            };

            let resp = UdpPacket::Pong(pong);
            if let Ok(bytes) = encode_udp(&resp) {
                let _ = socket.send_to(&bytes, src_addr).await;
            }
        }
        UdpPacket::Command(cmd) => {
            // FASE 2: Procesar comando de inicio o detención
            info!("Comando recibido #{}: {:?}", cmd.cmd_id, cmd.command);
            let (status, message) = match cmd.command {
                CommandType::Start => process_mgr.start(),
                CommandType::Stop => process_mgr.stop(),
            };

            let ack = AckMsg {
                cmd_id: cmd.cmd_id,
                status,
                message,
                timestamp_ms: current_time_ms(),
            };

            let resp = UdpPacket::Ack(ack);
            if let Ok(bytes) = encode_udp(&resp) {
                let _ = socket.send_to(&bytes, src_addr).await;
            }
        }
        UdpPacket::StatusRequest(req) => {
            // FASE 2: Telemetría de estado del proceso y hardware
            let stats = wifi_cache.read().map(|s| s.clone()).unwrap_or_default();
            let report = process_mgr.get_status(req.req_id, stats.rssi_dbm);
            let resp = UdpPacket::StatusReport(report);
            if let Ok(bytes) = encode_udp(&resp) {
                let _ = socket.send_to(&bytes, src_addr).await;
            }
        }
        _ => {
            warn!("Paquete no manejado por el nodo dron desde {}", src_addr);
        }
    }
}
