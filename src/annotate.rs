//! 标注图元：共享几何、工具枚举、图元定义与 Cairo 绘制。
//!
//! 所有坐标一律存**逻辑（视图）坐标**。导出时只需把 Cairo 上下文按物理像素缩放比
//! 变换一次即可复用同一套绘制代码，HiDPI 分数缩放下标注与底图始终对齐。

use gtk4::cairo;
use gtk4::gdk_pixbuf::{InterpType, Pixbuf};
use gtk4::prelude::*;
use std::f64::consts::TAU;

/// 小于该跨度的图元视为误操作，不入栈。
pub const MIN_SHAPE: f64 = 3.0;

/// 轴对齐矩形，保存两个对角点（允许反向拖拽）。
#[derive(Clone, Copy)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn left(&self) -> f64 {
        self.x0.min(self.x1)
    }

    pub fn top(&self) -> f64 {
        self.y0.min(self.y1)
    }

    pub fn right(&self) -> f64 {
        self.x0.max(self.x1)
    }

    pub fn bottom(&self) -> f64 {
        self.y0.max(self.y1)
    }

    pub fn width(&self) -> f64 {
        self.right() - self.left()
    }

    pub fn height(&self) -> f64 {
        self.bottom() - self.top()
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.left() && x <= self.right() && y >= self.top() && y <= self.bottom()
    }

    pub fn offset(&self, dx: f64, dy: f64) -> Self {
        Self {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 + dx,
            y1: self.y1 + dy,
        }
    }

    /// 把点夹进矩形内，保证标注不会溢出选区。
    pub fn clamp_point(&self, x: f64, y: f64) -> (f64, f64) {
        (
            x.clamp(self.left(), self.right()),
            y.clamp(self.top(), self.bottom()),
        )
    }
}

/// 描边颜色（不含 alpha，标注一律不透明以保证可读性）。
#[derive(Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

/// 调色板：工具栏上可直接点选的标注色。
pub const PALETTE: [Color; 8] = [
    Color { r: 0.93, g: 0.20, b: 0.20 }, // 红
    Color { r: 1.00, g: 0.58, b: 0.12 }, // 橙
    Color { r: 1.00, g: 0.85, b: 0.20 }, // 黄
    Color { r: 0.30, g: 0.80, b: 0.35 }, // 绿
    Color { r: 0.20, g: 0.78, b: 0.85 }, // 青
    Color { r: 0.25, g: 0.55, b: 1.00 }, // 蓝
    Color { r: 0.68, g: 0.42, b: 0.95 }, // 紫
    Color { r: 1.00, g: 1.00, b: 1.00 }, // 白
];

/// 默认标注色：醒目的红。
pub const DEFAULT_COLOR: Color = PALETTE[0];

/// 默认线宽。
pub const DEFAULT_STROKE: f64 = 3.0;

/// 默认字号（逻辑像素）。
pub const DEFAULT_FONT_SIZE: f64 = 18.0;

/// 可选字号列表。
pub const FONT_SIZES: [f64; 5] = [12.0, 16.0, 20.0, 28.0, 40.0];

/// 线宽可调范围（滚轮调节的上下限）。
pub const STROKE_MIN: f64 = 1.0;
pub const STROKE_MAX: f64 = 20.0;

/// 荧光笔透明度与加粗倍数。
const MARKER_ALPHA: f64 = 0.35;
const MARKER_SCALE: f64 = 3.0;

/// 新建图元时的样式快照。
///
/// 字号由独立的 `font_size` 字段控制，与线宽解耦。
/// 马赛克块 / 模糊强度 / 序号圆仍由线宽派生。
#[derive(Clone, Copy)]
pub struct Style {
    pub color: Color,
    pub stroke: f64,
    /// 序号计数器工具的下一个编号。
    pub counter: u32,
    /// 文字字号（逻辑像素）。
    pub font_size: f64,
}

impl Style {
    /// 文字字号：直接使用 font_size 字段。
    pub fn font_size(&self) -> f64 {
        self.font_size.clamp(12.0, 96.0)
    }

