//! 阶段2 自绘选区 + 标注编辑器。
//!
//! 设计约束（用户明确否决 portal 交互式对话框的观感）：
//!   portal 只负责抓一张静态全屏 PNG，选区交互全部由本模块 GTK4 + Cairo 自绘，
//!   手柄 6px 见方、边框 1px，避免 GNOME 原生截屏那种大圆点。
//! 依旧不引入 wlr-layer-shell（GNOME 不支持），用无边框 fullscreen 窗口覆盖屏幕。
//!
//! 所有 GTK 调用都必须在 GLib 主线程；Pixbuf 不是 Send，因此裁剪导出也在主线程完成，
//! 只把最终结果（PNG 路径，或 Ctrl+C 的「已复制」事实）回传给工作线程。

use crate::annotate::{
    Canvas, PALETTE, Rect, STROKE_MAX, STROKE_MIN, Shape, Style, Tool, draw_preedit,
    draw_text_caret, text_caret_metrics,
};
use crate::capture;
use crate::clipboard::{self, Backend};
use crate::toolbar::{Item, Toolbar};
use anyhow::{Context as _, Result, anyhow};
use gtk4::gdk;
use gtk4::gdk_pixbuf::{Colorspace, Pixbuf};
use gtk4::prelude::*;
use gtk4::{cairo, glib};
use gtk4::{
    DrawingArea, EventControllerKey, EventControllerMotion, EventControllerScroll,
    EventControllerScrollFlags, GestureClick, GestureDrag, IMMulticontext, Window,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// 选区外的暗化程度。
const MASK_ALPHA: f64 = 0.45;
/// 手柄边长（逻辑像素），刻意做小。
const HANDLE: f64 = 6.0;
/// 手柄命中判定的容差半径。
const HIT_SLOP: f64 = 7.0;
/// 小于该边长的选区视为误点击。
const MIN_SIZE: f64 = 4.0;

/// 编辑器的收尾结果。
pub enum Shot {
    /// 裁剪后已落盘到图片目录（Enter / 双击 / 工具栏保存）
    Saved(PathBuf),
    /// 仅复制到剪贴板、未生成文件（Ctrl+C）
    Copied(Backend),
}

/// 打开全屏选区编辑器；`None` 表示用户取消。
///
/// 由工作线程调用：窗口构建投递到 GLib 主线程，结果通过 channel 等回。
pub async fn select_region(shot: &Path) -> Result<Option<Shot>> {
    let shot = shot.to_path_buf();
    let (tx, rx) = async_channel::bounded::<Result<Option<Shot>, String>>(1);

    glib::MainContext::default().invoke(move || {
        if let Err(err) = build_window(&shot, tx.clone()) {
            let _ = tx.send_blocking(Err(format!("{err:#}")));
        }
    });

    rx.recv()
        .await
        .context("GLib 主循环未响应选区任务")?
        .map_err(|err| anyhow!(err))
}

/// 八个方向的调整手柄。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl Handle {
    const ALL: [Handle; 8] = [
        Handle::TopLeft,
        Handle::Top,
        Handle::TopRight,
        Handle::Right,
        Handle::BottomRight,
        Handle::Bottom,
        Handle::BottomLeft,
        Handle::Left,
    ];

    /// 手柄中心点（逻辑坐标）。
    fn center(self, r: &Rect) -> (f64, f64) {
        let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
        let (cx, cy) = ((l + rt) / 2.0, (t + b) / 2.0);
        match self {
            Handle::TopLeft => (l, t),
            Handle::Top => (cx, t),
            Handle::TopRight => (rt, t),
            Handle::Right => (rt, cy),
            Handle::BottomRight => (rt, b),
            Handle::Bottom => (cx, b),
            Handle::BottomLeft => (l, b),
            Handle::Left => (l, cy),
        }
    }

    fn cursor(self) -> &'static str {
        match self {
            Handle::TopLeft => "nw-resize",
            Handle::Top => "n-resize",
            Handle::TopRight => "ne-resize",
            Handle::Right => "e-resize",
            Handle::BottomRight => "se-resize",
            Handle::Bottom => "s-resize",
            Handle::BottomLeft => "sw-resize",
            Handle::Left => "w-resize",
        }
    }
}

/// 当前拖拽行为。
enum Drag {
    /// 空白处拉出新选区
    Create,
    /// 选区内部整体平移
    Move { origin: Rect },
    /// 拖动某个手柄调整边界
    Resize { handle: Handle, origin: Rect },
    /// 用当前标注工具拖出一个图元
    Draw,
}

struct State {
    /// portal 抓到的全屏原图，物理像素分辨率。
    shot: Pixbuf,
    /// 绘图区逻辑尺寸，每次绘制时刷新，用于逻辑坐标 → 物理像素换算（HiDPI 分数缩放同样适用）。
    view: (f64, f64),
    rect: Option<Rect>,
    drag: Option<Drag>,
    hover: Option<Handle>,
    /// 当前标注工具；`None` 表示处于选区调整模式。
    tool: Option<Tool>,
    /// 已落定的标注，同时充当撤销栈。
    shapes: Vec<Shape>,
    /// 撤销后被弹出的图元，重做时按后进先出推回 `shapes`。
    redo: Vec<Shape>,
    /// 正在拖出、尚未落定的图元。
    active: Option<Shape>,
    /// 文字输入态：`active` 是一段正在敲字的文本，键盘事件优先给它。
    editing: bool,
    /// 鼠标当前悬停的工具栏按钮。
    bar_hover: Option<Item>,
    /// 新建图元时使用的样式（颜色 / 线宽 / 序号）。
    style: Style,
    /// 输入法预编辑串（还没上屏的拼音）+ 串内字符光标位置。
    ///
    /// Wayland 下候选窗由 ibus 自己弹，但这段未确认文本必须客户端自绘，否则用户看不见输入内容。
    preedit: (String, i32),
}

