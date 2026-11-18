fn main() {
    // Declaring the app's commands generates an `allow-<command>` permission
    // for each, so every command must be granted explicitly in a capability
    // (spec section 9).
    let manifest = tauri_build::AppManifest::new().commands(&[
        "set_hit_regions",
        "get_buddy_state",
        "report_interaction",
        "get_appearance",
        "get_settings",
        "update_settings",
        "list_monitors",
        "list_upcoming",
        "list_tasks",
        "create_task",
        "update_task",
        "set_task_done",
        "delete_task",
        "snooze_all",
        "show_cat_menu",
        "open_settings",
        "set_popover_open",
        "list_accounts",
        "add_account",
        "update_account",
        "remove_account",
        "list_account_calendars",
        "test_connection",
        "sign_in_account",
        "sync_now",
        "get_current_bubble",
        "dismiss_bubble",
        "snooze_bubble",
        "get_paused",
        "set_paused",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("failed to run tauri-build");
}
