mod selection;
mod stack;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, RunEvent, WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

// Register the "read my selection aloud" handler on a shortcut. Shared by
// startup registration and the runtime set_hotkey command.
fn register_read_hotkey(app: &tauri::AppHandle, sc: Shortcut) -> Result<(), String> {
    app.global_shortcut()
        .on_shortcut(sc, |app, _sc, event| {
            if event.state() == ShortcutState::Pressed {
                let app = app.clone();
                std::thread::spawn(move || {
                    // Capture BEFORE showing the pet: the copy must go to the
                    // app that currently has focus.
                    let text = selection::capture_selection();
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.show();
                    }
                    let _ = app.emit("speak-selection", text);
                });
            }
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_hotkey(state: tauri::State<stack::Stack>) -> String {
    state.registered_hotkey().unwrap_or_else(|| state.hotkey())
}

// Re-bind the global hotkey at runtime and persist it. Restores the previous
// binding if the new one can't be registered, so there's never a dead key.
#[tauri::command]
fn set_hotkey(
    app: tauri::AppHandle,
    state: tauri::State<stack::Stack>,
    combo: String,
) -> Result<String, String> {
    let combo = combo.trim().to_string();
    if combo.is_empty() {
        return Err("empty combo".into());
    }
    let sc: Shortcut = combo.parse().map_err(|e| format!("unsupported combo: {e}"))?;

    let previous = state.registered_hotkey();
    if let Some(cur) = &previous {
        if let Ok(cursc) = cur.parse::<Shortcut>() {
            let _ = app.global_shortcut().unregister(cursc);
        }
    }
    if let Err(e) = register_read_hotkey(&app, sc) {
        if let Some(cur) = &previous {
            if let Ok(cursc) = cur.parse::<Shortcut>() {
                let _ = register_read_hotkey(&app, cursc);
            }
        }
        return Err(format!("could not register {combo}: {e}"));
    }
    state.set_registered_hotkey(combo.clone());
    if let Err(e) = state.persist_hotkey(&combo) {
        println!("[hotkey] persist failed: {e}");
    }
    println!("[hotkey] changed to: {combo}");
    Ok(combo)
}

#[tauri::command]
fn get_language(state: tauri::State<stack::Stack>) -> String {
    state.current_language().unwrap_or_else(|| "en".to_string())
}

// Download (if needed) + load the model for `lang`, hot-swapping llama-server.
// Runs the blocking download/reload off the async runtime; progress arrives via
// the "model-progress" event.
#[tauri::command]
async fn set_language(app: tauri::AppHandle, lang: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<stack::Stack>();
        stack::set_language(&app, state.inner(), &lang).map(|_| lang)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_quant(state: tauri::State<stack::Stack>) -> String {
    state.current_quant()
}

// Change the voice-model size/quality and reload the current language at it. The
// blocking download/reload runs off the async runtime; progress arrives via the
// "model-progress" event, exactly like a language switch.
#[tauri::command]
async fn set_quant(app: tauri::AppHandle, quant: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<stack::Stack>();
        stack::set_quant(&app, state.inner(), &quant).map(|_| quant)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn cancel_download(state: tauri::State<stack::Stack>) {
    stack::cancel_download(state.inner());
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(stack::Stack::default())
        .invoke_handler(tauri::generate_handler![
            stack::stack_status,
            stack::model_status,
            stack::installed_languages,
            get_hotkey,
            set_hotkey,
            get_language,
            set_language,
            get_quant,
            set_quant,
            cancel_download
        ])
        .setup(|app| {
            if let Some(win) = app.get_webview_window("main") {
                // Park the pet near the bottom-right of the primary screen,
                // above the taskbar.
                if let Ok(Some(monitor)) = win.primary_monitor() {
                    let screen = monitor.size();
                    let scale = monitor.scale_factor();
                    if let Ok(outer) = win.outer_size() {
                        let margin = (24.0 * scale) as i32;
                        let taskbar = (56.0 * scale) as i32;
                        let x = screen.width as i32 - outer.width as i32 - margin;
                        let y = screen.height as i32 - outer.height as i32 - taskbar;
                        let _ = win.set_position(tauri::PhysicalPosition { x, y });
                    }
                }
                let _ = win.set_always_on_top(true);
            }

            // System tray: Show / Hide / Quit.
            let show = MenuItem::with_id(app, "show", "Show pet", true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide", "Hide pet", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &hide, &quit])?;

            let mut tray = TrayIconBuilder::new()
                .tooltip("Orpheus Pet")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                        let _ = app.emit("pet-visibility", "show");
                    }
                    "hide" => {
                        let _ = app.emit("pet-visibility", "hide");
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.hide();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // Left-click the tray icon to bring the pet back.
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                        let _ = app.emit("pet-visibility", "show");
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            // Bring up the voice stack (llama-server + Orpheus-FastAPI) as
            // managed child processes — the pet is the only thing you launch.
            stack::start(app.state::<stack::Stack>().inner());

            // Launch the pet automatically at login. Only the RELEASE build
            // self-registers: the dev binary needs the Vite dev server running,
            // so autostarting it would just open a broken window. Run
            // `pnpm tauri build` and launch the bundled exe once to activate.
            #[cfg(not(debug_assertions))]
            {
                use tauri_plugin_autostart::ManagerExt;
                match app.autolaunch().enable() {
                    Ok(()) => println!("[autostart] enabled (launch at login)"),
                    Err(e) => println!("[autostart] enable failed: {e}"),
                }
            }

            // Global hotkey: highlight text in ANY app, press the hotkey, the
            // witch reads it aloud. Capture runs off-thread (it sleeps while
            // polling the clipboard), then the text is pushed to the frontend.
            // Try the configured hotkey first, then one-handed fallbacks, so a
            // mistyped or already-occupied combo still leaves a working key.
            {
                let configured = app.state::<stack::Stack>().inner().hotkey();
                let candidates = [configured.as_str(), "ctrl+q", "ctrl+alt+o"];
                let mut registered_as: Option<String> = None;
                for cand in candidates {
                    if cand.is_empty() {
                        continue;
                    }
                    let sc = match cand.parse::<Shortcut>() {
                        Ok(sc) => sc,
                        Err(e) => {
                            let m = format!("hotkey '{cand}' invalid: {e}");
                            println!("[hotkey] {m}");
                            stack::note(app.state::<stack::Stack>().inner(), m);
                            continue;
                        }
                    };
                    match register_read_hotkey(app.handle(), sc) {
                        Ok(()) => {
                            registered_as = Some(cand.to_string());
                            break;
                        }
                        Err(e) => {
                            let m = format!("hotkey '{cand}' registration failed: {e}");
                            println!("[hotkey] {m}");
                            stack::note(app.state::<stack::Stack>().inner(), m);
                        }
                    }
                }
                if let Some(k) = &registered_as {
                    app.state::<stack::Stack>().inner().set_registered_hotkey(k.clone());
                }
                let summary = match &registered_as {
                    Some(k) => format!("hotkey registered: {k}"),
                    None => "no hotkey could be registered".to_string(),
                };
                println!("[hotkey] {summary}");
                stack::note(app.state::<stack::Stack>().inner(), summary);
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing hides to the tray instead of quitting. Only the pet
            // window announces a hide (goodbye + tuck the panel away); the
            // panel just hides itself without disturbing the pet.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if window.label() == "main" {
                    let _ = window.emit("pet-visibility", "hide");
                }
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Quit (tray menu) exits the app: tear the voice stack down with it.
            if let RunEvent::Exit = event {
                stack::stop_all(app_handle.state::<stack::Stack>().inner());
            }
        });
}
