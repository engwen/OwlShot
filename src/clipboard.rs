//! 剪贴板写入：wl-copy 优先，失败自动降级到 GDK4。
//!
//! Wayland 的 wl_data_device::set_selection 要求调用方拥有获得焦点的 surface，
//! 而 owlshot 平时只是一个托盘进程、没有窗口，所以直接走 GDK 并不可靠。
//! wl-copy 会 fork 成独立的剪贴板所有者进程，不受焦点限制，作为首选路径。

use anyhow::{Context, Result, anyhow, bail};
use gtk4::glib;
use gtk4::prelude::*;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// 实际生效的剪贴板后端，用于日志区分是否发生了降级。
pub enum Backend {
    WlCopy,
    Gdk,
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Backend::WlCopy => f.write_str("wl-copy"),
            Backend::Gdk => f.write_str("GDK4 降级"),
        }
    }
}

pub async fn copy_png(path: &Path) -> Result<Backend> {
    match wl_copy(path) {
        Ok(()) => Ok(Backend::WlCopy),
        Err(err) => {
            eprintln!("[owlshot] wl-copy 不可用，降级到 GDK4 剪贴板：{err:#}");
            gdk_copy(path).await?;
            Ok(Backend::Gdk)
        }
    }
}

/// 同步版本，**只能在 GLib 主线程调用**（编辑器的 Ctrl+C 就在主线程）。
///
/// 因为已经在主线程上，GDK 降级路径无需再投递一次，直接同步写即可。
pub fn copy_png_on_main(path: &Path) -> Result<Backend> {
    match wl_copy(path) {
        Ok(()) => Ok(Backend::WlCopy),
        Err(err) => {
            eprintln!("[owlshot] wl-copy 不可用，降级到 GDK4 剪贴板：{err:#}");
            set_gdk_clipboard(path)?;
            Ok(Backend::Gdk)
        }
    }
}

fn wl_copy(path: &Path) -> Result<()> {
    let bytes =
        std::fs::read(path).with_context(|| format!("读取 {} 失败", path.display()))?;

    let mut child = Command::new("wl-copy")
        .args(["--type", "image/png"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("未找到 wl-copy，可执行 `sudo apt install wl-clipboard` 安装")?;

    child
        .stdin
        .take()
        .context("wl-copy stdin 管道不可用")?
        .write_all(&bytes)
        .context("向 wl-copy 写入 PNG 数据失败")?;

    // wl-copy 写完即 fork 到后台持有剪贴板，父进程会立刻退出。
    let status = child.wait().context("等待 wl-copy 退出失败")?;
    if !status.success() {
        bail!("wl-copy 异常退出：{status}");
    }
    Ok(())
}

/// GDK 相关调用必须回到 GLib 主线程执行，因此通过主上下文投递并等回结果。
async fn gdk_copy(path: &Path) -> Result<()> {
    let path = path.to_path_buf();
    let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);

    glib::MainContext::default().invoke(move || {
        let result = set_gdk_clipboard(&path).map_err(|err| format!("{err:#}"));
        let _ = tx.send_blocking(result);
    });

    rx.recv()
        .await
        .context("GLib 主循环未响应剪贴板任务")?
        .map_err(|err| anyhow!(err))
}

fn set_gdk_clipboard(path: &Path) -> Result<()> {
    let display = gtk4::gdk::Display::default().context("没有可用的 GDK Display")?;
    let texture = gtk4::gdk::Texture::from_filename(path)
        .with_context(|| format!("无法把 {} 解码成纹理", path.display()))?;
    display.clipboard().set_texture(&texture);
    Ok(())
}
