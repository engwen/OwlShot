//! 选区工具栏：纯 Cairo 自绘，紧贴选区显示。
//!
//! 不用 GTK 控件（Overlay + Box）而选择自绘的原因：
//!   1. 工具栏要跟随选区实时移动，GTK 布局改坐标要走一遍 measure/allocate，抖动明显；
//!   2. 主题会给按钮塞上系统样式（圆角、阴影、配色），与「不要 GNOME 原生观感」的要求冲突；
//!   3. 图标用矢量直接画，不引入任何图标资源与额外依赖。
//! 本模块只做布局 / 绘制 / 命中，不持有状态，动作交回 editor 处理。

use crate::annotate::{Color, FONT_SIZES, PALETTE, Rect, Tool};
use gtk4::cairo;
use std::f64::consts::{PI, TAU};

/// 按钮边长。
const BTN: f64 = 26.0;
/// 按钮间距。
const GAP: f64 = 3.0;
/// 工具栏内边距。
const PAD: f64 = 6.0;
/// 工具组之间的额外间隙。
const SEP: f64 = 10.0;
/// 与选区边缘的距离。
const MARGIN: f64 = 8.0;
/// 背景圆角。
const RADIUS: f64 = 6.0;
/// 单行按钮上限：过长的横条会超出屏幕，超出即自动换行。
const MAX_PER_ROW: usize = 24;

/// 工具栏上的一个按钮。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Tool(Tool),
    /// 调色板色块，取值为 `PALETTE` 下标。
    Color(usize),
    /// 字号下拉框按钮（点击展开/收起）。
    FontSizeDrop,
    /// 字号下拉列表中的某个选项，取值为 `FONT_SIZES` 下标。仅在下拉展开时存在。
    FontSizePick(usize),
    /// 调色盘：打开系统颜色选择器。
    ColorPicker,
    Undo,
    Redo,
    Copy,
    Save,
    Cancel,
}

/// 按钮顺序：11 个标注工具 / 1 个字号下拉框 / 8 个调色板色块 + 1 个调色盘 / 5 个操作按钮，四组之间留间隙。
const ITEMS: [Item; 26] = [
    Item::Tool(Tool::Rect),
    Item::Tool(Tool::FillRect),
    Item::Tool(Tool::Ellipse),
    Item::Tool(Tool::Line),
    Item::Tool(Tool::Arrow),
    Item::Tool(Tool::Pen),
    Item::Tool(Tool::Marker),
    Item::Tool(Tool::Counter),
    Item::Tool(Tool::Mosaic),
    Item::Tool(Tool::Blur),
    Item::Tool(Tool::Text),
    Item::FontSizeDrop,
    Item::Color(0),
    Item::Color(1),
    Item::Color(2),
    Item::Color(3),
    Item::Color(4),
    Item::Color(5),
    Item::Color(6),
    Item::Color(7),
    Item::ColorPicker,
    Item::Undo,
    Item::Redo,
    Item::Copy,
    Item::Save,
    Item::Cancel,
];

/// 分组分界下标：这些按钮之前插入 SEP 间隙。
const BREAKS: [usize; 3] = [11, 12, 21];

/// 字号下拉按钮宽度（比普通按钮宽，能放下 "18 ▾" 标签）。
const DROP_W: f64 = BTN * 2.2;

/// 一次布局的结果：整体范围 + 每个按钮的矩形。
pub struct Toolbar {
    pub bounds: Rect,
    slots: Vec<(Item, Rect)>,
}

