# OwlShot 项目进度文档
二进制名称：owlshot
环境：Ubuntu24.04 GNOME Wayland
约束：优先标准xdg‑desktop‑portal；Mutter私有接口仅作为可选开关，默认关闭。

## TODO列表
- [x] 阶段1：MVP截图捕获、托盘、保存复制剪贴板
  - [x] 初始化Cargo项目并配置依赖（ashpd/ksni/gtk4/tokio/async-channel/anyhow）
  - [x] xdg-desktop-portal截图核心逻辑（src/capture.rs）
  - [x] StatusNotifierItem系统托盘（src/tray.rs）
  - [x] 保存文件 + 剪贴板双路径（wl-copy优先，GDK4自动降级）（src/clipboard.rs）
  - [x] cargo build 编译验证通过，无警告
  - [x] 本机GUI实测通过（托盘可见、菜单可点、portal截图落盘成功）
- [x] 阶段2：GTK4标注编辑器（画笔/方框/圆形/箭头/马赛克/文字/撤销）
  - [x] 2.1 编辑器窗口骨架 + 自绘选区（暗化遮罩、细边框、小手柄、实时尺寸）
  - [x] 2.2 画笔 / 方框 / 圆形 / 箭头
  - [x] 2.3 马赛克 / 文字
  - [x] 2.4 撤销栈 + 工具栏
  - [x] 2.5 导出裁剪合成 → 保存 + 剪贴板
- [x] 阶段3：全局快捷键 + toml配置文件
  - [x] portal GlobalShortcuts 注册与信号分发（src/shortcuts.rs）
  - [x] toml 配置文件读取 + 首次运行生成模板（src/config.rs）
  - [x] CLI 单次截图模式 `--region` / `--full`（供桌面自定义快捷键绑定）
- [x] 阶段4：GNOME专属Mutter overlay可选增强模块
  - [x] org.gnome.Shell.Screenshot 封装 + 桌面环境探测（src/mutter.rs）
  - [x] 全屏抓图快路径 + 窗口截图能力，失败自动回落标准 portal
  - [x] 托盘「窗口截图」菜单项与 `--window` CLI（仅启用时出现）
- [x] 阶段5：README文档、构建说明
  - [x] README.md：功能、硬约束表、系统依赖、构建安装、运行方式
  - [x] GNOME 自定义快捷键绑定步骤 + config.toml 字段表 + 触发器语法
  - [x] 编辑器操作表、GNOME 私有增强说明、已知限制、项目结构与线程模型

## 当前状态：5 个阶段 + 增强轮次 2（6 项需求）代码全部完成并编译通过（零警告），待本机 GUI 实测

### 阶段1 实测结论
- 托盘：`org.kde.StatusNotifierItem` 注册成功，GNOME 顶栏可见（依赖 AppIndicator 类扩展）
- 截图：portal 链路可用，`~/图片/Screenshots/owlshot-*.png` 落盘成功（实测 930x477、1342x573 两张）
- 剪贴板：本机 `wl-copy` 实际已安装于 `/usr/bin/wl-copy`（此前文档记录有误），走 wl-copy 主路径
- 已知环境噪音：从非登录会话终端启动时会报 `dconf-CRITICAL ... /run/user/1000/dconf/user 权限不够`，从桌面终端正常启动不出现，不影响功能

### 阶段2 设计决定（用户否决 portal 交互式 UI 后的新方案）
用户反馈：portal `interactive(true)` 弹出的是 GNOME 原生截屏界面，选区手柄过大、观感差，不接受。
因此改为：portal 抓**静态全屏 PNG** → 自绘 GTK4 全屏窗口显示这张图 → Cairo 绘制暗化遮罩 / 细边框 / 小手柄 / 实时尺寸。
- portal 调用统一改为 `interactive(false)`，不再使用系统对话框选区
- 不引入 wlr-layer-shell（GNOME 不支持），用普通 fullscreen 无边框窗口覆盖
- GNOME Mutter 私有接口按原计划推迟到阶段4，阶段2 不使用

