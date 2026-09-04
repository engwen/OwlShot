//! 截图捕获：唯一途径是标准 xdg-desktop-portal（org.freedesktop.portal.Screenshot）。
//! 严禁引入任何 X11 API。
//!
//! portal 只负责抓取整块屏幕的静态 PNG（`interactive(false)`）；选区与窗口选取由
//! 阶段2 的自绘编辑器（src/editor.rs）完成 —— 用户明确否决了 portal 自带交互对话框的观感。

use anyhow::{Context, Result, anyhow, bail};
use ashpd::desktop::screenshot::Screenshot;
use ashpd::zbus;
use gtk4::gdk_pixbuf::Pixbuf;
use gtk4::glib;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 抓取整个屏幕，返回落盘的临时 PNG 路径。
///
/// 默认走标准 portal 的非交互模式（不弹系统对话框，首次调用时仍可能请求一次授权）；
/// 仅当配置显式开启 GNOME 私有增强时先试 Mutter 接口，失败自动回落 portal。
pub async fn capture() -> Result<PathBuf> {
    if crate::mutter::enabled() {
        match crate::mutter::screenshot_full().await {
            Ok(path) => return Ok(path),
            Err(err) => {
                eprintln!("[owlshot] GNOME 私有接口抓屏失败，回落标准 portal：{err:#}");
            }
        }
    }

    ensure_screenshot_permission().await;

    let response = Screenshot::request()
        .interactive(false)
        .modal(true)
        .send()
        .await
        .context("无法向 xdg-desktop-portal 发起截图请求")?
        .response()
        .context("截图请求被取消或被 portal 拒绝")?;

    file_uri_to_path(response.uri().as_str())
}

/// 把自身登记进 portal 的截图权限表（org.freedesktop.impl.portal.PermissionStore）。
///
/// 背景：被 gsd-media-keys（自定义快捷键）或 gnome-shell（应用图标）拉起时，进程落在
/// `app-gnome-owlshot-<pid>.scope` 这个 systemd scope 里，xdg-desktop-portal 据此把
/// app-id 解析为 `owlshot`；权限表中没有该条目时 portal 要先弹一次授权框，而
/// GNOME 46 的 portal 后端在请求方没有父窗口时弹不出来（日志：
/// `Failed to show access dialog: 已到超时限制`），25 秒后请求以
/// `Portal request didn't succeed with no information` 失败。
/// 而在终端里直接运行时 scope 是 `session-N.scope`，app-id 为空串，走的是另一条
/// 权限记录 —— 这正是「命令能截图、快捷键和图标不能」的原因。
///
/// 只在权限尚未设置时写入：用户若在授权框里明确选过「拒绝」，这里不覆盖其选择。
/// 任何一步失败都静默忽略，退回 portal 原本的授权流程。
async fn ensure_screenshot_permission() {
    const TABLE: &str = "screenshot";
    const ID: &str = "screenshot";
    const SERVICE: &str = "org.freedesktop.impl.portal.PermissionStore";
    const OBJECT_PATH: &str = "/org/freedesktop/impl/portal/PermissionStore";
    let app_id = env!("CARGO_PKG_NAME");

    let Ok(connection) = zbus::Connection::session().await else {
        return;
    };
    let Ok(proxy) = zbus::Proxy::new(&connection, SERVICE, OBJECT_PATH, SERVICE).await else {
        return;
    };

    // 权限表首次使用时并不存在，Lookup 会直接报错，此时同样需要写入。
    let looked_up: zbus::Result<(HashMap<String, Vec<String>>, zbus::zvariant::OwnedValue)> =
        proxy.call("Lookup", &(TABLE, ID)).await;
    if let Ok((permissions, _data)) = looked_up
        && permissions.contains_key(app_id)
    {
        return;
    }

    let granted: zbus::Result<()> = proxy
        .call("SetPermission", &(TABLE, true, ID, app_id, vec!["yes"]))
        .await;
    match granted {
        Ok(()) => println!("[owlshot] 已在 portal 权限表登记 {app_id} 的截图授权。"),
        Err(err) => {
            eprintln!("[owlshot] 登记截图授权失败，首次截图可能需要在系统弹窗中确认：{err}");
        }
    }
}

/// 抓取当前焦点窗口。标准 portal 无等价能力，只能依赖 GNOME 私有增强。
pub async fn capture_window() -> Result<PathBuf> {
    if !crate::mutter::enabled() {
        bail!(
            "窗口截图依赖 GNOME 私有接口，请在配置中设置 advanced.use_mutter_overlay = true（KDE/Sway 不支持）"
        );
    }
    crate::mutter::screenshot_window().await
}

/// 把截图落盘到 XDG 图片目录下的 Screenshots 子目录，返回最终路径。
pub fn save_to_pictures(src: &Path) -> Result<PathBuf> {
    let dest = new_dest_path()?;
    std::fs::copy(src, &dest)
        .with_context(|| format!("保存截图到 {} 失败", dest.display()))?;
    Ok(dest)
}

/// 把内存中的 Pixbuf（如编辑器裁剪结果）写成 PNG，返回最终路径。
pub fn save_pixbuf(pixbuf: &Pixbuf) -> Result<PathBuf> {
    let dest = new_dest_path()?;
    pixbuf
        .savev(&dest, "png", &[])
        .with_context(|| format!("写出 PNG 到 {} 失败", dest.display()))?;
    Ok(dest)
}

/// 生成带时间戳的目标路径，并确保目录存在。
fn new_dest_path() -> Result<PathBuf> {
    let cfg = &crate::config::get().capture;
    let dir = cfg.resolved_save_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("无法创建目录 {}", dir.display()))?;

    let stamp = glib::DateTime::now_local()
        .context("获取本地时间失败")?
        .format("%Y%m%d-%H%M%S")
        .context("格式化时间戳失败")?;

    Ok(dir.join(format!("{}-{stamp}.png", cfg.prefix())))
}

fn file_uri_to_path(uri: &str) -> Result<PathBuf> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or_else(|| anyhow!("portal 返回了非本地文件 URI：{uri}"))?;
    // 形如 file://<host>/path，host 为空或 localhost；只接受能定位到绝对路径的形式。
    let encoded = match rest.strip_prefix("localhost") {
        Some(stripped) => stripped,
        None => rest,
    };
    if !encoded.starts_with('/') {
        bail!("无法从 URI 解析出绝对路径：{uri}");
    }
    Ok(PathBuf::from(percent_decode(encoded)))
}

/// portal 返回的 URI 会对空格、中文等字符做 percent-encoding，需还原成真实路径。
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