impl State {
    /// 命中测试：优先手柄，其次选区内部。
    fn handle_at(&self, x: f64, y: f64) -> Option<Handle> {
        let rect = self.rect.as_ref()?;
        Handle::ALL.into_iter().find(|h| {
            let (hx, hy) = h.center(rect);
            (x - hx).abs() <= HIT_SLOP && (y - hy).abs() <= HIT_SLOP
        })
    }

    /// 标注模式需要一个已确定的选区作为画布。
    fn drawing(&self) -> bool {
        self.tool.is_some() && self.rect.is_some()
    }

    /// 丢弃最近一个标注（压入重做栈）；已无标注时返回 false，交给调用方决定后续行为。
    fn undo(&mut self) -> bool {
        match self.shapes.pop() {
            Some(shape) => {
                self.redo.push(shape);
                true
            }
            None => false,
        }
    }

    /// 把最近撤销的标注放回画布；重做栈为空时返回 false。
    fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(shape) => {
                self.shapes.push(shape);
                true
            }
            None => false,
        }
    }

    /// 把 `active` 落定入栈（无内容的直接丢弃），并退出文字输入态。
    ///
    /// 新图元一旦落定，之前的重做记录即失效（与常规编辑器一致）。
    fn commit_active(&mut self) {
        if let Some(shape) = self.active.take()
            && shape.is_meaningful()
        {
            // 序号计数器每落定一个就自增，下一个自动接着编号。
            if matches!(shape, Shape::Counter { .. }) {
                self.style.counter += 1;
            }
            self.shapes.push(shape);
            self.redo.clear();
        }
        self.editing = false;
        // 未确认的拼音随图元一起作废，避免下一段文字继承上一段的预编辑。
        self.preedit = (String::new(), 0);
    }

    /// 绘制上下文：底图 + 当前缩放比。
    fn canvas(&self) -> Canvas<'_> {
        Canvas {
            shot: &self.shot,
            scale: pixel_scale(self),
        }
    }

    /// 当前选区对应的工具栏布局；选区太小或不存在时没有工具栏。
    ///
    /// 每次都按当前选区重算而不缓存：布局是纯计算，且能天然跟随选区移动。
    fn toolbar(&self) -> Option<Toolbar> {
        let rect = self.rect?;
        if rect.width() < MIN_SIZE || rect.height() < MIN_SIZE {
            return None;
        }
        let (vw, vh) = self.view;
        Some(Toolbar::layout(&rect, vw, vh))
    }
}

/// 结果只允许发送一次：确认与取消可能被多个回调触发（Esc / 关闭窗口 / 双击）。
struct Outcome {
    tx: async_channel::Sender<Result<Option<Shot>, String>>,
    sent: RefCell<bool>,
}

impl Outcome {
    /// 回传结果并关窗。关窗动作**不受** `sent` 开关约束，
    /// 否则一旦结果已发出（例如被 close-request 抢先），窗口就会永久留在屏幕上不再响应。
    fn finish(&self, window: &Window, result: Result<Option<Shot>, String>) {
        self.send(result);
        window.close();
    }

    /// 只回传结果、不碰窗口：供 close-request 回调使用，避免与 `close()` 互相递归。
    fn send(&self, result: Result<Option<Shot>, String>) {
        if self.sent.replace(true) {
            return;
        }
        let _ = self.tx.send_blocking(result);
    }
}

fn build_window(
    shot: &Path,
    tx: async_channel::Sender<Result<Option<Shot>, String>>,
) -> Result<()> {
    let pixbuf = Pixbuf::from_file(shot)
        .with_context(|| format!("无法解码全屏截图 {}", shot.display()))?;

    let state = Rc::new(RefCell::new(State {
        shot: pixbuf,
        view: (1.0, 1.0),
        rect: None,
        drag: None,
        hover: None,
        tool: None,
        shapes: Vec::new(),
        redo: Vec::new(),
        active: None,
        editing: false,
        bar_hover: None,
        style: Style {
            color: crate::annotate::DEFAULT_COLOR,
            stroke: crate::annotate::DEFAULT_STROKE,
            counter: 1,
        },
        preedit: (String::new(), 0),
    }));

    let window = Window::new();
    window.set_decorated(false);
    window.set_title(Some("OwlShot 选区"));
    // Wayland 下窗口无法自主定位，只能靠 fullscreen 覆盖；指定显示器避免多屏时落错屏。
    match first_monitor() {
        Some(monitor) => window.fullscreen_on_monitor(&monitor),
        None => window.fullscreen(),
    }

    let area = DrawingArea::new();
    // GTK4 里 can-focus 默认已是 true，真正允许控件成为焦点的是 focusable（DrawingArea 默认 false）。
    area.set_focusable(true);
    area.set_cursor(gdk::Cursor::from_name("crosshair", None).as_ref());
    window.set_child(Some(&area));

    // 输入法上下文：Wayland 下中文输入必须走 IMContext，
    // 否则只能拿到 keyval（拼音键），既看不到预编辑串也拿不到汉字。
    let im = IMMulticontext::new();
    im.set_client_widget(Some(&area));

    let outcome = Rc::new(Outcome {
        tx,
        sent: RefCell::new(false),
    });

    {
        let state = state.clone();
        area.set_draw_func(move |_, cr, w, h| {
            let mut state = state.borrow_mut();
            state.view = (w as f64, h as f64);
            draw(cr, &state);
        });
    }

    wire_drag(&area, &state, &im);
    wire_motion(&area, &state);
    wire_scroll(&area, &state);
    wire_click(&area, &state, &window, &outcome);
    wire_keys(&state, &window, &area, &outcome, &im);
    wire_im(&area, &state, &im);

    {
        // 窗口被外部关掉（如合成器强制关闭）时也要放行等待中的工作线程。
        let outcome = outcome.clone();
        window.connect_close_request(move |window| {
            outcome.finish(window, Ok(None));
            glib::Propagation::Proceed
        });
    }

    window.present();
    area.grab_focus();
    Ok(())
}

