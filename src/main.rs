#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(clippy::clone_on_copy, clippy::collapsible_if)]

use std::{
    collections::HashMap,
    sync::Arc,
    time::Duration,
};

mod ui;
mod api;
mod cache;
mod domain;
mod http;
mod parsing;
mod support;

use dioxus::desktop::{Config as DesktopConfig, WindowBuilder, tao::window::Icon};
use dioxus::prelude::*;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use ui::{ApiPanel, DashboardPanel, ToastViewport, TrackingPanels};
use support::{
    AppRuntimeSettings, compiled_app_key, default_show_planning_workspace,
    default_theme_preference, first_non_empty_env, mask_key, next_theme_preference,
    normalize_theme_preference, now_unix_millis, now_unix_seconds, parse_csv_lower_set,
    replace_runtime_settings, runtime_settings_snapshot, theme_preference_label,
};
pub use domain::{
    ArcData, Dashboard, HideoutLevel, HideoutModule, Item, ItemRequirement, NeedRow, Project,
    ProjectPhase, Quest, SellRow, TrackedCraft, TrackedHideout, TrackedProject,
    aggregate_requirements, build_dashboard, hideout_name, item_name, localized_en,
    merge_counts, module_max_level, parse_u32_or_default, project_max_phase, project_name,
    quest_name,
};
use api::{
    ApiDiagnosticRow, build_api_diagnostics_report, fetch_stash_inventory,
    fetch_stash_inventory_with_cache, fetch_static_data, image_mime_type,
    local_item_image_data_uri, run_api_diagnostics, sync_user_progress,
};
use cache::{
    clear_all_cache, clear_cache_namespace, get_json_cached, load_tracked_state,
    read_cached_remote_image_data_uri, read_cache_typed, save_tracked_state, short_hash,
    write_cache_typed, write_cached_remote_image,
};
use http::{
    extract_http_status_code_from_error, extract_request_id_from_error,
    extract_request_id_from_payload, get_json, truncate_for_report,
};
pub use parsing::{
    HideoutResponse, ItemsResponse, ProjectsResponse, QuestsResponse, UserProfileInfo,
    extract_inventory_counts, extract_known_ids, extract_progress_level_map, has_next_page,
    parse_user_profile, unwrap_data_ref,
};

const API_BASE: &str = "https://arctracker.io";
const LOCAL_DATA_DEFAULT_DIR: &str = "vendor/arcraiders-data";
const DEFAULT_API_MIN_INTERVAL_MS: u64 = 1000;
const DEFAULT_API_MAX_RETRIES: usize = 2;
const DEFAULT_API_RETRY_BASE_MS: u64 = 1500;
const DEFAULT_API_RETRY_MAX_MS: u64 = 10000;
const DEFAULT_STATIC_CACHE_TTL_SECONDS: u64 = 60 * 60 * 24;
const DEFAULT_STARTUP_USER_CACHE_TTL_SECONDS: u64 = 60 * 5;
const DEFAULT_IMAGE_PREFETCH_COUNT: usize = 12;
const DEFAULT_SELL_EXCLUDE_WEAPONS: bool = true;
const DEFAULT_SELL_EXCLUDE_TYPES: &[&str] = &[
    "Augment",
    "Modification",
    "Ammunition",
    "Quick Use",
    "Shield",
    "Key",
];
const CACHE_NAMESPACE_STATIC: &str = "static_api";
const CACHE_NAMESPACE_USER: &str = "user_api";
const CACHE_NAMESPACE_IMAGES: &str = "images";
const CACHE_FILE_TRACKED_STATE: &str = "tracked_state.json";
const COMPILED_APP_KEY: Option<&str> = option_env!("ARC_APP_KEY");

fn main() {
    dotenvy::dotenv().ok();
    init_logging();
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            DesktopConfig::new()
                .with_window(WindowBuilder::new().with_window_icon(create_app_icon())),
        )
        .launch(App);
}

