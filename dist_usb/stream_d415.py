#!/usr/bin/env python3
"""
stream_d415.py: Transmisor en tiempo real Intel RealSense D415 -> Arch Linux -> Windows
Formato binario R3D1 empaquetado en RAM y fragmentación UDP R3U1 con canal inverso R3C1.
Optimizado para enlace Wi-Fi 2.4 GHz en Raspberry Pi ARM64.
"""
from __future__ import annotations

import argparse
import json
import os
import signal
import socket
import struct
import subprocess
import sys
import time
import zlib

try:
    import cv2
except ImportError:
    cv2 = None

try:
    import numpy as np
except ImportError:
    np = None

try:
    import pyrealsense2 as rs
except ImportError:
    rs = None

# Protocol constants
PACKET_MAGIC = b"R3D1"
UDP_CHUNK_MAGIC = b"R3U1"
UDP_ACK_MAGIC = b"R3C1"

_HEADER_STRUCT = struct.Struct("!4sI")       # 8 bytes
_LEN_STRUCT = struct.Struct("!I")           # 4 bytes
_UDP_CHUNK_STRUCT = struct.Struct("!4sIIHHH") # 18 bytes
_UDP_ACK_STRUCT = struct.Struct("!4sIH")     # 10 bytes

CHUNK_SIZE = 1200
DEFAULT_DEST_IP = "192.168.4.1"
DEFAULT_DEST_PORT = 8766

running = True


def signal_handler(signum, frame):
    global running
    print(f"\n[STREAM_D415] Señal {signum} recibida. Deteniendo streaming limpiamente...")
    running = False


signal.signal(signal.SIGINT, signal_handler)
signal.signal(signal.SIGTERM, signal_handler)


def parse_args():
    parser = argparse.ArgumentParser(description="Streaming ToF/RGB-D RealSense D415 en tiempo real")
    parser.add_argument("--dest-ip", default=DEFAULT_DEST_IP, help=f"IP destino Arch Linux (default: {DEFAULT_DEST_IP})")
    parser.add_argument("--dest-port", type=int, default=DEFAULT_DEST_PORT, help=f"Puerto UDP destino (default: {DEFAULT_DEST_PORT})")
    parser.add_argument("--width", type=int, default=480, help="Ancho de captura (default: 480)")
    parser.add_argument("--height", type=int, default=270, help="Alto de captura (default: 270)")
    parser.add_argument("--fps", type=int, default=15, help="Tasa de cuadros por segundo (default: 15)")
    parser.add_argument("--jpeg-quality", type=int, default=70, help="Calidad JPEG (default: 70)")
    parser.add_argument("--pacing-us", type=int, default=70, help="Micro-pacing entre datagramas en microsegundos (default: 70)")
    parser.add_argument("--mock", action="store_true", help="Forzar modo simulación sin hardware")
    return parser.parse_args()


class SyntheticIntrinsics:
    def __init__(self, w, h):
        self.fx = float(w * 0.8)
        self.fy = float(w * 0.8)
        self.ppx = float(w / 2.0)
        self.ppy = float(h / 2.0)


class V4L2DepthCapture:
    """Captura de profundidad 16-bit Z16 por V4L2 en RealSense D415."""
    def __init__(self, device: str = "/dev/video0", width: int = 480, height: int = 270, fps: int = 15):
        self.device = device
        self.width = width
        self.height = height
        self.fps = fps
        self.frame_size = width * height * 2
        self.proc = None
        self._start_capture()

    def _start_capture(self):
        # 1. Configurar emisor láser IR
        try:
            if os.path.exists("/usr/local/bin/d415_laser"):
                subprocess.run(["/usr/local/bin/d415_laser"], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except Exception:
            pass

        # 2. Iniciar tubería streaming v4l2-ctl
        cmd = [
            "v4l2-ctl",
            "--device", self.device,
            f"--set-fmt-video=width={self.width},height={self.height},pixelformat=Z16 ",
            "--stream-mmap",
            "--stream-to=-"
        ]
        self.proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            bufsize=self.frame_size * 4
        )

    def read_frame(self) -> bytes | None:
        if not self.proc or not self.proc.stdout:
            return None
        buf = self.proc.stdout.read(self.frame_size)
        if len(buf) == self.frame_size:
            return buf
        return None

    def close(self):
        if self.proc:
            try:
                self.proc.terminate()
                self.proc.wait(timeout=1.0)
            except Exception:
                try:
                    self.proc.kill()
                except Exception:
                    pass
            self.proc = None