### 2.1 实现要点（src/editor.rs，已编译通过 · 零警告）
- 全屏无边框窗口（`set_decorated(false)` + `fullscreen_on_monitor`），单个 `DrawingArea` 全部自绘
- 遮罩透明度 0.45；选区内 `clip` 后重绘原图还原亮度；1px 蓝边框做半像素偏移保证锐利
- 手柄 8 向、边长 6px（对应用户「四个点太大了」的反馈），命中容差 7px，hover 变色 + 光标随方位切换
- 尺寸标签显示**物理像素**，贴选区左上角，越界自动翻转并 clamp 到屏内
- HiDPI：不依赖 `Monitor::scale()`，用「原图物理像素 ÷ DrawingArea 逻辑尺寸」算实际比例，分数缩放同样正确，导出恒为原始分辨率
- 交互：拖拽创建 / 内部拖动 / 8 向缩放；双击或 Enter/Space 确认，Esc 直接退出，右键取消
- 键盘控制器挂在 **Window** 上并设为 `PropagationPhase::Capture`；`DrawingArea` 需 `set_focusable(true)`
  （GTK4 中 `can_focus` 默认已 true，真正生效的是 `focusable`，此前挂在 area 上导致 Esc 完全收不到）
- Pixbuf 非 Send，裁剪与落盘留在 GLib 主线程，仅通过 async-channel 回传 PNG 路径
- 刻意绕开已 deprecated 的 `pixbuf_get_from_surface` / `pixbuf_get_from_texture`

待本机实测项：全屏窗口在 GNOME 下能否稳定拿到键盘焦点；portal 非交互模式是否每次弹授权

### 2.2 - 2.5 实现要点（src/annotate.rs · src/toolbar.rs · src/editor.rs）
- 图元定义集中在 `annotate.rs`：`Rect` / `Color` / `Tool`（6 种）/ `Shape`（6 变体），坐标一律存**逻辑（视图）坐标**
- 绘制入口统一为 `Shape::draw(cr, &Canvas)`，`Canvas { shot, scale }` 携带底图与缩放比，预览与导出共用同一套代码
- 工具快捷键：`r` 方框 / `o` 圆形 / `a` 箭头 / `p` 画笔 / `m` 马赛克 / `t` 文字；再按一次同键退出该工具
- 撤销：`Ctrl+Z` 弹出 `shapes` 栈顶；无可撤销内容时不做任何事（不误关窗口）
- Esc 两级语义：先退出标注工具，已在选区模式才整体取消
- 图元拖拽点被 `Rect::clamp_point` 夹在选区内，导出裁剪后不会出现半截标注
- 椭圆用 `save → translate → scale → arc → restore → stroke`，避免线宽被非等比缩放拉扁
- 箭头头部 `head = (stroke * 4 + 6).min(len * 0.4)`，线段缩到箭头根部再画实心三角，短箭头不会糊成一团
- 画笔按 1px 间隔抽稀采样点，重绘不卡
- 马赛克：`Pixbuf` 两次 `scale_simple`（Bilinear 缩小取块均值 → Nearest 放大出硬边方块），块数按逻辑尺寸算，HiDPI 下视觉块大小不变
- 文字：未引入 `pangocairo`，用 Cairo toy text API（系统 sans 指向 Noto Sans CJK，中文可渲染）；黑色半透明描边 + 彩色填充保证深浅底都可读；`pos` 为文字块左上角，绘制时 `+ ascent` 换算基线
- 文字输入：`State.editing` 标志让按键优先给文本，字符键落字 / BackSpace 退格 / Enter 换行 / Ctrl+Enter 或 Esc 落定；未引入 IMContext，中文输入法预编辑留作后续增强
- 图元落定统一收口到 `State::commit_active()`（drag_end、切工具、Esc、确认导出四处共用），正在敲的文字不会丢
- 工具栏（`toolbar.rs`）纯 Cairo 自绘而非 GTK Overlay+Box：跟随选区实时移动不抖动、不被系统主题污染观感、矢量图标零资源依赖；模块只做布局/绘制/命中，动作交回 editor
- 工具栏摆位三级回退：选区下方外侧 → 上方外侧 → 压在选区内部底边；横向超界回拉，任何选区尺寸都可见可点；拖拽过程中隐藏
- 导出合成：`ImageSurface(Rgb24, w, h)` → 负偏移贴底图 → `translate` + `scale(pixel_scale)` 回到逻辑坐标 → 遍历 `Shape::draw` → 逐行按原生字节序整数取通道转 `Pixbuf(RGB888)` → `savev` 落盘
  （cairo-rs 的 `png` feature 未启用，不能用 `write_to_png`；无标注时走 `new_subpixbuf` 快路径，像素与原图完全一致）