/// 默认显示器（首屏）；单屏场景即为唯一那块。
fn first_monitor() -> Option<gdk::Monitor> {
    let display = gdk::Display::default()?;
    display
        .monitors()
        .item(0)
        .and_then(|obj| obj.downcast::<gdk::Monitor>().ok())
}

/// 逻辑坐标 → 原图物理像素的缩放比。
/// 全屏窗口的逻辑尺寸与原图物理尺寸之比即为实际生效的缩放（含 HiDPI 分数缩放）。
fn pixel_scale(state: &State) -> (f64, f64) {
    let (vw, vh) = state.view;
    if vw <= 0.0 || vh <= 0.0 {
        return (1.0, 1.0);
    }
    (
        state.shot.width() as f64 / vw,
        state.shot.height() as f64 / vh,
    )
}

fn draw(cr: &cairo::Context, state: &State) {
    let (vw, vh) = state.view;
    let (sx, sy) = pixel_scale(state);

    // 底图：把物理像素原图按逻辑尺寸铺满，分数缩放下不会出现双倍分辨率错位。
    let _ = cr.save();
    cr.scale(1.0 / sx, 1.0 / sy);
    cr.set_source_pixbuf(&state.shot, 0.0, 0.0);
    let _ = cr.paint();
    let _ = cr.restore();

    // 全屏暗化。
    cr.set_source_rgba(0.0, 0.0, 0.0, MASK_ALPHA);
    let _ = cr.paint();

    let Some(rect) = state.rect else {
        draw_hint(cr, vw, vh);
        return;
    };

    let (l, t, w, h) = (rect.left(), rect.top(), rect.width(), rect.height());

    // 选区内还原原始亮度。
    let _ = cr.save();
    cr.rectangle(l, t, w, h);
    cr.clip();
    cr.scale(1.0 / sx, 1.0 / sy);
    cr.set_source_pixbuf(&state.shot, 0.0, 0.0);
    let _ = cr.paint();
    let _ = cr.restore();

    // 标注一律裁剪在选区内绘制，与导出结果保持一致。
    let _ = cr.save();
    cr.rectangle(l, t, w, h);
    cr.clip();
    draw_shapes(cr, state);
    let _ = cr.restore();

    // 1px 细边框，偏移半像素让描边落在整像素上。
    cr.set_antialias(cairo::Antialias::None);
    cr.set_line_width(1.0);
    cr.set_source_rgba(0.20, 0.60, 1.0, 0.95);
    cr.rectangle(l + 0.5, t + 0.5, w - 1.0, h - 1.0);
    let _ = cr.stroke();
    cr.set_antialias(cairo::Antialias::Default);

    // 标注模式下隐藏手柄，避免与图元抢命中区。
    if !state.drawing() {
        draw_handles(cr, &rect, state.hover);
    }
    draw_size_label(cr, &rect, vw, vh, sx, sy);

    // 拖拽过程中隐藏工具栏，避免它跟着选区一起抖动。
    if state.drag.is_none()
        && let Some(bar) = state.toolbar()
    {
        bar.draw(
            cr,
            state.tool,
            state.bar_hover,
            state.style.color,
            !state.shapes.is_empty(),
            !state.redo.is_empty(),
        );
    }
}

/// 已落定的标注 + 正在拖出的图元。
fn draw_shapes(cr: &cairo::Context, state: &State) {
    let canvas = state.canvas();
    for shape in &state.shapes {
        shape.draw(cr, &canvas);
    }
    if let Some(shape) = &state.active {
        shape.draw(cr, &canvas);
        // 输入态额外画一根光标，让用户知道字会落在哪。
        if state.editing {
            draw_text_caret(cr, shape);
            draw_preedit(cr, shape, &state.preedit.0, state.preedit.1);
        }
    }
}

/// 无选区时的中央提示。
fn draw_hint(cr: &cairo::Context, vw: f64, vh: f64) {
    let text = "拖动鼠标框选区域　Enter 确认　Esc 取消";
    cr.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(16.0);
    let Ok(ext) = cr.text_extents(text) else {
        return;
    };
    let (tw, th) = (ext.width(), ext.height());
    let (bx, by) = ((vw - tw) / 2.0 - 14.0, (vh - th) / 2.0 - 10.0);

    cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
    cr.rectangle(bx, by, tw + 28.0, th + 20.0);
    let _ = cr.fill();

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.92);
    cr.move_to(bx + 14.0 - ext.x_bearing(), by + 10.0 - ext.y_bearing());
    let _ = cr.show_text(text);
}

/// 8 个 6px 小方块手柄，白底蓝边，悬停时高亮。
fn draw_handles(cr: &cairo::Context, rect: &Rect, hover: Option<Handle>) {
    if rect.width() < HANDLE * 2.0 || rect.height() < HANDLE * 2.0 {
        return;
    }
    cr.set_line_width(1.0);
    for h in Handle::ALL {
        let (cx, cy) = h.center(rect);
        let half = HANDLE / 2.0;
        cr.rectangle(cx - half, cy - half, HANDLE, HANDLE);
        if hover == Some(h) {
            cr.set_source_rgb(0.20, 0.60, 1.0);
        } else {
            cr.set_source_rgb(1.0, 1.0, 1.0);
        }
        let _ = cr.fill_preserve();
        cr.set_source_rgba(0.10, 0.35, 0.75, 0.95);
        let _ = cr.stroke();
    }
}

