use anyhow::Result;
use protocolo_comun::{
    compute_file_sha256, current_time_ms, read_framed_msg, write_framed_msg, FileEntry, Manifest,
    SyncRequest, SyncResponse,
};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info, warn};
use walkdir::WalkDir;

pub struct SyncServer {
    tcp_port: u16,
    data_dir: PathBuf,
}

impl SyncServer {
    pub fn new(tcp_port: u16, data_dir: PathBuf) -> Self {
        Self { tcp_port, data_dir }
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.tcp_port);
        let listener = TcpListener::bind(&addr).await?;
        info!("Servidor TCP de Sincronización escuchando en {}", addr);

        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    info!("Nueva conexión TCP para volcado desde {}", peer_addr);
                    let server = Arc::clone(&self);
                    tokio::spawn(async move {
                        if let Err(e) = server.handle_client(stream).await {
                            error!("Error en sesión TCP con {}: {}", peer_addr, e);
                        }
                    });
                }
                Err(e) => {
                    warn!("Fallo al aceptar conexión TCP: {}", e);
                }
            }
        }
    }

    async fn handle_client(&self, mut stream: TcpStream) -> Result<()> {
        let (mut reader, mut writer) = stream.split();

        loop {
            let req: SyncRequest = match read_framed_msg(&mut reader).await {
                Ok(r) => r,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    info!("Cliente TCP cerró la conexión");
                    break;
                }
                Err(e) => {
                    warn!("Error leyendo petición TCP: {}", e);
                    break;
                }
            };

            match req {
                SyncRequest::GetManifest => {
                    info!("Generando manifiesto de archivos en: {}", self.data_dir.display());
                    let manifest = self.generate_manifest()?;
                    info!(
                        "Manifiesto generado: {} archivos ({} bytes en total)",
                        manifest.files.len(),
                        manifest.total_bytes
                    );
                    write_framed_msg(&mut writer, &SyncResponse::Manifest(manifest)).await?;
                }
                SyncRequest::RequestFiles(requested_paths) => {
                    info!("Petición de transferencia para {} archivo(s)", requested_paths.len());
                    for rel_path in requested_paths {
                        let full_path = self.data_dir.join(&rel_path);
                        if !full_path.exists() || !full_path.is_file() {
                            warn!("Archivo solicitado no existe: {}", full_path.display());
                            write_framed_msg(
                                &mut writer,
                                &SyncResponse::Error(format!("Archivo no encontrado: {}", rel_path)),
                            )
                            .await?;
                            continue;
                        }

                        // Calcular tamaño y SHA-256
                        let size = match full_path.metadata() {
                            Ok(m) => m.len(),
                            Err(e) => {
                                write_framed_msg(
                                    &mut writer,
                                    &SyncResponse::Error(format!("Error leyendo metadatos: {}", e)),
                                )
                                .await?;
                                continue;
                            }
                        };

                        let sha256_hex = match compute_file_sha256(&full_path) {
                            Ok(hash) => hash,
                            Err(e) => {
                                write_framed_msg(
                                    &mut writer,
                                    &SyncResponse::Error(format!("Error calculando SHA-256: {}", e)),
                                )
                                .await?;
                                continue;
                            }
                        };

                        // Enviar encabezado de archivo
                        write_framed_msg(
                            &mut writer,
                            &SyncResponse::FileHeader {
                                relative_path: rel_path.clone(),
                                size_bytes: size,
                                sha256_hex: sha256_hex.clone(),
                            },
                        )
                        .await?;

                        // Transmitir chunks de 64 KB
                        let mut file = match File::open(&full_path) {
                            Ok(f) => f,
                            Err(e) => {
                                write_framed_msg(
                                    &mut writer,
                                    &SyncResponse::Error(format!("Error abriendo archivo: {}", e)),
                                )
                                .await?;
                                continue;
                            }
                        };

                        let mut buf = vec![0u8; 64 * 1024];
                        let mut read_total = 0u64;

                        loop {
                            let bytes_read = match file.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => n,
                                Err(e) => {
                                    error!("Error leyendo archivo {}: {}", full_path.display(), e);
                                    break;
                                }
                            };

                            read_total += bytes_read as u64;
                            let chunk_msg = SyncResponse::FileChunk {
                                relative_path: rel_path.clone(),
                                chunk: buf[..bytes_read].to_vec(),
                            };
                            write_framed_msg(&mut writer, &chunk_msg).await?;
                        }

                        // Enviar fin de archivo
                        write_framed_msg(
                            &mut writer,
                            &SyncResponse::FileEnd {
                                relative_path: rel_path.clone(),
                                sha256_hex,
                            },
                        )
                        .await?;
                        info!("Archivo completado: {} ({} bytes)", rel_path, read_total);
                    }

                    // Notificar que todos los archivos solicitados fueron enviados
                    write_framed_msg(&mut writer, &SyncResponse::AllDone).await?;
                }
                SyncRequest::Close => {
                    info!("Cliente solicitó cierre de sesión de sincronización");
                    break;
                }
            }
        }

        Ok(())
    }

    fn generate_manifest(&self) -> Result<Manifest> {
        let mut entries = Vec::new();
        let mut total_bytes = 0u64;

        if self.data_dir.exists() {
            for entry in WalkDir::new(&self.data_dir)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.is_file() {
                    if let Ok(rel) = path.strip_prefix(&self.data_dir) {
                        let rel_str = rel.to_string_lossy().to_string();
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        if let Ok(sha256) = compute_file_sha256(path) {
                            total_bytes += size;
                            entries.push(FileEntry {
                                relative_path: rel_str,
                                size_bytes: size,
                                sha256_hex: sha256,
                            });
                        }
                    }
                }
            }
        }

        Ok(Manifest {
            timestamp_ms: current_time_ms(),
            base_dir: self.data_dir.to_string_lossy().to_string(),
            files: entries,
            total_bytes,
        })
    }
}
