use dioxus::prelude::*;

use crate::ui::widgets::ProgressPanel;
use crate::{ApiDiagnosticRow, OperationProgress, UserProfileInfo};

#[component]
pub fn ApiPanel(
    app_key_masked: String,
    user_key: String,
    on_user_key_input: EventHandler<FormEvent>,
    api_min_interval_ms: String,
    on_api_min_interval_input: EventHandler<FormEvent>,
    api_max_retries: String,
    on_api_max_retries_input: EventHandler<FormEvent>,
    api_retry_base_ms: String,
    on_api_retry_base_ms_input: EventHandler<FormEvent>,
    api_retry_max_ms: String,
    on_api_retry_max_ms_input: EventHandler<FormEvent>,
    static_cache_ttl_seconds: String,
    on_static_cache_ttl_input: EventHandler<FormEvent>,
    startup_user_cache_ttl_seconds: String,
    on_startup_user_cache_ttl_input: EventHandler<FormEvent>,
    image_prefetch_count: String,
    on_image_prefetch_count_input: EventHandler<FormEvent>,
    screenshot_capture_delay_ms: String,
    on_screenshot_capture_delay_input: EventHandler<FormEvent>,
    screenshot_session_captures: String,
    on_screenshot_session_captures_input: EventHandler<FormEvent>,
    screenshot_session_interval_ms: String,
    on_screenshot_session_interval_input: EventHandler<FormEvent>,
    capture_crop_left_percent: String,
    on_capture_crop_left_percent_input: EventHandler<FormEvent>,
    capture_crop_top_percent: String,
    on_capture_crop_top_percent_input: EventHandler<FormEvent>,
    capture_crop_width_percent: String,
    on_capture_crop_width_percent_input: EventHandler<FormEvent>,
    capture_crop_height_percent: String,
    on_capture_crop_height_percent_input: EventHandler<FormEvent>,
    screenshot_grid_columns: String,
    on_screenshot_grid_columns_input: EventHandler<FormEvent>,
    screenshot_grid_rows: String,
    on_screenshot_grid_rows_input: EventHandler<FormEvent>,
    screenshot_slot_padding_percent: String,
    on_screenshot_slot_padding_percent_input: EventHandler<FormEvent>,
    screenshot_quantity_ocr_enabled: bool,
    on_screenshot_quantity_ocr_enabled_toggle: EventHandler<FormEvent>,
    sell_exclude_weapons: bool,
    on_sell_exclude_weapons_toggle: EventHandler<FormEvent>,
    sell_exclude_types: String,
    on_sell_exclude_types_input: EventHandler<FormEvent>,
    on_reset_settings: EventHandler<MouseEvent>,
    clearing_cache: bool,
    on_clear_user_cache: EventHandler<MouseEvent>,
    on_clear_image_cache: EventHandler<MouseEvent>,
    on_clear_all_cache: EventHandler<MouseEvent>,
    theme_button_text: String,
    on_theme_toggle: EventHandler<MouseEvent>,
    planning_button_text: String,
    on_planning_toggle: EventHandler<MouseEvent>,
    loading_data: bool,
    scanning_inventory: bool,
    syncing_progress: bool,
    diagnostics_running: bool,
    on_load_data: EventHandler<MouseEvent>,
    on_scan_inventory: EventHandler<MouseEvent>,
    on_capture_scroll_session: EventHandler<MouseEvent>,
    on_import_inventory: EventHandler<MouseEvent>,
    on_scan_inventory_api: EventHandler<MouseEvent>,
    on_auto_sync: EventHandler<MouseEvent>,
    on_run_diagnostics: EventHandler<MouseEvent>,
    profile: Option<UserProfileInfo>,
    status_message: String,
    error_message: String,
    diagnostics_rows: Vec<ApiDiagnosticRow>,
    diagnostics_report: String,
    progress: Option<OperationProgress>,
) -> Element {
    rsx! {
        section { class: "panel", "aria-label": "API and synchronization controls",
            h2 { "API + Sync" }
            p { class: "muted", "Run sync operations here. API credentials are available under Settings." }
            div { class: "actions",
                button {
                    disabled: loading_data,
                    onclick: move |evt| on_load_data.call(evt),
                    if loading_data { "Loading Data..." } else { "Load / Refresh Game Data" }
                }
                button {
                    disabled: scanning_inventory,
                    onclick: move |evt| on_scan_inventory.call(evt),
                    if scanning_inventory { "Capturing..." } else { "Capture Inventory From Screen" }
                }
                button {
                    disabled: scanning_inventory,
                    onclick: move |evt| on_capture_scroll_session.call(evt),
                    if scanning_inventory { "Capturing Session..." } else { "Capture Scroll Session" }
                }
                button {
                    disabled: syncing_progress,
                    onclick: move |evt| on_auto_sync.call(evt),
                    if syncing_progress { "Syncing..." } else { "Auto Sync Progress" }
                }
                button {
                    disabled: diagnostics_running,
                    onclick: move |evt| on_run_diagnostics.call(evt),
                    if diagnostics_running { "Running Diagnostics..." } else { "Run API Diagnostics" }
                }
            }
            details { class: "settings-disclosure",
                summary { class: "settings-summary", "Settings" }
                div { class: "settings-body",
                    div { class: "actions settings-actions",
                        button {
                            class: "ghost",
                            onclick: move |evt| on_theme_toggle.call(evt),
                            "{theme_button_text}"
                        }
                        button {
                            class: "ghost",
                            onclick: move |evt| on_planning_toggle.call(evt),
                            "{planning_button_text}"
                        }
                    }
                    p { class: "muted", "App key from .env: {app_key_masked}" }
                    div {
                        class: "settings-field",
                        label { class: "field-label", "User key (arc_u1_...)" }
                        input {
                            value: "{user_key}",
                            placeholder: "arc_u1_your_user_key",
                            "aria-label": "User API key",
                            oninput: move |evt| on_user_key_input.call(evt),
                        }
                    }
                    p { class: "muted", "On-screen capture is the primary inventory scanner. Click capture, switch to the game during the countdown, and let the app scan the resulting screenshot. Scroll sessions take repeated captures while you scroll manually and deduplicate overlapping rows." }
                    div { class: "settings-grid",
                        div { class: "settings-field",
                            label { class: "field-label", "API min interval (ms)" }
                            input {
                                r#type: "number",
                                min: "0",
                                step: "100",
                                value: "{api_min_interval_ms}",
                                oninput: move |evt| on_api_min_interval_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "API max retries" }
                            input {
                                r#type: "number",
                                min: "0",
                                step: "1",
                                value: "{api_max_retries}",
                                oninput: move |evt| on_api_max_retries_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Retry base (ms)" }
                            input {
                                r#type: "number",
                                min: "0",
                                step: "100",
                                value: "{api_retry_base_ms}",
                                oninput: move |evt| on_api_retry_base_ms_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Retry max (ms)" }
                            input {
                                r#type: "number",
                                min: "0",
                                step: "100",
                                value: "{api_retry_max_ms}",
                                oninput: move |evt| on_api_retry_max_ms_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Static cache TTL (s)" }
                            input {
                                r#type: "number",
                                min: "0",
                                step: "60",
                                value: "{static_cache_ttl_seconds}",
                                oninput: move |evt| on_static_cache_ttl_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Startup cache TTL (s)" }
                            input {
                                r#type: "number",
                                min: "0",
                                step: "60",
                                value: "{startup_user_cache_ttl_seconds}",
                                oninput: move |evt| on_startup_user_cache_ttl_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Image prefetch count" }
                            input {
                                r#type: "number",
                                min: "0",
                                step: "1",
                                value: "{image_prefetch_count}",
                                oninput: move |evt| on_image_prefetch_count_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Capture delay (ms)" }
                            input {
                                r#type: "number",
                                min: "0",
                                step: "100",
                                value: "{screenshot_capture_delay_ms}",
                                oninput: move |evt| on_screenshot_capture_delay_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Scroll session captures" }
                            input {
                                r#type: "number",
                                min: "2",
                                step: "1",
                                value: "{screenshot_session_captures}",
                                oninput: move |evt| on_screenshot_session_captures_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Scroll session interval (ms)" }
                            input {
                                r#type: "number",
                                min: "0",
                                step: "100",
                                value: "{screenshot_session_interval_ms}",
                                oninput: move |evt| on_screenshot_session_interval_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Capture crop left (%)" }
                            input {
                                r#type: "number",
                                min: "0",
                                max: "99",
                                step: "1",
                                value: "{capture_crop_left_percent}",
                                oninput: move |evt| on_capture_crop_left_percent_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Capture crop top (%)" }
                            input {
                                r#type: "number",
                                min: "0",
                                max: "99",
                                step: "1",
                                value: "{capture_crop_top_percent}",
                                oninput: move |evt| on_capture_crop_top_percent_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Capture crop width (%)" }
                            input {
                                r#type: "number",
                                min: "1",
                                max: "100",
                                step: "1",
                                value: "{capture_crop_width_percent}",
                                oninput: move |evt| on_capture_crop_width_percent_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Capture crop height (%)" }
                            input {
                                r#type: "number",
                                min: "1",
                                max: "100",
                                step: "1",
                                value: "{capture_crop_height_percent}",
                                oninput: move |evt| on_capture_crop_height_percent_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Screenshot grid columns" }
                            input {
                                r#type: "number",
                                min: "1",
                                step: "1",
                                value: "{screenshot_grid_columns}",
                                oninput: move |evt| on_screenshot_grid_columns_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Screenshot grid rows" }
                            input {
                                r#type: "number",
                                min: "1",
                                step: "1",
                                value: "{screenshot_grid_rows}",
                                oninput: move |evt| on_screenshot_grid_rows_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Screenshot slot padding (%)" }
                            input {
                                r#type: "number",
                                min: "0",
                                max: "40",
                                step: "1",
                                value: "{screenshot_slot_padding_percent}",
                                oninput: move |evt| on_screenshot_slot_padding_percent_input.call(evt),
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "OCR screenshot quantities" }
                            label { class: "checkbox-row",
                                input {
                                    r#type: "checkbox",
                                    checked: screenshot_quantity_ocr_enabled,
                                    onchange: move |evt| on_screenshot_quantity_ocr_enabled_toggle.call(evt),
                                }
                                span { "Read stack counts from screenshots with OCR (Windows-only today)" }
                            }
                        }
                        div { class: "settings-field",
                            label { class: "field-label", "Exclude weapons from Can Sell" }
                            label { class: "checkbox-row",
                                input {
                                    r#type: "checkbox",
                                    checked: sell_exclude_weapons,
                                    onchange: move |evt| on_sell_exclude_weapons_toggle.call(evt),
                                }
                                span { "Hide weapons and gear-like items from sell suggestions" }
                            }
                        }
                        div { class: "settings-field settings-field-wide",
                            label { class: "field-label", "Excluded sell types (comma-separated)" }
                            input {
                                value: "{sell_exclude_types}",
                                placeholder: "Augment, Modification, Ammunition, Quick Use, Shield, Key",
                                oninput: move |evt| on_sell_exclude_types_input.call(evt),
                            }
                        }
                    }
                    div { class: "actions settings-actions",
                        button {
                            class: "ghost",
                            onclick: move |evt| on_reset_settings.call(evt),
                            "Reset Runtime Settings"
                        }
                    }
                    div { class: "actions settings-actions",
                        button {
                            class: "ghost",
                            disabled: scanning_inventory,
                            onclick: move |evt| on_capture_scroll_session.call(evt),
                            if scanning_inventory {
                                "Capturing Scroll Session..."
                            } else {
                                "Start Capture Scroll Session"
                            }
                        }
                    }
                    div { class: "actions settings-actions",
                        button {
                            class: "ghost",
                            disabled: scanning_inventory,
                            onclick: move |evt| on_import_inventory.call(evt),
                            if scanning_inventory {
                                "Importing Screenshots..."
                            } else {
                                "Import Inventory Screenshots (Manual)"
                            }
                        }
                    }
                    div { class: "actions settings-actions",
                        button {
                            class: "ghost",
                            disabled: scanning_inventory,
                            onclick: move |evt| on_scan_inventory_api.call(evt),
                            if scanning_inventory {
                                "Syncing API Inventory..."
                            } else {
                                "Sync Inventory via API (Fallback)"
                            }
                        }
                    }
                    div { class: "actions settings-actions",
                        button {
                            class: "ghost",
                            disabled: clearing_cache,
                            onclick: move |evt| on_clear_user_cache.call(evt),
                            if clearing_cache { "Clearing..." } else { "Clear User Cache" }
                        }
                        button {
                            class: "ghost",
                            disabled: clearing_cache,
                            onclick: move |evt| on_clear_image_cache.call(evt),
                            if clearing_cache { "Clearing..." } else { "Clear Image Cache" }
                        }
                        button {
                            class: "ghost danger",
                            disabled: clearing_cache,
                            onclick: move |evt| on_clear_all_cache.call(evt),
                            if clearing_cache { "Clearing..." } else { "Clear All Cache" }
                        }
                    }
                    p { class: "muted", "For best results, tune the capture crop percentages so the captured region contains only the stash grid, then use scroll sessions to walk down the inventory gradually. User keys grant access to personal data. Keep them user-provided and revocable." }
                }
            }
            if let Some(progress) = progress.as_ref() {
                ProgressPanel { progress: progress.clone() }
            }
            if let Some(profile) = profile.as_ref() {
                p { class: "muted",
                    if let Some(level) = profile.level {
                        "User: {profile.username} (Level {level})"
                    } else {
                        "User: {profile.username}"
                    }
                    if let Some(member_since) = profile.member_since.as_ref() {
                        " | Member since: {member_since}"
                    }
                }
            }
            if !status_message.is_empty() {
                p { class: "status", role: "status", "aria-live": "polite", "{status_message}" }
            }
            if !error_message.is_empty() {
                p { class: "error", role: "alert", "{error_message}" }
            }
            if !diagnostics_rows.is_empty() {
                h3 { "API Diagnostics Report" }
                table { class: "table compact diagnostics-table", "aria-label": "API diagnostics results",
                    thead {
                        tr {
                            th { "Endpoint" }
                            th { "Status" }
                            th { "Request ID" }
                            th { "Details" }
                        }
                    }
                    tbody {
                        for row in diagnostics_rows.iter() {
                            tr { key: "{row.endpoint}",
                                td { "{row.endpoint}" }
                                td { if row.ok { "OK (200)" } else { "{row.status_code}" } }
                                td { "{row.request_id.clone().unwrap_or_else(|| \"-\".to_string())}" }
                                td { "{row.detail}" }
                            }
                        }
                    }
                }
            }
            if !diagnostics_report.is_empty() {
                p { class: "muted", "Copy and share this report with ArcTracker support if needed:" }
                pre { class: "diagnostics-report", "{diagnostics_report}" }
            }
        }
    }
}
