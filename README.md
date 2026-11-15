# DRON-LINK: Embedded Telemetry & ToF Capture Link in Rust

**NASA Space Apps Challenge 2026 — *Interplanetary Survival Guide: Martian Map*** (Team **Astroyans**).

High-availability modular Rust workspace for topographic scout-drone missions equipped with an **Intel RealSense D415** depth camera and onboard **Raspberry Pi (ARM64/Linux)** companion computer, controlled from an **Arch Linux** ground station.

---

## System Architecture

The system uses a hybrid **low-latency UDP + cryptographically verified TCP** model:

```text
 ┌─────────────────────────────────────────────────────────────┐
 │                LAPTOP (Arch Linux - Ground Station)         │
 │                    bin: estacion_tierra                     │
 └──────────────────────┬───────────────────────┬──────────────┘
                        │                       │
      [PHASE 1: Range Test (10 Hz)]   [PHASE 2: TCP Sync]
      [PHASE 2: START / STOP (UDP)]   [SHA-256 Verification]
                        │                       │
                        ▼                       ▼
 ┌─────────────────────────────────────────────────────────────┐
 │               RASPBERRY PI (Topographic Drone)              │
 │                      bin: nodo_dron                         │
 │                                                             │
 │   - UDP Socket (5000): Ping, Pong, Commands, Telemetry      │
 │   - TCP Server (5001): Integrity Manifest & Data Streaming  │
 │   - Supervisor: Launches /home/pi/tof/main.py in new PGID   │
 │   - Hardware: Intel RealSense D415 (Depth, PLY, RGB, JSON)  │
 └─────────────────────────────────────────────────────────────┘
```

### Cargo Workspace Structure
* [`crates/protocolo_comun`](crates/protocolo_comun): `bincode`-serialized wire types, length-prefixed TCP framing, UDP datagrams (`Ping`, `Pong`, `Command`, `Ack`, `StatusReport`), SHA-256 hashing (`sha2`), and link-quality evaluation rules.
* [`crates/nodo_dron`](crates/nodo_dron): Asynchronous embedded daemon (`tokio`). Reads RSSI and bitrate from `/proc/net/wireless` or `iw`, supervises the Python capture process inside an isolated process group (`setsid`), captures `stdout`/`stderr`, and serves capture files over TCP.
* [`crates/estacion_tierra`](crates/estacion_tierra): Interactive CLI (`clap`) for RFC 3550 jitter range testing, reliable command dispatch with 200 ms retries, live telemetry monitoring, and incremental SHA-256 sync.

---

## Build & Installation

### 1. Native Build on Arch Linux (Ground Station Laptop)
```bash
cargo build --release -p estacion_tierra

# Resulting binary:
# target/release/estacion_tierra
```

### 2. Cross-Compilation for Raspberry Pi (ARM64 / `aarch64`)

#### Option A: Cross-compile with `cross` (Recommended)
```bash
cargo install cross
cross build --target aarch64-unknown-linux-gnu --release -p nodo_dron

# Copy binary to the Raspberry Pi:
scp target/aarch64-unknown-linux-gnu/release/nodo_dron pi@192.168.4.2:/home/pi/nodo_dron
```

#### Option B: Native build on the Raspberry Pi
```bash
# On the Raspberry Pi (with Rust installed via rustup):
cd ~/dron-link
cargo build --release -p nodo_dron
sudo cp target/release/nodo_dron /usr/local/bin/nodo_dron
```

---

## RF / Wi-Fi Link Configuration (2.4 GHz Hotspot on Arch Linux)

To maximize outdoor line-of-sight range with an external antenna on channel 1 or 6 (2.4 GHz):

```bash
# Create Wi-Fi access point with NetworkManager
nmcli con add type wifi ifname wlan0 con-name DronHotspot autoconnect yes ssid DronTopografia
nmcli con modify DronHotspot 802-11-wireless.mode ap 802-11-wireless.band bg
nmcli con modify DronHotspot 802-11-wireless.channel 6
nmcli con modify DronHotspot ipv4.method shared ipv4.addresses 192.168.4.1/24
nmcli con modify DronHotspot wifi-sec.key-mgmt wpa-psk
nmcli con modify DronHotspot wifi-sec.psk "TopografiaDron2026"
nmcli con up DronHotspot
```

