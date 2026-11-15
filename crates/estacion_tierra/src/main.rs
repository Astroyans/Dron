mod control;
mod range_test;
mod relay;
mod sync_client;

use anyhow::Result;
use clap::{Parser, Subcommand};
use control::{print_status_report, request_status, send_command_reliable};
use protocolo_comun::{CommandType, DEFAULT_STREAM_PORT, DEFAULT_TCP_SYNC_PORT, DEFAULT_UDP_PORT};
use range_test::run_range_test;
use relay::run_relay;
use std::path::PathBuf;
use std::time::Duration;
use sync_client::run_sync;
use tokio::time::sleep;

#[derive(Parser, Debug)]
#[command(
    name = "estacion_tierra",
    author = "Dron-Link Core Team",
    version = "0.1.0",
    about = "Estación de Tierra (GCS CLI) para control, telemetría y descarga criptográfica del Dron Topográfico"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// FASE 1: Prueba continua de alcance, latencia RTT, jitter y nivel de señal Wi-Fi
    RangeTest {
        /// Dirección IP de la Raspberry Pi en el dron
        #[arg(long, default_value = "192.168.4.207")]
        drone_ip: String,

        /// Puerto UDP del servicio
        #[arg(long, default_value_t = DEFAULT_UDP_PORT)]
        udp_port: u16,

        /// Frecuencia de envío de paquetes PING en Hertz (ej: 10 Hz = cada 100 ms)
        #[arg(long, default_value_t = 10)]
        freq_hz: u64,

        /// Tamaño de la ventana móvil para cálculo de pérdida y jitter
        #[arg(long, default_value_t = 50)]
        window: usize,

        /// Ruta personalizada para exportar el archivo CSV de métricas
        #[arg(long)]
        output_csv: Option<PathBuf>,
    },

    /// FASE 2: Iniciar proceso de captura topográfica ToF en la Raspberry Pi con reintentos fiables
    Start {
        /// Dirección IP de la Raspberry Pi
        #[arg(long, default_value = "192.168.4.207")]
        drone_ip: String,

        /// Puerto UDP del servicio
        #[arg(long, default_value_t = DEFAULT_UDP_PORT)]
        udp_port: u16,

        /// Número máximo de reintentos
        #[arg(long, default_value_t = 15)]
        max_retries: u32,

        /// Intervalo entre reintentos en milisegundos
        #[arg(long, default_value_t = 200)]
        interval_ms: u64,
    },

    /// FASE 2: Detener proceso de captura limpiamente (SIGINT al pipeline de la RealSense D415)
    Stop {
        /// Dirección IP de la Raspberry Pi
        #[arg(long, default_value = "192.168.4.207")]
        drone_ip: String,

        /// Puerto UDP del servicio
        #[arg(long, default_value_t = DEFAULT_UDP_PORT)]
        udp_port: u16,

        /// Número máximo de reintentos
        #[arg(long, default_value_t = 15)]
        max_retries: u32,

        /// Intervalo entre reintentos en milisegundos
        #[arg(long, default_value_t = 200)]
        interval_ms: u64,
    },

    /// FASE 2: Consultar la telemetría en vuelo (estado del proceso, capturas en disco, RSSI)
    Status {
        /// Dirección IP de la Raspberry Pi
        #[arg(long, default_value = "192.168.4.207")]
        drone_ip: String,

        /// Puerto UDP del servicio
        #[arg(long, default_value_t = DEFAULT_UDP_PORT)]
        udp_port: u16,

        /// Modo monitor continuo (actualiza la telemetría en vivo cada 1 segundo)
        #[arg(long, short)]
        watch: bool,
    },

    /// FASE 2: Volcado seguro e incremental de archivos por TCP con verificación SHA-256 tras aterrizar
    Sync {
        /// Dirección IP de la Raspberry Pi
        #[arg(long, default_value = "192.168.4.207")]
        drone_ip: String,

        /// Puerto TCP del servicio de transferencia en el dron
        #[arg(long, default_value_t = DEFAULT_TCP_SYNC_PORT)]
        tcp_port: u16,

        /// Directorio local en la laptop para almacenar las capturas recibidas
        #[arg(long, default_value = "/home/jaguar/Dev/nasa/tof/capturas_relieve")]
        dest_dir: PathBuf,
    },

    /// FASE 2 EXT: Gateway Relay en tiempo real (Dron -> Laptop Arch -> Máquina Windows)
    Relay {
        /// Puerto UDP en la laptop para recibir el flujo ToF continuo desde el dron
        #[arg(long, default_value_t = DEFAULT_STREAM_PORT)]
        drone_listen_port: u16,

        /// Dirección y puerto de la máquina Windows para retransmitir (ej: 192.168.4.94:8766)
        #[arg(long, default_value = "192.168.4.94:8766")]
        forward_to: Option<String>,

        /// Directorio local opcional para guardar réplica del streaming
        #[arg(long)]
        save_dir: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::RangeTest {
            drone_ip,
            udp_port,
            freq_hz,
            window,
            output_csv,
        } => {
            run_range_test(drone_ip, udp_port, freq_hz, window, output_csv).await?;
        }
        Commands::Start {
            drone_ip,
            udp_port,
            max_retries,
            interval_ms,
        } => {
            match send_command_reliable(
                &drone_ip,
                udp_port,
                CommandType::Start,
                max_retries,
                interval_ms,
            )
            .await
            {
                Ok(ack) => {
                    println!("\n✅ \x1b[32;1mRESPUESTA RECIBIDA:\x1b[0m {:?}", ack.status);
                    println!("   Mensaje: {}", ack.message);
                }
                Err(e) => {
                    eprintln!("\n❌ \x1b[31;1mERROR AL INICIAR CAPTURA:\x1b[0m {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Stop {
            drone_ip,
            udp_port,
            max_retries,
            interval_ms,
        } => {
            match send_command_reliable(
                &drone_ip,
                udp_port,
                CommandType::Stop,
                max_retries,
                interval_ms,
            )
            .await
            {
                Ok(ack) => {
                    println!("\n✅ \x1b[32;1mRESPUESTA RECIBIDA:\x1b[0m {:?}", ack.status);
                    println!("   Mensaje: {}", ack.message);
                }
                Err(e) => {
                    eprintln!("\n❌ \x1b[31;1mERROR AL DETENER CAPTURA:\x1b[0m {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Status {
            drone_ip,
            udp_port,
            watch,
        } => {
            if watch {
                println!("Iniciando monitor continuo de telemetría (Ctrl+C para salir)...");
                loop {
                    match request_status(&drone_ip, udp_port).await {
                        Ok(rep) => {
                            print!("{esc}[2J{esc}[1;1H", esc = 27 as char); // Limpiar pantalla
                            print_status_report(&rep);
                        }
                        Err(e) => {
                            eprintln!("⚠️ Error obteniendo telemetría: {}", e);
                        }
                    }
                    sleep(Duration::from_secs(1)).await;
                }
            } else {
                match request_status(&drone_ip, udp_port).await {
                    Ok(rep) => {
                        print_status_report(&rep);
                    }
                    Err(e) => {
                        eprintln!("❌ Error obteniendo telemetría del dron: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
        Commands::Sync {
            drone_ip,
            tcp_port,
            dest_dir,
        } => {
            run_sync(drone_ip, tcp_port, dest_dir).await?;
        }
        Commands::Relay {
            drone_listen_port,
            forward_to,
            save_dir,
        } => {
            run_relay(drone_listen_port, forward_to, save_dir).await?;
        }
    }

    Ok(())
}
