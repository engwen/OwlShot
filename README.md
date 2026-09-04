# OwlShot

Wayland 原生截图工具。截图能力全部来自 **xdg-desktop-portal** 标准 D-Bus 接口，选区与标注由自绘 GTK4 编辑器完成，**不含任何 X11 代码**。

- 二进制名：`owlshot`
- 开发环境：Ubuntu 24.04 / GNOME 46 / Wayland
- 语言：Rust（edition 2024）

## 功能

- 框选截图：portal 抓静态全屏图 → 自绘全屏窗口选区（暗化遮罩、1px 细边框、6px 小手柄、实时像素尺寸）
- 标注：方框 / 实心矩形 / 圆形 / 直线 / 箭头 / 画笔 / 荧光笔 / 序号 / 马赛克 / 模糊 / 文字，共 11 种工具 + 8 色调色板，支持撤销与重做
- 文字标注接入 IMMulticontext，中文输入法预编辑串可见、候选窗跟随插入点
- 工具栏竖排贴在选区右侧，按钮放不下时自动分列
- 全屏截图，以及可选的窗口截图（依赖 GNOME 私有接口，默认关闭）
- 保存到图片目录 + 自动写入剪贴板（`wl-copy` 优先，GDK4 自动降级）
- 剪贴板历史：默认留最近 3 张（可配至 5 张），`owlshot --history N` 或托盘「粘贴历史」子菜单一键取回
- 贴图：`owlshot --paste` 把剪贴板里的图贴成可拖动缩放的无边框浮动窗口
- StatusNotifierItem 系统托盘 + 命令行单次模式
- HiDPI 分数缩放正确：导出恒为原始物理分辨率，不会出现两倍分辨率或错位

## 设计约束（硬性）

| 约束 | 落地方式 |
| --- | --- |
| 禁止 X11 | 不依赖 xcb / xlib / x11rb / pyautogui 等任何 X11 库 |
| 优先标准 portal | 抓屏走 `org.freedesktop.portal.Screenshot`，快捷键走 `org.freedesktop.portal.GlobalShortcuts` |
| 不抓键盘 | 全局快捷键只由 portal 授权分发，绝不做底层键盘 grab |
| 不做 X11 式全局遮罩 | 选区窗口是普通 GTK4 全屏无边框窗口，不用 wlr-layer-shell（GNOME 不支持） |
| Mutter 私有接口仅作可选增强 | `advanced.use_mutter_overlay` 默认 `false`，KDE / Plasma / Sway / wlroots / Hyprland 下强制禁用 |

## 系统依赖

```bash
# 构建依赖
sudo apt install build-essential pkg-config libgtk-4-dev

# 运行依赖（GNOME 桌面通常已自带 portal）
sudo apt install xdg-desktop-portal xdg-desktop-portal-gnome

# 强烈建议：剪贴板主路径
sudo apt install wl-clipboard
```

