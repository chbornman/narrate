//! The native menu bar (desktop-conventions pass, June 2026) — macOS only.
//!
//! WHY a native menu at all: the founder's "all the desktop general
//! things". On macOS the menu bar is not decoration — the Edit roles
//! (Cut/Copy/Paste/Select All/Undo/Redo) are what make Cmd+C/V/X work in
//! text fields AT ALL: WKWebView routes editing key equivalents through
//! the responder chain, and with no NSMenu item carrying ⌘V there is
//! nothing to perform `paste:`. Hide/Minimize/Close-Window/Quit likewise
//! come from their predefined items, handled entirely by AppKit (they
//! never reach the menu-event hook below).
//!
//! WHY macOS only: Windows/Linux run UNDECORATED with the custom DOM
//! titlebar (tauri.conf.json `decorations: false`) — a muda menubar would
//! be alien chrome there, and their webviews handle clipboard chords
//! natively without menu roles. This module is compiled out entirely
//! (`#[cfg(target_os = "macos")]` at the mod declaration in lib.rs), the
//! Tauri-recommended guard for platform menus.
//!
//! CUSTOM items deliberately carry ACTION-REGISTRY ids ("open-settings",
//! "toggle-rail", …): the on_menu_event hook forwards the id to the main
//! webview as one `menu-action` event (the runtime-status emit pattern,
//! pump.rs), and App.svelte resolves it through the SAME resolveAction →
//! perform path the keyboard uses. Zero new verbs; the menu is a fourth
//! rendering of the one action table (UI-ARCHITECTURE §3).
//!
//! Accelerator policy: Cmd-modified chords only. Plain-key verbs
//! (`\` rail, `?` cheatsheet, Tab lights-out) get menu rows WITHOUT
//! accelerators — a plain-key NSMenu equivalent would steal the key from
//! every text field; their keys stay webview facts (the cheatsheet shows
//! them). For chords the webview ALSO binds (⌘, ⌘F ⌘= ⌘− ⌘0), the
//! in-page handler preventDefaults first and WKWebView then never
//! re-dispatches the equivalent to the menu — single dispatch, with the
//! menu as the fallback when webview focus is elsewhere.

use tauri::menu::{
    AboutMetadataBuilder, MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder,
};
use tauri::{Emitter, Manager};

/// Build + install the menu bar and the one menu-event → webview bridge.
/// Called from setup (lib.rs); the menu is app-wide (every window shares
/// the macOS menu bar, so Edit roles serve the Settings window too).
pub fn install(app: &tauri::App) -> tauri::Result<()> {
    let handle = app.handle();

    // About panel: name + version from the crate; everything else is
    // AppKit's standard sheet. (The bundle identifier/icon flow in from
    // tauri.conf.json at bundle time.)
    let about = AboutMetadataBuilder::new()
        .name(Some("Photoproof"))
        .version(Some(app.package_info().version.to_string()))
        .build();

    // The first submenu becomes THE application menu on macOS.
    let app_menu = SubmenuBuilder::new(handle, "Photoproof")
        .about_with_text("About Photoproof", Some(about))
        .separator()
        .item(
            // Settings… lives in the app menu by macOS convention; ⌘,
            // is also the webview keymap's open-settings chord — see the
            // accelerator policy in the module docs.
            &MenuItemBuilder::with_id("open-settings", "Settings…")
                .accelerator("Cmd+,")
                .build(handle)?,
        )
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        // Predefined quit (⌘Q) exits through RunEvent::ExitRequested,
        // where lib.rs closes the session and flushes sidecars — same
        // shutdown path as the webview's own ⌘Q row (ipc.quit).
        .quit()
        .build()?;

    let file_menu = SubmenuBuilder::new(handle, "File")
        .close_window() // ⌘W — and the last window closing exits the app
        .build()?;

    let edit_menu = SubmenuBuilder::new(handle, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let view_menu = SubmenuBuilder::new(handle, "View")
        // UI scale — webview zoom, the registry's ui-zoom ladder. The two
        // directional ids are the menu's spelling of the parametrized
        // ui-zoom row (App.svelte maps them onto the chord arg).
        .item(
            &MenuItemBuilder::with_id("ui-zoom-in", "Zoom In")
                .accelerator("Cmd+=")
                .build(handle)?,
        )
        .item(
            &MenuItemBuilder::with_id("ui-zoom-out", "Zoom Out")
                .accelerator("Cmd+-")
                .build(handle)?,
        )
        .item(
            &MenuItemBuilder::with_id("ui-zoom-reset", "Reset Zoom")
                .accelerator("Cmd+0")
                .build(handle)?,
        )
        .separator()
        .item(
            &MenuItemBuilder::with_id("open-search", "Search")
                .accelerator("Cmd+F")
                .build(handle)?,
        )
        .item(&MenuItemBuilder::with_id("toggle-rail", "Toggle Source Rail").build(handle)?)
        .item(&MenuItemBuilder::with_id("toggle-cheatsheet", "Keyboard Map").build(handle)?)
        .item(&MenuItemBuilder::with_id("toggle-lights-out", "Lights-Out").build(handle)?)
        .separator()
        // CUSTOM fullscreen item, not PredefinedMenuItem::fullscreen: the
        // F11 toggle-fullscreen action owns shell.fullscreen as frontend
        // state (lib.rs strips FULLSCREEN from window-state restore for
        // the same reason) — a native toggle behind the frontend's back
        // would desync the F11 press after it. ⌃⌘F reaches this
        // accelerator because App.svelte forfeits ctrl+meta chords to the
        // menu (the KeyInput shape can't express the pair).
        .item(
            &MenuItemBuilder::with_id("toggle-fullscreen", "Toggle Full Screen")
                .accelerator("Ctrl+Cmd+F")
                .build(handle)?,
        )
        .build()?;

    let window_menu = SubmenuBuilder::new(handle, "Window")
        .minimize() // ⌘M
        .item(&PredefinedMenuItem::maximize(handle, Some("Zoom"))?)
        .build()?;
    // Hand the submenu to AppKit as THE Window menu: macOS appends the
    // open-window list and the tiling verbs itself.
    window_menu.set_as_windows_menu_for_nsapp()?;

    let menu = MenuBuilder::new(handle)
        .items(&[&app_menu, &file_menu, &edit_menu, &view_menu, &window_menu])
        .build()?;
    app.set_menu(menu)?;

    // Predefined items never land here (AppKit performs them); every
    // custom id forwards to the main webview's perform sink. Target the
    // MAIN window explicitly: the verbs are main-surface concepts even
    // when the Settings window holds focus.
    app.on_menu_event(|handle, event| {
        if let Some(main) = handle.get_webview_window("main") {
            let _ = main.emit("menu-action", event.id().as_ref());
        }
    });

    Ok(())
}
