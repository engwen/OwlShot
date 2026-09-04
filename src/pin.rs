//! 需求5：贴图窗口 —— 把剪贴板里的图片贴成屏幕上的一块浮动窗口。
//!
//! Wayland 限制（已与用户确认接受降级）：
//!   GNOME 不支持 wlr-layer-shell，GTK4 也没有 X11 那种 keep-above 接口，
//!   因此只能做到「贴出瞬间位于最前面」，切到别的窗口后会被盖住；
//!   窗口位置同样不能由程序指定，移动必须交给合成器（`gdk_toplevel_begin_move`）。
//!
//! 交互：左键拖动移动、滚轮缩放、Esc 或右键关闭。
//!
//! 线程：由工作线程调用，窗口构建投递到 GLib 主线程；
//! 直到窗口关闭才回传结果，否则单次模式（`owlshot --paste`）会在贴出瞬间就退出。

use anyhow::{Context as _, Result, anyhow};
use gtk4::gdk;
use gtk4::prelude::*;
use gtk4::{
    ContentFit, EventControllerKey, EventControllerScroll, EventControllerScrollFlags, GestureClick,
    GestureDrag, Picture, Window, glib,
};
use std::cell::Cell;
use std::rc::Rc;

/// 缩放下限 / 上限，相对初始逻辑尺寸。
const MIN_SCALE: f64 = 0.1;
const MAX_SCALE: f64 = 4.0;
/// 每格滚轮的缩放倍率。
const ZOOM_STEP: f64 = 1.1;
/// 超过该位移才认定为拖动，避免单击就把窗口交给合成器接管。
const DRAG_SLOP: f64 = 4.0;
/// 初始尺寸最多占屏幕的比例：贴一张全屏图时不至于糊满整个桌面。
const MAX_SCREEN_RATIO: f64 = 0.9;

/// 把剪贴板里的图片贴到屏幕上。
///
/// `wait_for_close` 为真（`owlshot --paste` 单次模式）时一直等到窗口关闭，
/// 否则进程会在贴出瞬间退出把窗口带走；常驻模式传假，工作循环立刻回去响应托盘。
pub async fn paste_from_clipboard(wait_for_close: bool) -> Result<()> {
    let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);

    glib::MainContext::default().invoke(move || {
        // 读剪贴板是异步操作，且必须在持有主上下文的线程上发起。
        glib::spawn_future_local(async move {
            if let Err(err) = open(tx.clone(), wait_for_close).await {
                let _ = tx.try_send(Err(format!("{err:#}")));
            }
        });
    });

    rx.recv()
        .await
        .context("GLib 主循环未响应贴图任务")?
        .map_err(|err| anyhow!(err))
}

/// 结果只允许回传一次：常驻模式在 present 后就放行，关窗回调不该再发一次。
struct Done {
    tx: async_channel::Sender<Result<(), String>>,
    sent: Cell<bool>,
}

impl Done {
    fn send(&self, result: Result<(), String>) {
        if self.sent.replace(true) {
            return;
        }
        // 容量 1 且只发一次，try_send 不会丢结果，也不会阻塞主循环。
        let _ = self.tx.try_send(result);
    }
}

/// 主线程：读剪贴板取图 → 建无边框浮动窗口。
async fn open(tx: async_channel::Sender<Result<(), String>>, wait_for_close: bool) -> Result<()> {
    let display = gdk::Display::default().context("拿不到 GDK Display，请确认运行在图形会话中")?;
    let texture = display
        .clipboard()
        .read_texture_future()
        .await
        .map_err(|err| anyhow!("读取剪贴板图片失败：{err}"))?
        .context("剪贴板里没有图片；先截一张图，或用托盘「粘贴历史」取回一张")?;

    let done = Rc::new(Done {
        tx,
        sent: Cell::new(false),
    });
    let base = initial_size(&texture);

    let window = Window::new();
    window.set_decorated(false);
    window.set_title(Some("OwlShot 贴图"));
    window.set_default_size(base.0, base.1);

    // 让图随窗口尺寸拉伸，并允许缩到比原图更小：窗口多大就画多大。
    let picture = Picture::for_paintable(&texture);
    picture.set_can_shrink(true);
    picture.set_content_fit(ContentFit::Fill);
    window.set_child(Some(&picture));

    wire_drag(&window);
    wire_zoom(&window, base);
    wire_close(&window);

    {
        let done = done.clone();
        window.connect_close_request(move |_| {
            done.send(Ok(()));
            glib::Propagation::Proceed
        });
    }

    window.present();
    if !wait_for_close {
        // 窗口已映射，GTK 自己持有引用，交给主循环继续活着即可。
        done.send(Ok(()));
    }
    println!("[owlshot] 已贴出剪贴板图片：拖动可移动、滚轮缩放、Esc 或右键关闭。");
    println!("[owlshot] 注意：Wayland 无置顶接口，切到其他窗口后贴图会被盖住。");
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
