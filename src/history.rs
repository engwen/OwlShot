//! 剪贴板历史：截图落定后在 `~/.local/share/owlshot/history/` 留一份副本，
//! 之后可用 `owlshot --history N` 把第 N 条重新写回剪贴板，再按 Ctrl+V 粘贴。
//!
//! 为什么不做「Ctrl+V+1」这种连击：粘贴动作发生在目标程序（浏览器/编辑器）里，
//! Wayland 下任何进程都无法拦截别人的 Ctrl+V，所以只能反过来做 —— 先切换剪贴板内容，
//! 再由用户用系统原生的 Ctrl+V 粘贴。
//!
//! 存储是「按文件名时间戳排序的环形目录」：新图进来后删掉超出 keep 的最旧文件。

use crate::config;
use anyhow::{Context, Result, bail};
use gtk4::glib;
use std::path::{Path, PathBuf};

/// 历史文件名前缀，用于过滤目录里的无关文件。
const PREFIX: &str = "owlshot-history-";

pub fn dir() -> PathBuf {
    glib::user_data_dir().join("owlshot").join("history")
}

/// 把刚产出的图片复制进历史目录，并裁掉超出保留条数的旧文件。
///
/// 失败只告警：历史是附加功能，不能影响截图主流程。
pub fn record(src: &Path) {
    let cfg = &config::get().history;
    if !cfg.enabled {
        return;
    }
    if let Err(err) = record_inner(src, cfg.keep()) {
        eprintln!("[owlshot] 写入剪贴板历史失败：{err:#}");
    }
}

fn record_inner(src: &Path, keep: usize) -> Result<()> {
    let dir = dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("无法创建目录 {}", dir.display()))?;

    // 时间戳精确到毫秒，避免同一秒内连续两张截图互相覆盖。
    let now = glib::DateTime::now_local().context("获取本地时间失败")?;
    let stamp = now
        .format("%Y%m%d-%H%M%S")
        .context("格式化时间戳失败")?;
    let dest = dir.join(format!("{PREFIX}{stamp}-{:03}.png", now.microsecond() / 1000));
    std::fs::copy(src, &dest).with_context(|| format!("复制到 {} 失败", dest.display()))?;

    prune(&dir, keep);
    Ok(())
}

/// 只保留最新的 `keep` 个，其余按时间从旧到新删除。
fn prune(dir: &Path, keep: usize) {
    let mut files = entries(dir);
    if files.len() <= keep {
        return;
    }
    // entries() 已按新→旧排序，超出部分即为待删。
    for path in files.drain(keep..) {
        if let Err(err) = std::fs::remove_file(&path) {
            eprintln!("[owlshot] 清理旧历史 {} 失败：{err}", path.display());
        }
    }
}

/// 历史文件列表，**按时间从新到旧**排序（索引 0 = 最近一张）。
///
/// 文件名里的时间戳是定长的，所以直接按名字倒序即可，无需读 mtime。
pub fn entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(PREFIX) && name.ends_with(".png"))
        })
        .collect();
    files.sort_unstable_by(|a, b| b.file_name().cmp(&a.file_name()));
    files
}

/// 取第 `index` 条历史（0 = 最近一张）的路径。
pub fn nth(index: usize) -> Option<PathBuf> {
    entries(&dir()).into_iter().nth(index)
}

/// 把历史文件名还原成人类可读的时间，形如 `09-03 10:15:30`，供托盘菜单显示。
///
/// 解析失败时退回文件名本身，保证菜单一定有内容可显示。
pub fn label_of(path: &Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let stem = name.strip_prefix(PREFIX).unwrap_or(name);
    // 期望形如 `20260903-101530-123.png`
    let Some((date, rest)) = stem.split_once('-') else {
        return name.into();
    };
    let ascii_digits = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
    if date.len() != 8 || rest.len() < 6 || !ascii_digits(date.as_bytes()) {
        return name.into();
    }
    let time = &rest.as_bytes()[..6];
    if !ascii_digits(time) {
        return name.into();
    }
    // 全为 ASCII 数字，按字节切片不会切坏字符。
    let time = &rest[..6];
    format!(
        "{}-{} {}:{}:{}",
        &date[4..6],
        &date[6..8],
        &time[..2],
        &time[2..4],
        &time[4..6]
    )
}

/// 把第 `index` 条历史写回剪贴板。
pub async fn restore(index: usize) -> Result<()> {
    let cfg = &config::get().history;
    if !cfg.enabled {
        bail!("剪贴板历史已在配置中关闭（history.enabled = false）");
    }
    let keep = cfg.keep();
    if index >= keep {
        bail!("当前只保留 {keep} 条历史，无法取第 {} 条", index + 1);
    }
    let Some(path) = nth(index) else {
        bail!("历史里还没有第 {} 张截图", index + 1);
    };
    let backend = crate::clipboard::copy_png(&path).await?;
    println!(
        "[owlshot] 已通过 {backend} 把第 {} 条历史写入剪贴板：{}",
        index + 1,
        path.display()
    );
    Ok(())
}
