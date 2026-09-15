#!/bin/sh
# DSH Launch Console — Linux 一键安装（唯一入口）
# 用法: ./install.sh [安装目录]
# 流程：选路径 -> 装依赖 -> 编译（产物直接写入安装目录）-> 生成启动项与卸载脚本
set -e
cd "$(dirname "$0")"

# ============ 第一步：指定安装路径 ============
INSTDIR="$1"
if [ -z "$INSTDIR" ]; then
  if command -v zenity >/dev/null 2>&1; then
    echo "正在打开安装目录选择框，请选择安装位置..."
    INSTDIR=$(zenity --file-selection --directory --title="请选择 DSH Launch Console 的安装目录" 2>/dev/null || true)
  fi
  if [ -z "$INSTDIR" ]; then
    printf "安装路径（直接回车使用默认 %s/.local/share/dsh-launch-console，输入 q 取消）：" "$HOME"
    read INSTDIR
    [ "$INSTDIR" = "q" ] && { echo "已取消安装。"; exit 0; }
  fi
  [ -z "$INSTDIR" ] && INSTDIR="$HOME/.local/share/dsh-launch-console"
fi
echo "安装位置：$INSTDIR"
mkdir -p "$INSTDIR"

# ============ 第二步：编译依赖（Debian/Ubuntu 系） ============
if command -v apt-get >/dev/null 2>&1 && ! pkg-config --exists webkit2gtk-4.1; then
  echo "==> 安装编译依赖（需要 sudo；其他发行版请自行安装等价包）"
  sudo apt-get update
  sudo apt-get install -y build-essential pkg-config \
    libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev libssl-dev curl
fi

# ============ 第三步：编译，构建产物直接写入安装目录 ============
echo "==> 编译（首次约几分钟），构建产物直接写入 $INSTDIR/target ..."
cargo build --release --target-dir "$INSTDIR/target"
cp "$INSTDIR/target/release/dsh-launch-console" "$INSTDIR/dsh-launch-console"
cp icon.svg "$INSTDIR/"
echo "程序已生成到安装目录。"

# ============ 第四步：应用菜单启动项 ============
mkdir -p "$HOME/.local/bin" \
         "$HOME/.local/share/applications" \
         "$HOME/.local/share/icons/hicolor/scalable/apps"
ln -sf "$INSTDIR/dsh-launch-console" "$HOME/.local/bin/dsh-launch-console"
cp icon.svg "$HOME/.local/share/icons/hicolor/scalable/apps/dsh-launch-console.svg"
cat > "$HOME/.local/share/applications/dsh-launch-console.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=DSH Launch Console
Comment=DSH 微浏览器
Exec="$INSTDIR/dsh-launch-console"
Icon=dsh-launch-console
Terminal=false
Categories=Development;Utility;
EOF

# ============ 第五步：在安装目录现场生成卸载脚本 ============
cat > "$INSTDIR/uninstall.sh" <<EOF
#!/bin/sh
# 卸载 DSH Launch Console
set -e
echo "正在卸载 DSH Launch Console ..."
rm -f "\$HOME/.local/bin/dsh-launch-console"
rm -f "\$HOME/.local/share/applications/dsh-launch-console.desktop"
rm -f "\$HOME/.local/share/icons/hicolor/scalable/apps/dsh-launch-console.svg"
rm -rf "$INSTDIR"
echo "卸载完成。"
EOF
chmod +x "$INSTDIR/uninstall.sh"

echo "安装完成！"
echo "  位置：$INSTDIR"
echo "  启动：应用菜单搜索 DSH Launch Console，或运行 dsh-launch-console"
echo "  卸载：运行 $INSTDIR/uninstall.sh（会连同 target 目录一起删除）"