impl Toolbar {
    /// 依据选区与屏幕尺寸摆放工具栏（横排多行，居中放在选区正下方）。
    ///
    /// 横向：以选区水平中心为基准居中，超出视口时左右夹住。
    /// 纵向：贴在选区底边下方 MARGIN 处；放不下时整体上移到选区上方。
    /// 按钮太多一行放不下时自动换行。
    pub fn layout(rect: &Rect, vw: f64, vh: f64, font_size_drop_open: bool) -> Self {
        // 计算每列宽度：普通按钮 BTN，字号下拉框 DROP_W。
        let col_widths: Vec<f64> = ITEMS
            .iter()
            .map(|item| {
                if *item == Item::FontSizeDrop {
                    DROP_W
                } else {
                    BTN
                }
            })
            .collect();
        let total = ITEMS.len();
        // 估算一行能放多少列：用 BTN 作基准，再微调。
        let avail_w = vw - PAD * 2.0 - SEP * 2.0;
        let fit = ((avail_w + GAP) / (BTN + GAP)).floor() as usize;
        let per_row = fit.clamp(1, MAX_PER_ROW).min(total);
        let rows = total.div_ceil(per_row);
        let cols = total.div_ceil(rows);

        // 记录每个分组分界所在行及其行内列号，用于后续偏移。
        let break_locs: Vec<(usize, usize)> = BREAKS
            .iter()
            .map(|&b| (b / cols, b % cols))
            .collect();
        // 计算最大行宽（各列按实际宽度累加）。
        let row_width = |row: usize| -> f64 {
            let start = row * cols;
            let end = (start + cols).min(total);
            let base: f64 = col_widths[start..end].iter().sum::<f64>()
                + GAP * (end - start - 1) as f64;
            let off: f64 = break_locs
                .iter()
                .filter(|&&(br, _bc)| br == row)
                .count() as f64
                * SEP;
            base + off
        };
        let w = PAD * 2.0 + (0..rows).map(row_width).fold(0.0f64, f64::max);
        let h = PAD * 2.0 + rows as f64 * BTN + (rows as f64 - 1.0) * GAP;

        // 横向右对齐于选区右边缘。
        let x = (rect.right() - w).clamp(0.0, (vw - w).max(0.0));
        // 纵向优先贴选区下方，放不下则贴上方。
        let y_below = rect.bottom() + MARGIN;
        let y = if y_below + h <= vh {
            y_below
        } else {
            (rect.top() - MARGIN - h).clamp(0.0, (vh - h).max(0.0))
        };

        let mut bounds = Rect::new(x, y, x + w, y + h);
        let mut slots = Vec::with_capacity(total);
        let mut drop_rect = Rect::new(0.0, 0.0, 0.0, 0.0);

        for (i, item) in ITEMS.into_iter().enumerate() {
            let (row, col) = (i / cols, i % cols);
            // 计算该列的累计 X 偏移。
            let start = row * cols;
            let col_x_off: f64 = col_widths[start..start + col]
                .iter()
                .map(|w| w + GAP)
                .sum();
            let break_off: f64 = break_locs
                .iter()
                .filter(|&&(br, bc)| br == row && bc < col)
                .count() as f64
                * SEP;
            let bx = x + PAD + col_x_off + break_off;
            let by = y + PAD + row as f64 * (BTN + GAP);
            let cw = col_widths[i];
            let r = Rect::new(bx, by, bx + cw, by + BTN);
            if item == Item::FontSizeDrop {
                drop_rect = r;
            }
            slots.push((item, r));
        }

        // 下拉展开时，追加字号选项到 slots，并扩展 bounds 包含下拉区域。
        if font_size_drop_open {
            let opt_h = BTN * 0.85;
            let opt_x = drop_rect.left();
            let mut opt_y = drop_rect.bottom() + 2.0;
            for (i, _size) in FONT_SIZES.iter().enumerate() {
                slots.push((
                    Item::FontSizePick(i),
                    Rect::new(opt_x, opt_y, opt_x + DROP_W, opt_y + opt_h),
                ));
                opt_y += opt_h + 1.0;
            }
            // 下拉列表下方留出一点 padding。
            let drop_bottom = opt_y + 2.0;
            if drop_bottom > bounds.bottom() {
                bounds = Rect::new(bounds.left(), bounds.top(), bounds.right(), drop_bottom);
            }
        }

        Self {
            bounds,
            slots,
        }
    }

