//! Native macOS application menu.
//!
//! Tauri installs a sensible default menu, but replacing it (to add our
//! "Install Command Line Tool…" item) means we must rebuild the standard macOS
//! structure ourselves — otherwise Cut/Copy/Paste/Undo and the Window commands
//! disappear, breaking text editing in the commit-message box.

use tauri::menu::{AboutMetadataBuilder, Menu, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Runtime};

// The CLI installer (and the menu item that triggers it) is macOS-only; see
// `crate::cli_install`. Windows/Linux menus omit the item entirely.
#[cfg(target_os = "macos")]
use tauri::menu::MenuItem;

#[cfg(target_os = "macos")]
use crate::cli_install;

/// Stable id for the custom "Install Command Line Tool…" item (macOS only).
#[cfg(target_os = "macos")]
pub const INSTALL_CLI_ID: &str = "install-cli";

/// Stable id for the "Window" submenu, so we can look it up again after the menu
/// is installed and register it as the macOS windows menu (see
/// [`set_windows_menu`]).
pub const WINDOW_MENU_ID: &str = "window-menu";

/// Build the full application menu, reconstructing the standard macOS App/Edit/
/// Window submenus and inserting the CLI-installer item into the App menu.
pub fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let app_name = app.package_info().name.clone();

    let about_metadata = AboutMetadataBuilder::new()
        .name(Some(app_name.clone()))
        .version(Some(app.package_info().version.to_string()))
        .build();

    // --- App menu ------------------------------------------------------------
    // Shared items are bound first so the two platform variants of the submenu
    // differ only by the macOS-only "Install Command Line Tool…" entry.
    let about = PredefinedMenuItem::about(app, None, Some(about_metadata))?;
    let sep_a = PredefinedMenuItem::separator(app)?;
    let services = PredefinedMenuItem::services(app, None)?;
    let sep_b = PredefinedMenuItem::separator(app)?;
    let hide = PredefinedMenuItem::hide(app, None)?;
    let hide_others = PredefinedMenuItem::hide_others(app, None)?;
    let show_all = PredefinedMenuItem::show_all(app, None)?;
    let sep_c = PredefinedMenuItem::separator(app)?;
    let quit = PredefinedMenuItem::quit(app, None)?;

    #[cfg(target_os = "macos")]
    let app_menu = {
        let install_cli = MenuItem::with_id(
            app,
            INSTALL_CLI_ID,
            "Install Command Line Tool…",
            true,
            None::<&str>,
        )?;
        let sep_cli = PredefinedMenuItem::separator(app)?;
        Submenu::with_items(
            app,
            &app_name,
            true,
            &[
                &about,
                &sep_a,
                &install_cli,
                &sep_cli,
                &services,
                &sep_b,
                &hide,
                &hide_others,
                &show_all,
                &sep_c,
                &quit,
            ],
        )?
    };

    #[cfg(not(target_os = "macos"))]
    let app_menu = Submenu::with_items(
        app,
        &app_name,
        true,
        &[
            &about,
            &sep_a,
            &services,
            &sep_b,
            &hide,
            &hide_others,
            &show_all,
            &sep_c,
            &quit,
        ],
    )?;

    // --- Edit menu (required for text editing in the commit box) -------------
    let edit_menu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    // --- Window menu ---------------------------------------------------------
    // Given a stable id so it can be re-fetched after the menu is installed and
    // registered as the macOS windows menu (see `set_windows_menu` — the actual
    // `set_as_windows_menu_for_nsapp` call cannot happen here, it must run *after*
    // the menu becomes the app menu).
    let window_menu = Submenu::with_id_and_items(
        app,
        WINDOW_MENU_ID,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?, // "Zoom" on macOS
            &PredefinedMenuItem::fullscreen(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;

    Menu::with_items(app, &[&app_menu, &edit_menu, &window_menu])
}

/// Register the "Window" submenu as the macOS windows menu so `AppKit` auto-populates
/// it with the open repo windows (plus the standard "Bring All to Front" grouping).
///
/// **Ordering is load-bearing.** muda documents that
/// `Submenu::set_as_windows_menu_for_nsapp` "must be called after adding this
/// submenu to `Menu` **and after calling `Menu::init_for_nsapp`** on that menu".
/// Tauri runs `init_for_nsapp` from inside `set_menu`/the builder's `.menu(...)`
/// hook. The previous code called `set_as_windows_menu_for_nsapp` during menu
/// *construction* — before the menu was ever installed — so `AppKit` never adopted
/// it and the submenu stayed inert, showing only our static items. Installing the
/// menu via the builder's `.menu(...)` (which calls `init_for_nsapp` synchronously
/// during `build()`) and then calling this from `setup` (which runs on the
/// `Ready` event, strictly afterward) satisfies the documented order.
#[cfg(target_os = "macos")]
pub fn set_windows_menu<R: Runtime>(app: &AppHandle<R>) {
    let Some(menu) = app.menu() else {
        eprintln!("Window menu registration skipped: no app menu installed");
        return;
    };
    let Some(submenu) = menu
        .get(WINDOW_MENU_ID)
        .and_then(|k| k.as_submenu().cloned())
    else {
        eprintln!("Window menu registration skipped: '{WINDOW_MENU_ID}' submenu not found");
        return;
    };
    if let Err(e) = submenu.set_as_windows_menu_for_nsapp() {
        eprintln!("Failed to register macOS windows menu: {e}");
    }
}

/// Dispatch a menu event. The CLI install runs off the main thread so the menu
/// (and any modal dialogs it raises) never blocks the UI event loop.
///
/// Dynamic file-context-menu ids (namespaced `ctx:*`) are forwarded to
/// [`crate::context_menu`]; they share this single `on_menu_event` sink with the
/// static application menu without colliding.
pub fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    #[cfg(target_os = "macos")]
    if id == INSTALL_CLI_ID {
        let app = app.clone();
        std::thread::spawn(move || cli_install::install_cli(&app));
        return;
    }
    crate::context_menu::handle_menu_event(app, id);
}
