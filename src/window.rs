//! Finestra nativa leggera: WebKit di sistema (macOS), WebView2 (Windows), WebKitGTK (Linux).
//! Il server resta locale su 127.0.0.1; la finestra mostra la stessa UI.

use anyhow::Result;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Theme, WindowBuilder};
use wry::WebViewBuilder;

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum AppEvent {
    OpenSettings,
    /// Dalla UI: trascinamento della finestra dalla barra superiore (titolo integrato).
    DragWindow,
    ToggleZoom,
    /// Dalla UI: tema scelto nelle impostazioni (None = segue il sistema).
    SetTheme(Option<Theme>),
}

/// Se qualcosa va in panic nel processo nativo, lascia una traccia leggibile invece di un crash muto.
fn install_panic_log() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("Archivist panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
        eprintln!("{msg}");
        if let Some(home) = std::env::var_os("HOME") {
            let dir = std::path::Path::new(&home).join("Library/Logs/Archivist");
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("crash.log"), &msg);
        }
    }));
}

/// Menu di sistema (macOS): "Informazioni su Archivist" nativo, "Impostazioni… ⌘," e le voci di
/// modifica, senza le quali Cmd+C / Cmd+V / Cmd+Q non funzionano nella webview.
#[cfg(target_os = "macos")]
fn install_menu(proxy: tao::event_loop::EventLoopProxy<AppEvent>) -> Result<()> {
    use muda::accelerator::{Accelerator, Code, Modifiers};
    use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    let menu = Menu::new();
    let app = Submenu::new("Archivist", true);
    let settings = MenuItem::with_id("settings", tr!("menu.settings"), true, Some(Accelerator::new(Some(Modifiers::SUPER), Code::Comma)));
    app.append_items(&[
        &PredefinedMenuItem::about(
            Some(&tr!("menu.about")),
            Some(muda::AboutMetadata {
                name: Some("Archivist".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                copyright: Some(tr!("menu.copyright")),
                ..Default::default()
            }),
        ),
        &PredefinedMenuItem::separator(),
        &settings,
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(Some(&tr!("menu.hide"))),
        &PredefinedMenuItem::hide_others(Some(&tr!("menu.hide_others"))),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(Some(&tr!("menu.quit"))),
    ])?;
    let edit = Submenu::new(tr!("menu.edit"), true);
    edit.append_items(&[
        &PredefinedMenuItem::undo(Some(&tr!("menu.undo"))),
        &PredefinedMenuItem::redo(Some(&tr!("menu.redo"))),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::cut(Some(&tr!("menu.cut"))),
        &PredefinedMenuItem::copy(Some(&tr!("menu.copy"))),
        &PredefinedMenuItem::paste(Some(&tr!("menu.paste"))),
        &PredefinedMenuItem::select_all(Some(&tr!("menu.select_all"))),
    ])?;
    let win = Submenu::new(tr!("menu.window"), true);
    win.append_items(&[
        &PredefinedMenuItem::minimize(Some(&tr!("menu.minimize"))),
        &PredefinedMenuItem::maximize(Some(&tr!("menu.maximize"))),
    ])?;
    menu.append_items(&[&app, &edit, &win])?;
    menu.init_for_nsapp();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        // Nessun unwrap qui: un panic dentro il callback del menu abortirebbe l'app.
        if e.id().as_ref() == "settings" {
            let _ = proxy.send_event(AppEvent::OpenSettings);
        }
    }));
    Ok(())
}

pub fn run() -> Result<()> {
    install_panic_log();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Err(e) = crate::server::run(0, false, false, Some(tx)) {
            eprintln!("{}", tr!("cli.server_error", m = format!("{e:#}")));
            std::process::exit(1);
        }
    });
    let url = rx.recv()?;

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    install_menu(event_loop.create_proxy())?;

    let ui_proxy = event_loop.create_proxy();

    let builder = WindowBuilder::new()
        .with_title("Archivist")
        .with_window_icon(Some(tao::window::Icon::from_rgba(include_bytes!("../assets/icon-256.rgba").to_vec(), 256, 256)?))
        .with_inner_size(LogicalSize::new(1000.0, 680.0))
        .with_min_inner_size(LogicalSize::new(720.0, 480.0));
    // macOS: barra del titolo integrata nel contenuto (come Impostazioni di Sistema, Note, Musica):
    // i semafori restano, la barra grigia scompare e la sidebar arriva fino al bordo alto.
    #[cfg(target_os = "macos")]
    let builder = {
        use tao::platform::macos::WindowBuilderExtMacOS;
        builder
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true)
    };
    let window = builder.build(&event_loop)?;

    let ipc_proxy = ui_proxy.clone();
    let builder = WebViewBuilder::new()
        .with_url(&url)
        .with_initialization_script(if cfg!(target_os = "macos") {
            "document.documentElement.classList.add('native','native-mac');"
        } else {
            "document.documentElement.classList.add('native');"
        })
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            let ev = match req.body().as_str() {
                "drag" => Some(AppEvent::DragWindow),
                "zoom" => Some(AppEvent::ToggleZoom),
                "theme:light" => Some(AppEvent::SetTheme(Some(Theme::Light))),
                "theme:dark" => Some(AppEvent::SetTheme(Some(Theme::Dark))),
                "theme:system" => Some(AppEvent::SetTheme(None)),
                _ => None,
            };
            if let Some(ev) = ev {
                let _ = ipc_proxy.send_event(ev);
            }
        });
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let webview = builder.build(&window)?;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().expect("finestra GTK senza vbox");
        builder.build_gtk(vbox)?
    };

    event_loop.run(move |event, _, control_flow| {
        let _keep = (&window, &webview);
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *control_flow = ControlFlow::Exit,
            Event::UserEvent(AppEvent::OpenSettings) => {
                let _ = webview.evaluate_script("if(typeof openSettings==='function')openSettings()");
            }
            Event::UserEvent(AppEvent::DragWindow) => {
                let _ = window.drag_window();
            }
            Event::UserEvent(AppEvent::ToggleZoom) => window.set_maximized(!window.is_maximized()),
            Event::UserEvent(AppEvent::SetTheme(t)) => window.set_theme(t),
            _ => {}
        }
    });
}
