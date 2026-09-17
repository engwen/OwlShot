//! 贴图窗口 —— 把剪贴板里的图片贴到屏幕上，用于数据对比（类似 Snipaste 贴图功能）。
//!
//! 功能：置顶（keep_above）、左键拖动、滚轮缩放、Esc/关闭按钮退出、悬停显示控制栏。
//!
//! 线程模型：贴图窗口必须在 GLib 主线程创建（剪贴板读取、窗口 present 都需要主上下文），
//! 但工作循环跑在 tokio 线程上。因此用 `std::sync::mpsc` 从工作线程向主线程发送贴图请求，
//! 主线程通过 GLib timeout 轮询接收，处理后通过 oneshot channel 回传结果。

use anyhow::{Context as _, Result, anyhow};
use gtk4::gdk;
use gtk4::prelude::*;
use gtk4::{
    Align, Button, ContentFit, EventControllerKey, EventControllerMotion,
    EventControllerScroll, EventControllerScrollFlags, GestureClick, GestureDrag, Label,
    Orientation, Overlay, Picture, Window, glib,
};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::OnceLock;

/// 设置窗口置顶/取消置顶。
/// 通过 GObject 属性 `keep-above` 直写，兼容所有支持该属性的合成器。
fn set_keep_above(window: &Window, keep: bool) {
    let val = glib::Value::from(keep);
    window.set_property("keep-above", &val);
}

/// 剪贴板为空时的降级方案：从历史目录读取最近一次截图文件。
fn load_latest_screenshot() -> Result<PathBuf> {
    let dir = crate::history::dir();
    let entries = crate::history::entries(&dir);
    entries
        .into_iter()
        .next()
        .with_context(|| format!("历史目录 {} 为空，请先截图一次", dir.display()))
}

/// 缩放下限 / 上限，相对初始逻辑尺寸。
const MIN_SCALE: f64 = 0.1;
const MAX_SCALE: f64 = 4.0;
/// 每格滚轮的缩放倍率。
const ZOOM_STEP: f64 = 1.1;
/// 超过该位移才认定为拖动，避免单击就把窗口交给合成器接管。
const DRAG_SLOP: f64 = 4.0;
/// 初始尺寸最多占屏幕的比例：贴一张全屏图时不至于糊满整个桌面。
const MAX_SCREEN_RATIO: f64 = 0.9;

/// 工作线程 → 主线程的贴图请求。
pub struct PinRequest {
    /// 主线程处理完毕后通过此 oneshot 回传结果。
    pub result_tx: tokio::sync::oneshot::Sender<Result<()>>,
    /// 单次模式（`owlshot --paste`）为真：窗口关闭前不回传，进程不会提前退出。
    pub wait_for_close: bool,
}

/// `mpsc::Sender<PinRequest>`，从主线程初始化后交给工作线程。
static PASTE_TX: OnceLock<mpsc::Sender<PinRequest>> = OnceLock::new();

/// 在主线程调用：初始化贴图 channel，返回 receiver 并安装 GLib timeout 轮询。
pub fn init_paste_channel() {
    let (tx, rx) = mpsc::channel::<PinRequest>();
    let _ = PASTE_TX.set(tx);

    // 每 50ms 轮询一次，检查是否有贴图请求。
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        while let Ok(req) = rx.try_recv() {
            let wait = req.wait_for_close;
            glib::spawn_future_local(async move {
                handle_paste(wait, req.result_tx).await;
            });
        }
        glib::ControlFlow::Continue
    });
}

/// 工作线程调用：向主线程发送贴图请求，阻塞等待结果。
pub async fn paste_from_clipboard(wait_for_close: bool) -> Result<()> {
    let tx = PASTE_TX
        .get()
        .context("贴图 channel 未初始化（init_paste_channel 未在主线程调用）")?;
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let _ = tx.send(PinRequest {
        result_tx,
        wait_for_close,
    });
    result_rx
        .await
        .context("主线程未响应贴图请求")?
}

/// 主线程：读剪贴板 → 建浮动窗口。由 GLib timeout 轮询触发。
async fn handle_paste(
    wait_for_close: bool,
    result_tx: tokio::sync::oneshot::Sender<Result<()>>,
) {
    // try_open 内部处理所有错误并通过 result_tx 回传，此处只需驱动调用。
    let _ = try_open(wait_for_close, result_tx).await;
}

