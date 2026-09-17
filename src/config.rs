//! 配置文件：`~/.config/owlshot/config.toml`
//!
//! 设计原则：
//!   * 文件缺失、字段缺失、解析失败都不阻断启动 —— 一律回落默认值并打印告警。
//!   * 首次运行写出一份带注释的模板，用户直接改即可。

use gtk4::glib;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 进程启动时加载一次，之后只读。
static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub shortcuts: ShortcutsConfig,
    pub capture: CaptureConfig,
    pub history: HistoryConfig,
    pub advanced: AdvancedConfig,
}

/// 全局快捷键：只走 org.freedesktop.portal.GlobalShortcuts（系统弹窗授权），不抓键盘。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ShortcutsConfig {
    /// 是否尝试向 portal 注册全局快捷键。
    pub enabled: bool,
    /// 框选截图触发器，XDG 语法：修饰键 CTRL/ALT/SHIFT/NUM/LOGO + keysym 名，用 `+` 连接。
    pub region: String,
    /// 全屏截图触发器。
    pub fullscreen: String,
    /// 贴图触发器（Ctrl+Shift+V）。
    pub paste: String,
}

impl Default for ShortcutsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            region: "CTRL+SHIFT+a".to_string(),
            fullscreen: "CTRL+SHIFT+s".to_string(),
            paste: "CTRL+SHIFT+v".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    /// 保存目录，留空表示 XDG 图片目录下的 Screenshots；支持 `~` 开头。
    pub save_dir: String,
    /// 文件名前缀，最终形如 `owlshot-20260101-120000.png`。
    pub file_prefix: String,
    /// 截图完成后是否自动写入剪贴板。
    pub copy_to_clipboard: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            save_dir: String::new(),
            file_prefix: "owlshot".to_string(),
            copy_to_clipboard: true,
        }
    }
}

/// 剪贴板历史：每次截图都在历史目录留一份副本，之后可用 `owlshot --history N` 取回。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    /// 是否记录历史。关闭后既不写入也不清理旧文件。
    pub enabled: bool,
    /// 保留条数，取值范围 1..=5（超出自动收敛）。
    pub keep: usize,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            keep: 3,
        }
    }
}

impl HistoryConfig {
    /// 上限硬编码为 5：托盘子菜单与 Ctrl+Alt+N 快捷键都按这个规模设计。
    pub const MAX_KEEP: usize = 5;

    /// 收敛到合法区间，避免配置写成 0 或 99。
    pub fn keep(&self) -> usize {
        self.keep.clamp(1, Self::MAX_KEEP)
    }
}

/// 阶段4 的 GNOME 私有 Mutter 接口开关，默认关闭；KDE/Sway 下强制忽略。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AdvancedConfig {
    pub use_mutter_overlay: bool,
}

/// 取全局配置；首次调用触发加载。
pub fn get() -> &'static Config {
    CONFIG.get_or_init(load)
}

fn load() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(config) => config,
            Err(err) => {
                eprintln!(
                    "[owlshot] 配置文件 {} 解析失败，已回落默认值：{err}",
                    path.display()
                );
                Config::default()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            write_template(&path);
            Config::default()
        }
        Err(err) => {
            eprintln!(
                "[owlshot] 无法读取配置文件 {}，已回落默认值：{err}",
                path.display()
            );
            Config::default()
        }
    }
}

pub fn config_path() -> PathBuf {
    glib::user_config_dir().join("owlshot").join("config.toml")
}

/// 首次运行落一份模板；失败只告警，不影响截图功能。
fn write_template(path: &Path) {
    let Some(dir) = path.parent() else { return };
    if let Err(err) = std::fs::create_dir_all(dir) {
        eprintln!("[owlshot] 无法创建配置目录 {}：{err}", dir.display());
        return;
    }
    if let Err(err) = std::fs::write(path, TEMPLATE) {
        eprintln!("[owlshot] 无法写出配置模板 {}：{err}", path.display());
        return;
    }
    println!("[owlshot] 已生成默认配置：{}", path.display());
}

impl CaptureConfig {
    /// 解析出真实保存目录：留空回落 XDG 图片目录 / Screenshots，支持 `~` 展开。
    pub fn resolved_save_dir(&self) -> PathBuf {
        let raw = self.save_dir.trim();
        if raw.is_empty() {
            // 交给 glib 解析 XDG user-dirs，可正确处理 ~/图片 这类本地化目录名。
            let pictures = glib::user_special_dir(glib::UserDirectory::Pictures)
                .unwrap_or_else(|| glib::home_dir().join("Pictures"));
            return pictures.join("Screenshots");
        }
        if raw == "~" {
            return glib::home_dir();
        }
        if let Some(rest) = raw.strip_prefix("~/") {
            return glib::home_dir().join(rest);
        }
        PathBuf::from(raw)
    }

    /// 文件名前缀，空值回落 `owlshot`。
    pub fn prefix(&self) -> &str {
        let prefix = self.file_prefix.trim();
        if prefix.is_empty() { "owlshot" } else { prefix }
    }
}

const TEMPLATE: &str = r#"# OwlShot 配置文件
# 修改后重启 owlshot 生效。

[shortcuts]
# 是否向 xdg-desktop-portal 注册全局快捷键（会弹系统授权窗口）。
# 注意：GNOME 46 及更早版本的 portal 后端尚未实现 GlobalShortcuts 接口，
#       此时请改用「GNOME 设置 → 键盘 → 自定义快捷键」绑定命令：
#           owlshot --region     （框选截图）
#           owlshot --full       （全屏截图）
enabled = true
# 触发器语法：修饰键 CTRL / ALT / SHIFT / NUM / LOGO 与键名以 + 连接，
# 键名取自 xkbcommon keysym（如 a、Print、space、Return）。
region = "CTRL+SHIFT+a"
fullscreen = "CTRL+SHIFT+s"
# 贴图（Ctrl+Shift+V）：把剪贴板里的图片贴到屏幕上，用于数据对比。
paste = "CTRL+SHIFT+v"

[capture]
# 保存目录，留空 = XDG 图片目录下的 Screenshots，可写 "~/Pictures/shots"。
save_dir = ""
# 文件名前缀，最终形如 owlshot-20260101-120000.png
file_prefix = "owlshot"
# 截图完成后自动写入剪贴板
copy_to_clipboard = true

[history]
# 是否记录剪贴板历史。历史文件存放在 ~/.local/share/owlshot/history/。
enabled = true
# 保留最近几张，范围 1-5（超出自动收敛到 5）。
# 取回方式：owlshot --history 1 表示上一张，2 表示上上一张，写回剪贴板后按 Ctrl+V 粘贴。
# 建议在「设置 → 键盘 → 自定义快捷键」把 Ctrl+Alt+1/2/3 分别绑到 owlshot --history 1/2/3。
keep = 3

[advanced]
# GNOME 私有 Mutter D-Bus 增强（阶段4），默认关闭；KDE/Sway 下强制忽略。
use_mutter_overlay = false
"#;
