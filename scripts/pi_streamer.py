#!/usr/bin/env python3
"""
pi_streamer.py: Transmisor en tiempo real de Intel RealSense D415 para Raspberry Pi
Fragmenta y envía nubes de puntos / mapas de profundidad por UDP tolerante a pérdidas
hacia la Estación de Tierra (Arch Linux) en 192.168.4.1:5005.
"""

import time
import socket
import struct
import os
import sys
import zlib
import json

TARGET_IP = "192.168.4.1"
TARGET_PORT = 5005
CHUNK_SIZE = 1200 # Bytes seguros bajo MTU para evitar fragmentación IP
LOCAL_BACKUP_DIR = "/home/pi/tof/capturas_relieve"

# Intentar importar pyrealsense2, si no existe entra en modo simulación
try:
    import pyrealsense2 as rs
    import numpy as np
    HAVE_REALSENSE = True
except ImportError:
    HAVE_REALSENSE = False

class RealSenseStreamer:
    def __init__(self, target_ip=TARGET_IP, target_port=TARGET_PORT):
        self.target_ip = target_ip
        self.target_port = target_port
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.frame_id = 0
        os.makedirs(LOCAL_BACKUP_DIR, exist_ok=True)

        if HAVE_REALSENSE:
            print("[STREAMER] Inicializando hardware Intel RealSense D415...")
            self.pipeline = rs.pipeline()
            config = rs.config()
            # 640x360 a 15 fps optimizado para enlace Wi-Fi de largo alcance
            config.enable_stream(rs.stream.depth, 640, 360, rs.format.z16, 15)
            config.enable_stream(rs.stream.color, 640, 360, rs.format.bgr8, 15)
            self.pipeline.start(config)
            self.pointcloud = rs.pointcloud()
            print("[STREAMER] Cámara lista. Transmitiendo en 640x360@15fps...")
        else:
            print("[STREAMER] Modo SIMULACIÓN activo (pyrealsense2 no disponible en host).")

    def get_frame_data(self):
        """Obtiene datos de profundidad y nubes de puntos comprimidos"""
        if HAVE_REALSENSE:
            frames = self.pipeline.wait_for_frames()
            depth_frame = frames.get_depth_frame()
            if not depth_frame:
                return None
            
            # Obtener puntos 3D decimados para transmisión fluida
            points = self.pointcloud.calculate(depth_frame)
            vtx = np.asanyarray(points.get_vertices())
            # Submuestrear a 10,000 puntos para optimizar ancho de banda
            vtx_sub = vtx[::10]
            raw_bytes = vtx_sub.tobytes()
        else:
            # Datos sintéticos de relieve para pruebas
            raw_bytes = os.urandom(1024 * 32) # 32 KB de datos de prueba

        # Compresión zlib rápida para reducir el tamaño al 40%
        compressed = zlib.compress(raw_bytes, level=1)
        return compressed

    def send_frame(self, data_type, payload):
        """Fragmenta y envía el frame en datagramas MTU-safe"""
        self.frame_id = (self.frame_id + 1) % 0xFFFFFFFF
        total_len = len(payload)
        total_chunks = (total_len + CHUNK_SIZE - 1) // CHUNK_SIZE

        timestamp_ms = int(time.time() * 1000)

        for chunk_idx in range(total_chunks):
            start = chunk_idx * CHUNK_SIZE
            end = min(start + CHUNK_SIZE, total_len)
            chunk_data = payload[start:end]

            # Encabezado binario compacto (16 bytes):
            # [frame_id: u32, chunk_idx: u16, total_chunks: u16, data_type: u8, pad: u8, timestamp: u64]
            header = struct.pack(
                "!IHHBBQ",
                self.frame_id,
                chunk_idx,
                total_chunks,
                data_type,
                0, # padding
                timestamp_ms
            )

            packet = header + chunk_data
            self.sock.sendto(packet, (self.target_ip, self.target_port))

    def run(self, fps=10):
        interval = 1.0 / fps
        print(f"[STREAMER] Enviando flujo continuo hacia {self.target_ip}:{self.target_port} a {fps} FPS...")
        try:
            while True:
                start_time = time.time()
                payload = self.get_frame_data()
                if payload:
                    # Tipo 1: PointCloudXYZ
                    self.send_frame(data_type=1, payload=payload)

                elapsed = time.time() - start_time
                sleep_time = max(0, interval - elapsed)
                time.sleep(sleep_time)
        except KeyboardInterrupt:
            print("\n[STREAMER] Transmisión detenida por usuario.")
        finally:
            if HAVE_REALSENSE:
                self.pipeline.stop()

if __name__ == "__main__":
    ip = sys.argv[1] if len(sys.argv) > 1 else TARGET_IP
    streamer = RealSenseStreamer(target_ip=ip)
    streamer.run(fps=10)