    /// 命中测试；工具栏区域内的点击不应落到画布上。
    pub fn hit(&self, x: f64, y: f64) -> Option<Item> {
        self.slots
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(item, _)| *item)
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        self.bounds.contains(x, y)
    }

    /// `active` 为当前选中的工具，`hover` 为鼠标所在按钮，`color` 为当前标注色。
    pub fn draw(
        &self,
        cr: &cairo::Context,
        active: Option<Tool>,
        hover: Option<Item>,
        color: Color,
        font_size: f64,
        can_undo: bool,
        can_redo: bool,
    ) {
        let b = &self.bounds;
        rounded_rect(cr, b.left(), b.top(), b.width(), b.height(), RADIUS);
        cr.set_source_rgba(0.10, 0.11, 0.13, 0.94);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.12);
        let _ = cr.stroke();

        for &(item, ref r) in &self.slots {
            let selected = match item {
                Item::Tool(t) => active == Some(t),
                Item::Color(i) => PALETTE[i] == color,
                Item::FontSizePick(i) => (FONT_SIZES[i] - font_size).abs() < 0.5,
                _ => false,
            };
            let dimmed = (item == Item::Undo && !can_undo) || (item == Item::Redo && !can_redo);

            // 色块自成一套观感：整格填色 + 选中白环，不套用通用高亮底。
            if let Item::Color(i) = item {
                let c = PALETTE[i];
                rounded_rect(cr, r.left() + 5.0, r.top() + 5.0, BTN - 10.0, BTN - 10.0, 3.0);
                cr.set_source_rgb(c.r, c.g, c.b);
                let _ = cr.fill();
                if selected || hover == Some(item) {
                    rounded_rect(cr, r.left() + 3.0, r.top() + 3.0, BTN - 6.0, BTN - 6.0, 4.0);
                    cr.set_line_width(1.4);
                    let a = if selected { 0.95 } else { 0.35 };
                    cr.set_source_rgba(1.0, 1.0, 1.0, a);
                    let _ = cr.stroke();
                }
                continue;
            }

            // 字号下拉框：显示当前字号 + 下箭头。
            if item == Item::FontSizeDrop {
                if hover == Some(item) {
                    rounded_rect(cr, r.left(), r.top(), r.width(), r.height(), 4.0);
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.14);
                    let _ = cr.fill();
                }
                // 字号数值。
                let label = format!("{} ▾", font_size as i32);
                cr.select_font_face(
                    "sans-serif",
                    cairo::FontSlant::Normal,
                    cairo::FontWeight::Normal,
                );
                cr.set_font_size(11.0);
                if let Ok(ext) = cr.text_extents(&label) {
                    cr.new_path();
                    cr.move_to(
                        r.left() + (r.width() - ext.width()) / 2.0 - ext.x_bearing(),
                        r.top() + (r.height() - ext.height()) / 2.0 - ext.y_bearing(),
                    );
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.92);
                    let _ = cr.show_text(&label);
                }
                continue;
            }

            // 字号下拉选项。
            if let Item::FontSizePick(i) = item {
                // 弹出菜单背景（仅第一个选项时画整个背景）。
                if i == 0 {
                    let pop_top = r.top();
                    let pop_h = FONT_SIZES.len() as f64 * (BTN * 0.85 + 1.0) + 4.0;
                    rounded_rect(cr, r.left() - 2.0, pop_top - 2.0, r.width() + 4.0, pop_h, 5.0);
                    cr.set_source_rgba(0.10, 0.11, 0.13, 0.97);
                    let _ = cr.fill();
                }
                if selected {
                    rounded_rect(cr, r.left(), r.top(), r.width(), r.height(), 3.0);
                    cr.set_source_rgba(0.20, 0.55, 1.0, 0.85);
                    let _ = cr.fill();
                } else if hover == Some(item) {
                    rounded_rect(cr, r.left(), r.top(), r.width(), r.height(), 3.0);
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.14);
                    let _ = cr.fill();
                }
                let label = format!("{} px", FONT_SIZES[i] as i32);
                cr.select_font_face(
                    "sans-serif",
                    cairo::FontSlant::Normal,
                    cairo::FontWeight::Normal,
                );
                cr.set_font_size(11.0);
                if let Ok(ext) = cr.text_extents(&label) {
                    cr.new_path();
                    cr.move_to(
                        r.left() + (r.width() - ext.width()) / 2.0 - ext.x_bearing(),
                        r.top() + (r.height() - ext.height()) / 2.0 - ext.y_bearing(),
                    );
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.92);
                    let _ = cr.show_text(&label);
                }
                continue;
            }

            // 调色盘按钮：渐变彩色圆。
            if item == Item::ColorPicker {
                if hover == Some(item) {
                    rounded_rect(cr, r.left(), r.top(), r.width(), r.height(), 4.0);
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.14);
                    let _ = cr.fill();
                }
                let cx = r.left() + r.width() / 2.0;
                let cy = r.top() + r.height() / 2.0;
                let radius = BTN * 0.35;
                // 画四个彩色扇区示意调色盘。
                let colors = [
                    (1.0, 0.3, 0.3),
                    (0.3, 1.0, 0.3),
                    (0.3, 0.3, 1.0),
                    (1.0, 1.0, 0.3),
                ];
                for (i, &(cr_r, cg, cb)) in colors.iter().enumerate() {
                    let start = PI * 0.5 * i as f64;
                    cr.new_path();
                    cr.move_to(cx, cy);
                    cr.arc(cx, cy, radius, start, start + PI * 0.5);
                    cr.close_path();
                    cr.set_source_rgb(cr_r, cg, cb);
                    let _ = cr.fill();
                }
                continue;
            }

            if selected {
                rounded_rect(cr, r.left(), r.top(), r.width(), r.height(), 4.0);
                cr.set_source_rgba(0.20, 0.55, 1.0, 0.85);
                let _ = cr.fill();
            } else if hover == Some(item) && !dimmed {
                rounded_rect(cr, r.left(), r.top(), r.width(), r.height(), 4.0);
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.14);
                let _ = cr.fill();
            }

            let alpha = if dimmed { 0.30 } else { 0.92 };
            match item {
                Item::Cancel => cr.set_source_rgba(1.0, 0.45, 0.42, alpha),
                Item::Save => cr.set_source_rgba(0.45, 0.90, 0.55, alpha),
                _ => cr.set_source_rgba(1.0, 1.0, 1.0, alpha),
            }
            draw_icon(cr, item, r);
        }
    }
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, PI * 1.5);
    cr.close_path();
}