/// 实际贴图逻辑：创建窗口并通过 `result_tx` 回传结果。
///
/// 窗口布局：Overlay 覆盖一个 control_bar（置顶 + 关闭），
/// 悬停在图片上时 control_bar 可见，离开时隐藏。
async fn try_open(
    wait_for_close: bool,
    result_tx: tokio::sync::oneshot::Sender<Result<()>>,
) -> Result<()> {
    let display = match gdk::Display::default() {
        Some(d) => d,
        None => {
            let _ = result_tx.send(Err(anyhow!("拿不到 GDK Display，请确认运行在图形会话中")));
            return Ok(());
        }
    };

    let texture = match display
        .clipboard()
        .read_texture_future()
        .await
    {
        Ok(Some(tex)) => tex,
        Ok(None) | Err(_) => {
            // 剪贴板为空（wl-copy 未安装或失败），自动读取最近一次截图文件。
            match load_latest_screenshot() {
                Ok(path) => {
                    match gdk::Texture::from_filename(&path) {
                        Ok(tex) => tex,
                        Err(err) => {
                            let _ = result_tx.send(Err(anyhow!("无法加载截图文件 {}：{err}", path.display())));
                            return Ok(());
                        }
                    }
                }
                Err(err) => {
                    let _ = result_tx.send(Err(anyhow!("剪贴板无图片且找不到截图历史：{err}；请先截图一次")));
                    return Ok(());
                }
            }
        }
    };

    let base = initial_size(&texture);

    let window = Window::new();
    window.set_decorated(false);
    window.set_title(Some("OwlShot 贴图"));
    window.set_default_size(base.0, base.1);
    // 置顶：在 GNOME/KDE/Sway 等主流 Wayland 合成器上均有效。
    set_keep_above(&window, true);

    // --- 图片 ---
    let picture = Picture::for_paintable(&texture);
    picture.set_can_shrink(true);
    picture.set_content_fit(ContentFit::Fill);

    // --- 控制栏：置顶切换 + 关闭 ---
    let control_bar = gtk4::Box::new(Orientation::Horizontal, 4);
    control_bar.set_margin_top(4);
    control_bar.set_margin_start(4);
    control_bar.set_halign(Align::Start);
    control_bar.set_valign(Align::Start);
    control_bar.add_css_class("osd"); // 半透明背景

    // 置顶按钮（toggle）：初始已置顶
    let pin_label = Label::new(Some("📌"));
    let pin_btn = Button::builder().child(&pin_label).tooltip_text("取消置顶").build();
    let win_ref = window.clone();
    let pin_state = Rc::new(Cell::new(true));
    {
        let pin_state = pin_state.clone();
        pin_btn.connect_clicked(move |btn| {
            let next = !pin_state.get();
            pin_state.set(next);
            set_keep_above(&win_ref, next);
            btn.set_tooltip_text(Some(if next { "取消置顶" } else { "置顶" }));
        });
    }

    // 关闭按钮
    let close_btn = Button::builder()
        .label("✕")
        .tooltip_text("关闭（Esc）")
        .build();
    {
        let win_ref = window.clone();
        close_btn.connect_clicked(move |_| win_ref.close());
    }

    control_bar.append(&pin_btn);
    control_bar.append(&close_btn);

    // --- Overlay：图片 + 控制栏浮层 ---
    let overlay = Overlay::new();
    overlay.set_child(Some(&picture));
    overlay.add_overlay(&control_bar);

    window.set_child(Some(&overlay));

    // 悬停检测：鼠标进入窗口区域时显示控制栏，离开时隐藏。
    control_bar.set_visible(false);
    {
        let bar = control_bar.clone();
        let motion = EventControllerMotion::new();
        motion.connect_enter(move |_, _, _| bar.set_visible(true));
        let bar2 = control_bar.clone();
        motion.connect_leave(move |_| bar2.set_visible(false));
        overlay.add_controller(motion);
    }

    wire_drag(&window);
    wire_zoom(&window, base);
    wire_close(&window);

    window.present();

    // 用 Rc 包装 oneshot sender，让闭包和后续代码都能拿到。
    let result_tx = Rc::new(Cell::new(Some(result_tx)));

    if wait_for_close {
        // 单次模式：窗口关闭时通过 oneshot 回传结果，进程不会提前退出。
        let tx = result_tx.clone();
        window.connect_close_request(move |_| {
            if let Some(tx) = tx.take() {
                let _ = tx.send(Ok(()));
            }
            glib::Propagation::Proceed
        });
    } else {
        // 常驻模式：窗口已映射，立即回传，工作循环继续响应托盘。
        if let Some(tx) = result_tx.take() {
            let _ = tx.send(Ok(()));
        }
    }

    println!("[owlshot] 贴图已置顶显示：悬停控制栏可切换置顶/关闭，滚轮缩放，Esc 关闭。");
    Ok(())
}

