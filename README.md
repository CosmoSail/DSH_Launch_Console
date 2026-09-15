# DSH Launch Console

DSH Launch Console 用 Rust 编写：内置微浏览器只访问写死的 DSH
地址，编译产物为**单个可执行文件**，双击即可运行，全程无终端窗口。

同一套源码按目标平台产出**两个工具集版本**：

| 版本 | 平台 | DSH 工具支持 |
| --- | --- | --- |
| **Windows 版** | Windows | 支持 Windows 原生工具链：read / write / pwsh / grep / glob / web_search / subagent 等全部可用；`bash`（Git Bash）需要会话权限为**完全访问**（见下节「Windows 版工具说明」） |
| **Linux/darwin 版** | Linux、macOS | 原生启动 DSH，**全部 POSIX 工具（bash、str_replace_editor 等）可用** |

## 与 DeepSeek Harness 的关系

DeepSeek Harness（DSH）与 DSH Launch Console 是**两个各自独立安装的程序**：

- **DSH 本体**：正常按照官网 <https://www.deepseek.com/harness/> 安装即可
  （装好 Node.js 后 `npx @deepseek-ai/dsh web` 就能启动 Web UI）。
- **DSH Launch Console**：**单独安装、安装即用**——它只负责「启动 DSH + 开一个
  锁定的窗口」。即使没有全局装过 DSH，启动器也会回退到与官网相同的
  `npx -y @deepseek-ai/dsh web` 就地取得并启动，无需另行配置。

## 获取

**方式一：下载安装包（推荐给最终用户）**
到 [Releases](../../releases) 下载 `DSH_Launch_Console-Setup-<版本>.exe`，双击安装。
按用户安装、无需管理员权限。

**方式二：从源码构建（开发者）**
本仓库**只存源码**。clone 后按「构建」一节编译，或直接跑 `安装.bat`
（Windows）/ `install.sh`（Linux、macOS）完成「编译 + 安装」一步到位。

> 仓库中不包含编译产物与打包产物：`target/`（约 1.3 GB）、`DSH_Launch_Console.exe`、
> `Output/` 均已被 `.gitignore` 排除，分发包请从 Releases 获取。

## 特性

- **单文件、双击即用**：编译产物是一个可执行文件，拷到任何地方都能跑，只需
  系统自带的 Edge WebView2 运行时；在进程内解析全局 dsh 包入口并直连 node
  启动，比 `npx` 快 **1.6 秒以上**，解析不到时自动回退。
- **窗口秒开**：不等 DSH 就绪就先显示带秒数计时的「DSH 启动中…」占位页，
  token 一到位无缝切进真实界面；热启动（cookie 有效或 token 已就绪）连
  占位页都不显示。
- **窗口锁定，该放的放**：导航白名单只放行写死在源码里的 DSH 地址
  （`src/main.rs` 顶部 `DSH_URL`）并禁用开发者工具；外部链接转交系统默认
  浏览器，同源弹窗共享会话（不再撞 401）；通知、剪贴板、下载、文件读写、
  自动播放等权限自动放行，麦克风/摄像头/定位一律拒绝。
- **托盘常驻 + 单实例**：同一时间只有一个窗口、一个托盘图标，重复双击只把
  已有窗口拉到前台；✕ 默认收进托盘让 DSH 继续后台运行。
- **退出即清理，且不误杀**：真正退出时结束由本程序启动的 DSH 进程树；若
  DSH 是你自己在终端里启动的（无登记表），只开窗口、退出时不杀它。

## 使用

Windows 下双击 **`安装.bat`** 一个入口搞定（源码不含编译产物）：

1. **选择安装路径**：弹出文件夹选择框点选，或把目标文件夹
   **拖拽到 `安装.bat` 上**，或命令行 `安装.bat "D:\自定义路径"`；
   取消选择框后可手动输入（回车用默认
   `%LOCALAPPDATA%\Programs\DSH Launch Console`，输入 q 取消）。
2. **编译并直接生成到安装目录**：自动执行 `cargo build --release
   --target-dir "<安装目录>\target"`（需要 Rust MSVC 工具链，首次约几分钟），
   **构建产物（含 target 目录）全部落在安装目录内**，包文件夹全程保持
   纯源码状态、无任何需要清理的中间文件；程序复制到安装目录根，
   并自动生成安装根目录的卸载脚本、开始菜单快捷方式与控制面板卸载条目，
   全程无需管理员权限。
3. **便携使用**：程序是单文件，安装目录里的
   `DSH_Launch_Console.exe` 拷贝到任何地方都能直接运行。
