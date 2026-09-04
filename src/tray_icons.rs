//! 托盘图标：从 assets/icons/ 目录加载 PNG 并转成 ksni 需要的 PNG 字节流。
//!
//! 图标文件需手动放入 assets/icons/：
//!   assets/icons/owlshot.png       — 托盘图标（推荐 128×128，至少 64×64）
//!   assets/icons/tray-snapshot.png — 菜单「框选截图」（推荐 22×22）
//!   assets/icons/tray-full.png     — 菜单「全屏截图」
//!   assets/icons/tray-window.png   — 菜单「窗口截图」
//!   assets/icons/tray-exit.png     — 菜单「退出」
//!   assets/icons/tray-history.png  — 菜单「粘贴历史」子菜单图标
//!
//! 文件不存在时降级为默认主题图标名，不报错。

use std::path::PathBuf;

/// 图标资源的根目录，相对于 Cargo manifest（即项目根）。
fn icon_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons"))
}

/// 从 `assets/icons/<name>.png` 读图标并编码为 PNG 字节。
/// 返回空 Vec 表示文件不存在或解码失败（调用方会降级为 icon_name）。
pub fn load_png(name: &str) -> Vec<u8> {
    let path = icon_dir().join(format!("{name}.png"));
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    // 先验证是合法 PNG；不是就退回空，避免把垃圾数据发到 D-Bus。
    if data.len() < 8 || &data[0..4] != &[0x89, b'P', b'N', b'G'] {
        eprintln!("[owlshot] 图标 {} 不是合法 PNG，已降级", path.display());
        return Vec::new();
    }
    data
}