    /// 马赛克块边长（逻辑像素）。
    pub fn mosaic_block(&self) -> f64 {
        (self.stroke * 3.0).max(4.0)
    }

    /// 模糊强度，等效于降采样半径。
    pub fn blur_radius(&self) -> f64 {
        (self.stroke * 4.0).max(6.0)
    }

    /// 序号圆半径。
    pub fn counter_radius(&self) -> f64 {
        (self.stroke * 3.5).max(9.0)
    }
}

/// 当前激活的标注工具（对齐 Flameshot 工具集）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// 空心矩形
    Rect,
    /// 实心矩形
    FillRect,
    /// 空心椭圆
    Ellipse,
    /// 直线（无箭头）
    Line,
    Arrow,
    /// 自由画笔
    Pen,
    /// 荧光笔：半透明粗线
    Marker,
    /// 序号计数器：点一下放一个自增编号
    Counter,
    /// 马赛克（像素化）
    Mosaic,
    /// 模糊
    Blur,
    Text,
}

/// 一个已落定（或正在拖出）的标注图元。
#[derive(Clone)]
pub enum Shape {
    Rect {
        rect: Rect,
        color: Color,
        stroke: f64,
    },
    /// 实心矩形：纯色填充，用来彻底遮住内容。
    FillRect { rect: Rect, color: Color },
    Ellipse {
        rect: Rect,
        color: Color,
        stroke: f64,
    },
    /// 直线：无箭头。
    Line {
        from: (f64, f64),
        to: (f64, f64),
        color: Color,
        stroke: f64,
    },
    Arrow {
        from: (f64, f64),
        to: (f64, f64),
        color: Color,
        stroke: f64,
    },
    Pen {
        points: Vec<(f64, f64)>,
        color: Color,
        stroke: f64,
    },
    /// 荧光笔：半透明加粗轨迹，`stroke` 已含加粗倍数。
    Marker {
        points: Vec<(f64, f64)>,
        color: Color,
        stroke: f64,
    },
    /// 序号计数器：实心圆 + 白色编号。
    Counter {
        pos: (f64, f64),
        index: u32,
        color: Color,
        radius: f64,
    },
    /// 马赛克：像素化底图的一块矩形区域。
    Mosaic { rect: Rect, block: f64 },
    /// 模糊：与马赛克同一条降采样路径，放大改用双线性插值。
    Blur { rect: Rect, radius: f64 },
    /// 文字：`pos` 为文字块左上角。
    Text {
        pos: (f64, f64),
        text: String,
        color: Color,
        size: f64,
    },
}

/// 绘制时需要的外部上下文：底图与「逻辑坐标 → 物理像素」缩放比。
///
/// 马赛克必须回读底图，因此绘制不能只依赖 Cairo 上下文；预览与导出共用同一入口，
/// 差异仅在于传入的 `scale`（预览是窗口逻辑尺寸比，导出为 1:1 物理像素）。
pub struct Canvas<'a> {
    pub shot: &'a Pixbuf,
    pub scale: (f64, f64),
}

impl Shape {
    /// 按工具类型创建一个起点与终点重合的空图元。
    ///
    /// 样式在创建瞬间快照进图元，之后改工具栏配色不会回溯影响已画的标注。
    pub fn start(tool: Tool, x: f64, y: f64, style: &Style) -> Self {
        let (color, stroke) = (style.color, style.stroke);
        match tool {
            Tool::Rect => Shape::Rect {
                rect: Rect::new(x, y, x, y),
                color,
                stroke,
            },
            Tool::FillRect => Shape::FillRect {
                rect: Rect::new(x, y, x, y),
                color,
            },
            Tool::Ellipse => Shape::Ellipse {
                rect: Rect::new(x, y, x, y),
                color,
                stroke,
            },
            Tool::Line => Shape::Line {
                from: (x, y),
                to: (x, y),
                color,
                stroke,
            },
            Tool::Arrow => Shape::Arrow {
                from: (x, y),
                to: (x, y),
                color,
                stroke,
            },
            Tool::Pen => Shape::Pen {
                points: vec![(x, y)],
                color,
                stroke,
            },
            Tool::Marker => Shape::Marker {
                points: vec![(x, y)],
                color,
                stroke: stroke * MARKER_SCALE,
            },
            Tool::Counter => Shape::Counter {
                pos: (x, y),
                index: style.counter,
                color,
                radius: style.counter_radius(),
            },
            Tool::Mosaic => Shape::Mosaic {
                rect: Rect::new(x, y, x, y),
                block: style.mosaic_block(),
            },
            Tool::Blur => Shape::Blur {
                rect: Rect::new(x, y, x, y),
                radius: style.blur_radius(),
            },
            Tool::Text => Shape::Text {
                pos: (x, y),
                text: String::new(),
                color,
                size: style.font_size(),
            },
        }
    }

