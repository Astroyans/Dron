# DRON-LINK: Sistema Embebido y Enlace de Telemetría/Captura ToF en Rust

Workspace modular en Rust de alta disponibilidad para misiones topográficas con dron equipado con cámara de profundidad **Intel RealSense D415** y computadora de a bordo **Raspberry Pi (ARM64/Linux)**, controlado desde una estación de tierra en **Arch Linux**.

---

## 🏗 Arquitectura del Sistema

El proyecto está diseñado bajo un modelo híbrido **UDP de baja latencia + TCP con verificación criptográfica**:

```
 ┌─────────────────────────────────────────────────────────────┐
 │                LAPTOP (Arch Linux - Estación)               │
 │                    bin: estacion_tierra                     │
 └──────────────────────┬───────────────────────┬──────────────┘
                        │                       │
      [FASE 1: Range Test (10 Hz)]    [FASE 2: Sync TCP]
      [FASE 2: START / STOP (UDP)]    [Verificación SHA-256]
                        │                       │
                        ▼                       ▼
 ┌─────────────────────────────────────────────────────────────┐
 │               RASPBERRY PI (Dron Topográfico)               │
 │                      bin: nodo_dron                         │
 │                                                             │
 │   - Socket UDP (5000): Ping, Pong, Cmds, Telemetría         │
 │   - Servidor TCP (5001): Manifiestos y streaming de datos   │
 │   - Supervisor: Lanza /home/pi/tof/main.py en nuevo PGID    │
 │   - Hardware: Intel RealSense D415 (Depth, PLY, RGB, JSON)  │
 └─────────────────────────────────────────────────────────────┘
```

