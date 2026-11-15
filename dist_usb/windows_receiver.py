#!/usr/bin/env python3
"""
windows_receiver.py: Receptor en tiempo real para la máquina Windows
Recibe el flujo ToF retransmitido por la laptop Arch Linux (Gateway Relay).
Reensambla los paquetes fragmentados, descomprime las nubes de puntos / relieve
y los entrega al generador de rutas y mapas 3D.
"""

import socket
import struct
import zlib
import time
import sys

LISTEN_PORT = 6000
HEADER_FORMAT = "!IHHBBQ"
HEADER_SIZE = struct.calcsize(HEADER_FORMAT)

def run_receiver(port=LISTEN_PORT):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("0.0.0.0", port))
    sock.settimeout(1.0)

    print("=" * 72)
    print("🗺️ RECEPTOR DE MAPEO Y RUTAS 3D (WINDOWS)")
    print(f"   Escuchando flujo ToF retransmitido por Arch Linux en puerto: {port}")
    print("   Esperando nubes de puntos y mapas de relieve...")
    print("=" * 72)

    frames_in_progress = {} # frame_id -> {chunks: {}, total_chunks: N, timestamp: T}
    last_stat_time = time.time()
    frames_completed_sec = 0
    total_bytes_sec = 0

    try:
        while True:
            try:
                data, addr = sock.recvfrom(65535)
            except socket.timeout:
                continue

            total_bytes_sec += len(data)

            if len(data) < HEADER_SIZE:
                continue

            header_bytes = data[:HEADER_SIZE]
            chunk_data = data[HEADER_SIZE:]

            frame_id, chunk_idx, total_chunks, data_type, _, timestamp_ms = struct.unpack(
                HEADER_FORMAT, header_bytes
            )

            if frame_id not in frames_in_progress:
                frames_in_progress[frame_id] = {
                    "chunks": {},
                    "total_chunks": total_chunks,
                    "data_type": data_type,
                    "first_chunk_time": time.time(),
                }

            frame_entry = frames_in_progress[frame_id]
            frame_entry["chunks"][chunk_idx] = chunk_data

            # Comprobar si el frame está completo
            if len(frame_entry["chunks"]) == frame_entry["total_chunks"]:
                # Reensamblar en orden
                full_payload = bytearray()
                for i in range(frame_entry["total_chunks"]):
                    full_payload.extend(frame_entry["chunks"][i])

                # Descomprimir datos de nube de puntos / mapa
                try:
                    raw_data = zlib.decompress(full_payload)
                    latency = int(time.time() * 1000) - timestamp_ms
                    frames_completed_sec += 1

                    # ==============================================================
                    # AQUÍ SE CONECTA TU GENERADOR DE RUTAS Y MAPEO:
                    # Ej: numpy.frombuffer(raw_data, dtype=...) o nube de puntos
                    # ==============================================================
                    # print(f"Frame #{frame_id} listo: {len(raw_data)} bytes | Latencia: {latency} ms")

                except Exception as e:
                    pass

                del frames_in_progress[frame_id]

            # Limpiar frames incompletos con más de 1 segundo de antigüedad (descartados por pérdida Wi-Fi)
            now = time.time()
            stale_keys = [
                k for k, v in frames_in_progress.items() if now - v["first_chunk_time"] > 1.0
            ]
            for k in stale_keys:
                del frames_in_progress[k]

            # Estadísticas cada 1 segundo
            if now - last_stat_time >= 1.0:
                kbps = (total_bytes_sec / 1024.0) / (now - last_stat_time)
                print(
                    f"[WINDOWS 3D MAP] FPS: {frames_completed_sec:<3} | Tasa: {kbps:>6.1f} KB/s | "
                    f"Frames recibidos correctamente desde Arch Linux"
                )
                frames_completed_sec = 0
                total_bytes_sec = 0
                last_stat_time = now

    except KeyboardInterrupt:
        print("\nReceptor finalizado.")
    finally:
        sock.close()

if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else LISTEN_PORT
    run_receiver(port)
