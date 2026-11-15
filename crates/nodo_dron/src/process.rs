use protocolo_comun::{AckStatus, ProcessState, StatusReportMsg};
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

pub struct ProcessManager {
    script_path: PathBuf,
    stream_script_path: PathBuf,
    stream_target: String,
    python_bin: String,
    data_dir: PathBuf,
    log_file_path: PathBuf,
    state: Arc<Mutex<ProcessInnerState>>,
}

struct ProcessInnerState {
    child: Option<Child>,
    pid: Option<u32>,
    start_time: Option<Instant>,
    last_error: Option<String>,
    current_state: ProcessState,
}

impl ProcessManager {
    pub fn new(
        script_path: PathBuf,
        stream_script_path: PathBuf,
        stream_target: String,
        python_bin: String,
        data_dir: PathBuf,
    ) -> Self {
        let log_file_path = PathBuf::from("/tmp/tof_capture.log");
        Self {
            script_path,
            stream_script_path,
            stream_target,
            python_bin,
            data_dir,
            log_file_path,
            state: Arc::new(Mutex::new(ProcessInnerState {
                child: None,
                pid: None,
                start_time: None,
                last_error: None,
                current_state: ProcessState::Idle,
            })),
        }
    }

    /// Inicia el script de captura en un grupo de procesos independiente
    pub fn start(&self) -> (AckStatus, String) {
        let mut inner = self.state.lock().unwrap();

        // Verificar si ya está corriendo
        if let Some(ref mut child) = inner.child {
            match child.try_wait() {
                Ok(None) => {
                    return (
                        AckStatus::AlreadyRunning,
                        format!("El script ya está corriendo con PID {}", inner.pid.unwrap_or(0)),
                    );
                }
                Ok(Some(status)) => {
                    info!("Proceso anterior terminó con estado: {:?}", status);
                    inner.child = None;
                    inner.pid = None;
                }
                Err(e) => {
                    warn!("Error comprobando proceso anterior: {}", e);
                    inner.child = None;
                    inner.pid = None;
                }
            }
        }

        // Determinar qué script ejecutar (priorizar stream_d415.py para flujo en vivo)
        let (target_script, is_stream) = if self.stream_script_path.exists() {
            (self.stream_script_path.clone(), true)
        } else {
            let sibling_stream = self
                .script_path
                .parent()
                .unwrap_or(Path::new("."))
                .join("stream_d415.py");
            if sibling_stream.exists() {
                (sibling_stream, true)
            } else if self.script_path.exists() {
                (self.script_path.clone(), false)
            } else {
                let msg = format!(
                    "Ningún script de captura encontrado (revisados: {} y {})",
                    self.stream_script_path.display(),
                    self.script_path.display()
                );
                error!("{}", msg);
                inner.last_error = Some(msg.clone());
                inner.current_state = ProcessState::Error;
                return (AckStatus::Error, msg);
            }
        };

        // Determinar qué binario de Python utilizar (preferir venv de ToF)
        let venv_python = Path::new("/home/pi/tof/.venv/bin/python");
        let python_to_use = if venv_python.exists() {
            venv_python.to_string_lossy().to_string()
        } else if Path::new(&self.python_bin).exists() {
            self.python_bin.clone()
        } else {
            "python3".to_string()
        };

        // Asegurar que el directorio de salida existe
        if let Err(e) = fs::create_dir_all(&self.data_dir) {
            let msg = format!("No se pudo crear directorio de capturas: {}", e);
            error!("{}", msg);
            return (AckStatus::Error, msg);
        }

        // Abrir archivo de log para capturar stdout/stderr
        let log_file = match OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(&self.log_file_path)
        {
            Ok(f) => f,
            Err(e) => {
                let msg = format!("No se pudo abrir archivo de logs {}: {}", self.log_file_path.display(), e);
                error!("{}", msg);
                return (AckStatus::Error, msg);
            }
        };

        let err_log_file = match log_file.try_clone() {
            Ok(f) => f,
            Err(e) => {
                let msg = format!("Error clonando descriptor de log: {}", e);
                return (AckStatus::Error, msg);
            }
        };

        info!(
            "Lanzando proceso ToF: {} {} (modo stream: {})",
            python_to_use,
            target_script.display(),
            is_stream
        );

        let mut cmd = Command::new(&python_to_use);
        cmd.arg(&target_script);

        if is_stream {
            let parts: Vec<&str> = self.stream_target.split(':').collect();
            let dest_ip = parts.first().unwrap_or(&"192.168.4.1");
            let dest_port = parts.get(1).unwrap_or(&"8766");
            cmd.arg("--dest-ip").arg(dest_ip);
            cmd.arg("--dest-port").arg(dest_port);
        }

        cmd.stdout(Stdio::from(log_file))
            .stderr(Stdio::from(err_log_file));

        // Asignar un Process Group independiente (setsid) para manejo limpio de señales
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }

        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id();
                info!("Proceso de captura iniciado con PID: {}", pid);
                inner.child = Some(child);
                inner.pid = Some(pid);
                inner.start_time = Some(Instant::now());
                inner.last_error = None;
                inner.current_state = ProcessState::Capturando;
                (
                    AckStatus::Started,
                    format!("Captura iniciada con PID {}", pid),
                )
            }
            Err(e) => {
                let msg = format!("Error al ejecutar {}: {}", python_to_use, e);
                error!("{}", msg);
                inner.last_error = Some(msg.clone());
                inner.current_state = ProcessState::Error;
                (AckStatus::Error, msg)
            }
        }
    }

    /// Detiene el script enviando SIGINT a su grupo de procesos y esperando finalización limpia
    pub fn stop(&self) -> (AckStatus, String) {
        let mut inner = self.state.lock().unwrap();

        let pid = match inner.pid {
            Some(p) => p,
            None => {
                return (
                    AckStatus::NotRunning,
                    "No hay ningún proceso de captura en ejecución".to_string(),
                );
            }
        };

        info!("Enviando SIGINT al grupo de procesos -{}", pid);
        // Enviar SIGINT al grupo de procesos (con valor negativo del PID)
        unsafe {
            libc::kill(-(pid as i32), libc::SIGINT);
        }

        // Esperar hasta 5 segundos a que la cámara RealSense cierre sus streams ordenadamente
        let wait_timeout = Duration::from_secs(5);
        let start_wait = Instant::now();
        let mut exited = false;

        if let Some(ref mut child) = inner.child {
            while start_wait.elapsed() < wait_timeout {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        info!("Proceso {} finalizó limpiamente con estado: {:?}", pid, status);
                        exited = true;
                        break;
                    }
                    Ok(None) => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => {
                        warn!("Error esperando finalización del proceso: {}", e);
                        break;
                    }
                }
            }

            // Si no respondió a SIGINT tras el timeout, forzar cierre con SIGKILL
            if !exited {
                warn!("Timeout de apagado limpio excedido para PID {}. Enviando SIGKILL.", pid);
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
                let _ = child.wait();
            }
        }

        inner.child = None;
        inner.pid = None;
        inner.start_time = None;
        inner.current_state = ProcessState::Idle;

        (
            AckStatus::Stopped,
            format!("Proceso de captura (PID {}) detenido correctamente", pid),
        )
    }

    /// Lee el estado actual del proceso, capturas en disco y recursos
    pub fn get_status(&self, req_id: u64, rssi_dbm: i32) -> StatusReportMsg {
        let mut inner = self.state.lock().unwrap();

        // Comprobar si el proceso terminó por su cuenta (ej: fallo de hardware/cámara)
        if let Some(ref mut child) = inner.child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    info!("El proceso terminó inesperadamente: {:?}", status);
                    let err_snippet = self.read_last_log_lines(10);
                    inner.last_error = Some(format!(
                        "Proceso terminó con estado {}. Últimos logs:\n{}",
                        status, err_snippet
                    ));
                    inner.current_state = if status.success() {
                        ProcessState::Idle
                    } else {
                        ProcessState::Error
                    };
                    inner.child = None;
                    inner.pid = None;
                    inner.start_time = None;
                }
                Ok(None) => {
                    inner.current_state = ProcessState::Capturando;
                }
                Err(e) => {
                    inner.last_error = Some(format!("Error consultando estado: {}", e));
                    inner.current_state = ProcessState::Error;
                }
            }
        }

        let elapsed_secs = inner
            .start_time
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);

        let captures_count = self.count_captures();
        let disk_free = self.get_disk_free_bytes();

        StatusReportMsg {
            req_id,
            state: inner.current_state,
            pid: inner.pid,
            elapsed_secs,
            captures_count,
            disk_free_bytes: disk_free,
            rssi_dbm,
            last_error: inner.last_error.clone(),
            timestamp_ms: protocolo_comun::current_time_ms(),
        }
    }

    /// Cuenta los subdirectorios tipo 'captura_*'
    fn count_captures(&self) -> usize {
        if let Ok(entries) = fs::read_dir(&self.data_dir) {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    if let Ok(ft) = e.file_type() {
                        if ft.is_dir() {
                            let name = e.file_name().to_string_lossy().to_string();
                            return name.starts_with("captura_");
                        }
                    }
                    false
                })
                .count()
        } else {
            0
        }
    }

    /// Obtiene los bytes libres en el sistema de archivos del directorio de capturas
    fn get_disk_free_bytes(&self) -> u64 {
        let path = if self.data_dir.exists() {
            &self.data_dir
        } else {
            Path::new("/")
        };

        if let Ok(c_path) = CString::new(path.to_string_lossy().as_bytes()) {
            unsafe {
                let mut stat: libc::statvfs = std::mem::zeroed();
                if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
                    return stat.f_bavail as u64 * stat.f_frsize as u64;
                }
            }
        }
        0
    }

    /// Lee las últimas N líneas del archivo de logs para reportar errores
    fn read_last_log_lines(&self, lines_count: usize) -> String {
        if let Ok(content) = fs::read_to_string(&self.log_file_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(lines_count);
            lines[start..].join("\n")
        } else {
            "Sin registros disponibles".to_string()
        }
    }
}
