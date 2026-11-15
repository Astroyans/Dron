#!/usr/bin/env bash
# ==============================================================================
# INSTALADOR UNIVERSAL Y AUTOMÁTICO DE DRON-LINK PARA RASPBERRY PI
# Diseñado para funcionar con cualquier usuario (pi, jaguar, admin, etc.)
# ==============================================================================
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Detectar usuario real (incluso si se ejecutó con sudo)
if [ -n "$SUDO_USER" ]; then
    REAL_USER="$SUDO_USER"
else
    REAL_USER="$USER"
fi

REAL_HOME=$(eval echo "~${REAL_USER}")

echo "========================================================================"
echo "🚁 INSTALACIÓN AUTOMÁTICA DE DRON-LINK EN RASPBERRY PI"
echo "   Usuario objetivo:  ${REAL_USER}"
echo "   Directorio Home:   ${REAL_HOME}"
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

# 1. Asegurar compatibilidad con /home/pi
echo "📁 [1/6] Verificando compatibilidad de directorios..."
if [ ! -d "/home/pi" ]; then
    echo "   -> /home/pi no existe físicamente en este sistema."
    if [ "$REAL_USER" != "pi" ]; then
        echo "   -> Creando enlace simbólico /home/pi -> ${REAL_HOME} para máxima compatibilidad..."
        run_elevated ln -sfn "${REAL_HOME}" /home/pi 2>/dev/null || {
            echo "   -> Creando directorio /home/pi con permisos para ${REAL_USER}..."
            run_elevated mkdir -p /home/pi
            run_elevated chown -R "${REAL_USER}:${REAL_USER}" /home/pi 2>/dev/null || true
        }
    else
        run_elevated mkdir -p /home/pi
    fi
fi

# 2. Desactivar modo de ahorro de energía Wi-Fi (Elimina picos de 200 ms - 8000 ms)
echo "📶 [2/6] Desactivando ahorro de energía Wi-Fi en wlan0..."
if command -v iw >/dev/null 2>&1; then
    run_elevated iw dev wlan0 set power_save off 2>/dev/null || true
elif command -v iwconfig >/dev/null 2>&1; then
    run_elevated iwconfig wlan0 power off 2>/dev/null || true
fi

if [ -f "/etc/rc.local" ]; then
    if ! grep -q "power_save off" /etc/rc.local 2>/dev/null; then
        run_elevated sed -i -e '$i \iw dev wlan0 set power_save off 2>/dev/null || true\n' /etc/rc.local 2>/dev/null || true
    fi
fi

# 3. Copiar binario nodo_dron a múltiples ubicaciones seguras
echo "📦 [3/6] Instalando binario ejecutable 'nodo_dron'..."

# Detener proceso previo si corre
run_elevated systemctl stop dron-link 2>/dev/null || true
pkill -f nodo_dron 2>/dev/null || true

cp -f "${SCRIPT_DIR}/nodo_dron" "${REAL_HOME}/nodo_dron"
chmod +x "${REAL_HOME}/nodo_dron"
echo "   ✓ Copiado a: ${REAL_HOME}/nodo_dron"

if [ "${REAL_HOME}" != "/home/pi" ] && [ -d "/home/pi" ] && [ ! -L "/home/pi" ]; then
    run_elevated cp -f "${SCRIPT_DIR}/nodo_dron" "/home/pi/nodo_dron" 2>/dev/null || true
    run_elevated chmod +x "/home/pi/nodo_dron" 2>/dev/null || true
    echo "   ✓ Copiado a: /home/pi/nodo_dron"
fi

if run_elevated cp -f "${SCRIPT_DIR}/nodo_dron" /usr/local/bin/nodo_dron 2>/dev/null; then
    run_elevated chmod +x /usr/local/bin/nodo_dron
    echo "   ✓ Instalado en comando global: /usr/local/bin/nodo_dron"
fi

# 4. Crear carpetas requeridas para captura ToF
echo "📂 [4/6] Creando directorios para capturas ToF..."
mkdir -p "${REAL_HOME}/tof/capturas_relieve"
echo "   ✓ Directorio listo: ${REAL_HOME}/tof/capturas_relieve"

if [ -d "/home/pi/tof" ] || [ -L "/home/pi" ]; then
    mkdir -p "/home/pi/tof/capturas_relieve" 2>/dev/null || true
fi

# 5. Configurar servicio systemd dinámico adaptado al entorno
echo "⚙️ [5/6] Configurando servicio systemd 'dron-link.service'..."
SERVICE_PATH="/etc/systemd/system/dron-link.service"

SERVICE_CONTENT="[Unit]
Description=Dron-Link: Demonio Embebido ToF (Intel RealSense D415)
After=network.target network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${REAL_USER}
WorkingDirectory=${REAL_HOME}
ExecStartPre=-/usr/sbin/iw dev wlan0 set power_save off
ExecStart=/usr/local/bin/nodo_dron --udp-port 5000 --tcp-port 5001 --script-path ${REAL_HOME}/tof/main.py --python-bin python3 --data-dir ${REAL_HOME}/tof/capturas_relieve --iface wlan0
Restart=always
RestartSec=3
KillMode=mixed
TimeoutStopSec=10
Environment=PYTHONUNBUFFERED=1

[Install]
WantedBy=multi-user.target"

if run_elevated bash -c "echo '${SERVICE_CONTENT}' > ${SERVICE_PATH}" 2>/dev/null; then
    run_elevated systemctl daemon-reload
    echo "   ✓ Servicio systemd configurado exitosamente en ${SERVICE_PATH}"
fi

# 6. Ajustar permisos finales
echo "🔒 [6/6] Ajustando permisos de usuario..."
run_elevated chown -R "${REAL_USER}:${REAL_USER}" "${REAL_HOME}/nodo_dron" "${REAL_HOME}/tof" 2>/dev/null || true

echo ""
echo "========================================================================"
echo "✨ INSTALACIÓN Y OPTIMIZACIÓN COMPLETADA CON ÉXITO"
echo "========================================================================"
echo ""
echo "🚀 Para ejecutar en PRIMER PLANO (Fase 1: Range Test con tu laptop):"
echo "   nodo_dron --iface wlan0"
echo ""
echo "🔋 Para activar ARRANQUE AUTOMÁTICO al encender la batería del dron:"
echo "   sudo systemctl enable --now dron-link"
echo "========================================================================"
