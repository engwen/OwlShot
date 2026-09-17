//! OwlShot —— Wayland 原生截图工具。
//!
//! 线程模型：
//!   * 主线程跑 GLib 主循环，负责所有 GDK/GTK 调用（剪贴板降级路径、后续阶段2 编辑器）。
//!   * 工作线程跑 tokio 运行时，承载 ksni 托盘服务与 xdg-desktop-portal 异步请求。
//! 两者通过 async-channel 通信，托盘回调永不阻塞。

mod annotate;
mod capture;
mod clipboard;
mod config;
mod editor;
mod history;
mod mutter;
mod pin;
mod shortcuts;
mod toolbar;
mod tray;

use anyhow::{Context, Result};
use gtk4::glib;

/// 托盘菜单发往工作循环的指令。
#[derive(Debug, Clone, Copy)]
pub enum Action {
    /// 先抓全屏静态图，再由自绘编辑器框选区域
    CaptureRegion,
    /// 直接抓取整个屏幕
    CaptureFullScreen,
    /// 抓当前焦点窗口（依赖 GNOME 私有增强，默认关闭）
    CaptureWindow,
    /// 把第 N 条剪贴板历史写回剪贴板（0 = 最近一张）
    RestoreHistory(usize),
    /// 把剪贴板里的图片贴成屏幕上的浮动窗口
    PastePin,
    Quit,
}

/// 命令行模式：单次截图跑完即退出，不注册托盘与快捷键。
enum Mode {
    /// 常驻托盘 + 全局快捷键
    Daemon,
    Once(Action),
}

fn parse_args() -> Option<Mode> {
    // 手写解析，避免为几个开关引入 clap。
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--region" | "-r" => return Some(Mode::Once(Action::CaptureRegion)),
            "--full" | "--fullscreen" | "-f" => return Some(Mode::Once(Action::CaptureFullScreen)),
            "--window" | "-w" => return Some(Mode::Once(Action::CaptureWindow)),
            // `--history N`：N 从 1 开始计数（1 = 上一张），内部换成 0 基索引。
            "--history" => {
                let raw = args.next().unwrap_or_default();
                let Ok(n) = raw.trim().parse::<usize>() else {
                    eprintln!("[owlshot] --history 需要一个 1-{} 的序号，收到：{raw:?}", config::HistoryConfig::MAX_KEEP);
                    return None;
                };
                if n == 0 || n > config::HistoryConfig::MAX_KEEP {
                    eprintln!(
                        "[owlshot] --history 序号超出范围（1-{}）：{n}",
                        config::HistoryConfig::MAX_KEEP
                    );
                    return None;
                }
                return Some(Mode::Once(Action::RestoreHistory(n - 1)));
            }
            "--paste" | "-p" => return Some(Mode::Once(Action::PastePin)),
            "--help" | "-h" => {
                print_help();
                return None;
            }
            "--version" | "-V" => {
                println!("owlshot {}", env!("CARGO_PKG_VERSION"));
                return None;
            }
            // GNOME 自定义快捷键常被误填成 .desktop 风格的 `owlshot --region %f`，
            // gsd-media-keys 不会替换 %f 而是原样传进来，直接忽略这类占位符。
            other if other.starts_with('%') => continue,
            other => {
                eprintln!("[owlshot] 未知参数：{other}");
                print_help();
                return None;
            }
        }
    }
    Some(Mode::Daemon)
}

fn print_help() {
    println!(
        "OwlShot —— Wayland 原生截图工具

用法：
  owlshot              常驻模式：系统托盘 + portal 全局快捷键
  owlshot --region     单次框选截图后退出
  owlshot --full       单次全屏截图后退出
  owlshot --window     单次窗口截图后退出（需开启 advanced.use_mutter_overlay，仅 GNOME）
  owlshot --history N  把第 N 条历史（1 = 上一张，最多 {max}）写回剪贴板，再按 Ctrl+V 粘贴
  owlshot --paste      把剪贴板里的图片贴成屏幕上的浮动窗口（需先截图一次）
  owlshot --help       显示本帮助
  owlshot --version    显示版本号

提示：若桌面的 xdg-desktop-portal 未提供 GlobalShortcuts 接口（如 GNOME 46），
      请在「设置 → 键盘 → 自定义快捷键」中把上面的命令绑定到按键，例如
          Ctrl+Alt+A      owlshot --region
          Ctrl+Alt+1/2/3  owlshot --history 1 / 2 / 3
          Ctrl+Shift+V    owlshot --paste
配置：{path}",
        max = config::HistoryConfig::MAX_KEEP,
        path = config::config_path().display()
    );
}

/// 单实例守护锁。尝试在 `XDG_RUNTIME_DIR/owlshot.pid` 写入当前 PID，
/// 若文件已存在且对应进程仍在运行，则返回 `Err`。
struct SingleGuard {
    path: std::path::PathBuf,
}

/// 通过 `/proc/<pid>/status` 判断进程是否存活（Linux 专用，无需 libc）。
fn pid_alive(pid: u32) -> bool {
    std::path::PathBuf::from(format!("/proc/{pid}/status")).exists()
}

impl SingleGuard {
    fn acquire() -> Result<Self> {
        let base = std::env::var("XDG_RUNTIME_DIR")
            .unwrap_or_else(|_| "/tmp".into());
        let path = std::path::PathBuf::from(base).join("owlshot.pid");

        if let Ok(existing) = std::fs::read_to_string(&path) {
            if let Ok(pid) = existing.trim().parse::<u32>() {
                if pid_alive(pid) {
                    anyhow::bail!("OwlShot 已在运行（PID {pid}），不重复启动。");
                }
            }
        }
        std::fs::write(&path, std::process::id().to_string())
            .context("无法写入单实例锁文件")?;
        Ok(Self { path })
    }
}