def init_camera(width: int, height: int, fps: int, force_mock: bool):
    if force_mock:
        print("[STREAM_D415] ℹ️ Modo mock forzado. Usando generador sintético...")
        return None, None, 0.001, SyntheticIntrinsics(width, height)

    # 1. Intentar pyrealsense2 si está disponible
    if rs is not None and np is not None and cv2 is not None:
        try:
            print("[STREAM_D415] Probando backend pyrealsense2...")
            pipe = rs.pipeline()
            cfg = rs.config()
            cfg.enable_stream(rs.stream.depth, width, height, rs.format.z16, fps)
            cfg.enable_stream(rs.stream.color, width, height, rs.format.bgr8, fps)
            profile = pipe.start(cfg)
            sensor = profile.get_device().first_depth_sensor()
            depth_scale = float(sensor.get_depth_scale())
            align = rs.align(rs.stream.color)
            intr = profile.get_stream(rs.stream.color).as_video_stream_profile().get_intrinsics()
            print(f"[STREAM_D415] ✅ pyrealsense2 activo: {width}x{height}@{fps}fps")
            return ("rs", pipe), align, depth_scale, intr
        except Exception as e:
            print(f"[STREAM_D415] pyrealsense2 no disponible ({e}), probando V4L2 nativo...")

    # 2. Backend V4L2 nativo (/dev/video0) para RealSense D415 en Linux sin SDK propietario
    if os.path.exists("/dev/video0"):
        try:
            print(f"[STREAM_D415] 🎥 Iniciando hardware RealSense D415 vía V4L2 (/dev/video0) a {width}x{height}...")
            v4l2_cap = V4L2DepthCapture(device="/dev/video0", width=width, height=height, fps=fps)
            # Prueba de lectura del primer frame
            test_frame = v4l2_cap.read_frame()
            if test_frame is not None:
                print(f"[STREAM_D415] ✅ Hardware D415 V4L2 capturando OK ({len(test_frame)} bytes/frame)")
                return ("v4l2", v4l2_cap), None, 0.001, SyntheticIntrinsics(width, height)
            else:
                print("[STREAM_D415] ⚠️ Fallo al leer frame de /dev/video0")
                v4l2_cap.close()
        except Exception as e:
            print(f"[STREAM_D415] ⚠️ Error iniciando V4L2: {e}")

    print("[STREAM_D415] ⚠️ No se detectó hardware D415 funcional. Usando generador sintético.")
    return None, None, 0.001, SyntheticIntrinsics(width, height)


