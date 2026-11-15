use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const DEFAULT_UDP_PORT: u16 = 5000;
pub const DEFAULT_TCP_SYNC_PORT: u16 = 5001;
pub const DEFAULT_STREAM_PORT: u16 = 8766;

pub const R3U1_CHUNK_MAGIC: [u8; 4] = *b"R3U1";
pub const R3C1_ACK_MAGIC: [u8; 4] = *b"R3C1";
pub const R3D1_PACKET_MAGIC: [u8; 4] = *b"R3D1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct R3u1Header {
    pub frame_id: u32,
    pub total_bytes: u32,
    pub chunk_idx: u16,
    pub total_chunks: u16,
    pub payload_len: u16,
}

impl R3u1Header {
    pub const SIZE: usize = 18;

    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE || &buf[0..4] != &R3U1_CHUNK_MAGIC {
            return None;
        }
        let frame_id = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let total_bytes = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let chunk_idx = u16::from_be_bytes([buf[12], buf[13]]);
        let total_chunks = u16::from_be_bytes([buf[14], buf[15]]);
        let payload_len = u16::from_be_bytes([buf[16], buf[17]]);
        Some(Self {
            frame_id,
            total_bytes,
            chunk_idx,
            total_chunks,
            payload_len,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct R3c1Ack {
    pub frame_id: u32,
    pub loss_x100: u16,
}

impl R3c1Ack {
    pub const SIZE: usize = 10;

    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE || &buf[0..4] != &R3C1_ACK_MAGIC {
            return None;
        }
        let frame_id = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let loss_x100 = u16::from_be_bytes([buf[8], buf[9]]);
        Some(Self {
            frame_id,
            loss_x100,
        })
    }
}

/// Devuelve el timestamp actual en milisegundos UTC.
pub fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Calcula el hash SHA-256 en hexadecimal de un archivo local.
pub fn compute_file_sha256<P: AsRef<Path>>(path: P) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024]; // 64 KB buffer
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Calcula el hash SHA-256 en hexadecimal de un slice de bytes.
pub fn compute_bytes_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

// ============================================================================
// PROTOCOLO UDP (Fase 1: Range Test y Fase 2: Control / Telemetría)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UdpPacket {
    Ping(PingMsg),
    Pong(PongMsg),
    Command(CommandMsg),
    Ack(AckMsg),
    StatusRequest(StatusRequestMsg),
    StatusReport(StatusReportMsg),
    Stream(StreamChunkMsg),
}

/// Fase 1: Paquete de prueba de enlace y latencia enviado por la estación de tierra
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PingMsg {
    pub seq_id: u64,
    pub timestamp_tx_ms: u64,
}

/// Fase 1: Respuesta del dron con telemetría de señal Wi-Fi
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PongMsg {
    pub seq_id: u64,
    pub timestamp_tx_ms: u64,
    pub timestamp_rx_ms: u64,
    pub rssi_dbm: i32,
    pub bitrate_mbps: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CommandType {
    Start,
    Stop,
}

/// Fase 2: Comandos de control (START / STOP)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandMsg {
    pub cmd_id: u64,
    pub command: CommandType,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AckStatus {
    Started,
    AlreadyRunning,
    Stopped,
    NotRunning,
    Error,
}

/// Fase 2: Confirmación de recepción y ejecución de comando
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AckMsg {
    pub cmd_id: u64,
    pub status: AckStatus,
    pub message: String,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusRequestMsg {
    pub req_id: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProcessState {
    Idle,
    Capturando,
    Error,
}

impl std::fmt::Display for ProcessState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessState::Idle => write!(f, "IDLE"),
            ProcessState::Capturando => write!(f, "CAPTURANDO"),
            ProcessState::Error => write!(f, "ERROR"),
        }
    }
}

/// Fase 2: Telemetría de estado del proceso de captura y hardware en vuelo
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusReportMsg {
    pub req_id: u64,
    pub state: ProcessState,
    pub pid: Option<u32>,
    pub elapsed_secs: u64,
    pub captures_count: usize,
    pub disk_free_bytes: u64,
    pub rssi_dbm: i32,
    pub last_error: Option<String>,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StreamDataType {
    DepthMap16Bit,
    PointCloudXYZ,
    ColorJpeg,
    TelemetryPose,
    ReliefSummary,
}

/// Transmisión de datos ToF en tiempo real fragmentados para transporte tolerante a pérdidas
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamChunkMsg {
    pub frame_id: u32,
    pub chunk_idx: u16,
    pub total_chunks: u16,
    pub data_type: StreamDataType,
    pub timestamp_ms: u64,
    pub payload: Vec<u8>,
}

/// Codifica un paquete UDP con bincode
pub fn encode_udp(packet: &UdpPacket) -> Result<Vec<u8>, bincode::Error> {
    bincode::serialize(packet)
}

/// Decodifica un paquete UDP con bincode
pub fn decode_udp(bytes: &[u8]) -> Result<UdpPacket, bincode::Error> {
    bincode::deserialize(bytes)
}

// ============================================================================
// PROTOCOLO TCP (Fase 2: Volcado de Archivos con Integridad Criptográfica)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    pub relative_path: String,
    pub size_bytes: u64,
    pub sha256_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub timestamp_ms: u64,
    pub base_dir: String,
    pub files: Vec<FileEntry>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SyncRequest {
    GetManifest,
    RequestFiles(Vec<String>),
    Close,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SyncResponse {
    Manifest(Manifest),
    FileHeader {
        relative_path: String,
        size_bytes: u64,
        sha256_hex: String,
    },
    FileChunk {
        relative_path: String,
        chunk: Vec<u8>,
    },
    FileEnd {
        relative_path: String,
        sha256_hex: String,
    },
    AllDone,
    Error(String),
}

/// Escribe un frame prefijado por longitud (4 bytes u32 big-endian) en un stream TCP
pub async fn write_framed_msg<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    msg: &T,
) -> io::Result<()> {
    let payload = bincode::serialize(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = payload.len() as u32;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Lee un frame prefijado por longitud desde un stream TCP
pub async fn read_framed_msg<R: AsyncRead + Unpin, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    // Límite de seguridad de 64 MB para frames de control/chunks
    if len > 64 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Frame excede el tamaño máximo permitido: {} bytes", len),
        ));
    }

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    bincode::deserialize(&payload)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ============================================================================
// EVALUACIÓN DE CALIDAD DE ENLACE (Fase 1)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkQuality {
    Optimo,
    Degradado,
    LimiteAlcance,
}

impl std::fmt::Display for LinkQuality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkQuality::Optimo => write!(f, "ÓPTIMO"),
            LinkQuality::Degradado => write!(f, "DEGRADADO"),
            LinkQuality::LimiteAlcance => write!(f, "LÍMITE DE ALCANCE"),
        }
    }
}

/// Evalúa el estado del enlace según RSSI y porcentaje de pérdida de paquetes en ventana móvil
pub fn evaluar_enlace(rssi_dbm: i32, loss_pct: f32) -> LinkQuality {
    // Si RSSI < -75 dBm o pérdida > 5% se considera límite de alcance crítico
    if rssi_dbm < -75 || loss_pct > 5.0 {
        LinkQuality::LimiteAlcance
    } else if rssi_dbm < -65 || loss_pct > 1.0 {
        LinkQuality::Degradado
    } else {
        LinkQuality::Optimo
    }
}
