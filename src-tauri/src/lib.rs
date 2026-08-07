//! WhimprFlow Tauri shell.
//!
//! A tray item, the Hub window, and the floating Flow Bar overlay — a transparent
//! non-activating NSPanel that renders the pill and follows the user across Spaces,
//! including other apps' full-screen ones. State reaches it as `whimpr://flowbar/state`
//! events emitted by the hotkey layer, which owns the real state machine.

mod appctx;
mod autolearn;
mod fnkey;
mod hotkey;
mod local_llm;
mod paste;

use serde::Serialize;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

const OVERLAY_LABEL: &str = "whimpr_bar";
const HUB_LABEL: &str = "main";

/// Anchor the overlay window bottom-center of the monitor the user is on.
///
/// Two things this gets right that the obvious version does not: it anchors to the
/// monitor's WORK AREA (which already excludes the Dock and menu bar, so the pill
/// can't tuck itself underneath the Dock), and it prefers the CURRENT monitor over
/// the primary one, so on a multi-display desk the pill appears on the screen being
/// looked at. Cheap enough to re-run on every record-start.
pub(crate) fn position_overlay(w: &WebviewWindow) {
    // current_monitor() can be None before the window maps; fall back sensibly.
    let monitor = w
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| w.primary_monitor().ok().flatten())
        .or_else(|| w.available_monitors().ok().and_then(|m| m.into_iter().next()));
    let Some(monitor) = monitor else {
        eprintln!("[whimpr] no monitor found — overlay stays at default position");
        return;
    };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let Ok(wsize) = w.outer_size() else { return };
    let inset = (12.0 * scale) as i32;
    let x = area.position.x + (area.size.width as i32 - wsize.width as i32) / 2;
    let y = area.position.y + area.size.height as i32 - wsize.height as i32 - inset;
    let _ = w.set_position(tauri::PhysicalPosition { x, y });
    eprintln!(
        "[whimpr] overlay placed: work area {}x{} @({},{}) scale {:.1} -> window {}x{} @({},{})",
        area.size.width, area.size.height, area.position.x, area.position.y, scale,
        wsize.width, wsize.height, x, y
    );
}

/// Turn the overlay's NSWindow into a non-activating NSPanel.
///
/// This is the difference between "floats over other windows" and "floats over
/// other SPACES". A full-screen app gets its own Space, and a plain NSWindow cannot
/// join it at any window level — raising the level does nothing, because the window
/// is not on that Space at all. Only a panel with NonactivatingPanel, combined with
/// CanJoinAllSpaces, follows the user into another app's full-screen Space.
///
/// Swapping an object's class at runtime is the same technique tauri-nspanel uses;
/// AppKit is fine with it here because NSPanel adds no instance variables over
/// NSWindow. Non-activating also means clicking the pill never steals focus from the
/// app being dictated into — which the paste path depends on.
fn promote_overlay_to_panel(w: &WebviewWindow) {
    use objc2::runtime::AnyClass;
    use objc2_app_kit::{NSPanel, NSWindowStyleMask};

    let Ok(ptr) = w.ns_window() else { return };
    if ptr.is_null() {
        return;
    }
    let Some(panel_cls) = AnyClass::get(c"NSPanel") else {
        eprintln!("[whimpr] NSPanel class not found — overlay stays a plain window");
        return;
    };
    // SAFETY: main thread only, and the pointer is the live NSWindow Tauri created.
    unsafe {
        objc2::ffi::object_setClass(ptr.cast(), (panel_cls as *const AnyClass).cast());
        let panel: &NSPanel = &*(ptr as *const NSPanel);
        panel.setStyleMask(panel.styleMask() | NSWindowStyleMask::NonactivatingPanel);
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(true);
    }
}