def main():
    args = parse_args()
    camera_handle, align, depth_scale, intr = init_camera(args.width, args.height, args.fps, args.mock)

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 1024 * 1024)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 512 * 1024)
    except Exception:
        pass
    sock.setblocking(False)

    dest_addr = (args.dest_ip, args.dest_port)
    print("===============================================================================")
    print("🚀 EMISOR STREAMING REAL-TIME D415 (RASPBERRY PI ARM64)")
    print(f"   Destino UDP: {dest_addr[0]}:{dest_addr[1]}")
    print(f"   Resolución objetivo: {args.width}x{args.height} @ {args.fps} FPS")
    print(f"   Pacing entre datagramas: {args.pacing_us} µs")
    print("===============================================================================")

    target_fps = args.fps
    effective_fps = target_fps
    target_jpeg_quality = args.jpeg_quality
    current_jpeg_quality = target_jpeg_quality
    pacing_s = max(0.0, float(args.pacing_us) / 1_000_000.0)

    frame_id = 0
    t0_stat = time.time()
    frames_sent = 0
    bytes_sent = 0
    acks_received = 0
    last_reported_loss = 0.0

    synthetic_t = 0.0

    try:
        while running:
            t_start = time.perf_counter()

            # 1. Leer canal inverso de retorno ACK (R3C1) para control de congestión
            try:
                while True:
                    ack_pkt, _ = sock.recvfrom(64)
                    if len(ack_pkt) >= 10 and ack_pkt[:4] == UDP_ACK_MAGIC:
                        _, ack_fid, loss_x100 = _UDP_ACK_STRUCT.unpack(ack_pkt[:10])
                        acks_received += 1
                        last_reported_loss = loss_x100 / 100.0
                        if loss_x100 > 1000: # > 10% de pérdida en el enlace Wi-Fi
                            effective_fps = max(8, effective_fps - 1)
                            current_jpeg_quality = max(40, current_jpeg_quality - 5)
                        elif loss_x100 < 300: # < 3% de pérdida: enlace estable, recuperar
                            effective_fps = min(target_fps, effective_fps + 1)
                            current_jpeg_quality = min(target_jpeg_quality, current_jpeg_quality + 2)
            except (BlockingIOError, socket.error):
                pass

            # 2. Captura de profundidad y color
            depth_data = None
            color_data = None
            w, h = args.width, args.height

            if camera_handle is not None and isinstance(camera_handle, tuple) and camera_handle[0] == "v4l2":
                v4l2_cap: V4L2DepthCapture = camera_handle[1]
                depth_bytes_raw = v4l2_cap.read_frame()
                if depth_bytes_raw is None:
                    time.sleep(0.01)
                    continue
                depth_data = depth_bytes_raw
            elif camera_handle is not None and isinstance(camera_handle, tuple) and camera_handle[0] == "rs":
                pipe = camera_handle[1]
                try:
                    frames = pipe.wait_for_frames(timeout_ms=1000)
                    if align:
                        aligned = align.process(frames)
                        depth_frame = aligned.get_depth_frame()
                        color_frame = aligned.get_color_frame()
                    else:
                        depth_frame = frames.get_depth_frame()
                        color_frame = frames.get_color_frame()

                    if not depth_frame:
                        continue

                    depth_data = np.asanyarray(depth_frame.get_data())
                    if color_frame:
                        color_data = np.asanyarray(color_frame.get_data())
                    h, w = depth_data.shape[:2]
                except Exception:
                    time.sleep(0.05)
                    continue
            else:
                # Generador sintético rápido de prueba si no hay hardware
                synthetic_t += 0.05
                if np is not None:
                    depth_data = (np.ones((h, w), dtype=np.uint16) * 1500)
                else:
                    depth_data = (b"\xDC\x05" * (w * h))
                color_data = None

            # 3. Empaquetado en RAM R3D1 (sin tocar microSD)
            color_bytes = b""
            depth_only = True

            if cv2 is not None and color_data is not None and isinstance(color_data, np.ndarray):
                try:
                    ok, jpeg_buf = cv2.imencode(
                        ".jpg",
                        color_data,
                        [int(cv2.IMWRITE_JPEG_QUALITY), int(current_jpeg_quality)]
                    )
                    if ok and len(jpeg_buf) > 0:
                        color_bytes = jpeg_buf.tobytes()
                        depth_only = False
                except Exception:
                    color_bytes = b""
                    depth_only = True

            # Compresión zlib nivel 1 de profundidad
            if isinstance(depth_data, np.ndarray):
                depth_raw = depth_data.tobytes()
            else:
                depth_raw = depth_data
            depth_compressed = zlib.compress(depth_raw, level=1)

            meta = {
                "ts": time.time(),
                "w": w,
                "h": h,
                "fx": float(intr.fx),
                "fy": float(intr.fy),
                "ppx": float(intr.ppx),
                "ppy": float(intr.ppy),
                "depth_scale": float(depth_scale),
                "imu_yaw": 0.0,
                "imu_pitch": 0.0,
                "imu_roll": 0.0,
                "depth_only": bool(depth_only),
            }
            meta_bytes = json.dumps(meta, separators=(",", ":")).encode("utf-8")

            # Construir payload binario R3D1 completo
            r3d1_payload = (
                _HEADER_STRUCT.pack(PACKET_MAGIC, len(meta_bytes))
                + meta_bytes
                + _LEN_STRUCT.pack(len(color_bytes))
                + color_bytes
                + _LEN_STRUCT.pack(len(depth_compressed))
                + depth_compressed
            )

            # 4. Fragmentación R3U1 y micro-pacing
            frame_id = (frame_id + 1) & 0xFFFFFFFF
            total_bytes = len(r3d1_payload)
            total_chunks = (total_bytes + CHUNK_SIZE - 1) // CHUNK_SIZE

            for chunk_idx in range(total_chunks):
                start = chunk_idx * CHUNK_SIZE
                end = min(start + CHUNK_SIZE, total_bytes)
                part = r3d1_payload[start:end]

                chunk_hdr = _UDP_CHUNK_STRUCT.pack(
                    UDP_CHUNK_MAGIC,
                    frame_id,
                    total_bytes,
                    chunk_idx,
                    total_chunks,
                    len(part),
                )
                packet = chunk_hdr + part
                try:
                    sock.sendto(packet, dest_addr)
                    bytes_sent += len(packet)
                except socket.error:
                    pass

                if pacing_s > 0:
                    time.sleep(pacing_s)

            frames_sent += 1

            # 5. Reporte periódico de consola cada 2 segundos
            now = time.time()
            if now - t0_stat >= 2.0:
                dt = now - t0_stat
                kbps = (bytes_sent * 8.0 / 1000.0) / dt
                fps_actual = frames_sent / dt
                print(
                    f"[STREAM_D415] Frame #{frame_id} | {fps_actual:.1f} FPS | "
                    f"{kbps:.1f} kbps | Frame={total_bytes/1024:.1f} KB ({total_chunks} chunks) | "
                    f"Pérdida Win: {last_reported_loss:.1f}% | JPEG Q: {current_jpeg_quality}"
                )
                frames_sent = 0
                bytes_sent = 0
                t0_stat = now

            # Control de tasa de cuadros
            target_period = 1.0 / max(1.0, float(effective_fps))
            elapsed = time.perf_counter() - t_start
            sleep_rem = target_period - elapsed
            if sleep_rem > 0:
                time.sleep(sleep_rem)

    finally:
        print("[STREAM_D415] Cerrando recursos...")
        if camera_handle is not None and isinstance(camera_handle, tuple):
            if camera_handle[0] == "v4l2":
                camera_handle[1].close()
            elif camera_handle[0] == "rs":
                try:
                    camera_handle[1].stop()
                except Exception:
                    pass
        sock.close()
        print("[STREAM_D415] Salida limpia finalizada.")


if __name__ == "__main__":
    main()