    /// 拖拽过程中更新终点；画笔 / 荧光笔则是持续追加轨迹点。
    pub fn extend_to(&mut self, x: f64, y: f64) {
        match self {
            Shape::Rect { rect, .. }
            | Shape::FillRect { rect, .. }
            | Shape::Ellipse { rect, .. }
            | Shape::Mosaic { rect, .. }
            | Shape::Blur { rect, .. } => {
                rect.x1 = x;
                rect.y1 = y;
            }
            Shape::Line { to, .. } | Shape::Arrow { to, .. } => *to = (x, y),
            Shape::Pen { points, .. } | Shape::Marker { points, .. } => {
                // 过密的采样点只会拖慢重绘，间隔小于 1px 直接丢弃。
                if let Some(&(px, py)) = points.last()
                    && (x - px).abs() < 1.0
                    && (y - py).abs() < 1.0
                {
                    return;
                }
                points.push((x, y));
            }
            // 文字与序号没有拖拽语义，落点即锚点。
            Shape::Counter { .. } | Shape::Text { .. } => {}
        }
    }

    /// 图元是否有足够跨度值得保留。
    pub fn is_meaningful(&self) -> bool {
        match self {
            Shape::Rect { rect, .. }
            | Shape::FillRect { rect, .. }
            | Shape::Ellipse { rect, .. }
            | Shape::Mosaic { rect, .. }
            | Shape::Blur { rect, .. } => {
                rect.width() >= MIN_SHAPE && rect.height() >= MIN_SHAPE
            }
            Shape::Line { from, to, .. } | Shape::Arrow { from, to, .. } => {
                (to.0 - from.0).hypot(to.1 - from.1) >= MIN_SHAPE * 2.0
            }
            Shape::Pen { points, .. } | Shape::Marker { points, .. } => points.len() >= 2,
            // 序号点一下就成立，不需要跨度。
            Shape::Counter { .. } => true,
            Shape::Text { text, .. } => !text.trim().is_empty(),
        }
    }

    /// 文字输入：追加一个字符（`\n` 即换行）。非文字图元忽略。
    pub fn push_char(&mut self, ch: char) {
        if let Shape::Text { text, .. } = self {
            text.push(ch);
        }
    }

    /// 文字输入：退格。按 char 边界删除，中文同样安全。
    pub fn pop_char(&mut self) {
        if let Shape::Text { text, .. } = self {
            text.pop();
        }
    }

    /// 改写图元颜色：正在拖出（或正在敲字）的图元换色时用，马赛克 / 模糊无颜色可改。
    pub fn set_color(&mut self, next: Color) {
        match self {
            Shape::Rect { color, .. }
            | Shape::FillRect { color, .. }
            | Shape::Ellipse { color, .. }
            | Shape::Line { color, .. }
            | Shape::Arrow { color, .. }
            | Shape::Pen { color, .. }
            | Shape::Marker { color, .. }
            | Shape::Counter { color, .. }
            | Shape::Text { color, .. } => *color = next,
            Shape::Mosaic { .. } | Shape::Blur { .. } => {}
        }
    }