待本机实测项（2.2 - 2.5）：马赛克与文字观感、工具栏点击热区、带标注导出的对齐精度

### 阶段3 环境结论与设计决定（src/config.rs · src/shortcuts.rs · src/main.rs）
- **本机 portal 不提供 GlobalShortcuts 接口**（实测三方交叉验证）：
  `gdbus introspect --session --dest org.freedesktop.portal.Desktop` 列出的 28 个接口中无 `org.freedesktop.portal.GlobalShortcuts`；
  `/usr/share/xdg-desktop-portal/portals/*.portal` 中亦无该接口声明；
  版本：xdg-desktop-portal 1.18.4 / xdg-desktop-portal-gnome 46.2 / xdg-desktop-portal-gtk 1.15.1
- 用户决策：**portal + CLI 双轨**。仍按硬约束完整实现 portal GlobalShortcuts 代码路径（未来 GNOME 升级即自动可用），
  同时提供 CLI 单次模式让用户在桌面环境自带的自定义快捷键里绑定。**绝不抓键盘、绝不用 X11 grab。**
- CLI：`owlshot --region` / `--full` 单次截图后退出（不注册托盘、不注册快捷键），另有 `--help` / `--version`；
  参数手写解析（`std::env::args()`），不引入 clap。GNOME 绑定路径：设置 → 键盘 → 查看及自定义快捷键 → 自定义快捷键
- 单次模式实现：`main()` 里预先把 `Action::Once` + `Action::Quit` 塞进 channel，工作循环处理完自然退出并 `main_loop.quit()`
- 配置：`~/.config/owlshot/config.toml`（路径由 `glib::user_config_dir()` 给出），`OnceLock` 缓存，进程内只读一次
  - 文件缺失 → 写出带中文注释的模板并用默认值继续；解析失败 → 只告警并回落默认值，绝不阻断截图
  - 全字段 `#[serde(default)]`，任意字段缺失都能启动
  - `[shortcuts] enabled / region / fullscreen`（触发器 XDG 语法：`CTRL+SHIFT+a`，修饰键限 CTRL/ALT/SHIFT/NUM/LOGO，键名取 xkbcommon keysym 去 `XKB_KEY_` 前缀）
  - `[capture] save_dir`（留空 = XDG 图片目录/Screenshots，支持 `~` 展开）/ `file_prefix` / `copy_to_clipboard`
  - `[advanced] use_mutter_overlay`（阶段4 用，默认 false）
- `capture.rs` 的落盘路径改为读配置（目录 + 文件名前缀）；`copy_to_clipboard = false` 时跳过剪贴板写入
- shortcuts.rs：`GlobalShortcuts::new()` → `create_session(CreateSessionOptions::default())` →
  `bind_shortcuts(&session, &[NewShortcut..], None, BindShortcutsOptions::default())` → `receive_activated()` 流循环，
  按 `shortcut_id()` 映射成 `Action` 并 `try_send`（不阻塞 D-Bus 任务，截图进行中重复触发直接丢弃）
- Session 必须持有到退出（drop 即注销），故 `worker()` 与 ksni handle 并列持有 `ShortcutGuard`，退出前 `close()`
- 接口缺失 / 用户拒绝授权时只打印降级提示（引导 CLI 绑定），托盘功能完全不受影响
- 依赖新增：ashpd feature `global_shortcuts`（零依赖 feature）、`toml 1.1`、`serde 1.0 + derive`、`futures-util 0.3`（消费 Activated 流需 `StreamExt`）

