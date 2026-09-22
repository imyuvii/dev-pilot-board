use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};

/// Frontend sets a short status glyph next to the tray icon (macOS shows it as text).
#[tauri::command]
fn set_tray_title(app: tauri::AppHandle, title: String) {
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_title(if title.is_empty() { None } else { Some(title) });
    }
}

/// Preview a system notification sound from the settings panel.
#[tauri::command]
fn play_sound(sound: String) {
    if !sound.chars().all(|c| c.is_ascii_alphanumeric()) || sound.len() > 32 {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let path = format!("/System/Library/Sounds/{}.aiff", sound);
        let _ = std::process::Command::new("afplay").arg(path).spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = sound; // sound preview not implemented on this platform yet
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Menu-bar app: no Dock icon on macOS
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let show = MenuItem::with_id(app, "show", "Open Dashboard", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            // Dedicated monochrome template glyph for the menu bar (macOS tints it)
            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
            TrayIconBuilder::with_id("main-tray")
                .icon(tray_icon)
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // Left click toggles the dashboard; right click opens the menu
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the dashboard hides it; the app lives in the tray
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![set_tray_title, play_sound])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