    pub fn draw(&self, cr: &cairo::Context, canvas: &Canvas) {
        match self {
            Shape::Rect {
                rect,
                color,
                stroke,
            } => {
                prepare(cr, color, *stroke);
                cr.rectangle(rect.left(), rect.top(), rect.width(), rect.height());
                let _ = cr.stroke();
            }
            Shape::FillRect { rect, color } => {
                cr.new_path();
                cr.set_source_rgb(color.r, color.g, color.b);
                cr.rectangle(rect.left(), rect.top(), rect.width(), rect.height());
                let _ = cr.fill();
            }
            Shape::Ellipse {
                rect,
                color,
                stroke,
            } => {
                prepare(cr, color, *stroke);
                let (rx, ry) = (rect.width() / 2.0, rect.height() / 2.0);
                if rx <= 0.0 || ry <= 0.0 {
                    return;
                }
                // 先在缩放坐标系里构造单位圆，restore 后再描边，线宽才不会被拉扁。
                let _ = cr.save();
                cr.translate(rect.left() + rx, rect.top() + ry);
                cr.scale(rx, ry);
                cr.new_path();
                cr.arc(0.0, 0.0, 1.0, 0.0, TAU);
                let _ = cr.restore();
                let _ = cr.stroke();
            }
            Shape::Line {
                from,
                to,
                color,
                stroke,
            } => {
                prepare(cr, color, *stroke);
                cr.move_to(from.0, from.1);
                cr.line_to(to.0, to.1);
                let _ = cr.stroke();
            }
            Shape::Arrow {
                from,
                to,
                color,
                stroke,
            } => draw_arrow(cr, *from, *to, color, *stroke),
            Shape::Pen {
                points,
                color,
                stroke,
            } => draw_pen(cr, points, color, *stroke, 1.0),
            Shape::Marker {
                points,
                color,
                stroke,
            } => draw_pen(cr, points, color, *stroke, MARKER_ALPHA),
            Shape::Counter {
                pos,
                index,
                color,
                radius,
            } => draw_counter(cr, *pos, *index, color, *radius),
            Shape::Mosaic { rect, block } => {
                pixelate(cr, rect, *block, canvas, InterpType::Nearest)
            }
            Shape::Blur { rect, radius } => {
                pixelate(cr, rect, *radius, canvas, InterpType::Bilinear)
            }
            Shape::Text {
                pos,
                text,
                color,
                size,
            } => draw_text(cr, *pos, text, color, *size),
        }
    }
}

fn prepare(cr: &cairo::Context, color: &Color, stroke: f64) {
    cr.set_source_rgb(color.r, color.g, color.b);
    cr.set_line_width(stroke);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.new_path();
}

/// 直线 + 实心三角箭头，箭头尺寸随线宽走并受线段长度约束。
fn draw_arrow(cr: &cairo::Context, from: (f64, f64), to: (f64, f64), color: &Color, stroke: f64) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = dx.hypot(dy);
    if len < 1.0 {
        return;
    }
    let head = (stroke * 4.0 + 6.0).min(len * 0.4);
    let (ux, uy) = (dx / len, dy / len);
    // 线段缩短到箭头根部，避免线头从三角形里透出来。
    let base = (to.0 - ux * head, to.1 - uy * head);
    let half = head * 0.45;

    prepare(cr, color, stroke);
    cr.move_to(from.0, from.1);
    cr.line_to(base.0, base.1);
    let _ = cr.stroke();

    cr.new_path();
    cr.move_to(to.0, to.1);
    cr.line_to(base.0 - uy * half, base.1 + ux * half);
    cr.line_to(base.0 + uy * half, base.1 - ux * half);
    cr.close_path();
    let _ = cr.fill();
}

/// 轨迹线；`alpha < 1.0` 即荧光笔效果。
///
/// 整条轨迹只描一次边，重叠处不会因为多次叠加而变深。
fn draw_pen(cr: &cairo::Context, points: &[(f64, f64)], color: &Color, stroke: f64, alpha: f64) {
    let Some(&(x0, y0)) = points.first() else {
        return;
    };
    prepare(cr, color, stroke);
    if alpha < 1.0 {
        cr.set_source_rgba(color.r, color.g, color.b, alpha);
    }
    if points.len() == 1 {
        cr.arc(x0, y0, stroke / 2.0, 0.0, TAU);
        let _ = cr.fill();
        return;
    }
    cr.move_to(x0, y0);
    for &(x, y) in &points[1..] {
        cr.line_to(x, y);
    }
    let _ = cr.stroke();
}

