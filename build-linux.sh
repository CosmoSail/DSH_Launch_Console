#!/bin/sh
# DSH Launch Console — Linux 构建与打包
# 产出：target/release/dsh-launch-console 二进制
#       Output/dsh-launch-console_0.2.0_amd64.deb 安装包
set -e
cd "$(dirname "$0")"
VERSION=0.2.0

# 1. 编译依赖（Debian/Ubuntu 系；其他发行版请自行安装等价包）
# 0.2.0 起为原生 GUI（egui/wgpu），不再需要 webkit2gtk；
# 需要 X11/Wayland 开发头与 OpenGL。
if command -v apt-get >/dev/null 2>&1; then
  echo "==> 安装编译依赖（需要 sudo）"
  sudo apt-get update
  sudo apt-get install -y build-essential pkg-config \
    libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
    libxkbcommon-dev libwayland-dev libgl1-mesa-dev \
    libssl-dev curl
fi

# 2. 编译
echo "==> cargo build --release"
cargo build --release
BIN="target/release/dsh-launch-console"

# 3. 组装 .deb 安装包
echo "==> 组装 .deb 安装包"
mkdir -p Output
DEBROOT="target/debroot"
rm -rf "$DEBROOT"
mkdir -p "$DEBROOT/DEBIAN" \
         "$DEBROOT/usr/bin" \
         "$DEBROOT/usr/share/applications" \
         "$DEBROOT/usr/share/icons/hicolor/scalable/apps"
cp "$BIN" "$DEBROOT/usr/bin/dsh-launch-console"
cp icon.svg "$DEBROOT/usr/share/icons/hicolor/scalable/apps/dsh-launch-console.svg"
cat > "$DEBROOT/DEBIAN/control" <<EOF
Package: dsh-launch-console
Version: $VERSION
Section: utils
Priority: optional
Architecture: amd64
Depends: libx11-6, libxkbcommon0, libgl1
Maintainer: DSH
Description: DSH Launch Console
  DeepSeek Harness 启动器: native GUI for DSH start/stop/versions/plugins; Web UI opens in the system browser.
EOF
cat > "$DEBROOT/usr/share/applications/dsh-launch-console.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=DSH Launch Console
Comment=DeepSeek Harness 启动器
Exec=dsh-launch-console
Icon=dsh-launch-console
Terminal=false
Categories=Development;Utility;
EOF
dpkg-deb --build "$DEBROOT" "Output/dsh-launch-console_${VERSION}_amd64.deb"

echo "==> 完成"
echo "  二进制: $BIN"
echo "  安装包: Output/dsh-launch-console_${VERSION}_amd64.deb"
echo "  一键安装: ./install.sh（先选路径，编译产物直接写入安装目录）"
