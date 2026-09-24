# DSH Launch Console

**DeepSeek Harness（DSH）的图形启动器**：用原生 GUI（Rust + egui，**不依赖 WebView**）
管理 DSH 的启动、关闭、版本与插件，Web UI 交给**系统默认浏览器**打开。
单个可执行文件，双击即用，全程没有终端窗口。

![控制台 · 浅色主题（DSH 运行中）](docs/02-console-running.png)

## 与 DSH 的关系

两者各自独立安装：

- **DSH 本体**：`npm i -g @deepseek-ai/dsh`，或官网 <https://www.deepseek.com/harness/> 的安装包。
- **本程序**：只负责启动 / 关闭 DSH、在浏览器打开 Web UI、管版本与插件，**不自带 DSH**。
  没装过也没关系——在「版本」页点「安装」，它会替你执行 `npm i -g`。

## 下载安装

**[⬇ 下载 DSH_Launch_Console-Setup-0.2.5.exe](https://github.com/CosmoSail/DSH_Launch_Console/releases/latest)**（Windows，5 MB）

双击安装即可 —— 按用户安装、**无需管理员权限**，带开始菜单与可选桌面快捷方式，可在「应用」里卸载。
需要 **Node.js ≥ 18**；DSH 本体没装过也没关系，装好后在「版本」页点「安装」会自动替你装。

也可以从 [Releases](https://github.com/CosmoSail/DSH_Launch_Console/releases) 拿源码包自行编译
（每个文件旁边附有 SHA-256；自己打包时校验和由 `package.bat` 自动写到 `Output\SHA256SUMS.txt`）：

| 文件 | 适用 | 用法 |
| --- | --- | --- |
| `DSH_Launch_Console-Setup-0.2.5.exe` | Windows | 双击安装（推荐） |
| `DSH_Launch_Console-0.2.5-windows-src.zip` | Windows | 解压后双击 `安装.bat` 自行编译安装 |
| `DSH_Launch_Console-0.2.5-linux.tar.gz` | Linux / macOS | 解压后 `./install.sh`，或 `./build-linux.sh` 打 .deb |

> 0.2.0 起为原生 OpenGL 渲染，**不需要 WebView2 运行时**。

## 功能

| 页面 | 说明 |
| --- | --- |
| **控制台** | `▶ 启动` / `■ 关闭` DSH；已在运行时再点「启动」只会**在浏览器新开一个 Web UI**，不会重复起服务；运行日志可滚动翻阅 |
| **版本** | 列出 npm 上 `@deepseek-ai/dsh` 的全部版本与 dist-tag；「安装 / 切换」= `npm i -g @deepseek-ai/dsh@<版本>` |
| **插件** | 插件市场（4000+，分类显示中文）+ GitHub 搜索；安装 / 卸载 / **更新** / 启用 / 禁用；**已装插件可一键打开它的项目仓库**；「刷新」重扫全部安装途径**并重查最新版本**；已装列表一屏 4 行，多了用滚轮或拖滚动条翻 |
| **设置** | **语言（中文 / English）**、风格（浅色 / 深色）、服务地址、Profile、关闭按钮行为、启动后自动打开浏览器、**插件更新策略**（只检查待确认 / 自动更新） |

**插件来源**：插件页的「刷新」会把下面这些途径一次扫完，并在包名后标出命中的途径（悬停看解释）：

| 途径 | 含义 |
| --- | --- |
| `依赖` | 登记在 profile 的 `package.json` dependencies 里（`dsh plugin add` / pnpm add） |
| `bundle 层` | 被选进 profile 的 `dsh.profile.bundles`，会加载进 boot graph |
| `node_modules` / `pnpm 存储` / `兜底目录` | 磁盘上确实存在、但没写进 manifest 的插件（手工拷贝、别的工具装的） |
| `共享目录` | `<DSH_HOME>/profiles/node_modules` 这类多 profile 共用的目录 |
| `dsh 自带` | 全局 dsh 安装的平台层，不是用户装的——只计入途径统计，**不在列表里显示** |

启停写的是插件包**自己** `cordis.patch.yml` 里 `insert` 的那些 id；一个包 insert 多行时会一起改
（避免只关掉半个插件）。传递依赖（如 `js-yaml`）不会被误列成插件。

**打开插件仓库**：已装插件那一行的「仓库」按钮会在系统浏览器里打开它的项目地址——
取自插件包自己 `package.json` 的 `repository` / `homepage`（`git+https://…git`、`git@host:owner/repo`
这些写法都会归一化成可直接打开的 https 地址）；包没写仓库字段时，npm 装的插件回落到它的
npm 页面。GitHub / 本地路径装的包没有可靠的仓库地址，这时不显示这个按钮——宁可不给，
也不给一个点了打不开的死链。

例如装在本地的三个插件，按钮分别打开：

| 插件 | 「仓库」打开 |
| --- | --- |
| `dsh-bloom-theme` | `https://github.com/webkubor/dsh-bloom-theme` |
| `dsh-frosted-window` | `https://github.com/SenryLee/dsh-frosted-window` |
| `dshmarket` | `https://github.com/dsh-market/dsh-market` |

**关闭行为**：点 ✕ 可选「最小化到系统托盘」或「直接关闭」，两种都会在退出时**连同 DSH 一起关闭**。
托盘右键菜单：显示/隐藏窗口、启动/关闭 DSH、打开 Web UI、关闭按钮行为（当前项带勾）、退出。

## 更新日志

| 版本 | 主要变化 |
| --- | --- |
| **0.2.5** | 已装插件可一键打开项目仓库；**去掉「补丁条目」**（插件页不再列出、也不再提供手动清除） |
| 0.2.4 | 界面语言（中文 / English）、插件更新策略与「更新」按钮、「⟳ 刷新」同时重查最新版本（该版加入的「补丁条目」已在 0.2.5 去掉） |
| 0.2.3 | 不再显示 dsh 自带的平台层；已装插件固定一屏 4 行可滚动 |
| 0.2.2 | 插件页重做：多途径自动检索、修好插件启停（按插件包自己的 `insert` 行取 id） |
| 0.2.0 | 去掉 WebView，改为原生 egui 界面；版本管理直接切换全局安装 |
| 0.1.0 | 首个版本（WebView 界面） |

每个版本的完整说明见 [Releases](https://github.com/CosmoSail/DSH_Launch_Console/releases)。

## 界面

![版本管理](docs/03-versions.png)

![插件市场](docs/04-plugins.png)

![设置页（浅色）](docs/05-settings.png)

深色主题在「设置 → 风格」里切换，立即生效并记住（文字用高对比浅色，不糊）：

![深色主题](docs/06-console-dark.png)

## 使用

Windows 下双击 **`安装.bat`**：选安装路径（或 `安装.bat "D:\自定义路径"`），
自动执行 `cargo build --release --target-dir "<安装目录>\target"` 并把
`DSH_Launch_Console.exe` 生成到安装目录；卸载用安装目录里的 `uninstall.bat`。
打包分发：先编译一次，再把 exe 复制到源码根目录，双击 `package.bat`
（产出 Windows 安装向导 + Windows / Linux 源码包）。`package.bat` 的源码包两步
由明文的 `make-archives.ps1` 完成，可单独跑：`pwsh -File make-archives.ps1 -Only linux`。

Linux / macOS：`./install.sh [安装目录]`；打包 .deb 用 `./build-linux.sh`。

## 构建

```bat
cargo build --release
:: 产物 target\release\dsh-launch-console.exe（已内嵌鲸鱼图标，复制为 DSH_Launch_Console.exe 即可便携运行）
```

需要 Rust 工具链（Windows 用 MSVC）；Linux 另需 X11/Wayland 开发头与 OpenGL
（`install.sh` / `build-linux.sh` 会自动装 Debian/Ubuntu 系依赖）。

**版本号只写一处**：项目根 `Cargo.toml` 的 `version`。exe 的「属性 → 详细信息」由
`build.rs` 在编译期从 cargo 注入的版本号生成，安装向导的版本由打包脚本读出后用
`ISCC /DMyAppVersion=` 传入，`安装.bat` 的卸载条目、`build-linux.sh` 的 .deb 版本号
也都在运行时读 `Cargo.toml`——不会出现「界面显示 0.2.5、安装器写 0.2.3」这种漂移。

## 开发与测试

```bash
cargo test        # 单元测试：URL / token 解析、版本排序、插件启停改写（含 YAML 合法性回归）等
```

端到端自检（独立端口 + 独立 `DSH_HOME`，不碰你正在用的会话；`--diagnose` 同义）：

```bat
set DSH_LAUNCH_CONSOLE_URL=http://127.0.0.1:3199
set DSH_HOME=%TEMP%\dsh-selftest-home
start /wait DSH_Launch_Console.exe --selftest
echo 退出码 %ERRORLEVEL%
```

[tests/](tests) 下另有一组 **Windows 实机脚本**（真实进程 / 真实窗口，比单元测试更接近
用户实际遇到的情况），用 [tests/README.md](tests/README.md) 里的护栏运行：改名副本 +
独立互斥体 + 独立端口（3197/3198/3199），并会先把你的设置文件备份挪走、跑完原样还原。
其中 [tests/test-isolation-guard.ps1](tests/test-isolation-guard.ps1) 零副作用，可随时跑。
运行前请先关掉你自己的启动器——脚本检测到它在运行会主动退出。

## 环境变量与文件位置

| 变量 | 说明 |
| --- | --- |
| `DSH_LAUNCH_CONSOLE_URL` | 覆盖服务地址（优先级高于设置文件，便于用独立端口起测试实例） |
| `DSH_LAUNCH_CONSOLE_NODE` | 指定 node 可执行文件 |
| `DSH_LAUNCH_CONSOLE_MUTEX` | 单实例互斥体名（并存多套配置 / 测试实例用） |
| `DSH_LAUNCH_CONSOLE_PROFILE` | 自检用哪个 profile（默认 `web`，等价于 `--profile`） |
| `DSH_HOME` | DSH 主目录（profile / 插件），默认 `%USERPROFILE%\.dsh` |

**启动器不创建任何目录**，只往系统临时目录写几个小文件：

| 文件（都在系统临时目录） | 内容 |
| --- | --- |
| `DSH-Launch-Console.log` | 启动器日志 |
| `DSH-Launch-Console-server.log` | DSH 输出（**含 token**） |
| `DSH-Launch-Console-settings.json` | 设置 |
| `DSH-Launch-Console-versions.json` | 版本列表缓存 |

> 设置只有一个真实位置，就是上表那份 JSON——**没有** `DSH_LAUNCH_CONSOLE_STATEDIR`
> 之类的变量，0.2.0 起也不再创建 `%LOCALAPPDATA%\DSH-Launch-Console`。
> `tests/` 与 `.dev/` 下的脚本会先把这份设置备份挪走、跑完再原样还原。

## 常见问题

- **浏览器里显示 401**：cookie 尚未建立。让启动器**自己启动一次** DSH（点「关闭」再「启动」），
  它会用本次运行的 token 打开界面。
- **关窗后 DSH 还在**：当前是「最小化到系统托盘」模式。想真正退出请右键托盘图标选「退出」，
  或把「关闭按钮行为」改成「直接关闭」。
- **「关闭 DeepSeek Harness」是灰的**：这个 DSH 不是你从启动器启动的（比如终端里敲 `dsh web` 起的）。
  为避免误杀，启动器只结束自己拉起的那棵进程树。
- **插件装不上，提示找不到 pnpm**：`dsh plugin` 是转发给 pnpm 的，先 `npm i -g pnpm`。
- **插件页找不到「补丁条目」了**：0.2.5 起去掉了这一项（它管的是配置残留，实际用不上）。
  原先在列表里显示过的条目就是 profile 的 `cordis.patch.yml` 里按 id 引用、但找不到对应包的
  那些行，它们一直留在你的配置文件里没被动过，需要的话手工编辑该文件即可。
- **已装插件没有「仓库」按钮**：这个包没写 `repository` / `homepage`，而且不是从 npm 装的
  （GitHub / 本地路径装的查不到可靠地址）——所以不给按钮，避免点了打不开。
- **切换版本后行为没变**：切换改的是全局安装那一份，正在运行的 DSH 仍是旧代码——关闭再启动一次。
- **双击没有出现新窗口**：单实例设计，已有实例在运行时会直接退出。
- **界面中文显示为方块**：系统缺少中文字体（Windows 一般自带微软雅黑，不会遇到）。

## 许可证

[MIT](LICENSE)
