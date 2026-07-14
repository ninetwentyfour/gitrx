mod cli_install;
mod commands;
mod context_menu;
pub mod error;
pub mod git;
mod image;
mod menu;
mod state;
mod watch;
mod windows;

// Shared git-layer test fixtures. Unit tests reach these via
// `crate::test_support::*`; the `hunk_staging` integration test pulls the same
// source in with `#[path]` (see `test_support.rs` for why it is not a feature).
#[cfg(test)]
mod test_support;

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::Ordering;

use tauri::{AppHandle, Manager, RunEvent, WindowEvent};

use state::AppState;
use windows::MAIN_LABEL;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        // The single-instance plugin MUST be registered first: it intercepts a
        // second launch, forwards its argv/cwd to this callback, and exits — so
        // `gitrx <path>` never spawns a duplicate Dock icon.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            handle_second_instance(app, &argv, &cwd);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .manage(AppState::new())
        // Install our menu here (not in `setup`) so the standard macOS commands
        // are preserved alongside the CLI-installer item AND so Tauri runs
        // `init_for_nsapp` synchronously during `build()`. That ordering is what
        // makes the later `set_as_windows_menu_for_nsapp` (in `setup`) actually
        // stick — see `menu::set_windows_menu`. Installing via `set_menu` inside
        // `setup` instead only *enqueues* `init_for_nsapp` on the main thread, so
        // it would run *after* our registration attempt, leaving the Window menu
        // inert.
        .menu(menu::build_menu)
        .setup(|app| {
            app.on_menu_event(|app, event| {
                menu::handle_menu_event(app, event.id().as_ref());
            });

            // The app menu is installed and `init_for_nsapp` has already run
            // (during `build()`), so AppKit will now adopt this submenu as the
            // windows menu and auto-populate it with the open repo windows.
            #[cfg(target_os = "macos")]
            menu::set_windows_menu(app.handle());

            startup_restore(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_repo,
            commands::get_status,
            commands::get_diff,
            commands::stage_file,
            commands::unstage_file,
            commands::discard_file,
            commands::stage_hunk,
            commands::unstage_hunk,
            commands::discard_hunk,
            commands::commit,
            commands::get_head_commit_message,
            commands::read_image,
            context_menu::show_file_context_menu
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application");

    // The run loop owns window-lifecycle persistence. Handling `Destroyed` here
    // (rather than a per-window callback) keeps it in the same ordered event
    // stream as `ExitRequested`, so the `exiting` guard is reliably set before
    // teardown closes every window.
    app.run(|app_handle, event| match event {
        RunEvent::ExitRequested { .. } => {
            app_handle
                .state::<AppState>()
                .exiting
                .store(true, Ordering::SeqCst);
        }
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::Destroyed,
            ..
        } => {
            let state = app_handle.state::<AppState>();
            // Skip while quitting: every window closes on quit, and rewriting the
            // set as it drains would clobber the snapshot we want to restore.
            if !state.exiting.load(Ordering::SeqCst) && windows::remove_window(app_handle, &label) {
                windows::persist_open_repos(app_handle);
            }
        }
        _ => {}
    });
}

/// A second `gitrx` invocation was intercepted. With a path arg → open-or-focus a
/// window for that repo; without → just surface an existing window.
fn handle_second_instance(app: &AppHandle, argv: &[String], cwd: &str) {
    match windows::first_path_arg(argv) {
        Some(arg) => match windows::resolve_cli_workdir(&arg, cwd) {
            Ok(workdir) => {
                if let Err(e) = windows::open_or_focus(app, workdir) {
                    eprintln!("Failed to open forwarded repo '{arg}': {e}");
                }
            }
            Err(e) => {
                eprintln!("Ignoring forwarded path '{arg}': {e}");
                windows::focus_any_window(app);
            }
        },
        None => windows::focus_any_window(app),
    }
}

/// Startup window population:
/// - a CLI-provided repo binds to the initial `main` window;
/// - persisted repos are restored — the first unclaimed one takes `main` (when
///   the CLI didn't), the rest open as their own stable-labelled windows;
/// - nothing to open leaves a plain no-repo `main` window.
fn startup_restore(app: &AppHandle) {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let argv: Vec<String> = std::env::args().collect();

    // The first instance runs in the launch cwd, so our own cwd is correct here.
    let cli_workdir = windows::first_path_arg(&argv).and_then(|arg| {
        windows::resolve_cli_workdir(&arg, &cwd)
            .map_err(|e| eprintln!("Ignoring CLI path '{arg}': {e}"))
            .ok()
    });

    // Identity = the repo's stable label; dedupes the CLI repo against the
    // persisted list and any duplicate persisted entries.
    let mut opened: HashSet<String> = HashSet::new();

    let mut main_bound = cli_workdir.is_some();
    if let Some(workdir) = cli_workdir {
        opened.insert(windows::label_for_repo(&workdir));
        // The initial `main` window is built during `build()`, so it is live here;
        // ignore the closed-window guard's result (it cannot fail at startup).
        let _ = windows::set_window_repo(app, MAIN_LABEL, workdir);
    }

    for path in windows::load_open_repos(app) {
        let Ok(workdir) = windows::resolve_workdir(Path::new(&path)) else {
            continue; // repo moved/removed since last session — skip silently
        };
        let label = windows::label_for_repo(&workdir);
        if !opened.insert(label.clone()) {
            continue; // already opened this repo
        }
        if !main_bound {
            let _ = windows::set_window_repo(app, MAIN_LABEL, workdir);
            main_bound = true;
        } else if let Err(e) = windows::create_repo_window(app, &label, workdir) {
            eprintln!("Failed to restore repo window: {e}");
        }
    }

    // Persist the freshly-opened set (prunes vanished repos; the empty-set guard
    // keeps a no-repo launch from clearing a still-valid saved session).
    windows::persist_open_repos(app);
}
