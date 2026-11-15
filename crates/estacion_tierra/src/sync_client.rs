use anyhow::{anyhow, Result};
use indicatif::{ProgressBar, ProgressStyle};
use protocolo_comun::{
    compute_file_sha256, read_framed_msg, write_framed_msg, FileEntry, Manifest, SyncRequest,
    SyncResponse,
};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use tokio::net::TcpStream;

pub async fn run_sync(
    drone_ip: String,
    tcp_port: u16,
    dest_dir: PathBuf,
) -> Result<()> {
    let target_addr = format!("{}:{}", drone_ip, tcp_port);
    println!("===============================================================================");
    println!("📦 FASE 2: VOLCADO DE ARCHIVOS CON VERIFICACIÓN CRIPTOGRÁFICA SHA-256");
    println!("   Conectando a servidor TCP: {}", target_addr);
    println!("   Directorio local destino:  {}", dest_dir.display());
    println!("===============================================================================");

    // Crear directorio de destino si no existe
    fs::create_dir_all(&dest_dir)?;

    let mut stream = match TcpStream::connect(&target_addr).await {
        Ok(s) => s,
        Err(e) => {
            return Err(anyhow!(
                "No se pudo conectar al dron en {}. Asegúrate de que el dron esté aterrizado y el demonio activo: {}",
                target_addr, e
            ));
        }
    };

    let (mut reader, mut writer) = stream.split();

    // 1. Solicitar Manifiesto de Integridad
    println!("📋 Solicitando manifiesto de archivos al dron...");
    write_framed_msg(&mut writer, &SyncRequest::GetManifest).await?;

    let manifest: Manifest = match read_framed_msg(&mut reader).await? {
        SyncResponse::Manifest(m) => m,
        other => return Err(anyhow!("Respuesta inesperada al pedir manifiesto: {:?}", other)),
    };

    println!(
        "   Manifiesto recibido: {} archivos encontrados (Total: {:.2} MB)",
        manifest.files.len(),
        manifest.total_bytes as f64 / (1024.0 * 1024.0)
    );

    if manifest.files.is_empty() {
        println!("ℹ️ No hay archivos de captura en el dron para sincronizar.");
        write_framed_msg(&mut writer, &SyncRequest::Close).await?;
        return Ok(());
    }

    // 2. Comparar con los archivos locales (Transferencia Incremental)
    let mut files_to_download: Vec<FileEntry> = Vec::new();
    let mut already_synced_count = 0usize;

    for remote_entry in &manifest.files {
        let local_path = dest_dir.join(&remote_entry.relative_path);
        if local_path.exists() && local_path.is_file() {
            // Verificar si el archivo local ya tiene el mismo SHA-256
            if let Ok(local_hash) = compute_file_sha256(&local_path) {
                if local_hash.eq_ignore_ascii_case(&remote_entry.sha256_hex) {
                    already_synced_count += 1;
                    continue;
                }
            }
        }
        files_to_download.push(remote_entry.clone());
    }

    println!(
        "   Archivos ya existentes y válidos: {}",
        already_synced_count
    );
    println!(
        "   Archivos pendientes de descarga:   {}",
        files_to_download.len()
    );

    if files_to_download.is_empty() {
        println!("\n✅ Todos los archivos ya se encuentran en la estación y verificados al 100%.");
        write_framed_msg(&mut writer, &SyncRequest::Close).await?;
        return Ok(());
    }

    // 3. Solicitar descarga de archivos pendientes
    let download_paths: Vec<String> = files_to_download
        .iter()
        .map(|f| f.relative_path.clone())
        .collect();

    let total_download_bytes: u64 = files_to_download.iter().map(|f| f.size_bytes).sum();
    write_framed_msg(&mut writer, &SyncRequest::RequestFiles(download_paths)).await?;

    // Configurar barra de progreso
    let pb = ProgressBar::new(total_download_bytes);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}")
            .unwrap()
            .progress_chars("#>-"),
    );

    // Variables de seguimiento para el archivo actual
    let mut current_file: Option<File> = None;
    let mut current_hasher: Option<Sha256> = None;
    let mut verification_results: Vec<(String, bool)> = Vec::new();

    loop {
        let resp: SyncResponse = read_framed_msg(&mut reader).await?;

        match resp {
            SyncResponse::FileHeader {
                relative_path,
                size_bytes: _,
                sha256_hex: _,
            } => {
                let local_path = dest_dir.join(&relative_path);
                if let Some(parent) = local_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                let file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&local_path)?;

                current_file = Some(file);
                current_hasher = Some(Sha256::new());

                pb.set_message(format!("Descargando: {}", relative_path));
            }
            SyncResponse::FileChunk {
                relative_path: _,
                chunk,
            } => {
                if let (Some(ref mut f), Some(ref mut hasher)) =
                    (&mut current_file, &mut current_hasher)
                {
                    f.write_all(&chunk)?;
                    hasher.update(&chunk);
                    pb.inc(chunk.len() as u64);
                }
            }
            SyncResponse::FileEnd {
                relative_path,
                sha256_hex,
            } => {
                if let (Some(mut f), Some(hasher)) = (current_file.take(), current_hasher.take()) {
                    f.flush()?;
                    drop(f);

                    let computed_hash = format!("{:x}", hasher.finalize());
                    let is_ok = computed_hash.eq_ignore_ascii_case(&sha256_hex);
                    verification_results.push((relative_path.clone(), is_ok));
                }
            }
            SyncResponse::AllDone => {
                pb.finish_with_message("Descarga completada");
                break;
            }
            SyncResponse::Error(e) => {
                eprintln!("\n⚠️ Error reportado por el dron: {}", e);
                break;
            }
            other => {
                eprintln!("\n⚠️ Mensaje inesperado recibido durante transferencia: {:?}", other);
                break;
            }
        }
    }

    // 4. Cerrar sesión
    write_framed_msg(&mut writer, &SyncRequest::Close).await.ok();

    // 5. Imprimir reporte de verificación de integridad
    println!("\n{}", "=".repeat(84));
    println!("🔍 RESULTADOS DE VERIFICACIÓN DE INTEGRIDAD (SHA-256)");
    println!("{}", "=".repeat(84));

    let mut all_ok = true;
    for (rel_path, is_ok) in verification_results {
        if is_ok {
            println!("  [OK]       \x1b[32mVERIFICADA (OK)\x1b[0m          {}", rel_path);
        } else {
            println!("  [FAIL]     \x1b[31;1mCORRUPTA (REINTENTAR)\x1b[0m    {}", rel_path);
            all_ok = false;
        }
    }

    if all_ok {
        println!("\n✨ Todos los archivos fueron descargados y validados con éxito en {}", dest_dir.display());
    } else {
        println!("\n⚠️ Algunos archivos presentaron diferencias de hash. Vuelve a ejecutar `sync`.");
    }

    Ok(())
}