/// Float the pill above the Dock and let it follow the user into full-screen Spaces.
///
/// Tauri's `always_on_top` maps to NSFloatingWindowLevel (3), which is BELOW the
/// Dock (20) — that is why a bottom-anchored pill reads as "not showing up". Raising
/// to 25 clears the Dock while staying under the screen saver. The collection
/// behavior is the other half: without FullScreenAuxiliary a window simply does not
/// exist on another app's full-screen Space, which is exactly where someone dictating
/// tends to be. Re-asserted on every state change, because activating another app or
/// switching Space can knock a window back down the stack.
fn raise_overlay_level(w: &WebviewWindow) {
    use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};

    let Ok(ptr) = w.ns_window() else { return };
    if ptr.is_null() {
        return;
    }
    // SAFETY: Tauri hands back the NSWindow backing this webview window; we only
    // touch it on the main thread (callers dispatch via run_on_main_thread).
    unsafe {
        let ns: &NSWindow = &*(ptr as *const NSWindow);
        let before = ns.level();
        ns.setLevel(OVERLAY_WINDOW_LEVEL);
        ns.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary,
        );
        // Order front WITHOUT making it key: the app being dictated into must keep
        // keyboard focus, or the paste lands in the wrong place.
        ns.orderFrontRegardless();
        eprintln!(
            "[whimpr] overlay level {} -> {} (wanted {}), collectionBehavior {:?}",
            before,
            ns.level(),
            OVERLAY_WINDOW_LEVEL,
            ns.collectionBehavior()
        );
    }
}

/// Above the Dock (20) and normal app windows (0), below the screen saver.
/// NSStatusWindowLevel; not exported as a constant by objc2-app-kit.
const OVERLAY_WINDOW_LEVEL: isize = 25;

fn build_overlay(app: &tauri::App) -> tauri::Result<WebviewWindow> {
    let overlay = WebviewWindowBuilder::new(
        app,
        OVERLAY_LABEL,
        WebviewUrl::App("overlay.html".into()),
    )
    .title("WhimprBar")
    // Tight window so it only catches clicks right around the pill, not a big
    // invisible box over the app behind it. Sized with enough margin for the
    // recording state's accent glow, which would otherwise clip at the edges.
    .inner_size(340.0, 92.0)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .focused(false)
    .resizable(false)
    .visible(true)
    .build()?;
    position_overlay(&overlay);
    // Panel first: changing the style mask can reset window state, so the level and
    // collection behavior are applied after it.
    promote_overlay_to_panel(&overlay);
    raise_overlay_level(&overlay);
    // Idle renders nothing, so the window must not swallow clicks aimed at whatever
    // is underneath it. Recording turns this back off so Cancel/Stop stay clickable.
    let _ = overlay.set_ignore_cursor_events(true);
    Ok(overlay)
}

fn build_hub(app: &tauri::App) -> tauri::Result<WebviewWindow> {
    WebviewWindowBuilder::new(app, HUB_LABEL, WebviewUrl::App("index.html".into()))
        .title("WhimprFlow")
        .inner_size(920.0, 640.0)
        .min_inner_size(720.0, 480.0)
        .visible(true)
        .build()
}

/// Bring the Hub back — from the tray's Open item, or a Dock click.
///
/// The window is only ever hidden, never destroyed (see the `CloseRequested`
/// handler), so it is always there to re-show. Order matters: `set_focus` is a
/// no-op on a hidden or minimized window, so unhide and unminimize first, and
/// `show` is what makes the app frontmost again after the red button hid it.
fn show_hub(app: &tauri::AppHandle) {
    let Some(hub) = app.get_webview_window(HUB_LABEL) else { return };
    let _ = hub.unminimize();
    let _ = hub.show();
    let _ = hub.set_focus();
}

/// Keep the overlay window in step with what the pill is showing.
///
/// `interactive` = the pill has buttons on screen (recording), so it must accept
/// clicks; otherwise it renders nothing and must let clicks through to the app
/// underneath. Recording also re-anchors the window, which is what puts the pill on
/// the display currently in use rather than wherever it was at launch.
///
/// Called from the hotkey tap thread, so the NSWindow work is hopped to the main
/// thread and nothing here blocks: a slow tap callback gets the event tap killed.
pub(crate) fn sync_overlay_window(app: &tauri::AppHandle, interactive: bool) {
    let Some(overlay) = app.get_webview_window(OVERLAY_LABEL) else { return };
    let _ = overlay.set_ignore_cursor_events(!interactive);
    let _ = app.run_on_main_thread(move || {
        if interactive {
            position_overlay(&overlay);
        }
        // Re-assert the level on EVERY state change, not just recording: activating
        // another app, switching Space, or changing display can all knock the window
        // back down the stack, and once it is behind the frontmost app it is useless.
        raise_overlay_level(&overlay);
    });
}