待本机实测项（阶段3）：GNOME 自定义快捷键绑定 `owlshot --region` 的实际手感；配置模板首次生成；改 `save_dir` 后落盘位置

### 阶段4 实现要点（src/mutter.rs）
- 默认关闭：`advanced.use_mutter_overlay = false`；探测结果用 `OnceLock` 只算一次，避免每次截图重复打印
- **KDE / Plasma / Sway / wlroots / Hyprland 强制禁用**（匹配 `XDG_CURRENT_DESKTOP`，即使配置开启也拒绝）；非 GNOME 桌面同样忽略
- 只用 `org.gnome.Shell.Screenshot` 的 `Screenshot` 与 `ScreenshotWindow`；**刻意不用 `SelectArea`**
  （其选区观感正是用户否决的那套，选区一律交自绘编辑器）
- 走 `ashpd::zbus`（ashpd 已 re-export，不新增依赖）直连会话总线，纯 D-Bus，无任何 X11 调用
- 回落策略：全屏抓图失败只打印告警并回落标准 portal，主链路永不中断；窗口截图无 portal 等价能力，未启用时直接给出可操作的错误提示
- 新增能力接入：`Action::CaptureWindow` + CLI `--window`；托盘「窗口截图」菜单项仅在 `mutter::enabled()` 为真时出现，避免点了必然报错
- 临时文件落 `glib::tmp_dir()`，文件名带 pid，多实例不互相覆盖
- **实测重要结论**：GNOME 45+ 已把该接口收紧，非白名单调用方会拿到
  `org.freedesktop.DBus.Error.AccessDenied: Screenshot is not allowed`（本机 `gdbus call` 实测复现）。
  因此本模块在 GNOME 46 上开启也基本必然回落 portal，属预期行为，代码保留供旧版本 / 未来放开时使用

待本机实测项（阶段4）：开启开关后 `owlshot --window` 是否真被 AccessDenied（预期是），回落链路是否无感

### 阶段5 实现要点（README.md）
- 内容取自代码事实而非设想：CLI 参数抄 `main.rs::parse_args`，配置字段抄 `config.rs` 的 `Default` 实现，
  编辑器快捷键抄 `editor.rs::tool_for_key` / `wire_keys`，依赖列表抄 `Cargo.toml`
- 重点章节：硬约束落地方式对照表、系统依赖（`libgtk-4-dev` + portal + `wl-clipboard`）、
  `export PATH="$HOME/.cargo/bin:$PATH"` 与 `cargo build --release`、`install -Dm755` 到 `~/.local/bin`
- **GNOME 绑定快捷键写成四步操作指引**，并强调命令要填**绝对路径**（图形会话 PATH 常不含 `~/.local/bin`），
  以及与系统自带「截屏」快捷键冲突时先清占用
- 已知限制章节把三条实测结论如实写出：GNOME 46 portal 无 GlobalShortcuts、GNOME 45+ Mutter 接口 AccessDenied、
  托盘需 AppIndicator 扩展；另附中文输入法预编辑未支持、dconf 告警属环境噪音
- 未新增任何依赖与代码，README 为纯文档；`touch src/main.rs` 强制重编后仍零 error 零 warning

待本机实测项（阶段5）：按 README 步骤在真实桌面会话走一遍安装 + 绑定 + 截图全流程，核对文档与实际一致

### 增强：编辑器 Ctrl+C 直接复制选区（src/editor.rs · src/clipboard.rs · src/main.rs）
- 语义：`Ctrl+C` = 落定未提交图元 → 按当前选区（含标注）裁剪 → 写剪贴板 → 关窗；**不在图片目录留文件**
- 按键插在 `wire_keys` 的 `Ctrl+Z` 之后、工具键之前；文字输入态下 `handle_text_key` 遇 ctrl 组合会放行，故编辑文字时按下也生效
- `Outcome` 结果类型由 `Option<PathBuf>` 升级为 `Option<Shot>`（`Shot::Saved(PathBuf)` / `Shot::Copied(Backend)`），
  `main.rs::capture_flow` 对 `Copied` 直接打印并返回，避免 `wl-copy` 被调用两次