/// 序号计数器：实心圆 + 居中白色数字，`pos` 为圆心。
fn draw_counter(cr: &cairo::Context, pos: (f64, f64), index: u32, color: &Color, radius: f64) {
    cr.new_path();
    cr.set_source_rgb(color.r, color.g, color.b);
    cr.arc(pos.0, pos.1, radius, 0.0, TAU);
    let _ = cr.fill();

    let label = index.to_string();
    set_text_font(cr, radius * 1.35);
    let Ok(ext) = cr.text_extents(&label) else {
        return;
    };
    // 用 extents 的实际墨迹盒对齐，数字位数变化时也保持视觉居中。
    cr.new_path();
    cr.move_to(
        pos.0 - ext.width() / 2.0 - ext.x_bearing(),
        pos.1 - ext.height() / 2.0 - ext.y_bearing(),
    );
    cr.set_source_rgb(1.0, 1.0, 1.0);
    let _ = cr.show_text(&label);
}

/// 马赛克 / 模糊：从底图取出对应区域 → 降采样成块网格 → 放大回原尺寸再贴回。
///
/// 直接在 Pixbuf 级别完成两次缩放，避免依赖 Cairo pattern 过滤器的实现差异；
/// 贴图在物理像素坐标系里进行，HiDPI 分数缩放下块边界不会出现半像素毛边。
/// `up` 决定观感：`Nearest` 得到硬边方块（马赛克），`Bilinear` 得到平滑过渡（模糊）。
fn pixelate(cr: &cairo::Context, rect: &Rect, block: f64, canvas: &Canvas, up: InterpType) {
    let (sx, sy) = canvas.scale;
    let (iw, ih) = (canvas.shot.width(), canvas.shot.height());

    let px = (rect.left() * sx).floor().clamp(0.0, iw as f64) as i32;
    let py = (rect.top() * sy).floor().clamp(0.0, ih as f64) as i32;
    let pw = ((rect.width() * sx).ceil() as i32).min(iw - px);
    let ph = ((rect.height() * sy).ceil() as i32).min(ih - py);
    if pw < 1 || ph < 1 {
        return;
    }

    // 块数按逻辑尺寸算，视觉块大小与屏幕缩放无关。
    let cols = ((rect.width() / block).round() as i32).clamp(1, pw);
    let rows = ((rect.height() / block).round() as i32).clamp(1, ph);

    let src = canvas.shot.new_subpixbuf(px, py, pw, ph);
    // 缩小一律用双线性取块内均值，放大方式由调用方决定。
    let Some(small) = src.scale_simple(cols, rows, InterpType::Bilinear) else {
        return;
    };
    let Some(big) = small.scale_simple(pw, ph, up) else {
        return;
    };

    let _ = cr.save();
    cr.rectangle(rect.left(), rect.top(), rect.width(), rect.height());
    cr.clip();
    cr.scale(1.0 / sx, 1.0 / sy);
    cr.set_source_pixbuf(&big, px as f64, py as f64);
    let _ = cr.paint();
    let _ = cr.restore();
}

fn set_text_font(cr: &cairo::Context, size: f64) {
    // pangocairo 未引入依赖，这里用 Cairo toy text API；系统 sans 指向 Noto Sans CJK，中文可渲染。
    cr.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(size);
}

/// 多行文字，纯填充，无描边。
fn draw_text(cr: &cairo::Context, pos: (f64, f64), text: &str, color: &Color, size: f64) {
    if text.is_empty() {
        return;
    }
    set_text_font(cr, size);
    let Ok(fe) = cr.font_extents() else {
        return;
    };

    cr.set_source_rgb(color.r, color.g, color.b);
    let mut y = pos.1 + fe.ascent();
    for line in text.split('\n') {
        if !line.is_empty() {
            cr.new_path();
            cr.move_to(pos.0, y);
            let _ = cr.show_text(line);
        }
        y += fe.height();
    }
}