### Estructura del Cargo Workspace
- [crates/protocolo_comun](file:///home/jaguar/Dev/nasa/dron-link/crates/protocolo_comun): Definiciones de tipos serializados con `bincode`, tramas delimitadas con prefijo de longitud para TCP, paquetes UDP (`Ping`, `Pong`, `Command`, `Ack`, `StatusReport`), hashing SHA-256 (`sha2`) y reglas de evaluación de calidad de enlace.
- [crates/nodo_dron](file:///home/jaguar/Dev/nasa/dron-link/crates/nodo_dron): Demonio embebido asíncrono (`tokio`). Lee RSSI y bitrate desde `/proc/net/wireless` o `iw`, gestiona el proceso Python en un Process Group aislado (`setsid`), captura `stdout`/`stderr` y sirve archivos por TCP.
- [crates/estacion_tierra](file:///home/jaguar/Dev/nasa/dron-link/crates/estacion_tierra): CLI interactiva (`clap`) para pruebas de alcance con jitter RFC 3550, envío fiable de comandos con reintentos cada 200 ms, monitor de telemetría y sincronización incremental con hash SHA-256.

---

## 🚀 Compilación e Instalación

### 1. Compilación Nativa en Arch Linux (Laptop)
```bash
cd /home/jaguar/Dev/nasa/dron-link
cargo build --release -p estacion_tierra

# Binario resultante:
# target/release/estacion_tierra
```

### 2. Compilación para la Raspberry Pi (ARM64 / aarch64)

#### Opción A: Compilación cruzada desde Arch Linux con `cross` (Recomendado)
```bash
cargo install cross
cross build --target aarch64-unknown-linux-gnu --release -p nodo_dron

# Transferir a la Raspberry Pi:
scp target/aarch64-unknown-linux-gnu/release/nodo_dron pi@192.168.4.2:/home/pi/nodo_dron
```

#### Opción B: Compilación nativa en la Raspberry Pi
```bash
# Dentro de la Raspberry Pi (con Rust instalado vía rustup):
cd ~/dron-link
cargo build --release -p nodo_dron
sudo cp target/release/nodo_dron /usr/local/bin/nodo_dron
```

---

## 📡 Configuración del Enlace Físico (Hotspot 2.4 GHz en Arch Linux)

Para maximizar el alcance con la antena externa en la banda de 2.4 GHz (canal 1 o 6 despejado):

```bash
# Crear punto de acceso con NetworkManager
nmcli con add type wifi ifname wlan0 con-name DronHotspot autoconnect yes ssid DronTopografia
nmcli con modify DronHotspot 802-11-wireless.mode ap 802-11-wireless.band bg
nmcli con modify DronHotspot 802-11-wireless.channel 6
nmcli con modify DronHotspot ipv4.method shared ipv4.addresses 192.168.4.1/24
nmcli con modify DronHotspot wifi-sec.key-mgmt wpa-psk
nmcli con modify DronHotspot wifi-sec.psk "TopografiaDron2026"
nmcli con up DronHotspot
```

Asignar IP estática en la Raspberry Pi (`/etc/dhcpcd.conf` o `NetworkManager`): `192.168.4.2`.

---

## 📋 Guía Operativa por Fases

### 📍 FASE 1: Prueba de Enlace, Latencia y Alcance Máximo (Range Test)

Antes de despegar, realizar la prueba de caminata / alejamiento con la estación:

1. **En la Raspberry Pi:**
   ```bash
   ./nodo_dron --iface wlan0
   ```

2. **En la Laptop (Arch Linux):**
   ```bash
   ./target/release/estacion_tierra range-test --drone-ip 192.168.4.2 --freq-hz 10
   ```

3. **Monitoreo en Vivo en Consola:**
   - Visualiza en tiempo real: `RTT (ms)`, `Jitter (ms)`, `% Pérdida` (ventana móvil de 50 paquetes), `RSSI (dBm)`, `Bitrate (Mbps)` y Estado del enlace (`ÓPTIMO`, `DEGRADADO`, `LÍMITE DE ALCANCE`).
   - El estado pasa a `LÍMITE DE ALCANCE` automáticamente si el RSSI desciende por debajo de **-75 dBm** o si la pérdida supera el **5%**.
   - Al presionar `Ctrl+C`, se detiene limpiamente y genera el archivo `test_alcance_<fecha>.csv` con todas las métricas para documentar la distancia operativa real en metros.

---

### 🚁 FASE 2: Operación en Vuelo y Volcado Criptográfico

Una vez validada la cobertura:

#### 1. Iniciar Captura ToF (START)
```bash
./target/release/estacion_tierra start --drone-ip 192.168.4.2
```
- Reintenta automáticamente cada 200 ms hasta recibir `ACK_STARTED`.
- La Raspberry Pi lanza `/home/pi/tof/main.py` en un Process Group nuevo (`setsid`), canalizando `stdout`/`stderr` a `/tmp/tof_capture.log`.

#### 2. Monitorear Telemetría en Vuelo (STATUS)
```bash
# Consulta puntual:
./target/release/estacion_tierra status --drone-ip 192.168.4.2

# Modo monitor continuo (actualizado cada segundo):
./target/release/estacion_tierra status --drone-ip 192.168.4.2 --watch
```
Reporta:
- Estado del proceso (`IDLE`, `CAPTURANDO`, `ERROR`)
- PID del proceso Python
- Tiempo transcurrido de captura (HH:MM:SS)
- Número de carpetas generadas (`captura_*`)
- Espacio libre en la microSD de la Raspberry Pi
- Nivel de señal Wi-Fi actual (RSSI)

#### 3. Detener Captura (STOP)
```bash
./target/release/estacion_tierra stop --drone-ip 192.168.4.2
```
- Envía señal `SIGINT` al Process Group del script Python para que la cámara Intel RealSense D415 cierre sus streams y vacíe los buffers ordenadamente sin corromper archivos `.npy` o `.ply`.

#### 4. Volcado al Aterrizar con Verificación SHA-256 (SYNC)
Una vez en tierra:
```bash
./target/release/estacion_tierra sync --drone-ip 192.168.4.2 --dest-dir /home/jaguar/Dev/nasa/tof/capturas_relieve
```
- Se conecta vía TCP al puerto 5001 del dron.
- Solicita el **Manifiesto de Integridad** con los hashes SHA-256 precalculados.
- Compara con el directorio local en la laptop y **omite archivos idénticos ya existentes** (sincronización incremental resistente a cortes).
- Descarga los archivos pendientes mostrando barra de progreso `indicatif`.
- Verifica en tiempo real el SHA-256 de cada archivo recibido contra el manifiesto y reporta `[VERIFICADA (OK)]` o `[CORRUPTA (REINTENTAR)]`.

---

## ⚙️ Servicio Systemd para la Raspberry Pi (`nodo_dron.service`)

Para que el demonio inicie automáticamente al encender el dron:

```ini
[Unit]
Description=Dron-Link Embedded Daemon (Intel RealSense D415)
After=network.target

[Service]
Type=simple
User=pi
ExecStart=/usr/local/bin/nodo_dron --script-path /home/pi/tof/main.py --data-dir /home/pi/tof/capturas_relieve --iface wlan0
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

```bash
sudo cp nodo_dron.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now nodo_dron
```