- 原 `crop_and_save` 拆成 `crop`（只出 Pixbuf）+ 调用方决定落盘或复制，选区取值抽成 `export_rect`（无选区退化整屏）
- 剪贴板两条后端都要文件输入，故写 `glib::tmp_dir()` 临时 PNG，复制完立即删除（wl-copy 已读入自身进程、GDK 已解码成纹理）
- 新增 `clipboard::copy_png_on_main()`：编辑器回调本就在 GLib 主线程，GDK 降级路径无需再投递，也避免把非 Send 的 Pixbuf 送出线程
- `capture.copy_to_clipboard = false` 不影响该快捷键（显式按键必须生效，否则快捷键形同失效）

待本机实测项（Ctrl+C）：选区+标注后按 Ctrl+C，确认窗口立即关闭、粘贴到聊天/图片编辑器内容正确、图片目录无新文件

## 增强轮次 2：用户提出的 6 项需求（全部完成 · 编译零警告）

- [x] 需求1 工具栏移到选区右侧（src/toolbar.rs）
- [x] 需求2 中文输入法预编辑可见（src/editor.rs · src/annotate.rs）
- [x] 需求3 对齐 Flameshot 工具集（src/annotate.rs · src/toolbar.rs · src/editor.rs）
- [x] 需求4 剪贴板历史（src/history.rs · src/config.rs · src/tray.rs · src/main.rs）
- [x] 需求5 贴图浮动窗口（src/pin.rs）
- [x] 需求6 Ctrl+Alt+A 快捷键不生效（根因：owlshot 不在图形会话 PATH 中）

### 需求1 实现要点（src/toolbar.rs）
- 布局由横排改**竖排**，摆位三级回退：选区右侧外侧 → 左侧外侧 → 压在选区内部右边；纵向与选区顶边对齐，越界上移
- 按钮多到一列放不下时**自动分列**向右扩展：`per_col` 受视口高度与 `MAX_PER_COL = 12` 双重约束，`cols = total.div_ceil(per_col)`，再按列数反算 `rows` 均摊，避免末列只剩一两个
- 分组间隙由 `BREAKS: [usize; 2] = [11, 19]` 驱动（工具组 / 调色板 / 操作组），分界落在列首时不额外留白

### 需求2 实现要点（src/editor.rs · src/annotate.rs）
- 接入 `gtk4::IMMulticontext`（不是 `IMContextSimple`，后者不接 fcitx/ibus）：`set_client_widget(Some(&area))`，进出文字工具时 `focus_in()` / `focus_out()` / `reset()`
- `wire_keys` 在文字输入态**先**走 `im.filter_keypress(&event)`：拼音、候选选择、翻页全部交给输入法，只把它不要的按键交给编辑逻辑
- 接四个信号：`commit`（落字）、`preedit-start` / `preedit-changed`（取 `preedit_string()` 存 `State.preedit = (String, cursor)`）、`preedit-end`（清空）
- 每次光标移动都 `set_cursor_location(&Rectangle)`（逻辑坐标换算成窗口坐标），候选窗才会跟着插入点走，而不是钉在屏幕左上角
- `annotate.rs` 新增 `draw_preedit`：预编辑串画在已提交文本之后，带下划线与不同底色以区分「还没上屏」；`draw_text_caret` 用 `caret_of` 定位闪烁竖线
- **RefCell 借用纪律（踩过坑）**：`im.reset()/focus_in()/focus_out()/set_cursor_location()/filter_keypress()` 都会同步回调 `commit`/`preedit-*` 并 `borrow_mut()`，因此所有 IM 调用必须写在 `state` 借用作用域之外；`if state.borrow().x && f(&state)` 的临时 `Ref` 活到整个条件求值结束，必须先落局部变量

