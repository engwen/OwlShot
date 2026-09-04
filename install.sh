#!/bin/bash
# 安装 owlshot：直接 dpkg 安装 deb 包
# 用法：bash install.sh
# 需要先执行 `make -f debian/rules package` 生成 deb 包。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEB_PKG="${SCRIPT_DIR}/owlshot_0.1.0-1_amd64.deb"

if [ ! -f "${DEB_PKG}" ]; then
    echo "[install] 找不到 deb 包：${DEB_PKG}"
    echo "[install] 请先执行：make -f debian/rules package"
    exit 1
fi

echo "[install] 安装 ${DEB_PKG} ..."
sudo dpkg -i "${DEB_PKG}"

echo "[install] 刷新图标缓存与桌面数据库..."
gtk-update-icon-cache /usr/share/icons/hicolor/128x128/apps/ 2>/dev/null || true
update-desktop-database /usr/share/applications/ 2>/dev/null || true

echo "[install] 安装完成！"
echo "    二进制：/usr/bin/owlshot"
echo "    图标：  /usr/share/icons/hicolor/128x128/apps/owlshot.png"
echo "    桌面项：/usr/share/applications/owlshot.desktop"
echo "    在程序列表中搜索 \"Owlshot\" 即可找到。"