Assign a static IP on the Raspberry Pi (`/etc/dhcpcd.conf` or `NetworkManager`): `192.168.4.2`.

---

## Operational Phase Guide

### Phase 1: Link, Latency & Maximum Range Test

Before takeoff, execute a walk-away link test with the ground station:

1. **On the Raspberry Pi:**
   ```bash
   ./nodo_dron --iface wlan0
   ```

2. **On the Laptop (Arch Linux):**
   ```bash
   ./target/release/estacion_tierra range-test --drone-ip 192.168.4.2 --freq-hz 10
   ```

3. **Live Console Monitoring:**
   * Displays real-time `RTT (ms)`, `Jitter (ms)`, `% Packet Loss` (50-packet sliding window), `RSSI (dBm)`, `Bitrate (Mbps)`, and link health status (`OPTIMAL`, `DEGRADED`, `RANGE LIMIT`).
   * Status automatically switches to `RANGE LIMIT` when RSSI drops below **-75 dBm** or packet loss exceeds **5%**.
   * Pressing `Ctrl+C` writes `test_alcance_<timestamp>.csv` with full field telemetry for operational range verification.

---

### Phase 2: Flight Capture & Cryptographic Data Sync

Once RF coverage is verified:

#### 1. Start ToF Capture (`START`)
```bash
./target/release/estacion_tierra start --drone-ip 192.168.4.2
```
* Automatically retries every 200 ms until `ACK_STARTED` is received.
* The Raspberry Pi spawns `/home/pi/tof/main.py` in a new Process Group (`setsid`), redirecting `stdout`/`stderr` to `/tmp/tof_capture.log`.

#### 2. Monitor In-Flight Telemetry (`STATUS`)
```bash
# Single query:
./target/release/estacion_tierra status --drone-ip 192.168.4.2

# Continuous monitor (refreshes every second):
./target/release/estacion_tierra status --drone-ip 192.168.4.2 --watch
```
Reports:
* Capture state (`IDLE`, `CAPTURING`, `ERROR`)
* Python process PID
* Elapsed capture duration (`HH:MM:SS`)
* Generated capture directory count (`captura_*`)
* Free space on the Raspberry Pi microSD card
* Current Wi-Fi signal strength (`RSSI`)

#### 3. Stop Capture (`STOP`)
```bash
./target/release/estacion_tierra stop --drone-ip 192.168.4.2
```
* Sends `SIGINT` to the Python process group so the Intel RealSense D415 flushes buffers and closes streams cleanly without corrupting `.npy` or `.ply` files.

#### 4. Post-Landing Sync with SHA-256 Verification (`SYNC`)
Once back on the ground:
```bash
./target/release/estacion_tierra sync --drone-ip 192.168.4.2 --dest-dir ./capturas_relieve
```
* Connects over TCP to port `5001` on the drone.
* Requests the precomputed **Integrity Manifest** containing SHA-256 digests.
* Compares against the local destination directory and **skips identical files already present** (resilient incremental sync).
* Downloads pending files with an `indicatif` progress bar and verifies SHA-256 checksums (`VERIFIED (OK)` / `CORRUPTED (RETRY)`).

---

## Systemd Service for Raspberry Pi (`nodo_dron.service`)

To launch the daemon automatically at boot on the drone companion computer:

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

---

## Related Astroyans Repositories

* **Web Mission Control & Astronaut iPad App:** [Astroyans/full-stack](https://github.com/Astroyans/full-stack)
* **Topographic Vision, Log-Odds Mapping & A*/Dijkstra Engine:** [Astroyans/sturdy-couscous](https://github.com/Astroyans/sturdy-couscous)
* **ESP32 Mesh & MicroSD Route Backup Firmware:** [Astroyans/Config-ESP32](https://github.com/Astroyans/Config-ESP32)