### 需求3 实现要点（src/annotate.rs · src/toolbar.rs）
- 工具从 6 个扩到 **11 个**：方框 / 实心矩形 / 圆形 / 直线 / 箭头 / 画笔 / 荧光笔 / 序号 / 马赛克 / 模糊 / 文字，`Shape` 同步 11 个变体
- 调色板 `PALETTE` 8 色（红/橙/黄/绿/青/蓝/紫/白），点击即改 `State.style.color`
- 操作按钮：撤销 / 重做 / 复制 / 保存 / 取消，共 24 个按钮
- 新增 `Ctrl+Shift+Z` 重做（判定必须排在 `Ctrl+Z` 之前，否则被撤销分支吃掉）；`redo` 栈在任何新落笔时清空
- **滚轮调线宽**（`wire_scroll`）：向上加粗向下变细，`clamp` 到 1..=20；无工具时滚轮不做事
- 荧光笔 = 半透明宽线 `set_operator(Operator::Multiply)`；模糊 = 多次 `scale_simple(Bilinear)` 缩放往返；序号工具每落一个自增 `State.style.counter`
- 工具快捷键（与 Flameshot 尽量一致）：`r` 方框 / `f` 实心 / `o` 圆 / `l` 直线 / `a` 箭头 / `p` 画笔 / `h` 荧光笔 / `n` 序号 / `m` 马赛克 / `b` 模糊 / `t` 文字，再按一次同键退出

### 需求4 实现要点（src/history.rs · src/config.rs · src/tray.rs · src/main.rs）
- **「Ctrl+V+1」这种连击在 Wayland 下做不到**：无法拦截其他应用的 Ctrl+V（那是被粘贴方自己的按键），也不能抓键盘。
  用户已确认改为「**Ctrl+Alt+1/2/3 + 托盘菜单**」：先把第 N 条历史**写回剪贴板**，再由用户按系统原生 Ctrl+V 粘贴
- 存储：`~/.local/share/owlshot/history/owlshot-history-<YYYYMMDD-HHMMSS-mmm>.png`（`glib::user_data_dir()`），
  文件名即时间戳，`entries()` 按文件名倒序 = 按时间新→旧，`prune()` `drain(keep..)` 删旧，构成环形目录
- 入库时机两处：`main.rs::capture_flow` 落盘成功后 `history::record(&saved)`；`editor.rs::copy_pixbuf`（Ctrl+C 路径）复制成功后也记一份
- 配置 `[history] enabled = true / keep = 3`，`MAX_KEEP = 5` 硬上限，`keep()` 用 `clamp(1, 5)` 收敛非法值
- CLI `owlshot --history N`：N 从 1 计数（1 = 上一张），越界/非数字直接报错退出；内部转 0 基 `Action::RestoreHistory(n - 1)`
- 托盘「粘贴历史」子菜单：`menu_about_to_show` 触发 ksni 重跑 `menu()`，保证列表不陈旧；条目标签形如「最近一张　09-03 10:15:30」
- 子菜单缩略图走 `thumbnail()`：`Pixbuf::from_file_at_size(path, 22, 22)` + `save_to_bufferv("png", &[])`。
  ksni 的 `icon_data` 是 **PNG 字节**（不是 ARGB32，ARGB32 只用于 `Tray::icon_pixmap()`）；若直接塞整张全屏图，菜单每次展开都要经 D-Bus 搬运数 MB
- 关闭历史时子菜单显示「历史记录已在配置中关闭」，无记录时显示「（暂无历史）」，均为 `enabled: false` 的灰条

### 需求5 实现要点（src/pin.rs）
- **Wayland 下无法真正置顶**，已与用户确认接受降级。三条硬限制：
  本机无 gtk4-layer-shell 且 GNOME 不支持 wlr-layer-shell；GTK4 移除了 `keep_above`；Wayland 客户端不能自设窗口坐标
