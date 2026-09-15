// 编译期把黑色鲸鱼图标 + 版本信息嵌入 exe（仅 Windows 需要）。
// 版本信息决定资源管理器「属性 → 详细信息」、任务管理器「名称/描述」
// 以及部分第三方工具里显示的名字，改名时必须同步。
fn main() {
    if cfg!(windows) {
        let mut res = winres::WindowsResource::new();
        res.set_icon("icon.ico");
        res.set("ProductName", "DSH Launch Console");
        res.set("FileDescription", "DSH Launch Console");
        res.set("OriginalFilename", "DSH_Launch_Console.exe");
        res.set("InternalName", "DSH_Launch_Console");
        res.set("CompanyName", "DSH Launch Console");
        res.set("LegalCopyright", "DSH Launch Console");
        res.set("FileVersion", "0.1.0.0");
        res.set("ProductVersion", "0.1.0.0");
        res.compile().expect("embed icon resource");
    }
}
