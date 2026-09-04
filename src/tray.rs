//! StatusNotifierItem 托盘（freedesktop 标准，纯 D-Bus，Wayland 原生可用）。
//! 菜单回调必须保持非阻塞：只往 channel 里投递指令，实际工作交给工作循环。

use crate::Action;
use crate::{config, history};
use gtk4::gdk_pixbuf::Pixbuf;
use ksni::menu::{StandardItem, SubMenu};
use ksni::{MenuItem, Tray};
use std::path::Path;

pub struct OwlTray {
    tx: async_channel::Sender<Action>,
}

impl OwlTray {
    pub fn new(tx: async_channel::Sender<Action>) -> Self {
        Self { tx }
    }

    fn dispatch(&self, action: Action) {
        // unbounded channel，try_send 不会阻塞 D-Bus 线程。
        if let Err(err) = self.tx.try_send(action) {
            eprintln!("[owlshot] 指令投递失败：{err}");
        }
    }
}

impl Tray for OwlTray {
    // 左键点击直接展开菜单，避免误触发截图。
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn title(&self) -> String {
        "OwlShot 截图".into()
    }

    fn icon_name(&self) -> String {
        "camera-photo".into()
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: "框选截图".into(),
                icon_name: "camera-photo".into(),
                activate: Box::new(|tray: &mut Self| tray.dispatch(Action::CaptureRegion)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "全屏截图".into(),
                icon_name: "video-display".into(),
                activate: Box::new(|tray: &mut Self| tray.dispatch(Action::CaptureFullScreen)),
                ..Default::default()
            }
            .into(),
        ];

        // 窗口截图依赖 GNOME 私有接口，未启用时不展示，避免点了必然报错。
        if crate::mutter::enabled() {
            items.push(
                StandardItem {
                    label: "窗口截图".into(),
                    icon_name: "window".into(),
                    activate: Box::new(|tray: &mut Self| tray.dispatch(Action::CaptureWindow)),
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(MenuItem::Separator);
        items.push(self.history_menu());
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "退出".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| tray.dispatch(Action::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }

    /// 菜单展开前重建一次，保证历史列表不是上次打开时的陈旧内容。
    /// ksni 在该 hook 内会重跑 `menu()`，这里无需自己做任何事。
    fn menu_about_to_show(&mut self) {}
}

impl OwlTray {
    /// 「粘贴历史」子菜单：点一项即把它写回剪贴板，之后用系统原生 Ctrl+V 粘贴。
    fn history_menu(&self) -> MenuItem<Self> {
        let cfg = &config::get().history;
        let mut submenu: Vec<MenuItem<Self>> = Vec::new();

        if !cfg.enabled {
            submenu.push(
                StandardItem {
                    label: "历史记录已在配置中关闭".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            let files = history::entries(&history::dir());
            for (index, path) in files.into_iter().take(cfg.keep()).enumerate() {
                let tag = if index == 0 {
                    "最近一张".to_string()
                } else {
                    format!("往前第 {index} 张")
                };
                submenu.push(
                    StandardItem {
                        label: format!("{tag}　{}", history::label_of(&path)),
                        icon_data: thumbnail(&path),
                        activate: Box::new(move |tray: &mut Self| {
                            tray.dispatch(Action::RestoreHistory(index))
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            if submenu.is_empty() {
                submenu.push(
                    StandardItem {
                        label: "（暂无历史）".into(),
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }

        SubMenu {
            label: "粘贴历史".into(),
            icon_name: "edit-paste".into(),
            submenu,
            ..Default::default()
        }
        .into()
    }
}

/// 把历史图缩成 22×22 的 PNG 字节流。ksni 的 `icon_data` 要求 PNG 编码，
/// 若直接塞整张全屏图，菜单每次展开都要经 D-Bus 搬运数 MB 数据。
/// 任一步失败就返回空字节，托盘会退回没有图标的纯文字条目。
fn thumbnail(path: &Path) -> Vec<u8> {
    let Ok(pixbuf) = Pixbuf::from_file_at_size(path, 22, 22) else {
        return Vec::new();
    };
    pixbuf.save_to_bufferv("png", &[]).unwrap_or_default()
}