/// 16x16 逻辑网格内的矢量图标，按钮居中。
fn draw_icon(cr: &cairo::Context, item: Item, slot: &Rect) {
    let unit = 16.0;
    let (ox, oy) = (
        slot.left() + (slot.width() - unit) / 2.0,
        slot.top() + (slot.height() - unit) / 2.0,
    );
    let p = |x: f64, y: f64| (ox + x, oy + y);

    cr.new_path();
    cr.set_line_width(1.6);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);

    match item {
        Item::Tool(Tool::Rect) => {
            let (x, y) = p(2.5, 3.5);
            cr.rectangle(x, y, 11.0, 9.0);
            let _ = cr.stroke();
        }
        Item::Tool(Tool::FillRect) => {
            let (x, y) = p(2.5, 3.5);
            cr.rectangle(x, y, 11.0, 9.0);
            let _ = cr.fill();
        }
        Item::Tool(Tool::Ellipse) => {
            let (cx, cy) = p(8.0, 8.0);
            let _ = cr.save();
            cr.translate(cx, cy);
            cr.scale(5.5, 4.5);
            cr.new_path();
            cr.arc(0.0, 0.0, 1.0, 0.0, TAU);
            let _ = cr.restore();
            let _ = cr.stroke();
        }
        Item::Tool(Tool::Line) => {
            let (x, y) = p(2.5, 13.0);
            cr.move_to(x, y);
            cr.line_to(x + 11.0, y - 10.5);
            let _ = cr.stroke();
        }
        Item::Tool(Tool::Arrow) => {
            let (fx, fy) = p(3.0, 13.0);
            let (tx, ty) = p(13.0, 3.0);
            cr.move_to(fx, fy);
            cr.line_to(tx, ty);
            let _ = cr.stroke();
            cr.new_path();
            cr.move_to(tx, ty);
            cr.line_to(tx - 6.0, ty + 1.5);
            cr.line_to(tx - 1.5, ty + 6.0);
            cr.close_path();
            let _ = cr.fill();
        }
        Item::Tool(Tool::Pen) => {
            let (x, y) = p(2.0, 11.0);
            cr.move_to(x, y);
            cr.curve_to(x + 3.0, y - 9.0, x + 7.0, y + 3.0, x + 12.0, y - 6.0);
            let _ = cr.stroke();
        }
        Item::Tool(Tool::Marker) => {
            // 粗斜条 + 底部细线，示意荧光笔的宽笔头。
            let (x, y) = p(3.0, 11.5);
            cr.set_line_width(5.0);
            cr.move_to(x, y);
            cr.line_to(x + 8.0, y - 8.0);
            let _ = cr.stroke();
            cr.new_path();
            cr.set_line_width(1.6);
            let (bx, by) = p(2.5, 14.0);
            cr.move_to(bx, by);
            cr.line_to(bx + 11.0, by);
            let _ = cr.stroke();
        }
        Item::Tool(Tool::Counter) => {
            // 空心圆 + 中间的 "1"。
            let (cx, cy) = p(8.0, 8.0);
            cr.arc(cx, cy, 6.0, 0.0, TAU);
            let _ = cr.stroke();
            cr.new_path();
            cr.move_to(cx - 1.5, cy - 2.0);
            cr.line_to(cx, cy - 3.5);
            cr.line_to(cx, cy + 3.5);
            let _ = cr.stroke();
        }
        Item::Tool(Tool::Mosaic) => {
            // 3x3 棋盘格，隔一格填一个。
            for row in 0..3 {
                for col in 0..3 {
                    if (row + col) % 2 != 0 {
                        continue;
                    }
                    let (x, y) = p(2.5 + col as f64 * 3.7, 2.5 + row as f64 * 3.7);
                    cr.rectangle(x, y, 3.4, 3.4);
                }
            }
            let _ = cr.fill();
        }
        Item::Tool(Tool::Blur) => {
            // 三条自上而下逐渐变淡的横线，示意模糊过渡。
            let alphas = [0.95, 0.6, 0.3];
            for (i, a) in alphas.into_iter().enumerate() {
                cr.new_path();
                cr.set_source_rgba(1.0, 1.0, 1.0, a);
                cr.set_line_width(2.6);
                let (x, y) = p(2.5, 4.0 + i as f64 * 4.0);
                cr.move_to(x, y);
                cr.line_to(x + 11.0, y);
                let _ = cr.stroke();
            }
        }
        Item::Tool(Tool::Text) => {
            let (lx, ly) = p(2.5, 3.5);
            cr.move_to(lx, ly);
            cr.line_to(lx + 11.0, ly);
            let _ = cr.stroke();
            cr.new_path();
            let (mx, my) = p(8.0, 3.5);
            cr.move_to(mx, my);
            cr.line_to(mx, my + 9.5);
            let _ = cr.stroke();
        }
        // 色块在 draw 里已单独绘制，不走图标路径。
        Item::Color(_) => {}
        // 字号按钮在 draw 里已单独绘制，不走图标路径。
        Item::FontSizeDrop => {}
        Item::FontSizePick(_) => {}
        // 调色盘在 draw 里已单独绘制，不走图标路径。
        Item::ColorPicker => {}
        Item::Undo => {
            // 逆时针弧 + 箭头尾，表示回退一步。
            let (cx, cy) = p(8.5, 9.0);
            cr.new_path();
            cr.arc(cx, cy, 5.0, PI, TAU - 0.3);
            let _ = cr.stroke();
            cr.new_path();
            cr.move_to(cx - 5.0, cy);
            cr.line_to(cx - 8.0, cy - 3.0);
            cr.line_to(cx - 2.0, cy - 3.0);
            cr.close_path();
            let _ = cr.fill();
        }
        Item::Redo => {
            // 与撤销镜像。
            let (cx, cy) = p(7.5, 9.0);
            cr.new_path();
            cr.arc_negative(cx, cy, 5.0, 0.0, PI + 0.3);
            let _ = cr.stroke();
            cr.new_path();
            cr.move_to(cx + 5.0, cy);
            cr.line_to(cx + 8.0, cy - 3.0);
            cr.line_to(cx + 2.0, cy - 3.0);
            cr.close_path();
            let _ = cr.fill();
        }
        Item::Copy => {
            // 两个错位矩形，经典「复制」符号。
            let (bx, by) = p(2.0, 2.0);
            cr.rectangle(bx, by, 8.5, 8.5);
            let _ = cr.stroke();
            cr.new_path();
            let (fx, fy) = p(5.5, 5.5);
            cr.rectangle(fx, fy, 8.5, 8.5);
            let _ = cr.stroke();
        }
        Item::Save => {
            // 对勾。
            let (x, y) = p(3.0, 8.5);
            cr.move_to(x, y);
            cr.line_to(x + 3.5, y + 3.5);
            cr.line_to(x + 10.0, y - 5.0);
            let _ = cr.stroke();
        }
        Item::Cancel => {
            let (x, y) = p(4.0, 4.0);
            cr.move_to(x, y);
            cr.line_to(x + 8.0, y + 8.0);
            cr.move_to(x + 8.0, y);
            cr.line_to(x, y + 8.0);
            let _ = cr.stroke();
        }
    }
}
