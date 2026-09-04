//! 全局快捷键：唯一实现途径是 org.freedesktop.portal.GlobalShortcuts。
//!
//! 硬约束：绝不直接抓取键盘输入（不使用 X11 grab、不使用 evdev），
//! 快捷键必须由用户在 portal 的系统弹窗中授权。
//!
//! 现实约束：GNOME 46 / xdg-desktop-portal 1.18 的后端尚未实现该接口，
//! 此时本模块只打印降级提示，引导用户改用 `owlshot --region` / `--full`
//! 配合桌面环境自带的「自定义快捷键」绑定，托盘功能不受影响。

use crate::Action;
use crate::config::ShortcutsConfig;
use anyhow::{Context, Result};
use ashpd::desktop::global_shortcuts::{
    Activated, BindShortcutsOptions, GlobalShortcuts, NewShortcut,
};
use ashpd::desktop::{CreateSessionOptions, Session};
use futures_util::StreamExt;

/// 快捷键 id，portal 回调里用它区分动作。
const ID_REGION: &str = "capture-region";
const ID_FULLSCREEN: &str = "capture-fullscreen";

/// Session 一旦 drop（或 close）快捷键即失效，必须持有到进程退出。
pub struct ShortcutGuard {
    session: Session<GlobalShortcuts>,
}

impl ShortcutGuard {
    pub async fn close(self) {
        if let Err(err) = self.session.close().await {
            eprintln!("[owlshot] 关闭快捷键会话失败：{err}");
        }
    }
}

/// 注册全局快捷键并在后台任务里派发动作。
///
/// 返回 `Ok(None)` 表示当前桌面不支持该 portal 接口（已打印降级提示）。
pub async fn register(
    cfg: &ShortcutsConfig,
    tx: async_channel::Sender<Action>,
) -> Result<Option<ShortcutGuard>> {
    if !cfg.enabled {
        println!("[owlshot] 配置已关闭全局快捷键（shortcuts.enabled = false）。");
        return Ok(None);
    }

    let proxy = match GlobalShortcuts::new().await {
        Ok(proxy) => proxy,
        Err(err) => {
            print_fallback_hint(&err.to_string());
            return Ok(None);
        }
    };

    let session = match proxy.create_session(CreateSessionOptions::default()).await {
        Ok(session) => session,
        Err(err) => {
            print_fallback_hint(&err.to_string());
            return Ok(None);
        }
    };

    let shortcuts = [
        new_shortcut(ID_REGION, "框选截图", &cfg.region),
        new_shortcut(ID_FULLSCREEN, "全屏截图", &cfg.fullscreen),
    ];

    // 无自有窗口，parent window identifier 传 None。
    let request = proxy
        .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
        .await
        .context("向 portal 注册全局快捷键失败")?;
    let bound = request
        .response()
        .context("用户拒绝了全局快捷键授权请求")?;

    for shortcut in bound.shortcuts() {
        println!(
            "[owlshot] 快捷键已注册：{} → {}",
            shortcut.trigger_description(),
            shortcut.description()
        );
    }

    let stream = proxy
        .receive_activated()
        .await
        .context("订阅快捷键触发信号失败")?;
    tokio::spawn(dispatch_loop(stream, tx));

    Ok(Some(ShortcutGuard { session }))
}

fn new_shortcut(id: &str, description: &str, trigger: &str) -> NewShortcut {
    let shortcut = NewShortcut::new(id, description);
    let trigger = trigger.trim();
    // 触发器为空时交给 portal 让用户自行设置。
    if trigger.is_empty() {
        shortcut
    } else {
        shortcut.preferred_trigger(trigger)
    }
}

async fn dispatch_loop(
    mut stream: impl futures_util::Stream<Item = Activated> + Unpin,
    tx: async_channel::Sender<Action>,
) {
    while let Some(activated) = stream.next().await {
        let action = match activated.shortcut_id() {
            ID_REGION => Action::CaptureRegion,
            ID_FULLSCREEN => Action::CaptureFullScreen,
            other => {
                eprintln!("[owlshot] 收到未知快捷键 id：{other}");
                continue;
            }
        };
        // 不阻塞 D-Bus 任务：截图正在进行时直接丢弃重复触发。
        if tx.try_send(action).is_err() {
            eprintln!("[owlshot] 快捷键动作未能入队（上一次截图仍在处理）。");
        }
    }
}

fn print_fallback_hint(reason: &str) {
    eprintln!("[owlshot] 当前桌面的 xdg-desktop-portal 不提供 GlobalShortcuts 接口：{reason}");
    eprintln!("[owlshot] 已降级：请在「设置 → 键盘 → 自定义快捷键」中绑定以下命令：");
    eprintln!("[owlshot]   框选截图：owlshot --region");
    eprintln!("[owlshot]   全屏截图：owlshot --full");
}