fn create_app_icon() -> Option<Icon> {
    let size = 64usize;
    let mut rgba = vec![0u8; size * size * 4];

    for y in 0..size {
        for x in 0..size {
            let i = (y * size + x) * 4;
            let t = y as f32 / (size - 1) as f32;

            // Deep slate -> steel blue gradient background.
            let r = (18.0 + (55.0 - 18.0) * t) as u8;
            let g = (29.0 + (89.0 - 29.0) * t) as u8;
            let b = (42.0 + (128.0 - 42.0) * t) as u8;

            rgba[i] = r;
            rgba[i + 1] = g;
            rgba[i + 2] = b;
            rgba[i + 3] = 255;
        }
    }

    // Rounded corner alpha mask.
    let radius = 12i32;
    for y in 0..size {
        for x in 0..size {
            let i = (y * size + x) * 4 + 3;
            let x = x as i32;
            let y = y as i32;
            let w = size as i32 - 1;
            let h = size as i32 - 1;
            let near_left = x < radius;
            let near_right = x > w - radius;
            let near_top = y < radius;
            let near_bottom = y > h - radius;

            let outside = (near_left
                && near_top
                && (x - radius).pow(2) + (y - radius).pow(2) > radius.pow(2))
                || (near_right
                    && near_top
                    && (x - (w - radius)).pow(2) + (y - radius).pow(2) > radius.pow(2))
                || (near_left
                    && near_bottom
                    && (x - radius).pow(2) + (y - (h - radius)).pow(2) > radius.pow(2))
                || (near_right
                    && near_bottom
                    && (x - (w - radius)).pow(2) + (y - (h - radius)).pow(2) > radius.pow(2));
            if outside {
                rgba[i] = 0;
            }
        }
    }

    // Stylized "A" glyph in light blue.
    for y in 10..54 {
        for x in 8..56 {
            let xf = x as f32;
            let yf = y as f32;
            let left_leg = (yf > 12.0)
                && ((xf - 17.0) < (yf - 12.0) * 0.55)
                && ((xf - 12.0) > (yf - 12.0) * 0.35);
            let right_leg = (yf > 12.0)
                && ((46.0 - xf) < (yf - 12.0) * 0.55)
                && ((51.0 - xf) > (yf - 12.0) * 0.35);
            let bar = (yf > 31.0 && yf < 36.0) && (xf > 22.0 && xf < 42.0);

            if left_leg || right_leg || bar {
                let i = (y * size + x) * 4;
                rgba[i] = 182;
                rgba[i + 1] = 222;
                rgba[i + 2] = 255;
                rgba[i + 3] = 255;
            }
        }
    }

    Icon::from_rgba(rgba, size as u32, size as u32).ok()
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if let Err(err) = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init()
    {
        eprintln!("failed to initialize logger: {err}");
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct OperationProgress {
    label: String,
    detail: String,
    current: u32,
    total: u32,
    indeterminate: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
struct Toast {
    id: u64,
    kind: ToastKind,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedTrackedState {
    tracked_crafts: Vec<TrackedCraft>,
    tracked_quests: Vec<String>,
    tracked_hideout: Vec<TrackedHideout>,
    tracked_projects: Vec<TrackedProject>,
    #[serde(default)]
    settings: AppRuntimeSettings,
    #[serde(default = "default_theme_preference")]
    theme_preference: String,
    #[serde(default = "default_show_planning_workspace")]
    show_planning_workspace: bool,
    saved_at_unix: u64,
}

#[component]
fn App() -> Element {
    let persisted_state = load_tracked_state().unwrap_or_default();
    let initial_tracked_crafts = persisted_state.tracked_crafts.clone();
    let initial_tracked_quests = persisted_state.tracked_quests.clone();
    let initial_tracked_hideout = persisted_state.tracked_hideout.clone();
    let initial_tracked_projects = persisted_state.tracked_projects.clone();
    let initial_runtime_settings = persisted_state.settings.clone();
    let initial_theme_preference = normalize_theme_preference(&persisted_state.theme_preference);
    let initial_show_planning_workspace = persisted_state.show_planning_workspace;
    replace_runtime_settings(initial_runtime_settings.clone());

    let default_app_key = first_non_empty_env(&["key", "ARC_APP_KEY"])
        .or_else(compiled_app_key)
        .unwrap_or_default();
    let default_user_key =
        first_non_empty_env(&["user_key", "ARC_USER_KEY", "arc_user_key"]).unwrap_or_default();

    let app_key = use_signal(|| default_app_key);
    let mut user_key = use_signal(|| default_user_key);
    let profile_info = use_signal(|| Option::<UserProfileInfo>::None);

    let static_data = use_signal::<Option<Arc<ArcData>>>(|| None);
    let inventory_counts = use_signal(HashMap::<String, u32>::new);
    let loadout_counts = use_signal(HashMap::<String, u32>::new);

    let tracked_crafts = use_signal(move || initial_tracked_crafts.clone());
    let tracked_quests = use_signal(move || initial_tracked_quests.clone());
    let tracked_hideout = use_signal(move || initial_tracked_hideout.clone());
    let tracked_projects = use_signal(move || initial_tracked_projects.clone());
    let runtime_settings = use_signal(move || initial_runtime_settings.clone());
    let theme_preference = use_signal(move || initial_theme_preference.clone());
    let show_planning_workspace = use_signal(move || initial_show_planning_workspace);
    let mut dashboard_filter = use_signal(String::new);

    let craft_pick = use_signal(String::new);
    let craft_qty = use_signal(|| "1".to_string());
    let quest_pick = use_signal(String::new);
    let hideout_pick = use_signal(String::new);
    let hideout_level = use_signal(|| "1".to_string());
    let project_pick = use_signal(String::new);
    let project_phase = use_signal(|| "1".to_string());

    let loading_data = use_signal(|| false);
    let scanning_inventory = use_signal(|| false);
    let syncing_progress = use_signal(|| false);
    let startup_sync_started = use_signal(|| false);
    let requirements_data_ready = use_signal(|| false);
    let requirements_data_issue = use_signal(String::new);
    let diagnostics_running = use_signal(|| false);
    let cache_action_running = use_signal(|| false);
    let diagnostics_rows = use_signal(Vec::<ApiDiagnosticRow>::new);
    let diagnostics_report = use_signal(String::new);
    let operation_progress = use_signal(|| Option::<OperationProgress>::None);
    let toasts = use_signal(Vec::<Toast>::new);
    let status_message = use_signal(String::new);
    let error_message = use_signal(String::new);

    let data_snapshot = static_data.read().clone();
    let crafts_snapshot = tracked_crafts.read().clone();
    let quests_snapshot = tracked_quests.read().clone();
    let hideout_snapshot = tracked_hideout.read().clone();
    let projects_snapshot = tracked_projects.read().clone();
    let diagnostics_rows_snapshot = diagnostics_rows.read().clone();
    let diagnostics_report_snapshot = diagnostics_report.read().clone();
    let progress_snapshot = operation_progress.read().clone();
    let toasts_snapshot = toasts.read().clone();

    let required_items_memo = {
        let static_data = static_data.clone();
        let tracked_crafts = tracked_crafts.clone();
        let tracked_quests = tracked_quests.clone();
        let tracked_hideout = tracked_hideout.clone();
        let tracked_projects = tracked_projects.clone();
        use_memo(move || {
            if let Some(data) = static_data.read().as_ref() {
                aggregate_requirements(
                    data,
                    &tracked_crafts.read(),
                    &tracked_quests.read(),
                    &tracked_hideout.read(),
                    &tracked_projects.read(),
                )
            } else {
                HashMap::new()
            }
        })
    };
    let has_manual_goals = {
        let tracked_crafts = tracked_crafts.clone();
        let tracked_quests = tracked_quests.clone();
        let tracked_hideout = tracked_hideout.clone();
        let tracked_projects = tracked_projects.clone();
        *use_memo(move || {
            !tracked_crafts.read().is_empty()
                || !tracked_quests.read().is_empty()
                || !tracked_hideout.read().is_empty()
                || !tracked_projects.read().is_empty()
        })
        .read()
    };
    let suppress_sell_recommendations = !*requirements_data_ready.read() && !has_manual_goals;

    let dashboard_memo = {
        let static_data = static_data.clone();
        let inventory_counts = inventory_counts.clone();
        let loadout_counts = loadout_counts.clone();
        let requirements_data_ready = requirements_data_ready.clone();
        let tracked_crafts = tracked_crafts.clone();
        let tracked_quests = tracked_quests.clone();
        let tracked_hideout = tracked_hideout.clone();
        let tracked_projects = tracked_projects.clone();
        let required_items_memo = required_items_memo.clone();
        use_memo(move || {
            let has_manual_goals = !tracked_crafts.read().is_empty()
                || !tracked_quests.read().is_empty()
                || !tracked_hideout.read().is_empty()
                || !tracked_projects.read().is_empty();
            let suppress_sell_recommendations =
                !*requirements_data_ready.read() && !has_manual_goals;
            if let Some(data) = static_data.read().as_ref() {
                build_dashboard(
                    data,
                    &required_items_memo.read(),
                    &inventory_counts.read(),
                    &loadout_counts.read(),
                    !suppress_sell_recommendations,
                    item_image_src,
                    is_excluded_from_sell,
                )
            } else {
                Dashboard::default()
            }
        })
    };
    let dashboard = dashboard_memo.read().clone();

    let required_rows_memo = {
        let static_data = static_data.clone();
        let inventory_counts = inventory_counts.clone();
        let required_items_memo = required_items_memo.clone();
        use_memo(move || {
            let required_items = required_items_memo.read();
            let mut rows: Vec<NeedRow> = if let Some(data) = static_data.read().as_ref() {
                required_items
                    .iter()
                    .map(|(id, required)| {
                        let have = *inventory_counts.read().get(id).unwrap_or(&0);
                        let missing = required.saturating_sub(have);
                        NeedRow {
                            name: item_name(data, id),
                            image_src: item_image_src(data, id),
                            required: *required,
                            have,
                            missing,
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            rows.sort_by(|a, b| b.missing.cmp(&a.missing).then(a.name.cmp(&b.name)));
            rows
        })
    };
    let required_rows = required_rows_memo.read().clone();

    let theme_value = normalize_theme_preference(theme_preference.read().as_str());
    let theme_data_attr = if theme_value == "system" {
        String::new()
    } else {
        theme_value.clone()
    };
    let theme_button_text = format!("Theme: {}", theme_preference_label(&theme_value));
    let settings_snapshot = runtime_settings.read().clone();

    let dashboard_query = dashboard_filter.read().trim().to_ascii_lowercase();
    let sell_rows_filtered: Vec<SellRow> = if dashboard_query.is_empty() {
        dashboard.sell.clone()
    } else {
        dashboard
            .sell
            .iter()
            .filter(|row| row.name.to_ascii_lowercase().contains(&dashboard_query))
            .cloned()
            .collect()
    };
    let need_rows_filtered: Vec<NeedRow> = if dashboard_query.is_empty() {
        dashboard.needs.clone()
    } else {
        dashboard
            .needs
            .iter()
            .filter(|row| row.name.to_ascii_lowercase().contains(&dashboard_query))
            .cloned()
            .collect()
    };
    let keep_rows_filtered: Vec<NeedRow> = if dashboard_query.is_empty() {
        dashboard.keep.clone()
    } else {
        dashboard
            .keep
            .iter()
            .filter(|row| row.name.to_ascii_lowercase().contains(&dashboard_query))
            .cloned()
            .collect()
    };
    let required_rows_filtered: Vec<NeedRow> = if dashboard_query.is_empty() {
        required_rows.clone()
    } else {
        required_rows
            .iter()
            .filter(|row| row.name.to_ascii_lowercase().contains(&dashboard_query))
            .cloned()
            .collect()
    };
    let sell_total_qty: u32 = dashboard.sell.iter().map(|row| row.quantity).sum();
    let sell_total_value: u64 = dashboard.sell.iter().map(|row| row.total_value).sum();
    let missing_total: u32 = dashboard.needs.iter().map(|row| row.missing).sum();
    let need_item_types = dashboard.needs.len() as u32;
    let keep_item_types = dashboard.keep.len() as u32;
    let sell_item_types = dashboard.sell.len() as u32;
    let keep_total_qty: u32 = dashboard.keep.iter().map(|row| row.required).sum();

    let load_data_action = {
        let static_data = static_data.clone();
        let mut loading_data = loading_data.clone();
        let mut status_message = status_message.clone();
        let mut error_message = error_message.clone();
        let mut operation_progress = operation_progress.clone();
        let toasts = toasts.clone();
        move |_| {
            if *loading_data.read() {
                return;
            }

            loading_data.set(true);
            error_message.set(String::new());
            status_message.set("Loading static items/quests/hideout/projects...".to_string());
            enqueue_toast(
                toasts.clone(),
                ToastKind::Info,
                "Loading static data...".to_string(),
            );
            operation_progress.set(Some(OperationProgress {
                label: "Loading game data".to_string(),
                detail: "Fetching items, quests, hideout and projects".to_string(),
                current: 0,
                total: 1,
                indeterminate: false,
            }));

            let mut static_data = static_data.clone();
            let mut loading_data = loading_data.clone();
            let mut status_message = status_message.clone();
            let mut error_message = error_message.clone();
            let mut operation_progress = operation_progress.clone();
            let toasts = toasts.clone();

            spawn(async move {
                info!("load_data_action: loading static data");
                let client = Client::new();
                match fetch_static_data(&client).await {
                    Ok(data) => {
                        let item_count = data.items_by_id.len();
                        let source_label = if data.local_images_dir.is_some() {
                            "local arcraiders-data repo"
                        } else {
                            "ArcTracker API"
                        };
                        static_data.set(Some(Arc::new(data)));
                        status_message.set(format!(
                            "Loaded {item_count} items and requirement datasets from {source_label}."
                        ));
                        enqueue_toast(
                            toasts,
                            ToastKind::Success,
                            format!("Loaded {item_count} items from {source_label}."),
                        );
                        info!(
                            item_count,
                            source = source_label,
                            "load_data_action: static data loaded"
                        );
                    }
                    Err(err) => {
                        error!(error = %err, "load_data_action: failed to load static data");
                        let message = format!("Failed to load static data: {err}");
                        error_message.set(message.clone());
                        enqueue_toast(toasts, ToastKind::Error, message);
                    }
                }
                operation_progress.set(None);
                loading_data.set(false);
            });
        }
    };

    let scan_inventory_action = {
        let app_key = app_key.clone();
        let user_key = user_key.clone();
        let inventory_counts = inventory_counts.clone();
        let mut scanning_inventory = scanning_inventory.clone();
        let mut status_message = status_message.clone();
        let mut error_message = error_message.clone();
        let mut operation_progress = operation_progress.clone();
        let toasts = toasts.clone();
        move |_| {
            if *scanning_inventory.read() {
                return;
            }

            scanning_inventory.set(true);
            error_message.set(String::new());
            status_message.set("Scanning stash inventory...".to_string());
            enqueue_toast(
                toasts.clone(),
                ToastKind::Info,
                "Scanning stash inventory...".to_string(),
            );
            operation_progress.set(Some(OperationProgress {
                label: "Scanning inventory".to_string(),
                detail: "Fetching stash pages from ArcTracker".to_string(),
                current: 0,
                total: 1,
                indeterminate: true,
            }));

            let app_key_value = app_key.read().clone();
            let user_key_value = user_key.read().clone();

            let mut inventory_counts = inventory_counts.clone();
            let mut scanning_inventory = scanning_inventory.clone();
            let mut status_message = status_message.clone();
            let mut error_message = error_message.clone();
            let mut operation_progress = operation_progress.clone();
            let toasts = toasts.clone();

            spawn(async move {
                info!("scan_inventory_action: scanning stash");
                let client = Client::new();
                match fetch_stash_inventory(&client, &app_key_value, &user_key_value).await {
                    Ok(counts) => {
                        let unique = counts.len();
                        let total: u32 = counts.values().sum();
                        inventory_counts.set(counts);
                        status_message.set(format!(
                            "Inventory scan complete: {total} total items across {unique} unique item types."
                        ));
                        enqueue_toast(
                            toasts,
                            ToastKind::Success,
                            format!("Inventory scan complete: {unique} item types."),
                        );
                        info!(total, unique, "scan_inventory_action: stash scan complete");
                    }
                    Err(err) => {
                        error!(error = %err, "scan_inventory_action: stash scan failed");
                        let message = format!("Inventory scan failed: {err}");
                        error_message.set(message.clone());
                        enqueue_toast(toasts, ToastKind::Error, message);
                    }
                }
                operation_progress.set(None);
                scanning_inventory.set(false);
            });
        }
    };

    let auto_sync_action = {
        let app_key = app_key.clone();
        let user_key = user_key.clone();
        let static_data = static_data.clone();
        let inventory_counts = inventory_counts.clone();
        let loadout_counts = loadout_counts.clone();
        let tracked_quests = tracked_quests.clone();
        let tracked_hideout = tracked_hideout.clone();
        let tracked_projects = tracked_projects.clone();
        let profile_info = profile_info.clone();
        let requirements_data_ready = requirements_data_ready.clone();
        let requirements_data_issue = requirements_data_issue.clone();
        let mut syncing_progress = syncing_progress.clone();
        let mut status_message = status_message.clone();
        let mut error_message = error_message.clone();
        let mut operation_progress = operation_progress.clone();
        let toasts = toasts.clone();
        move |_| {
            if *syncing_progress.read() {
                return;
            }

            let data = match static_data.read().as_ref() {
                Some(data) => Arc::clone(data),
                None => {
                    error_message
                        .set("Load static game data first, then run auto-sync.".to_string());
                    return;
                }
            };

            syncing_progress.set(true);
            error_message.set(String::new());
            status_message
                .set("Syncing profile, stash, loadout, quests, hideout, projects...".to_string());
            enqueue_toast(
                toasts.clone(),
                ToastKind::Info,
                "Syncing ArcTracker progress...".to_string(),
            );
            operation_progress.set(Some(OperationProgress {
                label: "Auto-sync progress".to_string(),
                detail: "Syncing profile, loadout, quests, hideout, and projects".to_string(),
                current: 0,
                total: 1,
                indeterminate: true,
            }));

            let app_key_value = app_key.read().clone();
            let user_key_value = user_key.read().clone();

            let mut inventory_counts = inventory_counts.clone();
            let mut loadout_counts = loadout_counts.clone();
            let mut tracked_quests = tracked_quests.clone();
            let mut tracked_hideout = tracked_hideout.clone();
            let mut tracked_projects = tracked_projects.clone();
            let mut profile_info = profile_info.clone();
            let mut requirements_data_ready = requirements_data_ready.clone();
            let mut requirements_data_issue = requirements_data_issue.clone();
            let mut syncing_progress = syncing_progress.clone();
            let mut status_message = status_message.clone();
            let mut error_message = error_message.clone();
            let mut operation_progress = operation_progress.clone();
            let toasts = toasts.clone();

            spawn(async move {
                info!("auto_sync_action: syncing profile/stash/loadout/quests/hideout/projects");
                let client = Client::new();
                match sync_user_progress(
                    &client,
                    &app_key_value,
                    &user_key_value,
                    &data,
                    true,
                    None,
                )
                .await
                {
                    Ok(sync) => {
                        if let Some(stash) = sync.stash_counts {
                            inventory_counts.set(stash);
                        }
                        let mut loadout_total = loadout_counts.read().len();
                        if let Some(loadout) = sync.loadout_counts {
                            loadout_total = loadout.len();
                            loadout_counts.set(loadout);
                        }

                        let mut quest_total = tracked_quests.read().len();
                        if let Some(quests) = sync.tracked_quests {
                            quest_total = quests.len();
                            tracked_quests.set(quests);
                        }

                        let mut hideout_total = tracked_hideout.read().len();
                        if let Some(hideout) = sync.tracked_hideout {
                            hideout_total = hideout.len();
                            tracked_hideout.set(hideout);
                        }

                        let mut project_total = tracked_projects.read().len();
                        if let Some(projects) = sync.tracked_projects {
                            project_total = projects.len();
                            tracked_projects.set(projects);
                        }

                        if let Some(profile) = sync.profile {
                            profile_info.set(Some(profile));
                        }

                        let mut missing_sources = Vec::new();
                        if !sync.quests_synced {
                            missing_sources.push("quests");
                        }
                        if !sync.hideout_synced {
                            missing_sources.push("hideout");
                        }
                        if !sync.projects_synced {
                            missing_sources.push("projects");
                        }
                        if missing_sources.is_empty() {
                            requirements_data_ready.set(true);
                            requirements_data_issue.set(String::new());
                        } else {
                            requirements_data_ready.set(false);
                            requirements_data_issue.set(format!(
                                "Progress data unavailable for {}. Sell suggestions are paused unless you add manual tracking.",
                                missing_sources.join(", ")
                            ));
                        }

                        let mut summary = format!(
                            "Auto-sync complete: {} quests, {} hideout targets, {} projects, {} loadout item types.",
                            quest_total, hideout_total, project_total, loadout_total
                        );
                        if !sync.warnings.is_empty() {
                            summary.push_str(" Partial warnings: ");
                            summary.push_str(&sync.warnings.join(" | "));
                        }
                        if sync.warnings.is_empty() {
                            info!(
                                quests = quest_total,
                                hideout = hideout_total,
                                projects = project_total,
                                loadout = loadout_total,
                                "auto_sync_action: sync complete"
                            );
                        } else {
                            warn!(
                                warnings = sync.warnings.len(),
                                quests = quest_total,
                                hideout = hideout_total,
                                projects = project_total,
                                loadout = loadout_total,
                                "auto_sync_action: sync complete with warnings"
                            );
                        }
                        status_message.set(summary);
                        if sync.warnings.is_empty() {
                            enqueue_toast(
                                toasts,
                                ToastKind::Success,
                                "Auto-sync completed successfully.".to_string(),
                            );
                        } else {
                            enqueue_toast(
                                toasts,
                                ToastKind::Warning,
                                format!(
                                    "Auto-sync completed with {} warning(s).",
                                    sync.warnings.len()
                                ),
                            );
                        }
                    }
                    Err(err) => {
                        requirements_data_ready.set(false);
                        requirements_data_issue.set(
                            "Auto-sync failed. Sell suggestions are paused unless you add manual tracking."
                                .to_string(),
                        );
                        error!(error = %err, "auto_sync_action: sync failed");
                        let message = format!("Auto-sync failed: {err}");
                        error_message.set(message.clone());
                        enqueue_toast(toasts, ToastKind::Error, message);
                    }
                }
                operation_progress.set(None);
                syncing_progress.set(false);
            });
        }
    };

    let api_diagnostics_action = {
        let app_key = app_key.clone();
        let user_key = user_key.clone();
        let mut diagnostics_running = diagnostics_running.clone();
        let mut diagnostics_rows = diagnostics_rows.clone();
        let mut diagnostics_report = diagnostics_report.clone();
        let mut status_message = status_message.clone();
        let mut error_message = error_message.clone();
        let mut operation_progress = operation_progress.clone();
        let toasts = toasts.clone();
        move |_| {
            if *diagnostics_running.read() {
                return;
            }

            diagnostics_running.set(true);
            diagnostics_rows.set(Vec::new());
            diagnostics_report.set(String::new());
            error_message.set(String::new());
            status_message.set("Running API diagnostics...".to_string());
            enqueue_toast(
                toasts.clone(),
                ToastKind::Info,
                "Running API diagnostics...".to_string(),
            );
            operation_progress.set(Some(OperationProgress {
                label: "API diagnostics".to_string(),
                detail: "Checking endpoint availability".to_string(),
                current: 0,
                total: 8,
                indeterminate: false,
            }));

            let app_key_value = app_key.read().clone();
            let user_key_value = user_key.read().clone();

            let mut diagnostics_running = diagnostics_running.clone();
            let mut diagnostics_rows = diagnostics_rows.clone();
            let mut diagnostics_report = diagnostics_report.clone();
            let mut status_message = status_message.clone();
            let mut error_message = error_message.clone();
            let mut operation_progress = operation_progress.clone();
            let toasts = toasts.clone();

            spawn(async move {
                info!("api_diagnostics: starting run");
                let client = Client::new();
                let mut progress_signal = operation_progress.clone();
                match run_api_diagnostics(
                    &client,
                    &app_key_value,
                    &user_key_value,
                    |done, total, endpoint| {
                        progress_signal.set(Some(OperationProgress {
                            label: "API diagnostics".to_string(),
                            detail: format!("Checked {endpoint}"),
                            current: done as u32,
                            total: total as u32,
                            indeterminate: false,
                        }));
                    },
                )
                .await
                {
                    Ok(rows) => {
                        let failed = rows.iter().filter(|row| !row.ok).count();
                        let passed = rows.len().saturating_sub(failed);
                        diagnostics_report.set(build_api_diagnostics_report(&rows));
                        diagnostics_rows.set(rows);

                        if failed == 0 {
                            info!(passed, "api_diagnostics: all endpoints passed");
                            status_message.set(format!(
                                "API diagnostics complete: {passed} passed, 0 failed."
                            ));
                            enqueue_toast(
                                toasts,
                                ToastKind::Success,
                                format!("API diagnostics passed ({passed}/{passed})."),
                            );
                        } else {
                            warn!(
                                passed,
                                failed, "api_diagnostics: completed with endpoint failures"
                            );
                            status_message.set(format!(
                                "API diagnostics complete: {passed} passed, {failed} failed."
                            ));
                            enqueue_toast(
                                toasts,
                                ToastKind::Warning,
                                format!("API diagnostics found {failed} failing endpoint(s)."),
                            );
                        }
                    }
                    Err(err) => {
                        error!(error = %err, "api_diagnostics: failed");
                        let message = format!("API diagnostics failed: {err}");
                        error_message.set(message.clone());
                        enqueue_toast(toasts, ToastKind::Error, message);
                    }
                }
                operation_progress.set(None);
                diagnostics_running.set(false);
            });
        }
    };

    {
        let app_key = app_key.clone();
        let user_key = user_key.clone();
        let static_data = static_data.clone();
        let inventory_counts = inventory_counts.clone();
        let loadout_counts = loadout_counts.clone();
        let tracked_quests = tracked_quests.clone();
        let tracked_hideout = tracked_hideout.clone();
        let tracked_projects = tracked_projects.clone();
        let profile_info = profile_info.clone();
        let requirements_data_ready = requirements_data_ready.clone();
        let requirements_data_issue = requirements_data_issue.clone();
        let loading_data = loading_data.clone();
        let scanning_inventory = scanning_inventory.clone();
        let syncing_progress = syncing_progress.clone();
        let mut startup_sync_started = startup_sync_started.clone();
        let status_message = status_message.clone();
        let error_message = error_message.clone();
        let operation_progress = operation_progress.clone();
        let toasts = toasts.clone();
        use_effect(move || {
            if *startup_sync_started.read() {
                return;
            }

            let app_key_value = app_key.read().trim().to_string();
            let user_key_value = user_key.read().trim().to_string();
            if app_key_value.is_empty() || user_key_value.is_empty() {
                return;
            }

            startup_sync_started.set(true);

            let mut static_data = static_data.clone();
            let mut inventory_counts = inventory_counts.clone();
            let mut loadout_counts = loadout_counts.clone();
            let mut tracked_quests = tracked_quests.clone();
            let mut tracked_hideout = tracked_hideout.clone();
            let mut tracked_projects = tracked_projects.clone();
            let mut profile_info = profile_info.clone();
            let mut requirements_data_ready = requirements_data_ready.clone();
            let mut requirements_data_issue = requirements_data_issue.clone();
            let mut loading_data = loading_data.clone();
            let mut scanning_inventory = scanning_inventory.clone();
            let mut syncing_progress = syncing_progress.clone();
            let mut status_message = status_message.clone();
            let mut error_message = error_message.clone();
            let mut operation_progress = operation_progress.clone();
            let toasts = toasts.clone();

            spawn(async move {
                info!("startup_sync: starting automatic startup load/scan/sync");
                let client = Client::new();

                loading_data.set(true);
                scanning_inventory.set(true);
                syncing_progress.set(true);
                error_message.set(String::new());
                status_message.set("Startup sync: loading static data...".to_string());
                operation_progress.set(Some(OperationProgress {
                    label: "Startup sync".to_string(),
                    detail: "Loading static game data".to_string(),
                    current: 0,
                    total: 3,
                    indeterminate: false,
                }));

                let data = match fetch_static_data(&client).await {
                    Ok(data) => data,
                    Err(err) => {
                        error!(error = %err, "startup_sync: failed while loading static data");
                        let message = format!("Startup sync failed loading static data: {err}");
                        error_message.set(message.clone());
                        enqueue_toast(toasts, ToastKind::Error, message);
                        operation_progress.set(None);
                        loading_data.set(false);
                        scanning_inventory.set(false);
                        syncing_progress.set(false);
                        return;
                    }
                };

                let data_arc = Arc::new(data);
                static_data.set(Some(Arc::clone(&data_arc)));
                operation_progress.set(Some(OperationProgress {
                    label: "Startup sync".to_string(),
                    detail: "Scanning stash inventory".to_string(),
                    current: 1,
                    total: 3,
                    indeterminate: false,
                }));

                status_message.set("Startup sync: scanning stash inventory...".to_string());
                let mut startup_warnings = Vec::new();
                match fetch_stash_inventory_with_cache(
                    &client,
                    &app_key_value,
                    &user_key_value,
                    Some(startup_user_cache_ttl()),
                )
                .await
                {
                    Ok(counts) => inventory_counts.set(counts),
                    Err(err) => {
                        warn!(error = %err, "startup_sync: stash scan warning");
                        startup_warnings.push(format!("startup/stash: {err}"));
                    }
                }

                status_message.set("Startup sync: syncing profile and progress...".to_string());
                operation_progress.set(Some(OperationProgress {
                    label: "Startup sync".to_string(),
                    detail: "Syncing profile and progression".to_string(),
                    current: 2,
                    total: 3,
                    indeterminate: false,
                }));
                match sync_user_progress(
                    &client,
                    &app_key_value,
                    &user_key_value,
                    data_arc.as_ref(),
                    false,
                    Some(startup_user_cache_ttl()),
                )
                .await
                {
                    Ok(sync) => {
                        if let Some(stash) = sync.stash_counts {
                            inventory_counts.set(stash);
                        }

                        let mut loadout_total = loadout_counts.read().len();
                        if let Some(loadout) = sync.loadout_counts {
                            loadout_total = loadout.len();
                            loadout_counts.set(loadout);
                        }

                        let mut quest_total = tracked_quests.read().len();
                        if let Some(quests) = sync.tracked_quests {
                            quest_total = quests.len();
                            tracked_quests.set(quests);
                        }

                        let mut hideout_total = tracked_hideout.read().len();
                        if let Some(hideout) = sync.tracked_hideout {
                            hideout_total = hideout.len();
                            tracked_hideout.set(hideout);
                        }

                        let mut project_total = tracked_projects.read().len();
                        if let Some(projects) = sync.tracked_projects {
                            project_total = projects.len();
                            tracked_projects.set(projects);
                        }

                        if let Some(profile) = sync.profile {
                            profile_info.set(Some(profile));
                        }

                        let mut missing_sources = Vec::new();
                        if !sync.quests_synced {
                            missing_sources.push("quests");
                        }
                        if !sync.hideout_synced {
                            missing_sources.push("hideout");
                        }
                        if !sync.projects_synced {
                            missing_sources.push("projects");
                        }
                        if missing_sources.is_empty() {
                            requirements_data_ready.set(true);
                            requirements_data_issue.set(String::new());
                        } else {
                            requirements_data_ready.set(false);
                            requirements_data_issue.set(format!(
                                "Progress data unavailable for {}. Sell suggestions are paused unless you add manual tracking.",
                                missing_sources.join(", ")
                            ));
                        }

                        startup_warnings.extend(sync.warnings);
                        let mut summary = format!(
                            "Startup sync complete: {} quests, {} hideout targets, {} projects, {} loadout item types.",
                            quest_total, hideout_total, project_total, loadout_total
                        );
                        if !startup_warnings.is_empty() {
                            summary.push_str(" Partial warnings: ");
                            summary.push_str(&startup_warnings.join(" | "));
                            warn!(
                                warnings = startup_warnings.len(),
                                "startup_sync: completed with warnings"
                            );
                            enqueue_toast(
                                toasts,
                                ToastKind::Warning,
                                format!(
                                    "Startup sync completed with {} warning(s).",
                                    startup_warnings.len()
                                ),
                            );
                        } else {
                            info!(
                                quests = quest_total,
                                hideout = hideout_total,
                                projects = project_total,
                                loadout = loadout_total,
                                "startup_sync: completed"
                            );
                            enqueue_toast(
                                toasts,
                                ToastKind::Success,
                                "Startup sync completed.".to_string(),
                            );
                        }
                        status_message.set(summary);
                    }
                    Err(err) => {
                        requirements_data_ready.set(false);
                        requirements_data_issue.set(
                            "Startup sync failed. Sell suggestions are paused unless you add manual tracking."
                                .to_string(),
                        );
                        error!(error = %err, "startup_sync: sync failed");
                        let message = format!("Startup sync failed: {err}");
                        error_message.set(message.clone());
                        enqueue_toast(toasts, ToastKind::Error, message);
                    }
                }

                operation_progress.set(None);
                loading_data.set(false);
                scanning_inventory.set(false);
                syncing_progress.set(false);
            });
        });
    }

    {
        let tracked_crafts = tracked_crafts.clone();
        let tracked_quests = tracked_quests.clone();
        let tracked_hideout = tracked_hideout.clone();
        let tracked_projects = tracked_projects.clone();
        let runtime_settings = runtime_settings.clone();
        let theme_preference = theme_preference.clone();
        let show_planning_workspace = show_planning_workspace.clone();
        use_effect(move || {
            let settings_snapshot = runtime_settings.read().clone();
            replace_runtime_settings(settings_snapshot.clone());
            let snapshot = PersistedTrackedState {
                tracked_crafts: tracked_crafts.read().clone(),
                tracked_quests: tracked_quests.read().clone(),
                tracked_hideout: tracked_hideout.read().clone(),
                tracked_projects: tracked_projects.read().clone(),
                settings: settings_snapshot,
                theme_preference: normalize_theme_preference(theme_preference.read().as_str()),
                show_planning_workspace: *show_planning_workspace.read(),
                saved_at_unix: now_unix_seconds(),
            };
            if let Err(err) = save_tracked_state(&snapshot) {
                warn!(error = %err, "state_persist: failed to save tracked state");
            }
        });
    }

    let app_key_masked = mask_key(&app_key.read());
    let data_loaded = data_snapshot.is_some();

    rsx! {
        style { "{APP_CSS}" }

        div { class: "app-shell", "data-theme": "{theme_data_attr}",
            a { class: "skip-link", href: "#dashboard-panel", "Skip to dashboard" }
            header { class: "header",
                div {
                    h1 { "ARC Cleaner Desktop" }
                    p { "Rust + Dioxus desktop tracker powered by ArcTracker.io" }
                }
                div { class: "header-controls",
                    p { class: "muted", "Dashboard-first workflow with sync + planning controls in Settings." }
                }
            }

            main { class: "main-content",
                ApiPanel {
                    app_key_masked: app_key_masked.clone(),
                    user_key: user_key.read().clone(),
                    on_user_key_input: move |evt: FormEvent| user_key.set(evt.value()),
                    api_min_interval_ms: settings_snapshot.api_min_interval_ms.to_string(),
                    on_api_min_interval_input: {
                        let mut runtime_settings = runtime_settings.clone();
                        move |evt: FormEvent| {
                            if let Ok(value) = evt.value().parse::<u64>() {
                                let mut next = runtime_settings.read().clone();
                                next.api_min_interval_ms = value;
                                runtime_settings.set(next);
                            }
                        }
                    },
                    api_max_retries: settings_snapshot.api_max_retries.to_string(),
                    on_api_max_retries_input: {
                        let mut runtime_settings = runtime_settings.clone();
                        move |evt: FormEvent| {
                            if let Ok(value) = evt.value().parse::<usize>() {
                                let mut next = runtime_settings.read().clone();
                                next.api_max_retries = value;
                                runtime_settings.set(next);
                            }
                        }
                    },
                    api_retry_base_ms: settings_snapshot.api_retry_base_ms.to_string(),
                    on_api_retry_base_ms_input: {
                        let mut runtime_settings = runtime_settings.clone();
                        move |evt: FormEvent| {
                            if let Ok(value) = evt.value().parse::<u64>() {
                                let mut next = runtime_settings.read().clone();
                                next.api_retry_base_ms = value;
                                if next.api_retry_max_ms < value {
                                    next.api_retry_max_ms = value;
                                }
                                runtime_settings.set(next);
                            }
                        }
                    },
                    api_retry_max_ms: settings_snapshot.api_retry_max_ms.to_string(),
                    on_api_retry_max_ms_input: {
                        let mut runtime_settings = runtime_settings.clone();
                        move |evt: FormEvent| {
                            if let Ok(value) = evt.value().parse::<u64>() {
                                let mut next = runtime_settings.read().clone();
                                next.api_retry_max_ms = value.max(next.api_retry_base_ms);
                                runtime_settings.set(next);
                            }
                        }
                    },
                    static_cache_ttl_seconds: settings_snapshot.static_cache_ttl_seconds.to_string(),
                    on_static_cache_ttl_input: {
                        let mut runtime_settings = runtime_settings.clone();
                        move |evt: FormEvent| {
                            if let Ok(value) = evt.value().parse::<u64>() {
                                let mut next = runtime_settings.read().clone();
                                next.static_cache_ttl_seconds = value;
                                runtime_settings.set(next);
                            }
                        }
                    },
                    startup_user_cache_ttl_seconds: settings_snapshot
                        .startup_user_cache_ttl_seconds
                        .to_string(),
                    on_startup_user_cache_ttl_input: {
                        let mut runtime_settings = runtime_settings.clone();
                        move |evt: FormEvent| {
                            if let Ok(value) = evt.value().parse::<u64>() {
                                let mut next = runtime_settings.read().clone();
                                next.startup_user_cache_ttl_seconds = value;
                                runtime_settings.set(next);
                            }
                        }
                    },
                    image_prefetch_count: settings_snapshot.image_prefetch_count.to_string(),
                    on_image_prefetch_count_input: {
                        let mut runtime_settings = runtime_settings.clone();
                        move |evt: FormEvent| {
                            if let Ok(value) = evt.value().parse::<usize>() {
                                let mut next = runtime_settings.read().clone();
                                next.image_prefetch_count = value;
                                runtime_settings.set(next);
                            }
                        }
                    },
                    sell_exclude_weapons: settings_snapshot.sell_exclude_weapons,
                    on_sell_exclude_weapons_toggle: {
                        let mut runtime_settings = runtime_settings.clone();
                        move |evt: FormEvent| {
                            let checked = matches!(
                                evt.value().to_ascii_lowercase().as_str(),
                                "true" | "on" | "1" | "yes"
                            );
                            let mut next = runtime_settings.read().clone();
                            next.sell_exclude_weapons = checked;
                            runtime_settings.set(next);
                        }
                    },
                    sell_exclude_types: {
                        let mut values: Vec<_> =
                            settings_snapshot.sell_exclude_types.iter().cloned().collect();
                        values.sort();
                        values.join(", ")
                    },
                    on_sell_exclude_types_input: {
                        let mut runtime_settings = runtime_settings.clone();
                        move |evt: FormEvent| {
                            let mut next = runtime_settings.read().clone();
                            next.sell_exclude_types = parse_csv_lower_set(&evt.value());
                            runtime_settings.set(next);
                        }
                    },
                    on_reset_settings: {
                        let mut runtime_settings = runtime_settings.clone();
                        move |_| runtime_settings.set(AppRuntimeSettings::from_env())
                    },
                    clearing_cache: *cache_action_running.read(),
                    on_clear_user_cache: {
                        let mut cache_action_running = cache_action_running.clone();
                        let status_message = status_message.clone();
                        let error_message = error_message.clone();
                        let toasts = toasts.clone();
                        move |_| {
                            if *cache_action_running.read() {
                                return;
                            }
                            cache_action_running.set(true);
                            let mut cache_action_running = cache_action_running.clone();
                            let mut status_message = status_message.clone();
                            let mut error_message = error_message.clone();
                            let toasts = toasts.clone();
                            spawn(async move {
                                match clear_cache_namespace(CACHE_NAMESPACE_USER) {
                                    Ok(()) => {
                                        status_message.set("Cleared user API cache.".to_string());
                                        enqueue_toast(
                                            toasts,
                                            ToastKind::Success,
                                            "Cleared user API cache.".to_string(),
                                        );
                                    }
                                    Err(err) => {
                                        let message = format!("Failed to clear user cache: {err}");
                                        error_message.set(message.clone());
                                        enqueue_toast(toasts, ToastKind::Error, message);
                                    }
                                }
                                cache_action_running.set(false);
                            });
                        }
                    },
                    on_clear_image_cache: {
                        let mut cache_action_running = cache_action_running.clone();
                        let status_message = status_message.clone();
                        let error_message = error_message.clone();
                        let toasts = toasts.clone();
                        move |_| {
                            if *cache_action_running.read() {
                                return;
                            }
                            cache_action_running.set(true);
                            let mut cache_action_running = cache_action_running.clone();
                            let mut status_message = status_message.clone();
                            let mut error_message = error_message.clone();
                            let toasts = toasts.clone();
                            spawn(async move {
                                match clear_cache_namespace(CACHE_NAMESPACE_IMAGES) {
                                    Ok(()) => {
                                        status_message.set("Cleared image cache.".to_string());
                                        enqueue_toast(
                                            toasts,
                                            ToastKind::Success,
                                            "Cleared image cache.".to_string(),
                                        );
                                    }
                                    Err(err) => {
                                        let message = format!("Failed to clear image cache: {err}");
                                        error_message.set(message.clone());
                                        enqueue_toast(toasts, ToastKind::Error, message);
                                    }
                                }
                                cache_action_running.set(false);
                            });
                        }
                    },
                    on_clear_all_cache: {
                        let mut cache_action_running = cache_action_running.clone();
                        let status_message = status_message.clone();
                        let error_message = error_message.clone();
                        let toasts = toasts.clone();
                        move |_| {
                            if *cache_action_running.read() {
                                return;
                            }
                            cache_action_running.set(true);
                            let mut cache_action_running = cache_action_running.clone();
                            let mut status_message = status_message.clone();
                            let mut error_message = error_message.clone();
                            let toasts = toasts.clone();
                            spawn(async move {
                                match clear_all_cache() {
                                    Ok(()) => {
                                        status_message.set("Cleared all cache files.".to_string());
                                        enqueue_toast(
                                            toasts,
                                            ToastKind::Success,
                                            "Cleared all cache files.".to_string(),
                                        );
                                    }
                                    Err(err) => {
                                        let message = format!("Failed to clear all cache: {err}");
                                        error_message.set(message.clone());
                                        enqueue_toast(toasts, ToastKind::Error, message);
                                    }
                                }
                                cache_action_running.set(false);
                            });
                        }
                    },
                    theme_button_text: theme_button_text.clone(),
                    on_theme_toggle: {
                        let mut theme_preference = theme_preference;
                        move |_| {
                            let next = next_theme_preference(theme_preference.read().as_str());
                            theme_preference.set(next);
                        }
                    },
                    planning_button_text: if *show_planning_workspace.read() {
                        "Hide Planning Workspace".to_string()
                    } else {
                        "Show Planning Workspace".to_string()
                    },
                    on_planning_toggle: {
                        let mut show_planning_workspace = show_planning_workspace;
                        move |_| {
                            let current = *show_planning_workspace.read();
                            show_planning_workspace.set(!current);
                        }
                    },
                    loading_data: *loading_data.read(),
                    scanning_inventory: *scanning_inventory.read(),
                    syncing_progress: *syncing_progress.read(),
                    diagnostics_running: *diagnostics_running.read(),
                    on_load_data: load_data_action,
                    on_scan_inventory: scan_inventory_action,
                    on_auto_sync: auto_sync_action,
                    on_run_diagnostics: api_diagnostics_action,
                    profile: profile_info.read().clone(),
                    status_message: status_message.read().clone(),
                    error_message: error_message.read().clone(),
                    diagnostics_rows: diagnostics_rows_snapshot.clone(),
                    diagnostics_report: diagnostics_report_snapshot.clone(),
                    progress: progress_snapshot.clone(),
                }

                if data_loaded {
                    DashboardPanel {
                        suppress_sell_recommendations,
                        requirements_data_issue: requirements_data_issue.read().clone(),
                        dashboard_filter: dashboard_filter.read().clone(),
                        on_dashboard_filter_input: move |evt: FormEvent| dashboard_filter.set(evt.value()),
                        sell_total_qty,
                        sell_total_value,
                        missing_total,
                        need_item_types,
                        keep_item_types,
                        sell_item_types,
                        keep_total_qty,
                        sell_rows: sell_rows_filtered.clone(),
                        need_rows: need_rows_filtered.clone(),
                        keep_rows: keep_rows_filtered.clone(),
                    }

                    TrackingPanels {
                        show_planning_workspace: *show_planning_workspace.read(),
                        data_snapshot: data_snapshot.clone(),
                        craft_pick: craft_pick.clone(),
                        craft_qty: craft_qty.clone(),
                        tracked_crafts: tracked_crafts.clone(),
                        crafts_snapshot: crafts_snapshot.clone(),
                        quest_pick: quest_pick.clone(),
                        tracked_quests: tracked_quests.clone(),
                        quests_snapshot: quests_snapshot.clone(),
                        hideout_pick: hideout_pick.clone(),
                        hideout_level: hideout_level.clone(),
                        tracked_hideout: tracked_hideout.clone(),
                        hideout_snapshot: hideout_snapshot.clone(),
                        project_pick: project_pick.clone(),
                        project_phase: project_phase.clone(),
                        tracked_projects: tracked_projects.clone(),
                        projects_snapshot: projects_snapshot.clone(),
                        required_rows_filtered: required_rows_filtered.clone(),
                    }
                } else {
                    div { class: "panel",
                        h2 { "Next Step" }
                        p { "Load game data first, then add tracking targets and scan your inventory." }
                    }
                }
            }

            ToastViewport { toasts: toasts_snapshot.clone() }
        }
    }
}
fn is_excluded_from_sell(data: &ArcData, item_id: &str) -> bool {
    let Some(item) = data.items_by_id.get(item_id) else {
        return false;
    };

    let config = runtime_settings_snapshot();
    let normalized_type = item
        .item_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);

    if config.sell_exclude_weapons {
        if item.is_weapon {
            return true;
        }

        if let Some(item_type) = normalized_type.as_deref() {
            if is_weapon_item_type(item_type) {
                return true;
            }
        }
    }

    if let Some(item_type) = normalized_type.as_deref() {
        return config.sell_exclude_types.contains(item_type);
    }

    false
}

fn is_weapon_item_type(item_type: &str) -> bool {
    matches!(
        item_type,
        "assault rifle"
            | "pistol"
            | "shotgun"
            | "battle rifle"
            | "smg"
            | "sniper rifle"
            | "special"
            | "lmg"
            | "hand cannon"
            | "shield"
    )
}

fn item_image_src(data: &ArcData, item_id: &str) -> String {
    if let Some(item) = data.items_by_id.get(item_id) {
        if let Some(src) = item.image_filename.as_ref() {
            if !src.trim().is_empty() {
                return src.trim().to_string();
            }
        }
    }

    if let Some(images_dir) = data.local_images_dir.as_ref() {
        if let Some(uri) = local_item_image_data_uri(images_dir, item_id) {
            return uri;
        }
    }

    String::new()
}

fn enqueue_toast(mut toasts: Signal<Vec<Toast>>, kind: ToastKind, message: String) {
    let id = now_unix_millis();
    toasts.write().push(Toast { id, kind, message });

    spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        toasts.write().retain(|toast| toast.id != id);
    });
}

fn static_cache_ttl() -> Duration {
    Duration::from_secs(runtime_settings_snapshot().static_cache_ttl_seconds)
}

fn startup_user_cache_ttl() -> Duration {
    Duration::from_secs(runtime_settings_snapshot().startup_user_cache_ttl_seconds)
}

fn image_prefetch_count() -> usize {
    runtime_settings_snapshot().image_prefetch_count
}

const APP_CSS: &str = include_str!("../assets/tailwind.css");