impl Drop for SingleGuard {
    fn drop(&mut self) {
        if let Ok(content) = std::fs::read_to_string(&self.path) {
            if content.trim() == std::process::id().to_string() {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

fn main() -> Result<()> {
    let Some(mode) = parse_args() else {
        return Ok(());
    };

    // 单次截图模式不抢锁：多个 `--region` 实例可并存。
    let _guard = if matches!(mode, Mode::Daemon) {
        Some(SingleGuard::acquire()?)
    } else {
        None
    };

    gtk4::init().context("GTK4 初始化失败，请确认运行在图形会话中")?;

    // 初始化贴图 channel：在 GLib 主循环中轮询工作线程发来的贴图请求。
    pin::init_paste_channel();

    let main_loop = glib::MainLoop::new(None, false);
    let (tx, rx) = async_channel::unbounded::<Action>();

    // 单次模式：先把动作塞进队列，处理完让工作循环自然结束。
    if let Mode::Once(action) = &mode {
        let _ = tx.try_send(*action);
        let _ = tx.try_send(Action::Quit);
    }

    let daemon = matches!(mode, Mode::Daemon);
    let worker_loop = main_loop.clone();
    std::thread::Builder::new()
        .name("owlshot-worker".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(err) => {
                    eprintln!("[owlshot] 无法创建 tokio 运行时：{err}");
                    worker_loop.quit();
                    return;
                }
            };
            if let Err(err) = runtime.block_on(worker(tx, rx, daemon)) {
                eprintln!("[owlshot] 后台服务异常退出：{err:#}");
            }
            worker_loop.quit();
        })
        .context("无法启动后台工作线程")?;

    main_loop.run();
    Ok(())
}

async fn worker(
    tx: async_channel::Sender<Action>,
    rx: async_channel::Receiver<Action>,
    daemon: bool,
) -> Result<()> {
    use ksni::TrayMethods;

    // 托盘 Handle 与快捷键 Session 一旦 drop 即注销，必须持有到退出为止。
    let mut _tray = None;
    let mut guard = None;
    if daemon {
        _tray = Some(
            tray::OwlTray::new(tx.clone())
                .spawn()
                .await
                .context("注册 StatusNotifierItem 托盘失败（GNOME 需安装 AppIndicator 扩展）")?,
        );
        guard = shortcuts::register(&config::get().shortcuts, tx).await?;
        println!("[owlshot] 托盘已就绪，右键菜单可发起截图。");
    }

    while let Ok(action) = rx.recv().await {
        match action {
            Action::Quit => break,
            Action::RestoreHistory(index) => {
                if let Err(err) = history::restore(index).await {
                    eprintln!("[owlshot] 取回剪贴板历史失败：{err:#}");
                }
            }
            Action::PastePin => {
                // 单次模式必须等窗口关掉才退出，否则进程一结束贴图就跟着消失。
                if let Err(err) = pin::paste_from_clipboard(!daemon).await {
                    eprintln!("[owlshot] 贴图失败：{err:#}");
                    eprintln!("[owlshot] 提示：先截图一次（Ctrl+Shift+A），再按贴图快捷键（Ctrl+Shift+V）");
                }
            }
            capture => run_capture(capture).await,
        }
    }

    if let Some(guard) = guard {
        guard.close().await;
    }
    Ok(())
}

/// 单次截图失败（含用户取消）不应终止常驻进程，因此只记录日志。
async fn run_capture(action: Action) {
    if let Err(err) = capture_flow(action).await {
        eprintln!("[owlshot] 截图未完成：{err:#}");
    }
}

/// portal 抓屏产出的临时 PNG，离开作用域即删除。
struct TempShot(std::path::PathBuf);

impl Drop for TempShot {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn capture_flow(action: Action) -> Result<()> {
    // 抓图阶段只产出一张静态位图，交互全部由自绘编辑器完成。
    let shot = match action {
        Action::CaptureWindow => capture::capture_window().await?,
        _ => capture::capture().await?,
    };
    // portal / Mutter 产出的临时全屏图无论后续成败都要清掉，否则 /tmp 会持续堆积。
    let _temp = TempShot(shot);
    let shot = &_temp.0;

    let saved = if matches!(action, Action::CaptureRegion) {
        match editor::select_region(shot).await? {
            Some(editor::Shot::Saved(path)) => path,
            // Ctrl+C：编辑器已在主线程写好剪贴板，这里不再落盘、不再复制一次。
            Some(editor::Shot::Copied(backend)) => {
                println!("[owlshot] 选区已通过 {backend} 写入剪贴板（未保存文件）");
                return Ok(());
            }
            None => {
                println!("[owlshot] 已取消选区。");
                return Ok(());
            }
        }
    } else {
        capture::save_to_pictures(shot)?
    };
    // 落盘成功即进历史，供 `--history N` 取回。
    history::record(&saved);

    let backend = if config::get().capture.copy_to_clipboard {
        Some(clipboard::copy_png(&saved).await?)
    } else {
        None
    };
    match backend {
        Some(backend) => println!(
            "[owlshot] 已保存至 {}，并通过 {} 写入剪贴板",
            saved.display(),
            backend
        ),
        None => println!("[owlshot] 已保存至 {}", saved.display()),
    }
    Ok(())
}