4. **卸载**：**安装根目录里的 `uninstall.bat`**（安装时自动生成，
   会连同 target 目录一起删除），或控制面板「应用」。
5. **打包分发**：先双击 `安装.bat` 完成一次编译安装，把安装目录里的
   `DSH_Launch_Console.exe` 复制回本文件夹根目录（Setup.exe 打包需要它），
   然后双击 `package.bat`，一次产出三个分发包：
   - Windows 安装向导 `Output\DSH_Launch_Console-Setup-0.1.0.exe`
     （需要 Inno Setup 便携编译器位于 `..\.innosetup\`，或系统已安装 Inno Setup）
   - Windows 源码包 `Output\DSH_Launch_Console-0.1.0-windows-src.zip`
   - Linux/macOS 源码包 `Output\DSH_Launch_Console-0.1.0-linux.tar.gz`

依赖系统自带 **Microsoft Edge WebView2 运行时**（Win10/11 一般已内置；
缺失时程序会弹窗提示，装一下即可）。

## Windows 版工具说明（重要）

`bash` 类工具在 Windows 上执行时（无论 Edge 还是本浏览器，规则一致）需要
DSH 会话的**完全访问权限**，否则会被沙箱拦截：

- 系统默认 `bash`（WSL 启动器）：WSL 未安装发行版时报
  `Wsl/EnumerateDistros/Service/E_ACCESSDENIED`；
- Git Bash（`C:\Program Files\Git\bin\bash.exe`）：受限沙箱禁止命名管道，
  报 `couldn't create signal pipe, Win32 error 5`；
- stock `minimal` / `standard` 预设的持久 bash 依赖仅支持 linux/darwin 的
  PTY 后端，在 win32 上会报
  `terminal inspection is unsupported on platform win32`。

**解决方式（DSH 界面内操作，与浏览器无关）**：在 DSH 的权限设置里把会话
权限预设设为 **danger-full-access（完全访问）**，之后 `bash` 可直接执行；
保持受限模式时，每次调用都会弹出一次"升级到完全访问"的审批——点允许即
成功、拒绝则失败。

## Linux / macOS

同一套源码同时支持 Linux 与 macOS（`#[cfg]` 按平台实现，Linux/darwin
分发包见 `Output\DSH_Launch_Console-0.1.0-linux.tar.gz`）：

- **一键安装**：`./install.sh [安装目录]` —— 先选路径（zenity 目录选择框 /
  参数指定 / 手动输入），自动装编译依赖（Debian/Ubuntu 系），
  `cargo build --release --target-dir "<安装目录>/target"` **构建产物全部
  落在安装目录内**，包文件夹保持纯源码；自动生成应用菜单启动项与
  安装目录内的 `uninstall.sh`（连 target 一起删除）。
- **打包 .deb**：`./build-linux.sh` —— 编译并组装
  `.deb` 安装包（装到 /usr/bin + 应用菜单，卸载
  `dpkg -r dsh-launch-console`）。
- 关闭联动：**关窗即退出并结束由本程序启动的 DSH**；`killpg`
  （SIGTERM→SIGKILL）+ ss/lsof 端口兜底。启动器崩溃时 DSH 会残留，
  由下次启动自动收养（Linux/macOS 同属此限制）；错误弹窗 macOS 用
  osascript、Linux 用 zenity。**单实例同样生效**：重复启动只把已有窗口
  拉回前台（flock + SIGUSR1）。
- **系统托盘暂为 Windows 版特性**：Linux/macOS 版关闭按钮仍直接退出
  （无托盘常驻）；托盘「关闭=后台常驻」后续再移植。

## 构建

```bat
:: 双击 安装.bat（自动编译+安装），或只想编译时手动：
cd /d <本文件夹>
cargo build --release
:: 产物: target\release\dsh-launch-console.exe
::（编译期已嵌入鲸鱼图标；复制为 DSH_Launch_Console.exe 即可便携运行）
```

需要 Rust 工具链（MSVC）+ Windows。首次构建会下载约 250 个依赖包。

## 开发与测试

```bash
# 单元测试：URL 白名单、token 解析、shim 解析、日志轮转、失败分类等
cargo test

# 只解析「会怎么启动 dsh」，不启动任何进程（排障首选）
./target/release/dsh-launch-console --print-launch-plan
```

`tests/` 目录下另有一组 Windows 实机验证脚本（启动链、托盘、权限、同源弹窗、
启动耗时对照等），用法与实测结论见 [`tests/README.md`](tests/README.md)。

## 日志

