// 编译期把黑色鲸鱼图标 + 版本信息嵌入 exe（**仅 Windows 目标**需要）。
// 版本信息决定资源管理器「属性 → 详细信息」、任务管理器「名称/描述」
// 以及部分第三方工具里显示的名字，改名时必须同步。
//
// 两个平台判断必须分清：
//   * `#[cfg(windows)]` 在 build.rs 里指的是**宿主**平台（build.rs 永远为宿主编译）
//   * 目标平台要看 cargo 注入的 `CARGO_CFG_TARGET_OS`
// 只用 cfg!(windows) 时，从 Windows 交叉编译 Linux 会去给 Linux 目标写 Windows
// 资源而直接失败；反过来在本机编译（宿主=目标）则看不出问题。

/// 宿主是 Windows 才需要 winres（见 Cargo.toml 里的 target 限定）。
#[cfg(windows)]
fn embed_icon_and_version() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }
    let mut res = winres::WindowsResource::new();
    res.set_icon("icon.ico");
    res.set("ProductName", "DSH Launch Console");
    res.set("FileDescription", "DSH Launch Console");
    res.set("OriginalFilename", "DSH_Launch_Console.exe");
    res.set("InternalName", "DSH_Launch_Console");
    res.set("CompanyName", "DSH Launch Console");
    res.set("LegalCopyright", "DSH Launch Console");
    res.set("FileVersion", "0.2.4.0");
    res.set("ProductVersion", "0.2.4.0");
    res.compile().expect("embed icon resource");
}

/// 非 Windows 宿主（Linux/macOS 本机编译）：什么都不用做。
#[cfg(not(windows))]
fn embed_icon_and_version() {}

fn main() {
    println!("cargo:rerun-if-changed=icon.ico");
    embed_icon_and_version();
}