- 效果：`owlshot --paste` 从剪贴板读图，弹出**无边框浮动窗口**，贴出瞬间在最前面，但切到其他窗口后会被盖住
- 读剪贴板走 `gdk::Clipboard::read_texture_future()`（`clipboard.rs` 只有写入路径）；该 API 断言调用线程持有 MainContext，
  故 `glib::MainContext::default().invoke(...)` 进主线程后再用 `glib::spawn_future_local(...)` 起异步任务
- 交互：左键拖动 = `Toplevel::begin_move()` 交给合成器（客户端无权自己挪窗）；滚轮缩放 `0.1..=4.0`；`Esc` / 右键关闭
- `Surface` → `Toplevel` **不能用 `downcast`**：`Toplevel` 是 `Interface<...> @requires Surface`，继承方向相反，只能 `dynamic_cast::<gdk::Toplevel>()`
- 缩放只调 `set_size_request()`：窗口已映射后 `set_default_size()` 不再生效
- 初始尺寸：纹理物理像素 ÷ `Monitor::scale()` 折算逻辑像素（HiDPI 分数缩放不放大两倍），再压到屏幕 90% 以内
- 单次模式（`--paste`）必须等 `close-request` 才让工作循环退出，否则进程一结束贴图跟着消失；daemon 模式 `present()` 后立即放行

### 需求6 根因与修复
- 根因：GNOME 自定义快捷键由 **gsd-media-keys** 执行，其进程环境的 PATH 不含 cargo 的 target 目录，
  用户填的裸命令 `owlshot --region` 找不到可执行文件，按键后毫无反应也无可见报错
- 修复：软链 `~/.cargo/bin/owlshot` → `target/debug/owlshot`（实测 gsd-media-keys 与 gnome-shell 的进程环境 PATH 均含 `~/.cargo/bin`）
- 附带容错：`parse_args` 遇到 `%` 开头的参数直接跳过 —— 用户若照 .desktop 习惯写成 `owlshot --region %f`，gsd-media-keys 不会替换 `%f` 而是原样传进来
- 附带修复：portal 抓出的临时全屏图改由 `TempShot` RAII 兜底删除，取消选区或中途失败都不会在 `/tmp` 堆积

待本机实测项（增强轮次 2）：工具栏右侧观感与分列时机、中文输入法候选窗位置与预编辑显示、11 个工具逐个观感、
`owlshot --history 1/2/3` 与托盘子菜单取回是否粘贴正确、`owlshot --paste` 拖动缩放手感、Ctrl+Alt+A 现在能否出图

## 打包与交付（deb）

### deb 打包（debian/rules 手写 Makefile，非 debhelper）
- `make -f debian/rules package` → `owlshot_0.1.0-1_amd64.deb`（约 1.8M）
- 装入内容：`/usr/bin/owlshot`、`/usr/share/applications/owlshot.desktop`、
  `/usr/share/icons/hicolor/128x128/apps/owlshot.png`（缺图标就是软件列表里没图标的原因）
- `dpkg-deb --build` 必须带 `--root-owner-group`，否则包内文件属主是构建用户
- 排错时先核对已安装二进制与新构建产物：`md5sum /usr/bin/owlshot target/release/owlshot`
  并查 `grep owlshot /var/log/dpkg.log | tail`。改完代码只重新打包、忘了 `dpkg -i`，
  加上常驻托盘进程还是旧二进制没重启，会造成「修了但现象没变」的假象

### 单实例守护（src/main.rs::SingleGuard）
- 锁文件 `$XDG_RUNTIME_DIR/owlshot.pid`；用 `/proc/<pid>/status` 是否存在判断进程存活，不引入 libc
- 仅 `Mode::Daemon` 抢锁，`--region` 等单次模式不抢（允许并存）；`Drop` 时校验 PID 一致才删锁

### Ctrl+Z 重绘延迟（src/editor.rs::wire_keys）
- 根因：对 `Window` 调 `queue_draw()` 不会可靠向下传播到子 `DrawingArea`
- 修复：`wire_keys` 增加 `area: &DrawingArea` 参数，闭包内 5 处改成对 `DrawingArea` 调 `queue_draw()`