日志**实时追加写盘**（每条消息立即落盘，不缓冲）；文件集合固定，不随启动
次数增加。体积受 `LOG_MAX_BYTES`（默认 1 MiB，可用 `DSH_LAUNCH_CONSOLE_LOGMAX`
覆盖）控制，不会无限膨胀：

- 启动器：`%TEMP%\DSH-Launch-Console.log` —— 超限时**截头保留最近一半**
  （对齐行边界，最近的记录永远可查）
- npx/DSH 输出：`%TEMP%\DSH-Launch-Console-server.log` —— 超限时在**下一次全新
  启动**前轮转为 `DSH-Launch-Console-server.log.old`（会话运行期间文件被 cmd
  持有，不做改动）
- DSH 自写日志：`%TEMP%\DSH-Launch-Console-webview.log` —— 同样在全新启动前
  按上限轮转为 `.old`
- 状态目录：`%LOCALAPPDATA%\DSH-Launch-Console`（owner 记录 + 隐藏启动脚本，
  owner 关窗时删除；`run-dsh.cmd` 每次覆盖）
- 启动时会顺手清理可能残留的 `%TEMP%\dsh-netstat.tmp`（杀树时的临时文件）

## 常见问题

- **更新 DSH 后窗口提示“找不到此 127.0.0.1 页”**：新版 DSH 的 Web UI
  要求 token 鉴权。浏览器自己启动 DSH 时会自动从服务日志解析**本次运行**的
  token 并换发签名 cookie（持久保存在 WebView 里）；终端手动启动的 DSH
  靠已保存的 cookie 直接打开。若看到 DSH 的 401 提示页，说明 cookie 尚
  不存在——让浏览器**自己启动一次 DSH**（托盘菜单「关闭」后重新双击）
  即可，之后两种启动方式都能直接用。
- **双击后窗口没出现**：窗口现在**立刻**弹出并显示“DSH 启动中…（秒数）”，
  首次 npx 下载 + web profile 初始化较慢（十几秒到一分钟）；失败时弹窗会
  给出**分类原因**（未找到 dsh 入口 / 端口被占 / npm 网络 / 权限 / 依赖缺失）
  和日志里第一条错误线索。想更快就全局装一次 `npm i -g @deepseek-ai/dsh`，
  启动器会在进程内解析它的 JS 入口并直接 `node …` 启动（省掉 npx 的 1.6 秒
  与 shell 层的 ~80ms）。
- **启动器会怎么启动 dsh？**：跑一次 `DSH_Launch_Console.exe --print-launch-plan`，
  它只解析不启动，会把 node 路径、包入口、解析来源、目标 host/port 写进
  `%TEMP%\DSH-Launch-Console.log`（Windows 下若从终端运行也会直接打印到该终端）。
- **插件的桌面通知不弹**：通知权限现在由启动器自动放行（通知、剪贴板、
  下载、文件读写、自动播放均放行，麦克风/摄像头/定位一律拒绝）。若仍不弹，
  检查插件自身的"仅后台时通知"设置——窗口最小化/隐藏时才算后台，被别的
  窗口遮挡不算。
- **`window.open` 打开的窗口是另一个进程的**：DSH 同源弹窗由 WebView2
  在共享环境下开窗（因此 cookie 一致、不会 401），窗口由 WebView2 托管、
  随主窗销毁而关闭；它的图标/标题栏是系统默认样式，不是本程序的自绘风格。
- **关闭窗口后 DSH 还在**：两种情况，均属设计如此：① Windows 版 ✕ 默认 =
  缩到系统托盘（想真正退出请右键托盘图标选「关闭」，或在
  托盘菜单「设置」里选「直接关闭DSH Launch Console」）；② 双击时 DSH 已在运行且没有
  登记表（附加模式，退出不杀它）。可看日志确认，必要时
  `taskkill /F /IM node.exe` 兜底（会误杀其它 node 进程，慎用）。
- **双击启动器没有出现新窗口**：单实例设计——已有窗口会被拉到前台
  （隐藏在托盘时自动恢复），不会开第二个；想开新会话请先经托盘菜单关闭。
- **托盘图标不见了**：单实例只有唯一一个托盘图标，随程序退出而消失；
  若窗口进程崩溃，托盘图标会残留到鼠标划过时被系统清除。
- **✕ 按钮行为设置存在哪里**：`%LOCALAPPDATA%\DSH-Launch-Console\close-action`
  （内容 `tray` = 最小化到托盘，`exit` = 直接关闭），删除该文件即恢复默认。

## 许可证

[MIT](LICENSE)
