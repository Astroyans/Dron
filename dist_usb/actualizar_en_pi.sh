#!/usr/bin/env bash
# ==============================================================================
# SCRIPT DE ACTUALIZACIÓN AUTOMÁTICA EN RASPBERRY PI (DRON-LINK TOF / REAL-TIME)
# ==============================================================================
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Detectar usuario real
if [ -n "$SUDO_USER" ]; then
    REAL_USER="$SUDO_USER"
elif [ "$USER" != "root" ]; then
    REAL_USER="$USER"
else
    if id "pi" >/dev/null 2>&1; then
        REAL_USER="pi"
    else
        REAL_USER="root"
    fi
fi

REAL_HOME=$(eval echo "~${REAL_USER}")

echo "========================================================================"
echo "⚡ ACTUALIZACIÓN DE NODO_DRON Y STREAMING D415 (ETAPA 2: REAL-TIME)"
echo "   Usuario detectado:   ${REAL_USER}"
echo "   Directorio base:     ${REAL_HOME}"
echo "   Origen (USB/Medio):  ${SCRIPT_DIR}"
echo "========================================================================"

run_elevated() {
    if [ "$EUID" -eq 0 ]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        "$@"
    fi
}

# 1. Detener servicios y matar instancias previas (previene 'Text file busy')
echo "🛑 [1/5] Deteniendo servicio previo y liberando ejecutables..."
run_elevated systemctl stop dron-link 2>/dev/null || true
run_elevated pkill -9 -f nodo_dron 2>/dev/null || true
run_elevated pkill -9 -f stream_d415.py 2>/dev/null || true
run_elevated pkill -9 -f "tof/main.py" 2>/dev/null || true
sleep 1
echo "   ✓ Procesos liberados correctamente."

# 2. Respaldo de seguridad del binario anterior
echo "🛡️  [2/5] Realizando respaldo de la versión anterior..."
if [ -f "${REAL_HOME}/nodo_dron" ]; then
    cp -f "${REAL_HOME}/nodo_dron" "${REAL_HOME}/nodo_dron.bak"
    echo "   ✓ Respaldo creado en: ${REAL_HOME}/nodo_dron.bak"
elif [ -f "/usr/local/bin/nodo_dron" ]; then
    run_elevated cp -f "/usr/local/bin/nodo_dron" "/usr/local/bin/nodo_dron.bak"
    echo "   ✓ Respaldo creado en: /usr/local/bin/nodo_dron.bak"
fi

# 3. Instalar nuevo binario nodo_dron (ARM64 release)
echo "📦 [3/5] Desplegando nuevo binario nodo_dron (ARM64)..."
cp -f "${SCRIPT_DIR}/nodo_dron" "${REAL_HOME}/nodo_dron"
chmod +x "${REAL_HOME}/nodo_dron"
chown "${REAL_USER}:${REAL_USER}" "${REAL_HOME}/nodo_dron" 2>/dev/null || true
echo "   ✓ Instalado en: ${REAL_HOME}/nodo_dron"

if [ -d "/home/pi" ] && [ "${REAL_HOME}" != "/home/pi" ]; then
    run_elevated cp -f "${SCRIPT_DIR}/nodo_dron" "/home/pi/nodo_dron" 2>/dev/null || true
    run_elevated chmod +x "/home/pi/nodo_dron" 2>/dev/null || true
    echo "   ✓ Copiado de compatibilidad: /home/pi/nodo_dron"
fi

if run_elevated cp -f "${SCRIPT_DIR}/nodo_dron" /usr/local/bin/nodo_dron 2>/dev/null; then
    run_elevated chmod +x /usr/local/bin/nodo_dron
    echo "   ✓ Instalado en sistema: /usr/local/bin/nodo_dron"
fi

# 4. Instalar script de streaming RealSense D415 (stream_d415.py)
echo "🎥 [4/5] Desplegando script de streaming ToF/RGB-D en tiempo real..."
mkdir -p "${REAL_HOME}/tof"
cp -f "${SCRIPT_DIR}/stream_d415.py" "${REAL_HOME}/tof/stream_d415.py"
chmod +x "${REAL_HOME}/tof/stream_d415.py"
chown -R "${REAL_USER}:${REAL_USER}" "${REAL_HOME}/tof" 2>/dev/null || true
echo "   ✓ Instalado en: ${REAL_HOME}/tof/stream_d415.py"

if [ -d "/home/pi/tof" ] && [ "${REAL_HOME}" != "/home/pi" ]; then
    run_elevated cp -f "${SCRIPT_DIR}/stream_d415.py" "/home/pi/tof/stream_d415.py" 2>/dev/null || true
    run_elevated chmod +x "/home/pi/tof/stream_d415.py" 2>/dev/null || true
    echo "   ✓ Copiado de compatibilidad: /home/pi/tof/stream_d415.py"
fi

# 5. Asegurar optimización Wi-Fi (Power Save OFF) y configurar dron-link.service
echo "⚙️  [5/5] Reconfigurando servicio dron-link.service con soporte Streaming..."
SERVICE_PATH="/etc/systemd/system/dron-link.service"

SERVICE_CONTENT="[Unit]
Description=Dron-Link: Demonio Embebido ToF y Streaming Real-Time (Intel RealSense D415)
After=network.target network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${REAL_USER}
WorkingDirectory=${REAL_HOME}
ExecStartPre=-/usr/sbin/iw dev wlan0 set power_save off
ExecStart=/usr/local/bin/nodo_dron --udp-port 5000 --tcp-port 5001 --script-path ${REAL_HOME}/tof/main.py --stream-script-path ${REAL_HOME}/tof/stream_d415.py --stream-target 192.168.4.1:8766 --python-bin python3 --data-dir ${REAL_HOME}/tof/capturas_relieve --iface wlan0
Restart=always
RestartSec=3
KillMode=mixed
TimeoutStopSec=10
Environment=PYTHONUNBUFFERED=1

[Install]
WantedBy=multi-user.target
"

run_elevated bash -c "cat <<'EOF' > ${SERVICE_PATH}
${SERVICE_CONTENT}
EOF"

run_elevated systemctl daemon-reload
run_elevated systemctl enable dron-link 2>/dev/null || true
run_elevated systemctl restart dron-link

sleep 1.5

echo ""
echo "========================================================================"
if systemctl is-active --quiet dron-link; then
    echo "✅ ÉXITO: El servicio dron-link está ACTIVO y corriendo en segundo plano."
else
    echo "⚠️ AVISO: El servicio intentó arrancar pero su estado actual es:"
fi
systemctl status dron-link --no-pager -n 8 || true
echo "========================================================================"
echo "Versión del binario instalada:"
${REAL_HOME}/nodo_dron --version || /usr/local/bin/nodo_dron --version || true
echo "========================================================================"