/// 实时尺寸提示，显示的是最终导出的物理像素数。
fn draw_size_label(cr: &cairo::Context, rect: &Rect, vw: f64, vh: f64, sx: f64, sy: f64) {
    let pw = (rect.width() * sx).round() as i64;
    let ph = (rect.height() * sy).round() as i64;
    if pw <= 0 || ph <= 0 {
        return;
    }
    let text = format!("{pw} × {ph}");

    cr.select_font_face(
        "sans-serif",
        cairo::FontSlant::Normal,
        cairo::FontWeight::Normal,
    );
    cr.set_font_size(13.0);
    let Ok(ext) = cr.text_extents(&text) else {
        return;
    };
    let (bw, bh) = (ext.width() + 14.0, ext.height() + 10.0);

    // 默认贴在选区左上角外侧，空间不足时翻到内侧，避免跑出屏幕。
    let mut bx = rect.left();
    let mut by = rect.top() - bh - 6.0;
    if by < 0.0 {
        by = rect.top() + 6.0;
    }
    if bx + bw > vw {
        bx = vw - bw;
    }
    if by + bh > vh {
        by = vh - bh;
    }
    bx = bx.max(0.0);
    by = by.max(0.0);

    cr.set_source_rgba(0.0, 0.0, 0.0, 0.65);
    cr.rectangle(bx, by, bw, bh);
    let _ = cr.fill();

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
    cr.move_to(bx + 7.0 - ext.x_bearing(), by + 5.0 - ext.y_bearing());
    let _ = cr.show_text(&text);
}

/// 左键拖拽：空白处拉新选区，选区内平移，手柄上调整边界。
fn wire_drag(area: &DrawingArea, state: &Rc<RefCell<State>>, im: &IMMulticontext) {
    let gesture = GestureDrag::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);

    {
        let state = state.clone();
        let area = area.clone();
        let im = im.clone();
        gesture.connect_drag_begin(move |_, x, y| {
            // 正在输入文字时按下鼠标，视为确认当前拼音：必须在改动状态之前 reset，
            // 否则输入法回吐的未确认文本会落到下一段图元上。
            let was_editing = state.borrow().editing;
            if was_editing {
                im.reset();
            }
            // 输入法调用必须等借用释放后再做：commit / preedit 回调会重新借用 state。
            let mut opened_text = false;
            {
                let mut state = state.borrow_mut();
                // 工具栏上的按下交给点击手势处理：既不产生选区，也不落图元。
                if state.toolbar().is_some_and(|bar| bar.contains(x, y)) {
                    state.drag = None;
                    return;
                }
                // 标注模式（选区已定 + 已选工具）优先，拖拽直接产出图元而非改选区。
                if let (Some(tool), Some(rect)) = (state.tool, state.rect) {
                    let (cx, cy) = rect.clamp_point(x, y);
                    let style = state.style;
                    if tool == Tool::Text {
                        // 文字不靠拖拽成形：先把上一段落定，再在落点开一段新输入。
                        state.commit_active();
                        state.active = Some(Shape::start(tool, cx, cy, &style));
                        state.editing = true;
                        opened_text = true;
                    } else {
                        state.active = Some(Shape::start(tool, cx, cy, &style));
                        state.drag = Some(Drag::Draw);
                    }
                } else {
                    let existing = state.rect;
                    state.drag = match (state.handle_at(x, y), existing) {
                        (Some(handle), Some(origin)) => Some(Drag::Resize { handle, origin }),
                        (None, Some(origin)) if origin.contains(x, y) => {
                            Some(Drag::Move { origin })
                        }
                        _ => {
                            state.rect = Some(Rect::new(x, y, x, y));
                            Some(Drag::Create)
                        }
                    };
                }
            }
            if opened_text {
                // 丢掉上一段残留的未确认拼音，再让输入法为新的一段开始工作。
                im.reset();
                im.focus_in();
                sync_cursor_location(&state, &im);
            }
            area.queue_draw();
        });
    }

    {
        let state = state.clone();
        let area = area.clone();
        gesture.connect_drag_update(move |gesture, dx, dy| {
            let Some((sx, sy)) = gesture.start_point() else {
                return;
            };
            let mut state = state.borrow_mut();
            apply_drag(&mut state, sx, sy, dx, dy);
            area.queue_draw();
        });
    }

    {
        let state = state.clone();
        let area = area.clone();
        gesture.connect_drag_end(move |gesture, dx, dy| {
            let mut state = state.borrow_mut();
            if let Some((sx, sy)) = gesture.start_point() {
                apply_drag(&mut state, sx, sy, dx, dy);
            }
            let was_draw = matches!(state.drag, Some(Drag::Draw));
            state.drag = None;
            if was_draw {
                // 有效图元入栈（同时成为撤销栈的一层），无效的直接丢弃。
                state.commit_active();
            } else if let Some(rect) = state.rect
                && (rect.width() < MIN_SIZE || rect.height() < MIN_SIZE)
            {
                // 误点击（几乎没有位移）直接丢弃，回到无选区状态。
                state.rect = None;
            }
            state.hover = None;
            area.queue_draw();
        });
    }

    area.add_controller(gesture);
}

/// 把一次拖拽位移落到选区上；`(sx, sy)` 是按下点，`(dx, dy)` 是相对位移。
fn apply_drag(state: &mut State, sx: f64, sy: f64, dx: f64, dy: f64) {
    let (vw, vh) = state.view;
    match state.drag {
        Some(Drag::Create) => {
            state.rect = Some(Rect::new(
                sx.clamp(0.0, vw),
                sy.clamp(0.0, vh),
                (sx + dx).clamp(0.0, vw),
                (sy + dy).clamp(0.0, vh),
            ));
        }
        Some(Drag::Move { origin }) => {
            // 平移不改变尺寸，因此位移量要先被屏幕边界夹住。
            let dx = dx.clamp(-origin.left(), vw - origin.right());
            let dy = dy.clamp(-origin.top(), vh - origin.bottom());
            state.rect = Some(origin.offset(dx, dy));
        }
        Some(Drag::Resize { handle, origin }) => {
            state.rect = Some(resize(&origin, handle, dx, dy, vw, vh));
        }
        Some(Drag::Draw) => {
            // 图元被夹在选区内，导出裁剪后不会出现半截标注。
            let Some(rect) = state.rect else { return };
            let (x, y) = rect.clamp_point(sx + dx, sy + dy);
            if let Some(shape) = state.active.as_mut() {
                shape.extend_to(x, y);
            }
        }
        None => {}
    }
}

