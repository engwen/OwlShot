//! GNOME 私有增强（可选，默认关闭）：org.gnome.Shell.Screenshot。
//!
//! 硬约束：
//!   * 仅在配置显式开启 `advanced.use_mutter_overlay = true` 时启用；
//!   * KDE / Plasma / Sway / wlroots / Hyprland 下**强制禁用**，即使配置开启；
//!   * 任何失败都静默回落到标准 xdg-desktop-portal，绝不影响主链路；
//!   * 不使用任何 X11 API，纯 D-Bus 调用。
//!
//! 提供两项标准 portal 无法覆盖的能力：
//!   1. 抓全屏时跳过 portal 中转（更快、无授权提示）；
//!   2. 抓当前焦点窗口（portal 非交互模式没有等价能力）。
//!
//! 刻意**不使用** `SelectArea`：其选区观感已被用户否决，选区一律由自绘编辑器负责。

use anyhow::{Context, Result, bail};
use ashpd::zbus;
use gtk4::glib;
use std::path::PathBuf;
use std::sync::OnceLock;

const DEST: &str = "org.gnome.Shell";
const OBJECT_PATH: &str = "/org/gnome/Shell/Screenshot";
const INTERFACE: &str = "org.gnome.Shell.Screenshot";

/// 探测结果只算一次，避免每次截图重复打印提示。
static ENABLED: OnceLock<bool> = OnceLock::new();

/// 是否允许走 GNOME 私有接口。
pub fn enabled() -> bool {
    *ENABLED.get_or_init(probe)
}

fn probe() -> bool {
    if !crate::config::get().advanced.use_mutter_overlay {
        return false;
    }

    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let upper = desktop.to_uppercase();

    // 非 GNOME 合成器上该接口不存在，且这类环境有各自的原生方案，一律禁用。
    const BLOCKED: [&str; 5] = ["KDE", "PLASMA", "SWAY", "WLROOTS", "HYPRLAND"];
    if let Some(name) = BLOCKED.iter().find(|name| upper.contains(**name)) {
        eprintln!("[owlshot] 检测到 {name} 桌面，已强制禁用 GNOME 私有增强，仅使用标准 portal。");
        return false;
    }
    if !upper.contains("GNOME") {
        eprintln!(
            "[owlshot] 当前桌面（XDG_CURRENT_DESKTOP={desktop}）非 GNOME，已忽略 use_mutter_overlay。"
        );
        return false;
    }

    println!("[owlshot] 已启用 GNOME 私有增强（失败会自动回落标准 portal）。");
    true
}

/// 抓整块屏幕，返回临时 PNG 路径（语义与 portal 版一致）。
pub async fn screenshot_full() -> Result<PathBuf> {
    let dest = temp_path("full");
    let path = dest
        .to_str()
        .context("临时路径含非 UTF-8 字符，无法通过 D-Bus 传递")?;
    call_screenshot("Screenshot", &(false, false, path)).await
}

/// 抓当前焦点窗口（含窗口边框，不含鼠标指针）。
pub async fn screenshot_window() -> Result<PathBuf> {
    let dest = temp_path("window");
    let path = dest
        .to_str()
        .context("临时路径含非 UTF-8 字符，无法通过 D-Bus 传递")?;
    call_screenshot("ScreenshotWindow", &(true, false, false, path)).await
}

async fn call_screenshot<B>(method: &str, body: &B) -> Result<PathBuf>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let connection = zbus::Connection::session()
        .await
        .context("无法连接会话总线")?;
    let proxy = zbus::Proxy::new(&connection, DEST, OBJECT_PATH, INTERFACE)
        .await
        .context("GNOME Shell 未提供 org.gnome.Shell.Screenshot 接口")?;

    // GNOME 45+ 会对非授权调用方返回 AccessDenied，交由上层回落 portal。
    let (success, used): (bool, String) = proxy
        .call(method, body)
        .await
        .with_context(|| format!("调用 {INTERFACE}.{method} 失败"))?;
    if !success {
        bail!("GNOME Shell 拒绝了 {method} 请求");
    }
    Ok(PathBuf::from(used))
}

/// 落到系统临时目录，带 pid 避免多实例互相覆盖。
fn temp_path(tag: &str) -> PathBuf {
    glib::tmp_dir().join(format!("owlshot-{tag}-{}.png", std::process::id()))
}
