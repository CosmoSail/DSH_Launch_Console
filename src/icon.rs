//! 图标与中文字体。
//!
//! 图标沿用 0.1.0 编译期嵌入的官方黑色鲸鱼 RGBA（16/32/256 三档同源），
//! 直接喂给 egui，不再需要 HICON / CreateIcon 那套 Win32 位图构造。
//!
//! 中文字体**不内嵌**（微软雅黑 19 MB / Noto Sans SC 17 MB，会让安装包体积
//! 翻十倍）：改为运行期从系统字体目录加载，找不到时优雅降级为 egui 默认字体
//! （中文会显示为方块，但不影响程序运行）。

use std::sync::OnceLock;

/// 任务栏 / 窗口图标（256×256 高清鲸鱼）。
pub const TASKBAR_ICON_RGBA: &[u8] = include_bytes!("icon-256.rgba");
pub const TASKBAR_ICON_SIZE: u32 = 256;

/// 侧栏 / 标题使用的 32×32 鲸鱼（黑色）。
pub const LOGO_RGBA: &[u8] = include_bytes!("icon-32.rgba");
/// 深色模式下用的白色鲸鱼：黑鲸鱼放在深色顶栏上会看不见，
/// 这份是同一张图反相 RGB、保留 alpha 得到的（构建期不生成，直接随源码提交）。
pub const LOGO_WHITE_RGBA: &[u8] = include_bytes!("icon-32-white.rgba");
pub const LOGO_SIZE: u32 = 32;

/// 供 eframe 设置窗口图标。
pub fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: TASKBAR_ICON_RGBA.to_vec(),
        width: TASKBAR_ICON_SIZE,
        height: TASKBAR_ICON_SIZE,
    }
}

/// 把 32×32 鲸鱼转成 egui 纹理（每种风格各缓存一次）。
/// `dark = true` 时用白色版本，保证深色顶栏上也看得清。
pub fn logo_texture(ctx: &egui::Context, dark: bool) -> egui::TextureHandle {
    static LIGHT: OnceLock<egui::TextureHandle> = OnceLock::new();
    static DARK: OnceLock<egui::TextureHandle> = OnceLock::new();
    let (cell, rgba, name) = if dark {
        (&DARK, LOGO_WHITE_RGBA, "dsh-whale-white")
    } else {
        (&LIGHT, LOGO_RGBA, "dsh-whale")
    };
    cell.get_or_init(|| {
        let img = egui::ColorImage::from_rgba_unmultiplied(
            [LOGO_SIZE as usize, LOGO_SIZE as usize],
            rgba,
        );
        ctx.load_texture(name, img, egui::TextureOptions::LINEAR)
    })
    .clone()
}

/// 候选中文字体路径（按优先级）。
#[cfg(windows)]
fn cjk_font_candidates() -> Vec<std::path::PathBuf> {
    let win = std::path::Path::new("C:\\Windows\\Fonts");
    // 微软雅黑优先（观感最好）；其次是 Noto Sans SC 可变字体、等线、黑体。
    ["msyh.ttc", "NotoSansSC-VF.ttf", "Deng.ttf", "simhei.ttf", "msyhl.ttc"]
        .iter()
        .map(|f| win.join(f))
        .collect()
}

/// 用 fontconfig 问系统要一个中文字体（Linux）。
#[cfg(all(unix, not(target_os = "macos")))]
fn cjk_font_candidates() -> Vec<std::path::PathBuf> {
    const NAMES: [&str; 6] = [
        "Noto Sans CJK SC",
        "Noto Sans SC",
        "Source Han Sans SC",
        "WenQuanYi Micro Hei",
        "WenQuanYi Zen Hei",
        "Droid Sans Fallback",
    ];
    let mut out = Vec::new();
    for name in NAMES {
        if let Ok(o) = std::process::Command::new("fc-match")
            .args(["-f", "%{file}", name])
            .output()
        {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !p.is_empty() {
                out.push(std::path::PathBuf::from(p));
            }
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn cjk_font_candidates() -> Vec<std::path::PathBuf> {
    [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .collect()
}

/// 安装中文字体到 egui。返回实际加载的字体路径（用于日志/诊断）。
pub fn install_cjk_font(ctx: &egui::Context) -> Option<String> {
    for path in cjk_font_candidates() {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "cjk".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        // 插到最前：中文用系统字体，其余字符回落到 egui 默认字体。
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "cjk".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push("cjk".to_owned());
        ctx.set_fonts(fonts);
        return Some(path.display().to_string());
    }
    None
}