/// 初始逻辑尺寸：以纹理像素为基准，按显示器缩放折算，再限制不超过屏幕的 90%。
fn initial_size(texture: &gdk::Texture) -> (i32, i32) {
    let (mut w, mut h) = (texture.width() as f64, texture.height() as f64);
    if let Some(monitor) = first_monitor() {
        // 纹理是物理像素，窗口尺寸是逻辑像素，HiDPI / 分数缩放下必须先除掉缩放比，
        // 否则 2x 屏上贴出来会是两倍大。
        let scale = monitor.scale().max(1.0);
        w /= scale;
        h /= scale;

        let geo = monitor.geometry();
        let limit_w = geo.width() as f64 * MAX_SCREEN_RATIO;
        let limit_h = geo.height() as f64 * MAX_SCREEN_RATIO;
        let shrink = (limit_w / w).min(limit_h / h);
        if shrink < 1.0 {
            w *= shrink;
            h *= shrink;
        }
    }
    (w.round().max(1.0) as i32, h.round().max(1.0) as i32)
}

/// 默认显示器（首屏）。
fn first_monitor() -> Option<gdk::Monitor> {
    let display = gdk::Display::default()?;
    display
        .monitors()
        .item(0)
        .and_then(|obj| obj.downcast::<gdk::Monitor>().ok())
}

/// 左键拖动移动窗口。
///
/// Wayland 下客户端不能自己设定窗口坐标，只能把拖动交给合成器：
/// 拿到本次事件的 device / button / timestamp，调 `gdk_toplevel_begin_move`。
fn wire_drag(window: &Window) {
    let drag = GestureDrag::new();
    drag.set_button(gdk::BUTTON_PRIMARY);
    let win = window.clone();
    drag.connect_drag_update(move |gesture, dx, dy| {
        // 小位移当抖动忽略，否则一次单击也会触发合成器接管。
        if dx.hypot(dy) < DRAG_SLOP {
            return;
        }
        let Some((sx, sy)) = gesture.start_point() else {
            return;
        };
        let Some(device) = gesture.device() else {
            return;
        };
        let Some(surface) = win.surface() else {
            return;
        };
        // Toplevel 是 Interface（@requires Surface），方向与 downcast 相反，只能 dynamic_cast。
        let Ok(toplevel) = surface.dynamic_cast::<gdk::Toplevel>() else {
            return;
        };
        // 交给合成器后本手势不会再收到事件，主动复位避免残留状态。
        gesture.set_state(gtk4::EventSequenceState::Denied);
        toplevel.begin_move(
            &device,
            gesture.current_button() as i32,
            sx,
            sy,
            gesture.current_event_time(),
        );
    });
    window.add_controller(drag);
}

/// 滚轮缩放：直接改窗口尺寸，`ContentFit::Fill` 会让图跟着拉伸。
fn wire_zoom(window: &Window, base: (i32, i32)) {
    let scroll = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
    let win = window.clone();
    let factor = Rc::new(Cell::new(1.0f64));
    scroll.connect_scroll(move |_, _, dy| {
        // 向上滚（dy < 0）放大。
        let next = if dy < 0.0 {
            factor.get() * ZOOM_STEP
        } else {
            factor.get() / ZOOM_STEP
        }
        .clamp(MIN_SCALE, MAX_SCALE);
        if next == factor.get() {
            return glib::Propagation::Stop;
        }
        factor.set(next);
        let w = ((base.0 as f64 * next).round() as i32).max(1);
        let h = ((base.1 as f64 * next).round() as i32).max(1);
        // 已映射的窗口 set_default_size 不生效，只有请求尺寸能让合成器重新分配。
        win.set_size_request(w, h);
        glib::Propagation::Stop
    });
    window.add_controller(scroll);
}

/// Esc 或右键关闭贴图。
fn wire_close(window: &Window) {
    let keys = EventControllerKey::new();
    {
        let win = window.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if matches!(key, gdk::Key::Escape) {
                win.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    window.add_controller(keys);

    let click = GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    {
        let win = window.clone();
        click.connect_pressed(move |_, _, _, _| win.close());
    }
    window.add_controller(click);
}