### 「命令能截图、快捷键和图标都不能」的真正根因（src/capture.rs::ensure_screenshot_permission）
排查了几轮才定位，与 dconf / PATH / 单实例锁全都无关，是 **portal 的 app-id 权限表**：
- 被 gsd-media-keys（自定义快捷键）或 gnome-shell（应用图标）拉起时，进程落在
  `app-gnome-owlshot-<pid>.scope` systemd scope 内，xdg-desktop-portal 据此解析出 app-id = `owlshot`
- 该 app-id 不在 `org.freedesktop.impl.portal.PermissionStore` 的 `screenshot` 表里 → portal 先弹授权框，
  而 GNOME 46 后端在请求方无父窗口时弹不出来（`Failed to show access dialog: 已到超时限制`），
  25 秒后请求失败为 `Portal request didn't succeed with no information`
- 终端直跑时 scope 是 `session-N.scope`，app-id 解析为空串 `''`，而 `''` 早已是 `yes` → 所以命令行一直好使
- 交叉验证：把 scope 名改成未授权的 `app-gnome-nosuchapp-*` 立刻复现失败；改回 `app-gnome-owlshot-*` 即成功
- 修复：抓屏前调一次 `PermissionStore.SetPermission("screenshot", true, "screenshot", "owlshot", ["yes"])`，
  已存在条目则跳过（不覆盖用户明确选过的「拒绝」），任何失败静默回落 portal 原授权流程。纯标准 D-Bus，KDE 同样适用

### postinst 不能自动配 GNOME 快捷键（结论）
- `dconf write` 绕过 gsettings 的 relocatable schema 注册：`dconf dump` 能看到值，但
  `gsettings get ...custom-keybindings.custom0 command` 报「没有这个架构」，gsd-media-keys 读不到
- 因此 postinst 只打印中文手动绑定指引，不再尝试写 dconf
- 另需注意：gsd-media-keys 的 PATH 通常只有 `/usr/local/bin:/usr/bin:/bin`，命令必须填 `/usr/bin/owlshot --region` 绝对路径

## 全局待办（交付给用户的本机实测清单）
1. `cargo build --release` 后 `install -Dm755 target/release/owlshot ~/.local/bin/owlshot`
2. 桌面终端跑 `owlshot`：确认托盘出现、右键菜单可用、快捷键降级提示如期打印
3. 跑 `owlshot --region`：确认全屏窗口拿到键盘焦点、选区手柄观感、右侧工具栏点击热区
4. 逐个试 r/f/o/l/a/p/h/n/m/b/t 与 Ctrl+Z / Ctrl+Shift+Z / 滚轮调线宽，核对观感与带标注导出的对齐精度
5. 文字工具下切中文输入法，确认预编辑串可见、候选窗跟随插入点
6. 按 README 在「设置 → 键盘 → 自定义快捷键」绑定以下命令（**命令须写绝对路径或确保在 PATH 中**）：
   - `Ctrl+Alt+A` → `owlshot --region`
   - `Ctrl+Alt+1/2/3` → `owlshot --history 1/2/3`（写回剪贴板后按系统 Ctrl+V 粘贴）
   - `Ctrl+Shift+V` → `owlshot --paste`（贴图浮动窗口）
7. 检查 `~/.config/owlshot/config.toml` 是否含 `[history]` 段（老配置文件没有，靠 `#[serde(default)]` 回落 enabled=true/keep=3；
   想改条数需手工补上该段），改 `save_dir` 后确认落盘位置生效
8. 注意：config 里 `[shortcuts] region` 默认是 `CTRL+SHIFT+a`，与用户手工绑的 `Ctrl+Alt+A` 是**两套独立路径**
   （前者走 portal，本机不可用；后者走 GNOME 自定义快捷键 + CLI），不必强求一致

运行方式：`cargo run`（托盘右键菜单发起截图）；单次模式 `cargo run -- --region`