/// 按手柄方向调整边界；用绝对边（left/top/right/bottom）避免反向拖拽时坐标错乱。
fn resize(origin: &Rect, handle: Handle, dx: f64, dy: f64, vw: f64, vh: f64) -> Rect {
    let (mut l, mut t, mut r, mut b) =
        (origin.left(), origin.top(), origin.right(), origin.bottom());

    match handle {
        Handle::TopLeft => {
            l += dx;
            t += dy;
        }
        Handle::Top => t += dy,
        Handle::TopRight => {
            r += dx;
            t += dy;
        }
        Handle::Right => r += dx,
        Handle::BottomRight => {
            r += dx;
            b += dy;
        }
        Handle::Bottom => b += dy,
        Handle::BottomLeft => {
            l += dx;
            b += dy;
        }
        Handle::Left => l += dx,
    }

    Rect::new(
        l.clamp(0.0, vw),
        t.clamp(0.0, vh),
        r.clamp(0.0, vw),
        b.clamp(0.0, vh),
    )
}

/// 悬停反馈：手柄高亮 + 光标形状切换。
fn wire_motion(area: &DrawingArea, state: &Rc<RefCell<State>>) {
    let motion = EventControllerMotion::new();
    let state = state.clone();
    let area_ref = area.clone();
    motion.connect_motion(move |_, x, y| {
        let mut state = state.borrow_mut();
        if state.drag.is_some() {
            return;
        }
        // 工具栏区域：只更新按钮悬停态，不做画布命中，光标恢复默认。
        if let Some(bar) = state.toolbar().filter(|bar| bar.contains(x, y)) {
            let hit = bar.hit(x, y);
            if state.bar_hover != hit {
                state.bar_hover = hit;
                area_ref.queue_draw();
            }
            area_ref.set_cursor(gdk::Cursor::from_name("default", None).as_ref());
            return;
        }
        if state.bar_hover.is_some() {
            state.bar_hover = None;
            area_ref.queue_draw();
        }
        // 标注模式下不做手柄命中，光标恒为十字。
        if state.drawing() {
            if state.hover.is_some() {
                state.hover = None;
                area_ref.queue_draw();
            }
            area_ref.set_cursor(gdk::Cursor::from_name("crosshair", None).as_ref());
            return;
        }
        let hover = state.handle_at(x, y);
        let inside = state.rect.map(|r| r.contains(x, y)).unwrap_or(false);
        let name = match (hover, inside) {
            (Some(handle), _) => handle.cursor(),
            (None, true) => "move",
            (None, false) => "crosshair",
        };
        area_ref.set_cursor(gdk::Cursor::from_name(name, None).as_ref());
        if state.hover != hover {
            state.hover = hover;
            area_ref.queue_draw();
        }
    });
    area.add_controller(motion);
}

/// 滚轮调线宽：字号 / 马赛克块 / 模糊强度 / 序号圆都由线宽派生，一个通道调全部工具。
fn wire_scroll(area: &DrawingArea, state: &Rc<RefCell<State>>) {
    let scroll = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
    let state = state.clone();
    let area_ref = area.clone();
    scroll.connect_scroll(move |_, _, dy| {
        let mut st = state.borrow_mut();
        // 向上滚（dy < 0）加粗，向下滚变细。
        let next = (st.style.stroke - dy).clamp(STROKE_MIN, STROKE_MAX);
        if next == st.style.stroke {
            return glib::Propagation::Stop;
        }
        st.style.stroke = next;
        drop(st);
        area_ref.queue_draw();
        glib::Propagation::Stop
    });
    area.add_controller(scroll);
}

/// 左键双击确认，右键取消。
fn wire_click(
    area: &DrawingArea,
    state: &Rc<RefCell<State>>,
    window: &Window,
    outcome: &Rc<Outcome>,
) {
    let confirm = GestureClick::new();
    confirm.set_button(gdk::BUTTON_PRIMARY);
    {
        let state = state.clone();
        let window = window.clone();
        let outcome = outcome.clone();
        confirm.connect_pressed(move |_, n_press, x, y| {
            let bar_hit = {
                let state = state.borrow();
                state
                    .toolbar()
                    .filter(|bar| bar.contains(x, y))
                    .map(|bar| bar.hit(x, y))
            };
            // 工具栏吞掉落在其上的所有点击；只有首次按下才触发按钮，双击不重复执行。
            if let Some(hit) = bar_hit {
                if n_press == 1
                    && let Some(item) = hit
                {
                    activate_item(&state, &window, &outcome, item);
                }
                return;
            }
            if n_press < 2 {
                return;
            }
            let inside = {
                let state = state.borrow();
                state.rect.map(|r| r.contains(x, y)).unwrap_or(false)
            };
            if inside {
                confirm_selection(&state, &window, &outcome);
            }
        });
    }
    area.add_controller(confirm);

    let cancel = GestureClick::new();
    cancel.set_button(gdk::BUTTON_SECONDARY);
    {
        let window = window.clone();
        let outcome = outcome.clone();
        cancel.connect_pressed(move |_, _, _, _| outcome.finish(&window, Ok(None)));
    }
    area.add_controller(cancel);
}

