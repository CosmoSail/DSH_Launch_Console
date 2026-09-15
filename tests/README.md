# 集成测试脚本

这些是从开发过程中沉淀下来的 **Windows 实机验证脚本**，用来证明启动器各项行为
真的成立（而不是"理论上应该成立"）。它们全部是 PowerShell，直接操作真实进程与
真实窗口，因此比单元测试更接近用户实际遇到的情况。

Rust 侧的逻辑测试（URL 白名单、token 解析、shim 解析、日志轮转、失败分类等）
在源码里，跑 `cargo test` 即可。

## 前置条件

1. **构建出被测程序**（脚本只测 `target\release\` 下的产物）：

   ```powershell
   cargo build --release
   ```

2. **部分脚本需要一份"旧版对照" exe**：`test-fastpath.ps1`、`test-launch-ab.ps1`、
   `test-window.ps1` 会拿仓库根目录的 `DSH_Launch_Console.exe` 当作对照基线。请先把
   编译产物复制过去：

   ```powershell
   copy target\release\dsh-launch-console.exe DSH_Launch_Console.exe
   ```

   （该文件已被 `.gitignore` 排除，不会进仓库。）

3. 需要 **Node.js**；涉及 DSH 启动的脚本还需要全局包 `@deepseek-ai/dsh`
   （`npm i -g @deepseek-ai/dsh`），或用 `-DshBin` 显式指定入口路径。

## 安全约定

脚本遵循一套共同的护栏，设计上**不会干扰你正在使用的 DSH 会话**：

- 一律使用 **改名副本**（`dsh-lab*.exe`）和**独立互斥体**（`DSH_LAUNCH_CONSOLE_MUTEX`），
  因此可以和正在运行的实例并存；
- 一律使用**独立状态目录**（`DSH_LAUNCH_CONSOLE_STATEDIR`）指向临时目录，不动你的配置；
- 只结束**本脚本自己记录的 PID**，绝不 `taskkill /IM node.exe` 这类按映像名批量杀；
- 使用 **3197 / 3198 / 3199** 等测试端口；启动耗时系列脚本默认用 **3180 / 3181**，
  并且显式拒绝 3080（真实 DSH 端口）。

运行前请先关闭你自己的 DSH Launch Console，或确认脚本的安全护栏提示后继续。

## 脚本一览

| 脚本 | 验证内容 |
| --- | --- |
| `test-launch.ps1` | 启动链重构：直连包入口、失败分类、npx 回退三条路径 |
| `test-fastpath.ps1` | 启动速度对比：旧版+npx / 新版+直连 `dsh` / 新版+npx 回退 |
| `test-launch-ab.ps1` | A/B 对比：新直连链路 vs 旧"隐藏脚本 + shim"链路（各 4 轮取中位数） |
| `test-window.ps1` | 窗口可见时间（进程启动 → 出现第一个可见顶层窗口） |
| `test-loading.ps1` | 「窗口先显示 + 启动中页带秒数」：mock DSH 故意 10 秒后才给 token，第 5 秒截图 |
| `probe-icon.ps1` | 像素级校验占位页：找出墨水像素并打印 ASCII 图（配合 `test-loading.ps1` 的截图） |
| `test-attach.ps1` | 附加模式：DSH 已在运行时只开窗口、退出不杀它，并截图确认界面 |
| `test-permission.ps1` | WebView2 对通知权限的默认处理（mock 页面回报 `Notification.permission`） |
| `test-webview.ps1` | WebView 权限放行（通知/剪贴板）与同源弹窗的 cookie 共享 |
| `test-name2.ps1` | 名称显示：托盘提示文案与托盘右键菜单项（UIA 读取真实菜单） |
| `test-startup-timing.ps1` | **DSH 启动耗时对照**：node 直启 vs 经启动器，测到端口就绪的毫秒数 |
| `test-startup-io-profile.ps1` | **启动画像**：采样 CPU 时间 / 读取字节 / 工作集，判断 CPU 受限还是 IO 受限 |
| `test-startup-wait-diag.ps1` | **启动等待定位**：观测启动期间的 TCP 连接，确认是否阻塞在网络 |

## 运行

```powershell
# 在仓库根目录执行
pwsh -File .\tests\test-launch.ps1

# 启动耗时对照（默认各 2 轮；-Runs 3 提高置信度）
pwsh -File .\tests\test-startup-timing.ps1 -Runs 3

# 全局 dsh 入口不在默认位置时显式指定
pwsh -File .\tests\test-startup-timing.ps1 -DshBin 'D:\somewhere\@deepseek-ai\dsh\lib\bin.js'
```

## 关于「DSH 启动耗时」这组脚本的结论

`test-startup-*.ps1` 三个脚本是为排查"启动慢"而写的，实测结论记录在这里，
免得后来者重复劳动：

- **启动器几乎不增加开销**：直启 node 与经启动器启动到端口就绪，中位数相差
  约 70 ms（在噪声范围内，方向还是启动器略快）。原因是启动器走 `node <入口>`
  直连、没有 shell 中间层，而窗口与 WebView2 是在 node 启动之后并行创建的。
- **耗时几乎全在 DSH 自身**：node 启动约 36 ms，参数解析 + profile 组合约 61 ms，
  剩下的约 6.3 秒是 ESM 模块加载与 cordis 插件树挂载。
- **瓶颈是 CPU 而不是磁盘或网络**：启动期间 CPU 时间 ≈ 墙钟时间（单核打满），
  读操作仅约 5,500 次 / 26 MB（NVMe 上可忽略），且**没有任何非监听 TCP 连接**。
  因此系统繁忙时会被线性拉长（实测波动区间 2.3 s ~ 28.4 s）。

## 踩坑记录

排查过程中踩到的两个坑，写下来避免重复：

1. **PowerShell 参数名不匹配不会报错**：函数写成 `param([int]$P)` 却用
   `Test-Port -Port 3180` 调用时，PowerShell 不会抛异常，而是把 `$P` 静默绑成
   **0**，于是端口探测恒为假，DSH 明明 6 秒就绪却被误判成"卡住 120 秒"。
2. **`$P` 与 `$p` 是同一个变量**：PowerShell 变量名不区分大小写，用 `$P` 存目录、
   再在循环里用 `$p` 存文件路径，会把外层变量覆盖掉。