#[tauri::command]
fn get_settings() -> whimpr_core::Settings {
    hotkey::current_settings()
}

#[tauri::command]
fn set_settings(settings: whimpr_core::Settings) {
    hotkey::update_settings(settings);
}

/// Aggregated dictation stats for the Hub dashboard. `tz_offset_minutes` is the
/// browser's `Date.getTimezoneOffset()` so "today"/streak match the user's clock.
#[tauri::command]
fn get_stats(tz_offset_minutes: i32) -> whimpr_core::StatsSummary {
    hotkey::stats_summary(tz_offset_minutes)
}

/// One page of dictation history (newest first), filtered by search text and start
/// time. Paged here rather than in the webview because the log only ever grows:
/// shipping every dictation ever made so the UI can show ten of them gets slower
/// every day, and a search over a truncated list silently misses older matches.
#[tauri::command]
fn get_history(query: whimpr_core::HistoryQuery) -> whimpr_core::HistoryPage {
    hotkey::history_page(query)
}

/// Erase the stored text of every dictation, keeping the word counts and streaks.
#[tauri::command]
fn clear_transcripts() {
    hotkey::clear_transcripts();
}

/// Dictionary entries for the Hub Dictionary screen.
#[tauri::command]
fn get_dictionary() -> Vec<hotkey::DictEntryDto> {
    hotkey::dictionary_entries()
}

/// Add a manual dictionary entry (word + optional known mishears).
#[tauri::command]
fn add_dictionary_entry(correct: String, mishears: Vec<String>) {
    hotkey::dictionary_add(correct, mishears);
}

/// Remove a dictionary entry by its spelling.
#[tauri::command]
fn remove_dictionary_entry(correct: String) {
    hotkey::dictionary_remove(&correct);
}

/// Permission + capability status shown in the Hub.
#[derive(Clone, Serialize)]
struct StatusReport {
    accessibility: bool,
    microphone: bool,
    input_monitoring: bool,
    has_openai_key: bool,
    has_anthropic_key: bool,
    /// "loading" | "ready" | "missing" — the local cleanup worker's model.
    local_state: &'static str,
    /// GGUF filename actually in use, when `local_state` is "ready".
    local_model: Option<String>,
    /// Whisper model filename actually on disk, if any.
    asr_model: Option<String>,
    /// What macOS does with a lone Fn press: "do_nothing" | "input_source" |
    /// "emoji" | "dictation" | "unknown". Anything but "do_nothing" fires on top
    /// of dictation — most visibly as the emoji picker.
    fn_key_action: &'static str,
}

#[tauri::command]
fn get_status() -> StatusReport {
    let local = hotkey::local_model_status();
    StatusReport {
        accessibility: paste::is_trusted(),
        microphone: paste::microphone_granted(),
        input_monitoring: paste::input_monitoring_granted(),
        has_openai_key: has_key("openai_api_key"),
        has_anthropic_key: has_key("anthropic_api_key"),
        local_state: local.state,
        local_model: local.model,
        asr_model: hotkey::asr_model_name(),
        fn_key_action: fnkey::fn_key_action(),
    }
}

/// Stop the current recording and paste what was said — the pill's ■ button.
#[tauri::command]
fn stop_dictation() {
    hotkey::stop_now();
}

/// Discard the current dictation, including one already being transcribed — the
/// pill's ✕ button. Nothing is pasted and nothing is logged to stats.
#[tauri::command]
fn cancel_dictation() {
    hotkey::cancel_now();
}

/// Open Keyboard settings, where "Press 🌐 key to" lives. WhimprFlow cannot change
/// it: the Fn action belongs to macOS, and our tap only listens.
#[tauri::command]
fn open_keyboard_settings() {
    #[cfg(target_os = "macos")]
    open_url("x-apple.systempreferences:com.apple.Keyboard-Settings.extension");
}