/// 执行一个工具栏按钮的动作。
fn activate_item(
    state: &Rc<RefCell<State>>,
    window: &Window,
    outcome: &Rc<Outcome>,
    item: Item,
) {
    match item {
        Item::Tool(tool) => {
            let mut st = state.borrow_mut();
            // 与快捷键一致：点当前工具即取消选中，回到选区调整模式。
            st.tool = if st.tool == Some(tool) { None } else { Some(tool) };
            st.commit_active();
            drop(st);
            window.queue_draw();
        }
        Item::Color(i) => {
            let mut st = state.borrow_mut();
            st.style.color = PALETTE[i];
            // 正在敲的文字立即换色，不必重新开一段。
            if let Some(shape) = st.active.as_mut() {
                shape.set_color(PALETTE[i]);
            }
            drop(st);
            window.queue_draw();
        }
        Item::Undo => {
            if state.borrow_mut().undo() {
                window.queue_draw();
            }
        }
        Item::Redo => {
            if state.borrow_mut().redo() {
                window.queue_draw();
            }
        }
        Item::Copy => copy_selection(state, window, outcome),
        Item::Save => confirm_selection(state, window, outcome),
        Item::Cancel => outcome.finish(window, Ok(None)),
    }
}

/// Enter / Space 确认，Esc 逐级退出，字母键切换标注工具。
///
/// 控制器挂在 Window 而非 DrawingArea：键盘事件由 toplevel 向下分发，
/// 挂窗口层 + Capture 阶段可确保无论焦点落在哪个子控件（甚至没有焦点控件）都能收到按键。
fn wire_keys(
    state: &Rc<RefCell<State>>,
    window: &Window,
    area: &DrawingArea,
    outcome: &Rc<Outcome>,
    im: &IMMulticontext,
) {
    let keys = EventControllerKey::new();
    let state = state.clone();
    let outcome = outcome.clone();
    let win = window.clone();
    let area_ref = area.clone();
    let im_ctx = im.clone();
    keys.connect_key_pressed(move |ctrl, key, _, modifier| {
        // 借用先落到局部变量：`if state.borrow().x && f()` 里的临时 Ref 会活到整个条件求值结束，
        // 被调函数再 borrow_mut 就会 panic。
        let editing = state.borrow().editing;
        // 文字输入态优先吃掉按键，否则字母会被当成工具快捷键。
        if editing {
            // 先给输入法过滤：拼音、候选选择、翻页等按键都应由 IM 消费，
            // 只把它没要的按键交给下面的编辑逻辑。
            if let Some(event) = ctrl.current_event()
                && im_ctx.filter_keypress(&event)
            {
                area_ref.queue_draw();
                return glib::Propagation::Stop;
            }
            if handle_text_key(&state, &win, &im_ctx, key, modifier) {
                return glib::Propagation::Stop;
            }
        }
        // Ctrl+Shift+Z 重做；放在 Ctrl+Z 之前判定，否则会被撤销分支吃掉。
        if modifier.contains(gdk::ModifierType::CONTROL_MASK)
            && modifier.contains(gdk::ModifierType::SHIFT_MASK)
            && matches!(key, gdk::Key::z | gdk::Key::Z)
        {
            if state.borrow_mut().redo() {
                area_ref.queue_draw();
            }
            return glib::Propagation::Stop;
        }
        // Ctrl+Z 撤销最近一笔标注；无标注可撤时不做任何事（不误退出窗口）。
        if modifier.contains(gdk::ModifierType::CONTROL_MASK)
            && matches!(key, gdk::Key::z | gdk::Key::Z)
        {
            if state.borrow_mut().undo() {
                area_ref.queue_draw();
            }
            return glib::Propagation::Stop;
        }
        // Ctrl+C：直接把当前选区复制到剪贴板并关窗，不落盘。
        if modifier.contains(gdk::ModifierType::CONTROL_MASK)
            && matches!(key, gdk::Key::c | gdk::Key::C)
        {
            copy_selection(&state, &win, &outcome);
            return glib::Propagation::Stop;
        }
        // 工具键只在已有选区时生效，否则无处落笔；带 Ctrl/Alt 的组合键不当工具键。
        if !modifier.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK)
            && let Some(tool) = tool_for_key(key)
        {
            let mut st = state.borrow_mut();
            if st.rect.is_none() {
                return glib::Propagation::Stop;
            }
            // 再按一次同一个键即退出该工具，回到选区调整模式。
            st.tool = if st.tool == Some(tool) { None } else { Some(tool) };
            st.commit_active();
            drop(st);
            area_ref.queue_draw();
            return glib::Propagation::Stop;
        }
        match key {
            gdk::Key::Escape => {
                // 先退出标注工具，已在选区模式才整体取消。
                let exited = {
                    let mut st = state.borrow_mut();
                    let had = st.tool.take().is_some();
                    st.commit_active();
                    had
                };
                if exited {
                    area_ref.queue_draw();
                } else {
                    outcome.finish(&win, Ok(None));
                }
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space => {
                confirm_selection(&state, &win, &outcome);
            }
            // 其余按键放行给 GTK（未来的快捷键在此之前拦截）。
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    window.add_controller(keys);
}

/// 文字输入态的按键处理；返回 true 表示该按键已被消费。
///
/// 调用前 `wire_keys` 已让 `IMMulticontext` 先过滤一遍，所以这里只处理输入法不要的按键：
/// 退格、换行、结束输入，以及无输入法时的直接键入字符（英文、数字、标点）。
fn handle_text_key(
    state: &Rc<RefCell<State>>,
    window: &Window,
    im: &IMMulticontext,
    key: gdk::Key,
    modifier: gdk::ModifierType,
) -> bool {
    let ctrl = modifier.contains(gdk::ModifierType::CONTROL_MASK);
    let alt = modifier.contains(gdk::ModifierType::ALT_MASK);
    // 预编辑未确认时，退格等键已被 IM 吃掉；能走到这里说明预编辑是空的。
    let mut committed = false;
    {
        let mut st = state.borrow_mut();

        match key {
            gdk::Key::BackSpace => {
                if let Some(shape) = st.active.as_mut() {
                    shape.pop_char();
                }
            }
            // Ctrl+Enter 落定，单独 Enter 换行。
            gdk::Key::Return | gdk::Key::KP_Enter if !ctrl => {
                if let Some(shape) = st.active.as_mut() {
                    shape.push_char('\n');
                }
            }
            // Esc / Ctrl+Enter 结束这段文字（空文本自动丢弃），工具仍保持选中。
            gdk::Key::Escape | gdk::Key::Return | gdk::Key::KP_Enter => {
                st.commit_active();
                committed = true;
            }
            _ => {
                // 组合键留给上层（撤销等）；其余可打印字符直接落字。
                if ctrl || alt {
                    return false;
                }
                let Some(ch) = key.to_unicode().filter(|c| !c.is_control()) else {
                    return false;
                };
                if let Some(shape) = st.active.as_mut() {
                    shape.push_char(ch);
                }
            }
        }
    }

    // IM 调用必须在借用释放后：reset 会同步触发 commit / preedit 回调，回调里要 borrow_mut。
    if committed {
        im.reset();
        im.focus_out();
    } else {
        sync_cursor_location(state, im);
    }
    window.queue_draw();
    true
}

/// 把输入法上下文接到编辑器状态上：预编辑串自绘，确认后的文本落进文字图元。
///
/// Wayland 下候选窗由 ibus 自己弹出并按 `set_cursor_location` 定位，
/// 但预编辑（尚未上屏的拼音）必须客户端自己画，否则用户看不到自己敲了什么。
fn wire_im(area: &DrawingArea, state: &Rc<RefCell<State>>, im: &IMMulticontext) {
    {
        let state = state.clone();
        let area = area.clone();
        im.connect_commit(move |_, text| {
            {
                let mut st = state.borrow_mut();
                // 预编辑已经上屏，清掉自绘的那份，避免和正式文本重影。
                st.preedit = (String::new(), 0);
                if let Some(shape) = st.active.as_mut() {
                    for ch in text.chars().filter(|c| !c.is_control() || *c == '\n') {
                        shape.push_char(ch);
                    }
                }
            }
            area.queue_draw();
        });
    }

    {
        let state = state.clone();
        let area = area.clone();
        im.connect_preedit_start(move |_| {
            state.borrow_mut().preedit = (String::new(), 0);
            area.queue_draw();
        });
    }

    {
        let state = state.clone();
        let area = area.clone();
        let im_ctx = im.clone();
        im.connect_preedit_changed(move |_| {
            // 只取文本与光标偏移；属性（下划线/高亮）由 draw_preedit 统一样式，不逐段还原。
            let (text, _attrs, cursor) = im_ctx.preedit_string();
            state.borrow_mut().preedit = (text.to_string(), cursor);
            sync_cursor_location(&state, &im_ctx);
            area.queue_draw();
        });
    }

    {
        let state = state.clone();
        let area = area.clone();
        im.connect_preedit_end(move |_| {
            state.borrow_mut().preedit = (String::new(), 0);
            area.queue_draw();
        });
    }
}

/// 告知输入法当前文字光标的位置，让候选窗贴着光标弹出而不是飘到屏幕角落。
///
/// 坐标是相对 client widget（DrawingArea）的逻辑像素，与图元坐标系一致。
/// 调用方必须确保此时没有持有 `state` 的借用：`set_cursor_location` 可能同步回调进来。
fn sync_cursor_location(state: &Rc<RefCell<State>>, im: &IMMulticontext) {
    let metrics = {
        let st = state.borrow();
        st.active.as_ref().and_then(text_caret_metrics)
    };
    let Some((x, top, h)) = metrics else {
        return;
    };
    im.set_cursor_location(&gdk::Rectangle::new(
        x as i32,
        top as i32,
        1,
        h.max(1.0) as i32,
    ));
}

/// 标注工具快捷键：r 空心框 / f 实心框 / o 圆形 / l 直线 / a 箭头 / p 画笔 /
/// h 荧光笔 / n 序号 / m 马赛克 / b 模糊 / t 文字。
fn tool_for_key(key: gdk::Key) -> Option<Tool> {
    match key {
        gdk::Key::r | gdk::Key::R => Some(Tool::Rect),
        gdk::Key::f | gdk::Key::F => Some(Tool::FillRect),
        gdk::Key::o | gdk::Key::O => Some(Tool::Ellipse),
        gdk::Key::l | gdk::Key::L => Some(Tool::Line),
        gdk::Key::a | gdk::Key::A => Some(Tool::Arrow),
        gdk::Key::p | gdk::Key::P => Some(Tool::Pen),
        gdk::Key::h | gdk::Key::H => Some(Tool::Marker),
        gdk::Key::n | gdk::Key::N => Some(Tool::Counter),
        gdk::Key::m | gdk::Key::M => Some(Tool::Mosaic),
        gdk::Key::b | gdk::Key::B => Some(Tool::Blur),
        gdk::Key::t | gdk::Key::T => Some(Tool::Text),
        _ => None,
    }
}

/// 裁剪并落盘。没有有效选区时按整屏确认，避免按键「毫无反应」的观感。
fn confirm_selection(state: &Rc<RefCell<State>>, window: &Window, outcome: &Rc<Outcome>) {
    // 未落定的图元（尤其正在敲的文字）也要一起导出。
    state.borrow_mut().commit_active();
    let result = {
        let state = state.borrow();
        let rect = export_rect(&state);
        crop(&state, &rect)
            .and_then(|pixbuf| capture::save_pixbuf(&pixbuf))
            .map(|path| Some(Shot::Saved(path)))
            .map_err(|err| format!("{err:#}"))
    };
    outcome.finish(window, result);
}

/// Ctrl+C：裁剪当前选区直接写剪贴板并关窗，不在图片目录留文件。
fn copy_selection(state: &Rc<RefCell<State>>, window: &Window, outcome: &Rc<Outcome>) {
    state.borrow_mut().commit_active();
    let result = {
        let state = state.borrow();
        let rect = export_rect(&state);
        crop(&state, &rect)
            .and_then(copy_pixbuf)
            .map(|backend| Some(Shot::Copied(backend)))
            .map_err(|err| format!("{err:#}"))
    };
    outcome.finish(window, result);
}

/// 两条剪贴板后端都以文件为输入，故先写一份临时 PNG。
///
/// wl-copy 会把字节读进自己的进程再 fork 持有 selection，GDK 也会立刻解码成纹理，
/// 因此复制完成后即可删除临时文件，不影响后续粘贴。
fn copy_pixbuf(pixbuf: Pixbuf) -> Result<Backend> {
    let temp = glib::tmp_dir().join(format!("owlshot-clip-{}.png", std::process::id()));
    pixbuf
        .savev(&temp, "png", &[])
        .with_context(|| format!("写出临时 PNG 到 {} 失败", temp.display()))?;
    // 编辑器回调本就在 GLib 主线程，直接用同步版本，避免把 Pixbuf 送出线程。
    let backend = clipboard::copy_png_on_main(&temp);
    // Ctrl+C 不落盘，但仍要进历史，否则这条路径的图之后取不回来。
    if backend.is_ok() {
        crate::history::record(&temp);
    }
    let _ = std::fs::remove_file(&temp);
    backend
}

/// 待导出的逻辑选区；无有效选区时退化为整屏。
fn export_rect(state: &State) -> Rect {
    match state.rect {
        Some(rect) if rect.width() >= MIN_SIZE && rect.height() >= MIN_SIZE => rect,
        _ => {
            let (vw, vh) = state.view;
            println!("[owlshot] 无选区，按整屏导出。");
            Rect::new(0.0, 0.0, vw, vh)
        }
    }
}

/// 逻辑选区换算成物理像素后裁剪，保证 HiDPI 下导出的是原始清晰度。
fn crop(state: &State, rect: &Rect) -> Result<Pixbuf> {
    let (sx, sy) = pixel_scale(state);
    let (iw, ih) = (state.shot.width(), state.shot.height());

    let x = (rect.left() * sx).round().clamp(0.0, iw as f64) as i32;
    let y = (rect.top() * sy).round().clamp(0.0, ih as f64) as i32;
    let w = ((rect.width() * sx).round() as i32).min(iw - x).max(1);
    let h = ((rect.height() * sy).round() as i32).min(ih - y).max(1);

    // 没有标注就直接切子图，像素与原图完全一致，也省掉一次全区合成。
    if state.shapes.is_empty() {
        Ok(state.shot.new_subpixbuf(x, y, w, h))
    } else {
        compose(state, x, y, w, h)
    }
}

/// 把选区底图与标注合成成一张物理分辨率的图。
///
/// 标注存的是逻辑坐标，这里把用户坐标系按 `pixel_scale` 变换回逻辑坐标，
/// 于是预览与导出共用同一套 `Shape::draw`，HiDPI 下也不会错位。
fn compose(state: &State, x: i32, y: i32, w: i32, h: i32) -> Result<Pixbuf> {
    let (sx, sy) = pixel_scale(state);
    // 底图不透明，用 Rgb24 可省掉预乘 alpha 的换算。
    let surface = cairo::ImageSurface::create(cairo::Format::Rgb24, w, h)
        .context("创建导出画布失败")?;
    {
        let cr = cairo::Context::new(&surface).context("创建导出绘制上下文失败")?;
        // 底图按 1:1 物理像素贴入，负偏移即裁剪。
        cr.set_source_pixbuf(&state.shot, -(x as f64), -(y as f64));
        cr.paint().context("写入底图失败")?;

        cr.translate(-(x as f64), -(y as f64));
        cr.scale(sx, sy);
        let canvas = Canvas {
            shot: &state.shot,
            scale: (sx, sy),
        };
        for shape in &state.shapes {
            shape.draw(&cr, &canvas);
        }
    }
    // Context 必须先析构，surface 引用计数回到 1 才能取走像素。
    surface_to_pixbuf(surface)
}

/// Cairo RGB24 → Pixbuf(RGB888)。
///
/// Cairo 每像素是一个 32 位**原生字节序**整数（高 8 位未用），因此按整数移位取通道，
/// 不去假设内存里的 BGRX 排布。
fn surface_to_pixbuf(surface: cairo::ImageSurface) -> Result<Pixbuf> {
    let (w, h, stride) = (surface.width(), surface.height(), surface.stride());
    let data = surface
        .take_data()
        .map_err(|err| anyhow!("读取导出画布像素失败：{err}"))?;

    let row = w as usize * 3;
    let mut out = vec![0u8; row * h as usize];
    for line in 0..h as usize {
        let src = &data[line * stride as usize..];
        let dst = &mut out[line * row..][..row];
        for col in 0..w as usize {
            let i = col * 4;
            let px = u32::from_ne_bytes([src[i], src[i + 1], src[i + 2], src[i + 3]]);
            dst[col * 3] = (px >> 16) as u8;
            dst[col * 3 + 1] = (px >> 8) as u8;
            dst[col * 3 + 2] = px as u8;
        }
    }

    Ok(Pixbuf::from_mut_slice(
        out,
        Colorspace::Rgb,
        false,
        8,
        w,
        h,
        row as i32,
    ))
}
