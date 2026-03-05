use dioxus::prelude::*;

use crate::ui::widgets::ProgressPanel;
use crate::{ApiDiagnosticRow, OperationProgress, UserProfileInfo};

#[component]
pub fn ApiPanel(
    app_key_masked: String,
    user_key: String,
    on_user_key_input: EventHandler<FormEvent>,
    loading_data: bool,
    scanning_inventory: bool,
    syncing_progress: bool,
    diagnostics_running: bool,
    on_load_data: EventHandler<MouseEvent>,
    on_scan_inventory: EventHandler<MouseEvent>,
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
            p { class: "muted", "App key from .env: {app_key_masked}" }
            label { "User key (arc_u1_...):" }
            input {
                value: "{user_key}",
                placeholder: "arc_u1_your_user_key",
                "aria-label": "User API key",
                oninput: move |evt| on_user_key_input.call(evt),
            }
            div { class: "actions",
                button {
                    disabled: loading_data,
                    onclick: move |evt| on_load_data.call(evt),
                    if loading_data { "Loading Data..." } else { "Load / Refresh Game Data" }
                }
                button {
                    disabled: scanning_inventory,
                    onclick: move |evt| on_scan_inventory.call(evt),
                    if scanning_inventory { "Scanning..." } else { "Scan Inventory" }
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