/// 文字光标几何：`(x, 顶部 y, 行高)`；非文字图元返回 `None`。
///
/// 需要一个 Cairo 上下文来测量最后一行的宽度，函数内部会先绑定该图元的字体。
fn caret_of(cr: &cairo::Context, shape: &Shape) -> Option<(f64, f64, f64)> {
    let Shape::Text {
        pos, text, size, ..
    } = shape
    else {
        return None;
    };
    set_text_font(cr, *size);
    let fe = cr.font_extents().ok()?;
    let lines = text.split('\n').count().max(1);
    let last = text.split('\n').next_back().unwrap_or("");
    let advance = cr.text_extents(last).map(|e| e.x_advance()).unwrap_or(0.0);
    let baseline = pos.1 + fe.ascent() + fe.height() * (lines as f64 - 1.0);
    Some((
        pos.0 + advance + 1.0,
        baseline - fe.ascent(),
        fe.ascent() + fe.descent(),
    ))
}

/// 文字输入中的光标竖线（只在正在编辑的图元上画）。
pub fn draw_text_caret(cr: &cairo::Context, shape: &Shape) {
    let Some((x, top, h)) = caret_of(cr, shape) else {
        return;
    };
    let Shape::Text { color, .. } = shape else {
        return;
    };
    cr.new_path();
    cr.set_line_width(1.5);
    cr.set_source_rgb(color.r, color.g, color.b);
    cr.move_to(x, top);
    cr.line_to(x, top + h);
    let _ = cr.stroke();
}

/// 供输入法候选窗定位使用：脱离绘制流程单独量一次文字光标位置。
///
/// 用 1×1 的离屏 surface 只为拿到字体度量，不产生实际绘制开销。
pub fn text_caret_metrics(shape: &Shape) -> Option<(f64, f64, f64)> {
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1).ok()?;
    let cr = cairo::Context::new(&surface).ok()?;
    caret_of(&cr, shape)
}

/// 绘制输入法预编辑串：光标处的深色衬底 + 白字 + 下划线 + 串内光标。
///
/// Wayland 下 ibus 的候选窗由输入法自己弹出，但预编辑（还没上屏的拼音）必须由客户端自己画，
/// 否则用户看不到自己敲了什么 —— 这正是自绘编辑器必须补上的一环。
pub fn draw_preedit(cr: &cairo::Context, shape: &Shape, preedit: &str, cursor: i32) {
    if preedit.is_empty() {
        return;
    }
    let Some((x, top, h)) = caret_of(cr, shape) else {
        return;
    };
    let Shape::Text { size, .. } = shape else {
        return;
    };
    let Ok(fe) = cr.font_extents() else {
        return;
    };
    let Ok(ext) = cr.text_extents(preedit) else {
        return;
    };
    let w = ext.x_advance();

    cr.new_path();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.78);
    cr.rectangle(x, top, w + 6.0, h);
    let _ = cr.fill();

    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.move_to(x + 3.0, top + fe.ascent());
    let _ = cr.show_text(preedit);

    // 下划线：预编辑串的通用视觉约定，提示这段文字尚未确认。
    cr.new_path();
    cr.set_line_width((size * 0.06).max(1.0));
    cr.move_to(x + 3.0, top + h - 1.0);
    cr.line_to(x + 3.0 + w, top + h - 1.0);
    let _ = cr.stroke();

    // 串内光标：`cursor` 是字符偏移，按 char 截断后重新量宽。
    let head: String = preedit.chars().take(cursor.max(0) as usize).collect();
    let cx = x + 3.0 + cr.text_extents(&head).map(|e| e.x_advance()).unwrap_or(0.0);
    cr.new_path();
    cr.set_line_width(1.5);
    cr.move_to(cx, top);
    cr.line_to(cx, top + h);
    let _ = cr.stroke();
}