fn has_key(account: &str) -> bool {
    keyring::Entry::new("com.whimpr.whimprflow", account)
        .ok()
        .and_then(|e| e.get_password().ok())
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

/// Request microphone access: trigger the native prompt (bundle has a usage string)
/// by briefly opening the input device, and open the Microphone settings pane.
#[tauri::command]
fn request_microphone() {
    #[cfg(target_os = "macos")]
    {
        std::thread::spawn(|| {
            if let Ok(h) = whimpr_audio::start(|_: &[f32]| {}) {
                std::thread::sleep(std::time::Duration::from_millis(400));
                let _ = h.stop();
            }
        });
        open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone");
    }
}

/// Request Accessibility — the permission that makes the Fn key work in every app and
/// lets us type into other apps. Fire the native prompt, then open the pane.
#[tauri::command]
fn request_accessibility() {
    #[cfg(target_os = "macos")]
    {
        let _ = paste::prompt_accessibility();
        open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
    }
}

/// Request Input Monitoring (needed for the Fn key to be seen in every app, not
/// just while WhimprFlow is frontmost): register + prompt, then open the pane.
#[tauri::command]
fn request_input_monitoring() {
    #[cfg(target_os = "macos")]
    {
        let _ = paste::request_input_monitoring();
        open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent");
    }
}

/// Save (or clear, when empty) an API key in the OS keychain, then rebuild providers
/// so it takes effect immediately.
#[tauri::command]
fn set_api_key(provider: String, key: String) -> Result<(), String> {
    let account = match provider.as_str() {
        "openai" => "openai_api_key",
        "anthropic" => "anthropic_api_key",
        _ => return Err(format!("unknown provider {provider}")),
    };
    let entry =
        keyring::Entry::new("com.whimpr.whimprflow", account).map_err(|e| e.to_string())?;
    let key = key.trim();
    // Delete any existing item first so the new one is created by (and readable to)
    // this app — a key added via the `security` CLI isn't readable by the app.
    let _ = entry.delete_credential();
    if !key.is_empty() {
        entry.set_password(key).map_err(|e| e.to_string())?;
    }
    hotkey::rebuild_providers();
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_settings,
            set_settings,
            get_stats,
            get_history,
            clear_transcripts,
            get_dictionary,
            add_dictionary_entry,
            remove_dictionary_entry,
            get_status,
            request_microphone,
            request_accessibility,
            request_input_monitoring,
            open_keyboard_settings,
            stop_dictation,
            cancel_dictation,
            set_api_key
        ])
        // The red button hides the Hub, it does not close it. Closing would DESTROY
        // the window — the app would keep running (the overlay holds it open) with no
        // way back: `get_webview_window(HUB_LABEL)` returns None from then on, so the
        // tray's Open item and the Dock icon both become dead. Hiding keeps the window
        // (and its webview state) around for `show_hub`.
        .on_window_event(|window, event| {
            if window.label() == HUB_LABEL {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            // Regular app: shows in the Dock with a normal, focusable main window.
            // (Can switch to a menu-bar-only accessory app later for the Wispr look.)
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Regular);

            build_overlay(app)?;
            let hub = build_hub(app)?;
            let _ = hub.show();
            let _ = hub.set_focus();

            // Wire the Fn key to the pill via the real state machine.
            hotkey::install(app.handle().clone());

            let open = MenuItem::with_id(app, "open", "Open WhimprFlow", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit WhimprFlow", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &sep, &quit])?;

            let mut tray = TrayIconBuilder::new()
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_hub(app),
                    "quit" => app.exit(0),
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon().cloned() {
                tray = tray.icon(icon);
            }
            tray.build(app)?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building WhimprFlow")
        .run(|app, event| {
            // Dock click. `has_visible_windows` is not a useful signal here: the
            // overlay is a window and is technically visible whenever the pill is up,
            // so it would report true with the Hub hidden. Just re-show the Hub.
            if let tauri::RunEvent::Reopen { .. } = event {
                show_hub(app);
            }
        });
}