Rust 工具链用 [rustup](https://rustup.rs) 安装即可，最低验证版本 `rustc 1.98.0`。

托盘图标在 GNOME 下需要 AppIndicator 类扩展（如 `gnome-shell-extension-appindicator`）才会显示；没有扩展时程序照样能用，只是顶栏看不到图标——此时请用命令行单次模式。

## 构建

```bash
# 若 cargo 不在 PATH 中
export PATH="$HOME/.cargo/bin:$PATH"

cargo build --release
```

产物：`target/release/owlshot`。安装到 PATH：

```bash
install -Dm755 target/release/owlshot ~/.local/bin/owlshot
```

开发期直接跑：`cargo run` / `cargo run -- --region`。

## 运行方式

```
owlshot              常驻模式：系统托盘 + portal 全局快捷键
owlshot --region     单次框选截图后退出
owlshot --full       单次全屏截图后退出
owlshot --window     单次窗口截图后退出（需开启 advanced.use_mutter_overlay，仅 GNOME）
owlshot --history N  把第 N 条历史（1 = 上一张，最多 5）写回剪贴板，再按 Ctrl+V 粘贴
owlshot --paste      把剪贴板里的图片贴成屏幕上的浮动窗口（Esc 关闭）
owlshot --help       显示帮助
owlshot --version    显示版本号
```

短选项：`-r` / `-f` / `-w` / `-p` / `-h` / `-V`。

单次模式不注册托盘、不注册快捷键，做完一件事立即退出，适合绑定到系统快捷键（`--paste` 例外：它要等贴图窗口关闭才退出）。

## 在 GNOME 中绑定快捷键

GNOME 46 及更早版本的 portal 后端**尚未实现 GlobalShortcuts 接口**（本机实测：`org.freedesktop.portal.Desktop` 的 28 个接口中没有它）。此时 owlshot 会打印降级提示，请改用 GNOME 自带的自定义快捷键：

1. 打开「设置」→「键盘」→「查看及自定义快捷键」
2. 拉到底部，进入「自定义快捷键」，点 `+`
3. 填写：
   - 名称：`OwlShot 框选截图`
   - 命令：`/home/<你的用户名>/.local/bin/owlshot --region`（**必须写绝对路径**，见下方说明）
   - 快捷键：按下你想用的组合，例如 `Ctrl+Alt+A`
4. 按同样方式绑定其余命令：

| 建议按键 | 命令 | 作用 |
| --- | --- | --- |
| `Ctrl+Alt+A` | `owlshot --region` | 框选截图 |
| `Ctrl+Alt+S` | `owlshot --full` | 全屏截图 |
| `Ctrl+Alt+1` | `owlshot --history 1` | 把上一张历史写回剪贴板 |
| `Ctrl+Alt+2` | `owlshot --history 2` | 把上上一张写回剪贴板 |
| `Ctrl+Alt+3` | `owlshot --history 3` | 把再往前一张写回剪贴板 |
| `Ctrl+Shift+V` | `owlshot --paste` | 把剪贴板里的图贴到屏幕上 |

> **命令必须能被找到**：GNOME 的自定义快捷键由 `gnome-settings-daemon` 的 media-keys 插件执行，它的进程环境 PATH 与你的终端不同（通常不含 `~/.local/bin`、更不含 cargo 的 `target/debug`）。填裸命令 `owlshot --region` 时若不在其 PATH 中，按键后**毫无反应也没有任何报错**。要么写绝对路径，要么把可执行文件放进已在 PATH 中的目录（例如 `~/.cargo/bin`）。
>
> 另外别照 `.desktop` 的习惯在命令末尾加 `%f` / `%U`：media-keys 不会替换这些占位符而是原样传进来。owlshot 已做容错（`%` 开头的参数直接忽略），但不建议写。

若组合键已被 GNOME 自带截图占用，先在同一页面把系统的「截屏」快捷键清掉，再绑定 owlshot。

当所在桌面的 portal 已支持 GlobalShortcuts（未来 GNOME 版本、或 KDE Plasma 5.27+）时，无需上述手工绑定：直接常驻运行 `owlshot`，系统会弹窗请求快捷键授权，触发键由配置文件的 `[shortcuts]` 决定。注意 `[shortcuts]` 里的触发器与上面手工绑的按键是**两套独立路径**，不必强求一致。

## 配置文件

路径：`~/.config/owlshot/config.toml`（由 `glib::user_config_dir()` 解析）。首次运行若文件不存在会自动写出一份带注释的模板。文件缺失、字段缺失、解析失败都不会阻断启动，只回落默认值并打印告警。**改完需重启 owlshot 生效。**

```toml
[shortcuts]
# 是否向 xdg-desktop-portal 注册全局快捷键（会弹系统授权窗口）
enabled = true
# 触发器语法见下表
region = "CTRL+SHIFT+a"
fullscreen = "CTRL+SHIFT+s"

[capture]
# 保存目录，留空 = XDG 图片目录下的 Screenshots，可写 "~/Pictures/shots"
save_dir = ""
# 文件名前缀，最终形如 owlshot-20260101-120000.png
file_prefix = "owlshot"
# 截图完成后自动写入剪贴板
copy_to_clipboard = true

[advanced]
# GNOME 私有 Mutter D-Bus 增强，默认关闭；KDE/Sway 下强制忽略
use_mutter_overlay = false
```

字段说明：

| 字段 | 默认值 | 含义 |
| --- | --- | --- |
| `shortcuts.enabled` | `true` | 设为 `false` 则完全跳过 portal 快捷键注册（只用托盘 + CLI） |
| `shortcuts.region` | `CTRL+SHIFT+a` | 框选截图触发器，留空表示不指定首选键、交由系统分配 |
| `shortcuts.fullscreen` | `CTRL+SHIFT+s` | 全屏截图触发器 |
| `capture.save_dir` | `""` | 留空 = `XDG_PICTURES_DIR/Screenshots`；支持 `~` 与 `~/xxx` |
| `capture.file_prefix` | `owlshot` | 空值回落 `owlshot` |
| `capture.copy_to_clipboard` | `true` | `false` 时保存后不写剪贴板；不影响编辑器里显式按下的 `Ctrl+C` |
| `advanced.use_mutter_overlay` | `false` | 见下节「GNOME 私有增强」 |

触发器语法（XDG Shortcuts 规范）：修饰键 `CTRL` / `ALT` / `SHIFT` / `NUM` / `LOGO` 与键名用 `+` 连接，键名取 xkbcommon keysym 去掉 `XKB_KEY_` 前缀，例如 `CTRL+SHIFT+a`、`LOGO+Print`、`ALT+space`。

## 编辑器操作

| 操作 | 说明 |
| --- | --- |
| 左键拖拽 | 空白处拉出选区；选区内平移；手柄上 8 向缩放 |
| 双击 / `Enter` / `Space` | 确认，裁剪并保存（同时按配置写剪贴板） |
| `Ctrl+C` | 直接把当前选区复制到剪贴板并关闭编辑器，**不生成文件** |
| 右键 | 取消 |
| `Esc` | 先退出当前标注工具；已在选区模式时整体取消 |
| `r` / `f` / `o` / `l` / `a` / `p` / `h` / `n` / `m` / `b` / `t` | 方框 / 实心矩形 / 圆形 / 直线 / 箭头 / 画笔 / 荧光笔 / 序号 / 马赛克 / 模糊 / 文字，再按一次同键退出 |
| `Ctrl+Z` | 撤销最近一笔标注（无标注时不会误关窗口） |
| `Ctrl+Shift+Z` | 重做 |
| 滚轮 | 调整当前工具线宽（1-20） |
| 工具栏 | 贴在选区右侧，含 11 个工具 + 8 色调色板 + 撤销 / 重做 + 保存 / 取消 |

文字工具：进入后直接键入，`BackSpace` 退格，`Enter` 换行，`Ctrl+Enter` 或 `Esc` 落定；空文本自动丢弃。

## GNOME 私有增强（可选，默认关闭）

`advanced.use_mutter_overlay = true` 时，会尝试用 `org.gnome.Shell.Screenshot` 的 `Screenshot` / `ScreenshotWindow` 抓图，并解锁托盘的「窗口截图」菜单项与 `--window` 参数。

- 只在 `XDG_CURRENT_DESKTOP` 含 `GNOME` 时生效；含 KDE / Plasma / Sway / wlroots / Hyprland 时**强制禁用**并打印说明
- 全屏抓图失败会静默回落标准 portal，主链路永不中断
- 刻意**不使用** `SelectArea`：其选区观感正是本项目要替换掉的，选区一律交自绘编辑器
- 走 `ashpd` re-export 的 zbus 直连会话总线，纯 D-Bus，无任何 X11 调用

## 已知限制

- **GNOME 46 的 portal 没有 GlobalShortcuts 接口**：全局快捷键请按上文用 GNOME 自定义快捷键绑定 CLI
- **GNOME 45+ 收紧了 `org.gnome.Shell.Screenshot`**：非白名单调用方会拿到 `org.freedesktop.DBus.Error.AccessDenied: Screenshot is not allowed`（本机 `gdbus call` 实测复现）。因此该增强在 GNOME 46 上开启也基本必然回落 portal，`--window` 会失败——代码保留是为了旧版本与未来放开
- 窗口截图**没有** portal 等价能力，未开启增强时会直接给出提示而不是静默失败
- 托盘图标依赖 AppIndicator 类扩展
- 贴图窗口无原生置顶能力：切到其他窗口后会被盖住（Wayland 限制）
- 从非登录会话终端启动时可能出现 `dconf-CRITICAL ... /run/user/1000/dconf/user 权限不够` 告警，属环境噪音，不影响功能

## 项目结构

```
src/
  main.rs       CLI 解析、线程模型、截图主流程
  capture.rs    portal 抓图 + 落盘（读配置目录与前缀）
  editor.rs     自绘全屏选区 / 标注编辑器（GTK4 + Cairo）
  annotate.rs   标注图元定义与绘制（Rect / Color / Tool / Shape / Canvas）
  toolbar.rs    纯 Cairo 自绘工具栏（布局 / 绘制 / 命中）
  clipboard.rs  wl-copy 优先、GDK4 降级的剪贴板写入
  config.rs     ~/.config/owlshot/config.toml 读取与模板生成
  shortcuts.rs  portal GlobalShortcuts 注册与信号分发
  mutter.rs     GNOME 私有 D-Bus 可选增强（默认关闭）
  tray.rs       StatusNotifierItem 托盘
  history.rs    剪贴板历史存储与取回
  pin.rs        贴图浮动窗口（Ctrl+Shift+V / --paste）
```

线程模型：主线程跑 GLib 主循环（所有 GDK/GTK 调用），工作线程跑 tokio 多线程运行时（托盘服务、portal 异步请求、快捷键信号流），两者用 async-channel 通信；`Pixbuf` 非 `Send`，裁剪 / 合成 / 落盘一律留在主线程。

开发进度与各阶段实现要点见 [PROGRESS.md](./PROGRESS.md)。
