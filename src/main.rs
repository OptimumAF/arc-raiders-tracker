use std::{
    collections::{HashMap, HashSet},
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use dioxus::desktop::{Config as DesktopConfig, WindowBuilder, tao::window::Icon};
use dioxus::prelude::*;
use reqwest::{Client, StatusCode, header::HeaderMap};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

const API_BASE: &str = "https://arctracker.io";
const LOCAL_DATA_DEFAULT_DIR: &str = "vendor/arcraiders-data";
const DEFAULT_API_MIN_INTERVAL_MS: u64 = 1000;
const DEFAULT_API_MAX_RETRIES: usize = 2;
const DEFAULT_API_RETRY_BASE_MS: u64 = 1500;
const DEFAULT_API_RETRY_MAX_MS: u64 = 10000;
const DEFAULT_STATIC_CACHE_TTL_SECONDS: u64 = 60 * 60 * 24;
const DEFAULT_STARTUP_USER_CACHE_TTL_SECONDS: u64 = 60 * 5;
const DEFAULT_IMAGE_PREFETCH_COUNT: usize = 12;
const CACHE_NAMESPACE_STATIC: &str = "static_api";
const CACHE_NAMESPACE_USER: &str = "user_api";
const CACHE_NAMESPACE_IMAGES: &str = "images";
const CACHE_FILE_TRACKED_STATE: &str = "tracked_state.json";
static API_REQUEST_THROTTLE: OnceLock<ApiRequestThrottle> = OnceLock::new();
static API_RETRY_CONFIG: OnceLock<ApiRetryConfig> = OnceLock::new();

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

#[derive(Debug)]
struct ApiRequestThrottle {
    min_interval: Duration,
    next_allowed: tokio::sync::Mutex<Option<Instant>>,
}

impl ApiRequestThrottle {
    fn from_env() -> Self {
        let configured_ms = first_non_empty_env(&["ARC_API_MIN_INTERVAL_MS"])
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(DEFAULT_API_MIN_INTERVAL_MS);
        let min_interval = Duration::from_millis(configured_ms);
        info!(
            min_interval_ms = configured_ms,
            "api_throttle: configured global API minimum interval"
        );
        Self {
            min_interval,
            next_allowed: tokio::sync::Mutex::new(None),
        }
    }

    async fn wait_turn(&self, endpoint_hint: Option<&str>) {
        if self.min_interval.is_zero() {
            return;
        }

        let now = Instant::now();
        let delay = {
            let mut next_allowed = self.next_allowed.lock().await;
            let scheduled = next_allowed.filter(|next| *next > now).unwrap_or(now);
            *next_allowed = Some(scheduled + self.min_interval);
            scheduled.saturating_duration_since(now)
        };

        if !delay.is_zero() {
            debug!(
                wait_ms = delay.as_millis() as u64,
                endpoint = endpoint_hint.unwrap_or("unknown"),
                "api_throttle: delaying request"
            );
            tokio::time::sleep(delay).await;
        }
    }
}

fn api_request_throttle() -> &'static ApiRequestThrottle {
    API_REQUEST_THROTTLE.get_or_init(ApiRequestThrottle::from_env)
}

#[derive(Debug)]
struct ApiRetryConfig {
    max_retries: usize,
    base_delay: Duration,
    max_delay: Duration,
}

impl ApiRetryConfig {
    fn from_env() -> Self {
        let max_retries = first_non_empty_env(&["ARC_API_MAX_RETRIES"])
            .and_then(|raw| raw.parse::<usize>().ok())
            .unwrap_or(DEFAULT_API_MAX_RETRIES);
        let base_ms = first_non_empty_env(&["ARC_API_RETRY_BASE_MS"])
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(DEFAULT_API_RETRY_BASE_MS);
        let max_ms = first_non_empty_env(&["ARC_API_RETRY_MAX_MS"])
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(DEFAULT_API_RETRY_MAX_MS)
            .max(base_ms);

        info!(
            max_retries,
            retry_base_ms = base_ms,
            retry_max_ms = max_ms,
            "api_retry: configured retry policy"
        );

        Self {
            max_retries,
            base_delay: Duration::from_millis(base_ms),
            max_delay: Duration::from_millis(max_ms),
        }
    }

    fn delay_for_attempt(&self, attempt: usize) -> Duration {
        if self.base_delay.is_zero() {
            return Duration::from_millis(0);
        }
        let exp = attempt.min(10) as u32;
        let multiplier = 1u64 << exp;
        let base_ms = self.base_delay.as_millis() as u64;
        let max_ms = self.max_delay.as_millis() as u64;
        Duration::from_millis(base_ms.saturating_mul(multiplier).min(max_ms))
    }
}

fn api_retry_config() -> &'static ApiRetryConfig {
    API_RETRY_CONFIG.get_or_init(ApiRetryConfig::from_env)
}

#[derive(Debug, Clone, Default)]
struct ArcData {
    items_by_id: HashMap<String, Item>,
    craftable_items: Vec<Item>,
    quests: Vec<Quest>,
    hideout_modules: Vec<HideoutModule>,
    projects: Vec<Project>,
    local_images_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    id: String,
    #[serde(default)]
    name: HashMap<String, String>,
    #[serde(default)]
    recipe: Option<HashMap<String, u32>>,
    #[serde(default)]
    craft_quantity: Option<u32>,
    #[serde(default)]
    image_filename: Option<String>,
    #[serde(default)]
    value: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemRequirement {
    item_id: String,
    quantity: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Quest {
    id: String,
    #[serde(default)]
    name: HashMap<String, String>,
    #[serde(default)]
    required_item_ids: Vec<ItemRequirement>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HideoutLevel {
    level: u32,
    #[serde(default)]
    requirement_item_ids: Vec<ItemRequirement>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HideoutModule {
    id: String,
    #[serde(default)]
    name: HashMap<String, String>,
    #[serde(default)]
    max_level: u32,
    #[serde(default)]
    levels: Vec<HideoutLevel>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectPhase {
    phase: u32,
    #[serde(default)]
    requirement_item_ids: Vec<ItemRequirement>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    #[serde(default)]
    name: HashMap<String, String>,
    #[serde(default)]
    phases: Vec<ProjectPhase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemsResponse {
    items: Vec<Item>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuestsResponse {
    quests: HashMap<String, Quest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HideoutResponse {
    hideout_modules: HashMap<String, HideoutModule>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectsResponse {
    projects: HashMap<String, Project>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct TrackedCraft {
    item_id: String,
    quantity: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct TrackedHideout {
    module_id: String,
    target_level: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct TrackedProject {
    project_id: String,
    #[serde(default = "default_start_phase")]
    start_phase: u32,
    target_phase: u32,
}

#[derive(Debug, Clone, Default)]
struct NeedRow {
    name: String,
    image_src: String,
    required: u32,
    have: u32,
    missing: u32,
}

#[derive(Debug, Clone, Default)]
struct SellRow {
    name: String,
    image_src: String,
    quantity: u32,
    total_value: u64,
}

#[derive(Debug, Clone, Default)]
struct Dashboard {
    needs: Vec<NeedRow>,
    keep: Vec<NeedRow>,
    sell: Vec<SellRow>,
}

#[derive(Debug, Clone, Default)]
struct UserProfileInfo {
    username: String,
    level: Option<u32>,
    member_since: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct UserSyncResult {
    profile: Option<UserProfileInfo>,
    stash_counts: Option<HashMap<String, u32>>,
    loadout_counts: Option<HashMap<String, u32>>,
    tracked_quests: Option<Vec<String>>,
    tracked_hideout: Option<Vec<TrackedHideout>>,
    tracked_projects: Option<Vec<TrackedProject>>,
    quests_synced: bool,
    hideout_synced: bool,
    projects_synced: bool,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct ApiDiagnosticRow {
    endpoint: String,
    status_code: String,
    request_id: Option<String>,
    detail: String,
    ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedTrackedState {
    tracked_crafts: Vec<TrackedCraft>,
    tracked_quests: Vec<String>,
    tracked_hideout: Vec<TrackedHideout>,
    tracked_projects: Vec<TrackedProject>,
    saved_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEnvelope {
    saved_at_unix: u64,
    value: Value,
}

#[component]
fn App() -> Element {
    let persisted_state = load_tracked_state().unwrap_or_default();
    let initial_tracked_crafts = persisted_state.tracked_crafts.clone();
    let initial_tracked_quests = persisted_state.tracked_quests.clone();
    let initial_tracked_hideout = persisted_state.tracked_hideout.clone();
    let initial_tracked_projects = persisted_state.tracked_projects.clone();

    let default_app_key = first_non_empty_env(&["key", "ARC_APP_KEY"]).unwrap_or_default();
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

    let mut craft_pick = use_signal(String::new);
    let mut craft_qty = use_signal(|| "1".to_string());
    let mut quest_pick = use_signal(String::new);
    let mut hideout_pick = use_signal(String::new);
    let mut hideout_level = use_signal(|| "1".to_string());
    let mut project_pick = use_signal(String::new);
    let mut project_phase = use_signal(|| "1".to_string());

    let loading_data = use_signal(|| false);
    let scanning_inventory = use_signal(|| false);
    let syncing_progress = use_signal(|| false);
    let startup_sync_started = use_signal(|| false);
    let requirements_data_ready = use_signal(|| false);
    let requirements_data_issue = use_signal(String::new);
    let diagnostics_running = use_signal(|| false);
    let diagnostics_rows = use_signal(Vec::<ApiDiagnosticRow>::new);
    let diagnostics_report = use_signal(String::new);
    let status_message = use_signal(String::new);
    let error_message = use_signal(String::new);

    let data_snapshot = static_data.read().clone();
    let inventory_snapshot = inventory_counts.read().clone();
    let loadout_snapshot = loadout_counts.read().clone();
    let crafts_snapshot = tracked_crafts.read().clone();
    let quests_snapshot = tracked_quests.read().clone();
    let hideout_snapshot = tracked_hideout.read().clone();
    let projects_snapshot = tracked_projects.read().clone();
    let diagnostics_rows_snapshot = diagnostics_rows.read().clone();
    let diagnostics_report_snapshot = diagnostics_report.read().clone();

    let required_items = if let Some(data) = data_snapshot.as_ref() {
        aggregate_requirements(
            data,
            &crafts_snapshot,
            &quests_snapshot,
            &hideout_snapshot,
            &projects_snapshot,
        )
    } else {
        HashMap::new()
    };

    let has_manual_goals = !crafts_snapshot.is_empty()
        || !quests_snapshot.is_empty()
        || !hideout_snapshot.is_empty()
        || !projects_snapshot.is_empty();
    let suppress_sell_recommendations = !*requirements_data_ready.read() && !has_manual_goals;

    let dashboard = if let Some(data) = data_snapshot.as_ref() {
        build_dashboard(
            data,
            &required_items,
            &inventory_snapshot,
            &loadout_snapshot,
            !suppress_sell_recommendations,
        )
    } else {
        Dashboard::default()
    };

    let mut required_rows: Vec<NeedRow> = if let Some(data) = data_snapshot.as_ref() {
        required_items
            .iter()
            .map(|(id, required)| {
                let have = *inventory_snapshot.get(id).unwrap_or(&0);
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
    required_rows.sort_by(|a, b| b.missing.cmp(&a.missing).then(a.name.cmp(&b.name)));

    let load_data_action = {
        let static_data = static_data.clone();
        let mut loading_data = loading_data.clone();
        let mut status_message = status_message.clone();
        let mut error_message = error_message.clone();
        move |_| {
            if *loading_data.read() {
                return;
            }

            loading_data.set(true);
            error_message.set(String::new());
            status_message.set("Loading static items/quests/hideout/projects...".to_string());

            let mut static_data = static_data.clone();
            let mut loading_data = loading_data.clone();
            let mut status_message = status_message.clone();
            let mut error_message = error_message.clone();

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
                        info!(
                            item_count,
                            source = source_label,
                            "load_data_action: static data loaded"
                        );
                    }
                    Err(err) => {
                        error!(error = %err, "load_data_action: failed to load static data");
                        error_message.set(format!("Failed to load static data: {err}"));
                    }
                }
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
        move |_| {
            if *scanning_inventory.read() {
                return;
            }

            scanning_inventory.set(true);
            error_message.set(String::new());
            status_message.set("Scanning stash inventory...".to_string());

            let app_key_value = app_key.read().clone();
            let user_key_value = user_key.read().clone();

            let mut inventory_counts = inventory_counts.clone();
            let mut scanning_inventory = scanning_inventory.clone();
            let mut status_message = status_message.clone();
            let mut error_message = error_message.clone();

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
                        info!(total, unique, "scan_inventory_action: stash scan complete");
                    }
                    Err(err) => {
                        error!(error = %err, "scan_inventory_action: stash scan failed");
                        error_message.set(format!("Inventory scan failed: {err}"));
                    }
                }
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
                    }
                    Err(err) => {
                        requirements_data_ready.set(false);
                        requirements_data_issue.set(
                            "Auto-sync failed. Sell suggestions are paused unless you add manual tracking."
                                .to_string(),
                        );
                        error!(error = %err, "auto_sync_action: sync failed");
                        error_message.set(format!("Auto-sync failed: {err}"));
                    }
                }
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
        move |_| {
            if *diagnostics_running.read() {
                return;
            }

            diagnostics_running.set(true);
            diagnostics_rows.set(Vec::new());
            diagnostics_report.set(String::new());
            error_message.set(String::new());
            status_message.set("Running API diagnostics...".to_string());

            let app_key_value = app_key.read().clone();
            let user_key_value = user_key.read().clone();

            let mut diagnostics_running = diagnostics_running.clone();
            let mut diagnostics_rows = diagnostics_rows.clone();
            let mut diagnostics_report = diagnostics_report.clone();
            let mut status_message = status_message.clone();
            let mut error_message = error_message.clone();

            spawn(async move {
                info!("api_diagnostics: starting run");
                let client = Client::new();
                match run_api_diagnostics(&client, &app_key_value, &user_key_value).await {
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
                        } else {
                            warn!(
                                passed,
                                failed, "api_diagnostics: completed with endpoint failures"
                            );
                            status_message.set(format!(
                                "API diagnostics complete: {passed} passed, {failed} failed."
                            ));
                        }
                    }
                    Err(err) => {
                        error!(error = %err, "api_diagnostics: failed");
                        error_message.set(format!("API diagnostics failed: {err}"));
                    }
                }
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

            spawn(async move {
                info!("startup_sync: starting automatic startup load/scan/sync");
                let client = Client::new();

                loading_data.set(true);
                scanning_inventory.set(true);
                syncing_progress.set(true);
                error_message.set(String::new());
                status_message.set("Startup sync: loading static data...".to_string());

                let data = match fetch_static_data(&client).await {
                    Ok(data) => data,
                    Err(err) => {
                        error!(error = %err, "startup_sync: failed while loading static data");
                        error_message
                            .set(format!("Startup sync failed loading static data: {err}"));
                        loading_data.set(false);
                        scanning_inventory.set(false);
                        syncing_progress.set(false);
                        return;
                    }
                };

                let data_arc = Arc::new(data);
                static_data.set(Some(Arc::clone(&data_arc)));

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
                        } else {
                            info!(
                                quests = quest_total,
                                hideout = hideout_total,
                                projects = project_total,
                                loadout = loadout_total,
                                "startup_sync: completed"
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
                        error_message.set(format!("Startup sync failed: {err}"));
                    }
                }

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
        use_effect(move || {
            let snapshot = PersistedTrackedState {
                tracked_crafts: tracked_crafts.read().clone(),
                tracked_quests: tracked_quests.read().clone(),
                tracked_hideout: tracked_hideout.read().clone(),
                tracked_projects: tracked_projects.read().clone(),
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

        div { class: "app-shell",
            div { class: "header",
                h1 { "ARC Cleaner Desktop" }
                p { "Rust + Dioxus desktop tracker powered by ArcTracker.io" }
            }

            div { class: "panel",
                h2 { "API" }
                p { class: "muted", "App key from .env: {app_key_masked}" }
                label { "User key (arc_u1_...):" }
                input {
                    value: "{user_key.read()}",
                    placeholder: "arc_u1_your_user_key",
                    oninput: move |evt| user_key.set(evt.value()),
                }
                div { class: "actions",
                    button {
                        disabled: *loading_data.read(),
                        onclick: load_data_action,
                        if *loading_data.read() { "Loading Data..." } else { "Load / Refresh Game Data" }
                    }
                    button {
                        disabled: *scanning_inventory.read(),
                        onclick: scan_inventory_action,
                        if *scanning_inventory.read() { "Scanning..." } else { "Scan Inventory" }
                    }
                    button {
                        disabled: *syncing_progress.read(),
                        onclick: auto_sync_action,
                        if *syncing_progress.read() { "Syncing..." } else { "Auto Sync Progress" }
                    }
                    button {
                        disabled: *diagnostics_running.read(),
                        onclick: api_diagnostics_action,
                        if *diagnostics_running.read() { "Running Diagnostics..." } else { "Run API Diagnostics" }
                    }
                }
                if let Some(profile) = profile_info.read().as_ref() {
                    p { class: "muted",
                        if let Some(level) = profile.level {
                            "User: {profile.username} (Level {level})"
                        } else {
                            "User: {profile.username}"
                        }
                        if let Some(member_since) = profile.member_since.as_ref() {
                            " • Member since: {member_since}"
                        }
                    }
                }
                if !status_message.read().is_empty() {
                    p { class: "status", "{status_message.read()}" }
                }
                if !error_message.read().is_empty() {
                    p { class: "error", "{error_message.read()}" }
                }
                if !diagnostics_rows_snapshot.is_empty() {
                    h3 { "API Diagnostics Report" }
                    table { class: "table compact diagnostics-table",
                        thead {
                            tr {
                                th { "Endpoint" }
                                th { "Status" }
                                th { "Request ID" }
                                th { "Details" }
                            }
                        }
                        tbody {
                            for row in diagnostics_rows_snapshot.iter() {
                                tr {
                                    td { "{row.endpoint}" }
                                    td { if row.ok { "OK (200)" } else { "{row.status_code}" } }
                                    td { "{row.request_id.clone().unwrap_or_else(|| \"-\".to_string())}" }
                                    td { "{row.detail}" }
                                }
                            }
                        }
                    }
                }
                if !diagnostics_report_snapshot.is_empty() {
                    p { class: "muted", "Copy and share this report with ArcTracker support if needed:" }
                    pre { class: "diagnostics-report", "{diagnostics_report_snapshot}" }
                }
            }

            if data_loaded {
                div { class: "panel dashboard-panel",
                    h2 { "Dashboard" }
                    p { class: "muted", "Compares scanned inventory against all tracked requirements. Prioritizing what you can sell first." }

                    div { class: "dashboard-priority",
                        div { class: "dashboard-card can-sell-card",
                            h3 { "Can Sell" }
                            if suppress_sell_recommendations {
                                p {
                                    class: "muted",
                                    if requirements_data_issue.read().is_empty() {
                                        "Sell suggestions are paused because full progress data is not available."
                                    } else {
                                        "{requirements_data_issue.read()}"
                                    }
                                }
                            }
                            table { class: "table compact",
                                thead { tr { th { "Item" } th { "Qty" } th { "Value" } } }
                                tbody {
                                    if suppress_sell_recommendations {
                                        tr { td { colspan: "3", class: "muted", "Paused to avoid inaccurate sell recommendations." } }
                                    } else if dashboard.sell.is_empty() {
                                        tr { td { colspan: "3", class: "muted", "No excess items to suggest selling." } }
                                    }
                                    for row in dashboard.sell.iter().take(40) {
                                        tr {
                                            td {
                                                div { class: "item-cell",
                                                    if !row.image_src.is_empty() {
                                                        img { class: "item-icon", src: "{row.image_src}", alt: "{row.name}" }
                                                    }
                                                    span { "{row.name}" }
                                                }
                                            }
                                            td { "{row.quantity}" }
                                            td { "{row.total_value}" }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    div { class: "dashboard-secondary",
                        div { class: "dashboard-card",
                            h3 { "Need To Find" }
                            table { class: "table compact",
                                thead { tr { th { "Item" } th { "Missing" } th { "Have/Need" } } }
                                tbody {
                                    if dashboard.needs.is_empty() {
                                        tr { td { colspan: "3", class: "muted", "No missing items based on current tracking." } }
                                    }
                                    for row in dashboard.needs.iter().take(20) {
                                        tr {
                                            td {
                                                div { class: "item-cell",
                                                    if !row.image_src.is_empty() {
                                                        img { class: "item-icon", src: "{row.image_src}", alt: "{row.name}" }
                                                    }
                                                    span { "{row.name}" }
                                                }
                                            }
                                            td { "{row.missing}" }
                                            td { "{row.have}/{row.required}" }
                                        }
                                    }
                                }
                            }
                        }

                        div { class: "dashboard-card",
                            h3 { "Keep In Stash" }
                            table { class: "table compact",
                                thead { tr { th { "Item" } th { "Need" } th { "Have" } } }
                                tbody {
                                    if dashboard.keep.is_empty() {
                                        tr { td { colspan: "3", class: "muted", "No tracked requirement items yet." } }
                                    }
                                    for row in dashboard.keep.iter().take(20) {
                                        tr {
                                            td {
                                                div { class: "item-cell",
                                                    if !row.image_src.is_empty() {
                                                        img { class: "item-icon", src: "{row.image_src}", alt: "{row.name}" }
                                                    }
                                                    span { "{row.name}" }
                                                }
                                            }
                                            td { "{row.required}" }
                                            td { "{row.have}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "grid-two",
                    div { class: "panel",
                        h2 { "Track Crafts" }
                        p { class: "muted", "Pick a craftable item and how many outputs you want to build." }
                        div { class: "row",
                            select {
                                value: "{craft_pick.read()}",
                                onchange: move |evt| craft_pick.set(evt.value()),
                                option { value: "", "Select craft item..." }
                                for item in data_snapshot.as_ref().map(|d| d.craftable_items.clone()).unwrap_or_default() {
                                    option { value: "{item.id}", "{localized_en(&item.name)}" }
                                }
                            }
                            input {
                                value: "{craft_qty.read()}",
                                r#type: "number",
                                min: "1",
                                oninput: move |evt| craft_qty.set(evt.value()),
                            }
                            button {
                                onclick: {
                                    let craft_pick = craft_pick.clone();
                                    let craft_qty = craft_qty.clone();
                                    let mut tracked_crafts = tracked_crafts.clone();
                                    move |_| {
                                        let item_id = craft_pick.read().trim().to_string();
                                        if item_id.is_empty() {
                                            return;
                                        }
                                        let qty = parse_u32_or_default(&craft_qty.read(), 1).max(1);
                                        let mut crafts = tracked_crafts.write();
                                        if let Some(existing) = crafts.iter_mut().find(|entry| entry.item_id == item_id) {
                                            existing.quantity = existing.quantity.saturating_add(qty);
                                        } else {
                                            crafts.push(TrackedCraft { item_id, quantity: qty });
                                        }
                                    }
                                },
                                "Add"
                            }
                        }

                        table {
                            class: "table",
                            thead {
                                tr {
                                    th { "Item" }
                                    th { "Qty" }
                                    th { "" }
                                }
                            }
                            tbody {
                                if crafts_snapshot.is_empty() {
                                    tr { td { colspan: "3", class: "muted", "No tracked crafts yet." } }
                                }
                                for (idx, craft) in crafts_snapshot.iter().enumerate() {
                                    tr {
                                        td { "{data_snapshot.as_ref().map(|d| item_name(d, &craft.item_id)).unwrap_or_else(|| craft.item_id.clone())}" }
                                        td { "{craft.quantity}" }
                                        td {
                                            button {
                                                class: "danger",
                                                onclick: {
                                                    let mut tracked_crafts = tracked_crafts.clone();
                                                    move |_| {
                                                        tracked_crafts.write().remove(idx);
                                                    }
                                                },
                                                "Remove"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    div { class: "panel",
                        h2 { "Track Quests" }
                        p { class: "muted", "Add quests you want item requirements for." }
                        div { class: "row",
                            select {
                                value: "{quest_pick.read()}",
                                onchange: move |evt| quest_pick.set(evt.value()),
                                option { value: "", "Select quest..." }
                                for quest in data_snapshot.as_ref().map(|d| d.quests.clone()).unwrap_or_default() {
                                    option { value: "{quest.id}", "{localized_en(&quest.name)}" }
                                }
                            }
                            button {
                                onclick: {
                                    let quest_pick = quest_pick.clone();
                                    let mut tracked_quests = tracked_quests.clone();
                                    move |_| {
                                        let quest_id = quest_pick.read().trim().to_string();
                                        if quest_id.is_empty() {
                                            return;
                                        }
                                        let mut quests = tracked_quests.write();
                                        if !quests.contains(&quest_id) {
                                            quests.push(quest_id);
                                        }
                                    }
                                },
                                "Add"
                            }
                        }

                        table {
                            class: "table",
                            thead { tr { th { "Quest" } th { "" } } }
                            tbody {
                                if quests_snapshot.is_empty() {
                                    tr { td { colspan: "2", class: "muted", "No tracked quests." } }
                                }
                                for (idx, quest_id) in quests_snapshot.iter().enumerate() {
                                    tr {
                                        td { "{data_snapshot.as_ref().map(|d| quest_name(d, quest_id)).unwrap_or_else(|| quest_id.clone())}" }
                                        td {
                                            button {
                                                class: "danger",
                                                onclick: {
                                                    let mut tracked_quests = tracked_quests.clone();
                                                    move |_| {
                                                        tracked_quests.write().remove(idx);
                                                    }
                                                },
                                                "Remove"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "grid-two",
                    div { class: "panel",
                        h2 { "Track Hideout Upgrades" }
                        p { class: "muted", "Target level includes all requirements from level 1 up to that level." }
                        div { class: "row",
                            select {
                                value: "{hideout_pick.read()}",
                                onchange: move |evt| hideout_pick.set(evt.value()),
                                option { value: "", "Select hideout module..." }
                                for module in data_snapshot.as_ref().map(|d| d.hideout_modules.clone()).unwrap_or_default() {
                                    option { value: "{module.id}", "{localized_en(&module.name)}" }
                                }
                            }
                            input {
                                value: "{hideout_level.read()}",
                                r#type: "number",
                                min: "1",
                                oninput: move |evt| hideout_level.set(evt.value()),
                            }
                            button {
                                onclick: {
                                    let hideout_pick = hideout_pick.clone();
                                    let hideout_level = hideout_level.clone();
                                    let mut tracked_hideout = tracked_hideout.clone();
                                    let data_snapshot = data_snapshot.clone();
                                    move |_| {
                                        let module_id = hideout_pick.read().trim().to_string();
                                        if module_id.is_empty() {
                                            return;
                                        }

                                        let mut level = parse_u32_or_default(&hideout_level.read(), 1).max(1);
                                        if let Some(data) = data_snapshot.as_ref() {
                                            if let Some(max_level) = module_max_level(data, &module_id) {
                                                level = level.min(max_level.max(1));
                                            }
                                        }

                                        let mut entries = tracked_hideout.write();
                                        if let Some(existing) = entries.iter_mut().find(|entry| entry.module_id == module_id) {
                                            existing.target_level = level;
                                        } else {
                                            entries.push(TrackedHideout { module_id, target_level: level });
                                        }
                                    }
                                },
                                "Add / Update"
                            }
                        }

                        table {
                            class: "table",
                            thead { tr { th { "Module" } th { "Target" } th { "" } } }
                            tbody {
                                if hideout_snapshot.is_empty() {
                                    tr { td { colspan: "3", class: "muted", "No tracked hideout upgrades." } }
                                }
                                for (idx, entry) in hideout_snapshot.iter().enumerate() {
                                    tr {
                                        td { "{data_snapshot.as_ref().map(|d| hideout_name(d, &entry.module_id)).unwrap_or_else(|| entry.module_id.clone())}" }
                                        td { "L{entry.target_level}" }
                                        td {
                                            button {
                                                class: "danger",
                                                onclick: {
                                                    let mut tracked_hideout = tracked_hideout.clone();
                                                    move |_| {
                                                        tracked_hideout.write().remove(idx);
                                                    }
                                                },
                                                "Remove"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    div { class: "panel",
                        h2 { "Track Projects" }
                        p { class: "muted", "Target phase includes all requirements from phase 1 up to that phase." }
                        div { class: "row",
                            select {
                                value: "{project_pick.read()}",
                                onchange: move |evt| project_pick.set(evt.value()),
                                option { value: "", "Select project..." }
                                for project in data_snapshot.as_ref().map(|d| d.projects.clone()).unwrap_or_default() {
                                    option { value: "{project.id}", "{localized_en(&project.name)}" }
                                }
                            }
                            input {
                                value: "{project_phase.read()}",
                                r#type: "number",
                                min: "1",
                                oninput: move |evt| project_phase.set(evt.value()),
                            }
                            button {
                                onclick: {
                                    let project_pick = project_pick.clone();
                                    let project_phase = project_phase.clone();
                                    let mut tracked_projects = tracked_projects.clone();
                                    let data_snapshot = data_snapshot.clone();
                                    move |_| {
                                        let project_id = project_pick.read().trim().to_string();
                                        if project_id.is_empty() {
                                            return;
                                        }

                                        let mut phase = parse_u32_or_default(&project_phase.read(), 1).max(1);
                                        if let Some(data) = data_snapshot.as_ref() {
                                            if let Some(max_phase) = project_max_phase(data, &project_id) {
                                                phase = phase.min(max_phase.max(1));
                                            }
                                        }

                                        let mut entries = tracked_projects.write();
                                        if let Some(existing) = entries.iter_mut().find(|entry| entry.project_id == project_id) {
                                            existing.start_phase = 1;
                                            existing.target_phase = phase;
                                        } else {
                                            entries.push(TrackedProject { project_id, start_phase: 1, target_phase: phase });
                                        }
                                    }
                                },
                                "Add / Update"
                            }
                        }

                        table {
                            class: "table",
                            thead { tr { th { "Project" } th { "Target" } th { "" } } }
                            tbody {
                                if projects_snapshot.is_empty() {
                                    tr { td { colspan: "3", class: "muted", "No tracked projects." } }
                                }
                                for (idx, entry) in projects_snapshot.iter().enumerate() {
                                    tr {
                                        td { "{data_snapshot.as_ref().map(|d| project_name(d, &entry.project_id)).unwrap_or_else(|| entry.project_id.clone())}" }
                                        td {
                                            if entry.start_phase > 1 {
                                                "Phase {entry.start_phase}-{entry.target_phase}"
                                            } else {
                                                "Phase {entry.target_phase}"
                                            }
                                        }
                                        td {
                                            button {
                                                class: "danger",
                                                onclick: {
                                                    let mut tracked_projects = tracked_projects.clone();
                                                    move |_| {
                                                        tracked_projects.write().remove(idx);
                                                    }
                                                },
                                                "Remove"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "panel",
                    h2 { "All Required Items" }
                    table { class: "table",
                        thead { tr { th { "Item" } th { "Required" } th { "Have" } th { "Missing" } } }
                        tbody {
                            if required_rows.is_empty() {
                                tr { td { colspan: "4", class: "muted", "No requirements tracked yet." } }
                            }
                            for row in required_rows.iter() {
                                tr {
                                    td {
                                        div { class: "item-cell",
                                            if !row.image_src.is_empty() {
                                                img { class: "item-icon", src: "{row.image_src}", alt: "{row.name}" }
                                            }
                                            span { "{row.name}" }
                                        }
                                    }
                                    td { "{row.required}" }
                                    td { "{row.have}" }
                                    td { "{row.missing}" }
                                }
                            }
                        }
                    }
                }
            } else {
                div { class: "panel",
                    h2 { "Next Step" }
                    p { "Load game data first, then add tracking targets and scan your inventory." }
                }
            }
        }
    }
}

async fn fetch_static_data(client: &Client) -> Result<ArcData> {
    if let Some(repo_dir) = resolve_local_data_dir() {
        info!(path = %repo_dir.display(), "fetch_static_data: attempting local data source");
        match load_static_data_from_repo(&repo_dir) {
            Ok(data) => {
                info!(path = %repo_dir.display(), "fetch_static_data: loaded from local data source");
                return Ok(data);
            }
            Err(err) => {
                warn!(
                    path = %repo_dir.display(),
                    error = %err,
                    "fetch_static_data: local load failed, falling back to ArcTracker API"
                );
            }
        }
    }

    info!("fetch_static_data: loading static datasets from ArcTracker API");
    let items_url = format!("{API_BASE}/api/items");
    let quests_url = format!("{API_BASE}/api/quests");
    let hideout_url = format!("{API_BASE}/api/hideout");
    let projects_url = format!("{API_BASE}/api/projects?season=1,2");
    let static_ttl = static_cache_ttl();
    let mut items_resp: ItemsResponse = get_json_cached(
        client.get(items_url),
        CACHE_NAMESPACE_STATIC,
        "items",
        static_ttl,
    )
    .await
    .context("failed to load items dataset")?;
    let quests_resp: QuestsResponse = get_json_cached(
        client.get(quests_url),
        CACHE_NAMESPACE_STATIC,
        "quests",
        static_ttl,
    )
    .await
    .context("failed to load quests dataset")?;
    let hideout_resp: HideoutResponse = get_json_cached(
        client.get(hideout_url),
        CACHE_NAMESPACE_STATIC,
        "hideout",
        static_ttl,
    )
    .await
    .context("failed to load hideout dataset")?;
    let projects_resp: ProjectsResponse = get_json_cached(
        client.get(projects_url),
        CACHE_NAMESPACE_STATIC,
        "projects_1_2",
        static_ttl,
    )
    .await
    .context("failed to load projects dataset")?;

    cache_remote_item_images(client, &mut items_resp.items).await;

    info!(
        items = items_resp.items.len(),
        quests = quests_resp.quests.len(),
        hideout_modules = hideout_resp.hideout_modules.len(),
        projects = projects_resp.projects.len(),
        "fetch_static_data: static datasets loaded from API"
    );

    Ok(build_arc_data(
        items_resp.items,
        quests_resp.quests.into_values().collect(),
        hideout_resp.hideout_modules.into_values().collect(),
        projects_resp.projects.into_values().collect(),
        None,
    ))
}

fn resolve_local_data_dir() -> Option<PathBuf> {
    if let Some(configured) = first_non_empty_env(&[
        "ARC_DATA_REPO_DIR",
        "ARC_DATA_REPO_PATH",
        "ARCRAIDERS_DATA_DIR",
    ]) {
        let configured_path = PathBuf::from(configured);
        if configured_path.exists() {
            return Some(configured_path);
        }
        return None;
    }

    let candidate = PathBuf::from(LOCAL_DATA_DEFAULT_DIR);
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

fn load_static_data_from_repo(repo_dir: &Path) -> Result<ArcData> {
    let items_dir = repo_dir.join("items");
    let quests_dir = repo_dir.join("quests");
    let hideout_dir = repo_dir.join("hideout");
    let projects_file = repo_dir.join("projects.json");
    let local_images_dir = repo_dir.join("images").join("items");

    let mut items: Vec<Item> = load_json_files_from_dir(&items_dir)
        .with_context(|| format!("failed to load items from '{}'", items_dir.display()))?;
    for item in &mut items {
        if let Some(local_uri) = local_item_image_data_uri(&local_images_dir, &item.id) {
            item.image_filename = Some(local_uri);
        }
    }

    let quests: Vec<Quest> = load_json_files_from_dir(&quests_dir)
        .with_context(|| format!("failed to load quests from '{}'", quests_dir.display()))?;

    let hideout_modules: Vec<HideoutModule> = load_json_files_from_dir(&hideout_dir)
        .with_context(|| format!("failed to load hideout from '{}'", hideout_dir.display()))?;

    let projects: Vec<Project> = load_json_file(&projects_file)
        .with_context(|| format!("failed to load projects from '{}'", projects_file.display()))?;

    Ok(build_arc_data(
        items,
        quests,
        hideout_modules,
        projects,
        Some(local_images_dir),
    ))
}

fn build_arc_data(
    items: Vec<Item>,
    mut quests: Vec<Quest>,
    mut hideout_modules: Vec<HideoutModule>,
    mut projects: Vec<Project>,
    local_images_dir: Option<PathBuf>,
) -> ArcData {
    let mut items_by_id = HashMap::new();
    for item in items {
        items_by_id.insert(item.id.clone(), item);
    }

    let mut craftable_items: Vec<Item> = items_by_id
        .values()
        .filter(|item| item.recipe.as_ref().map(|r| !r.is_empty()).unwrap_or(false))
        .cloned()
        .collect();
    craftable_items.sort_by(|a, b| localized_en(&a.name).cmp(&localized_en(&b.name)));

    quests.sort_by(|a, b| localized_en(&a.name).cmp(&localized_en(&b.name)));
    hideout_modules.sort_by(|a, b| localized_en(&a.name).cmp(&localized_en(&b.name)));
    projects.sort_by(|a, b| localized_en(&a.name).cmp(&localized_en(&b.name)));

    ArcData {
        items_by_id,
        craftable_items,
        quests,
        hideout_modules,
        projects,
        local_images_dir,
    }
}

fn load_json_file<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed reading file '{}'", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("failed parsing JSON from '{}'", path.display()))
}

fn load_json_files_from_dir<T>(dir: &Path) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("failed reading directory '{}'", dir.display()))?
    {
        let path = entry?.path();
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("json"))
            .unwrap_or(false)
        {
            files.push(path);
        }
    }

    files.sort();

    let mut result = Vec::with_capacity(files.len());
    for file in files {
        result.push(load_json_file(&file)?);
    }

    Ok(result)
}

fn local_item_image_data_uri(images_dir: &Path, item_id: &str) -> Option<String> {
    let candidate = images_dir.join(format!("{item_id}.png"));
    if !candidate.exists() {
        return None;
    }
    let bytes = fs::read(&candidate).ok()?;
    Some(format!("data:image/png;base64,{}", BASE64.encode(bytes)))
}

async fn fetch_stash_inventory(
    client: &Client,
    app_key: &str,
    user_key: &str,
) -> Result<HashMap<String, u32>> {
    fetch_stash_inventory_with_cache(client, app_key, user_key, None).await
}

async fn fetch_stash_inventory_with_cache(
    client: &Client,
    app_key: &str,
    user_key: &str,
    cache_ttl: Option<Duration>,
) -> Result<HashMap<String, u32>> {
    let app_key = app_key.trim();
    let user_key = user_key.trim();

    if app_key.is_empty() {
        return Err(anyhow!(
            "Missing app key. Add ARC_APP_KEY (or key) in your .env file."
        ));
    }
    if user_key.is_empty() {
        return Err(anyhow!(
            "Missing user key. Set user_key (or ARC_USER_KEY) in .env, or paste your arc_u1_ key in the app."
        ));
    }

    let cache_key = format!("stash_counts_{}", short_hash(user_key));
    if let Some(ttl) = cache_ttl {
        if let Some(cached) =
            read_cache_typed::<HashMap<String, u32>>(CACHE_NAMESPACE_USER, &cache_key, Some(ttl))
        {
            info!(
                unique_items = cached.len(),
                total_items = cached.values().sum::<u32>(),
                "fetch_stash_inventory: cache hit"
            );
            return Ok(cached);
        }
    }

    info!("fetch_stash_inventory: start");
    let mut all_counts: HashMap<String, u32> = HashMap::new();
    let mut page = 1u32;

    loop {
        let url =
            format!("{API_BASE}/api/v2/user/stash?locale=en&page={page}&per_page=500&sort=slot");

        let payload: Value = get_json(
            client
                .get(url)
                .header("X-App-Key", app_key)
                .header("Authorization", format!("Bearer {user_key}")),
        )
        .await
        .context("ArcTracker stash API call failed")?;

        let data = payload.get("data").unwrap_or(&payload);
        let page_counts = extract_inventory_counts(data);
        let page_total: u32 = page_counts.values().sum();
        debug!(
            page,
            unique_items = page_counts.len(),
            total_items = page_total,
            "fetch_stash_inventory: parsed page"
        );
        merge_counts(&mut all_counts, page_counts);

        if !has_next_page(&payload, page) {
            break;
        }

        page = page.saturating_add(1);
        if page > 20 {
            break;
        }
    }

    let total: u32 = all_counts.values().sum();
    info!(
        pages_scanned = page,
        unique_items = all_counts.len(),
        total_items = total,
        "fetch_stash_inventory: complete"
    );
    if cache_ttl.is_some() {
        write_cache_typed(CACHE_NAMESPACE_USER, &cache_key, &all_counts);
    }
    Ok(all_counts)
}

async fn get_user_json_value(
    client: &Client,
    app_key: &str,
    user_key: &str,
    path_with_query: &str,
    cache_ttl: Option<Duration>,
) -> Result<Value> {
    debug!(path = path_with_query, "get_user_json_value: request");
    let cache_key = format!("{}_{}", short_hash(user_key), short_hash(path_with_query));
    if let Some(ttl) = cache_ttl {
        if let Some(cached) = read_cache_typed::<Value>(CACHE_NAMESPACE_USER, &cache_key, Some(ttl))
        {
            debug!(path = path_with_query, "get_user_json_value: cache hit");
            return Ok(cached);
        }
    }

    let url = format!("{API_BASE}{path_with_query}");
    let fetched: Result<Value> = get_json(
        client
            .get(url)
            .header("X-App-Key", app_key)
            .header("Authorization", format!("Bearer {user_key}")),
    )
    .await;

    match fetched {
        Ok(value) => {
            if cache_ttl.is_some() {
                write_cache_typed(CACHE_NAMESPACE_USER, &cache_key, &value);
            }
            Ok(value)
        }
        Err(err) => {
            if cache_ttl.is_some() {
                if let Some(stale) =
                    read_cache_typed::<Value>(CACHE_NAMESPACE_USER, &cache_key, None)
                {
                    warn!(
                        path = path_with_query,
                        error = %err,
                        "get_user_json_value: using stale cache after fetch failure"
                    );
                    return Ok(stale);
                }
            }
            Err(err)
        }
    }
}

async fn run_api_diagnostics(
    client: &Client,
    app_key: &str,
    user_key: &str,
) -> Result<Vec<ApiDiagnosticRow>> {
    let app_key = app_key.trim();
    let user_key = user_key.trim();

    if app_key.is_empty() {
        return Err(anyhow!(
            "Missing app key. Set key (or ARC_APP_KEY) in .env."
        ));
    }
    if user_key.is_empty() {
        return Err(anyhow!(
            "Missing user key. Set user_key (or ARC_USER_KEY) in .env, or paste arc_u1_ in the app."
        ));
    }

    let endpoints = [
        "/api/v2/user/profile?locale=en",
        "/api/v2/user/stash?locale=en&page=1&per_page=50&sort=slot",
        "/api/v2/user/loadout?locale=en",
        "/api/v2/user/quests?locale=en&filter=incomplete",
        "/api/v2/user/quests?locale=en&filter=completed",
        "/api/v2/user/hideout?locale=en",
        "/api/v2/user/projects?locale=en&season=1",
        "/api/v2/user/projects?locale=en&season=2",
    ];

    let mut rows = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        match get_user_json_value(client, app_key, user_key, endpoint, None).await {
            Ok(payload) => rows.push(ApiDiagnosticRow {
                endpoint: endpoint.to_string(),
                status_code: "200".to_string(),
                request_id: extract_request_id_from_payload(&payload),
                detail: "OK".to_string(),
                ok: true,
            }),
            Err(err) => {
                let status_code = extract_http_status_code_from_error(&err)
                    .map(|status| status.to_string())
                    .unwrap_or_else(|| "error".to_string());
                rows.push(ApiDiagnosticRow {
                    endpoint: endpoint.to_string(),
                    status_code,
                    request_id: extract_request_id_from_error(&err),
                    detail: truncate_for_report(&err.to_string(), 220),
                    ok: false,
                });
            }
        }
    }

    Ok(rows)
}

fn build_api_diagnostics_report(rows: &[ApiDiagnosticRow]) -> String {
    let failed = rows.iter().filter(|row| !row.ok).count();
    let passed = rows.len().saturating_sub(failed);

    let mut report = format!("ArcTracker API diagnostics: {passed} passed, {failed} failed\n");
    for row in rows {
        let result = if row.ok { "OK" } else { "ERR" };
        let request_id = row.request_id.as_deref().unwrap_or("-");
        report.push_str(&format!(
            "[{result}] {} status={} requestId={} detail={}\n",
            row.endpoint, row.status_code, request_id, row.detail
        ));
    }

    report
}

async fn sync_user_progress(
    client: &Client,
    app_key: &str,
    user_key: &str,
    data: &ArcData,
    include_stash: bool,
    user_cache_ttl: Option<Duration>,
) -> Result<UserSyncResult> {
    let app_key = app_key.trim();
    let user_key = user_key.trim();

    if app_key.is_empty() {
        return Err(anyhow!(
            "Missing app key. Set key (or ARC_APP_KEY) in .env."
        ));
    }
    if user_key.is_empty() {
        return Err(anyhow!(
            "Missing user key. Set user_key (or ARC_USER_KEY) in .env, or paste arc_u1_ in the app."
        ));
    }

    info!(include_stash, "sync_user_progress: start");
    let mut result = UserSyncResult::default();
    let quest_ids: HashSet<String> = data.quests.iter().map(|q| q.id.clone()).collect();
    let hideout_ids: HashSet<String> = data.hideout_modules.iter().map(|m| m.id.clone()).collect();
    let project_ids: HashSet<String> = data.projects.iter().map(|p| p.id.clone()).collect();

    if include_stash {
        match fetch_stash_inventory_with_cache(client, app_key, user_key, user_cache_ttl).await {
            Ok(stash) => {
                info!(
                    unique_items = stash.len(),
                    total_items = stash.values().sum::<u32>(),
                    "sync_user_progress: stash loaded"
                );
                result.stash_counts = Some(stash);
            }
            Err(err) => result.warnings.push(format!("stash: {err}")),
        }
    }

    match get_user_json_value(
        client,
        app_key,
        user_key,
        "/api/v2/user/profile?locale=en",
        user_cache_ttl,
    )
    .await
    {
        Ok(profile_value) => {
            result.profile = parse_user_profile(unwrap_data_ref(&profile_value));
            debug!(
                has_profile = result.profile.is_some(),
                "sync_user_progress: profile parsed"
            );
        }
        Err(err) => result.warnings.push(format!("profile: {err}")),
    }

    match get_user_json_value(
        client,
        app_key,
        user_key,
        "/api/v2/user/loadout?locale=en",
        user_cache_ttl,
    )
    .await
    {
        Ok(loadout_value) => {
            let loadout_data = unwrap_data_ref(&loadout_value);
            let counts = extract_inventory_counts(loadout_data);
            debug!(
                unique_items = counts.len(),
                total_items = counts.values().sum::<u32>(),
                "sync_user_progress: loadout parsed"
            );
            result.loadout_counts = Some(counts);
        }
        Err(err) => result.warnings.push(format!("loadout: {err}")),
    }

    let mut incomplete_quests = HashSet::new();
    let mut quests_synced = false;
    let mut quests_server_error = false;
    match get_user_json_value(
        client,
        app_key,
        user_key,
        "/api/v2/user/quests?locale=en&filter=incomplete",
        user_cache_ttl,
    )
    .await
    {
        Ok(quest_value) => {
            quests_synced = true;
            result.quests_synced = true;
            incomplete_quests = extract_known_ids(
                unwrap_data_ref(&quest_value),
                &quest_ids,
                &["questId", "quest_id", "id", "slug"],
            );
        }
        Err(err) => {
            if extract_http_status_code_from_error(&err)
                .map(|status| (500..=599).contains(&status))
                .unwrap_or(false)
            {
                quests_server_error = true;
            }
            result.warnings.push(format!("quests/incomplete: {err}"));
        }
    }

    if incomplete_quests.is_empty() && !quests_server_error {
        match get_user_json_value(
            client,
            app_key,
            user_key,
            "/api/v2/user/quests?locale=en&filter=completed",
            user_cache_ttl,
        )
        .await
        {
            Ok(completed_value) => {
                quests_synced = true;
                result.quests_synced = true;
                let completed = extract_known_ids(
                    unwrap_data_ref(&completed_value),
                    &quest_ids,
                    &["questId", "quest_id", "id", "slug"],
                );
                for id in &quest_ids {
                    if !completed.contains(id) {
                        incomplete_quests.insert(id.clone());
                    }
                }
            }
            Err(err) => result.warnings.push(format!("quests/completed: {err}")),
        }
    } else if quests_server_error {
        warn!(
            "sync_user_progress: skipping quests/completed fallback after server error on quests/incomplete"
        );
    }

    if quests_synced {
        let mut tracked_quests: Vec<String> = incomplete_quests.into_iter().collect();
        tracked_quests.sort();
        debug!(
            tracked = tracked_quests.len(),
            "sync_user_progress: quests parsed"
        );
        result.tracked_quests = Some(tracked_quests);
    }

    match get_user_json_value(
        client,
        app_key,
        user_key,
        "/api/v2/user/hideout?locale=en",
        user_cache_ttl,
    )
    .await
    {
        Ok(hideout_value) => {
            result.hideout_synced = true;
            let progress = extract_progress_level_map(
                unwrap_data_ref(&hideout_value),
                &hideout_ids,
                &["moduleId", "module_id", "id"],
                &["currentLevel", "current_level", "level"],
                &["completedLevels", "completed_levels"],
            );

            let mut targets = Vec::new();
            for module in &data.hideout_modules {
                let current = *progress.get(&module.id).unwrap_or(&0);
                let max_level = module
                    .levels
                    .iter()
                    .map(|level| level.level)
                    .max()
                    .unwrap_or(module.max_level);
                let next = current.saturating_add(1);
                if max_level > 0 && next <= max_level {
                    targets.push(TrackedHideout {
                        module_id: module.id.clone(),
                        target_level: next,
                    });
                }
            }
            targets.sort_by(|a, b| a.module_id.cmp(&b.module_id));
            debug!(
                tracked = targets.len(),
                "sync_user_progress: hideout targets derived"
            );
            result.tracked_hideout = Some(targets);
        }
        Err(err) => result.warnings.push(format!("hideout: {err}")),
    }

    let mut project_progress: HashMap<String, u32> = HashMap::new();
    let mut project_errors = Vec::new();
    let mut projects_synced = false;
    let mut projects_server_errors = 0usize;
    for season in [1u32, 2u32] {
        let path = format!("/api/v2/user/projects?locale=en&season={season}");
        match get_user_json_value(client, app_key, user_key, &path, user_cache_ttl).await {
            Ok(project_value) => {
                projects_synced = true;
                result.projects_synced = true;
                let progress = extract_progress_level_map(
                    unwrap_data_ref(&project_value),
                    &project_ids,
                    &["projectId", "project_id", "id", "slug"],
                    &["currentPhase", "current_phase", "phase"],
                    &["completedPhases", "completedPhaseIds", "completed_phases"],
                );
                for (project_id, current_phase) in progress {
                    project_progress
                        .entry(project_id)
                        .and_modify(|existing| *existing = (*existing).max(current_phase))
                        .or_insert(current_phase);
                }
            }
            Err(err) => {
                if extract_http_status_code_from_error(&err)
                    .map(|status| (500..=599).contains(&status))
                    .unwrap_or(false)
                {
                    projects_server_errors = projects_server_errors.saturating_add(1);
                }
                project_errors.push(format!("projects/season={season}: {err}"));
            }
        }
    }

    if project_progress.is_empty() && projects_server_errors < 2 {
        match get_user_json_value(
            client,
            app_key,
            user_key,
            "/api/v2/user/projects?locale=en",
            user_cache_ttl,
        )
        .await
        {
            Ok(project_value) => {
                projects_synced = true;
                result.projects_synced = true;
                project_progress = extract_progress_level_map(
                    unwrap_data_ref(&project_value),
                    &project_ids,
                    &["projectId", "project_id", "id", "slug"],
                    &["currentPhase", "current_phase", "phase"],
                    &["completedPhases", "completedPhaseIds", "completed_phases"],
                );
            }
            Err(err) => project_errors.push(format!("projects: {err}")),
        }
    } else if project_progress.is_empty() && projects_server_errors >= 2 {
        warn!(
            "sync_user_progress: skipping /projects fallback after server errors on both season calls"
        );
    }

    if !project_progress.is_empty() {
        write_cache_typed(
            CACHE_NAMESPACE_USER,
            &format!(
                "{}_{}",
                short_hash(user_key),
                short_hash("projects_progress")
            ),
            &project_progress,
        );
    }

    if projects_synced {
        let mut targets = Vec::new();
        for project in &data.projects {
            let current = *project_progress.get(&project.id).unwrap_or(&0);
            let max_phase = project
                .phases
                .iter()
                .map(|phase| phase.phase)
                .max()
                .unwrap_or(0);
            let start = current.saturating_add(1);
            if max_phase > 0 && start <= max_phase {
                targets.push(TrackedProject {
                    project_id: project.id.clone(),
                    start_phase: start,
                    target_phase: max_phase,
                });
            }
        }
        targets.sort_by(|a, b| a.project_id.cmp(&b.project_id));
        debug!(
            tracked = targets.len(),
            "sync_user_progress: project targets derived"
        );
        result.tracked_projects = Some(targets);
    }

    if result.warnings.is_empty() {
        info!("sync_user_progress: complete without warnings");
    } else {
        warn!(
            warnings = result.warnings.len(),
            "sync_user_progress: complete with warnings"
        );
    }
    if !projects_synced && !project_errors.is_empty() {
        result.warnings.push(project_errors.join(" | "));
    }

    if result.warnings.is_empty() {
        info!("sync_user_progress: complete without warnings");
    } else {
        warn!(
            warnings = result.warnings.len(),
            "sync_user_progress: complete with warnings"
        );
    }
    Ok(result)
}

fn unwrap_data_ref(value: &Value) -> &Value {
    value.get("data").unwrap_or(value)
}

fn parse_user_profile(value: &Value) -> Option<UserProfileInfo> {
    let root = value
        .get("profile")
        .or_else(|| value.get("user"))
        .unwrap_or(value);

    let username = root
        .get("username")
        .or_else(|| root.get("name"))
        .and_then(value_as_string)
        .or_else(|| {
            root.get("user")
                .and_then(|user| user.get("username"))
                .and_then(value_as_string)
        })?;

    let level = root
        .get("level")
        .and_then(value_as_u32)
        .or_else(|| root.get("playerLevel").and_then(value_as_u32));

    let member_since = root
        .get("memberSince")
        .or_else(|| root.get("createdAt"))
        .or_else(|| root.get("member_since"))
        .and_then(value_as_string);

    Some(UserProfileInfo {
        username,
        level,
        member_since,
    })
}

fn extract_known_ids(
    value: &Value,
    known_ids: &HashSet<String>,
    preferred_keys: &[&str],
) -> HashSet<String> {
    fn walk(
        value: &Value,
        known_ids: &HashSet<String>,
        preferred_keys: &[&str],
        out: &mut HashSet<String>,
    ) {
        match value {
            Value::Array(items) => {
                for item in items {
                    walk(item, known_ids, preferred_keys, out);
                }
            }
            Value::Object(map) => {
                for key in preferred_keys {
                    if let Some(raw_id) = map.get(*key).and_then(value_as_string) {
                        if let Some(mapped) = map_to_known_id(&raw_id, known_ids) {
                            out.insert(mapped);
                        }
                    }
                }

                if let Some(raw_id) = map.get("id").and_then(value_as_string) {
                    if let Some(mapped) = map_to_known_id(&raw_id, known_ids) {
                        out.insert(mapped);
                    }
                }

                for (key, child) in map {
                    if let Some(mapped) = map_to_known_id(key, known_ids) {
                        if child.is_object() || child.is_array() {
                            out.insert(mapped);
                        }
                    }

                    if key.to_ascii_lowercase().contains("quest") {
                        if let Some(ids) = child.as_array() {
                            for id in ids {
                                if let Some(raw) = value_as_string(id) {
                                    if let Some(mapped) = map_to_known_id(&raw, known_ids) {
                                        out.insert(mapped);
                                    }
                                }
                            }
                        }
                    }

                    walk(child, known_ids, preferred_keys, out);
                }
            }
            _ => {}
        }
    }

    let mut out = HashSet::new();
    walk(value, known_ids, preferred_keys, &mut out);
    out
}

fn extract_progress_level_map(
    value: &Value,
    known_ids: &HashSet<String>,
    preferred_id_keys: &[&str],
    preferred_level_keys: &[&str],
    completed_level_array_keys: &[&str],
) -> HashMap<String, u32> {
    fn parse_level_from_object(
        map: &Map<String, Value>,
        preferred_level_keys: &[&str],
        completed_level_array_keys: &[&str],
    ) -> Option<u32> {
        for key in preferred_level_keys {
            if let Some(level) = map.get(*key).and_then(value_as_u32) {
                return Some(level);
            }
        }

        for key in completed_level_array_keys {
            if let Some(values) = map.get(*key).and_then(|v| v.as_array()) {
                let mut max_level = 0u32;
                for value in values {
                    if let Some(level) = value_as_u32(value) {
                        max_level = max_level.max(level);
                    }
                }
                if max_level > 0 {
                    return Some(max_level);
                }
            }
        }

        None
    }

    fn walk(
        value: &Value,
        known_ids: &HashSet<String>,
        preferred_id_keys: &[&str],
        preferred_level_keys: &[&str],
        completed_level_array_keys: &[&str],
        out: &mut HashMap<String, u32>,
    ) {
        match value {
            Value::Array(items) => {
                for item in items {
                    walk(
                        item,
                        known_ids,
                        preferred_id_keys,
                        preferred_level_keys,
                        completed_level_array_keys,
                        out,
                    );
                }
            }
            Value::Object(map) => {
                let mut matched_id: Option<String> = None;

                for key in preferred_id_keys {
                    if let Some(raw_id) = map.get(*key).and_then(value_as_string) {
                        if let Some(mapped) = map_to_known_id(&raw_id, known_ids) {
                            matched_id = Some(mapped);
                            break;
                        }
                    }
                }

                if matched_id.is_none() {
                    if let Some(raw_id) = map.get("id").and_then(value_as_string) {
                        matched_id = map_to_known_id(&raw_id, known_ids);
                    }
                }

                if let Some(id) = matched_id {
                    if let Some(level) = parse_level_from_object(
                        map,
                        preferred_level_keys,
                        completed_level_array_keys,
                    ) {
                        let entry = out.entry(id).or_insert(0);
                        *entry = (*entry).max(level);
                    }
                }

                for (key, child) in map {
                    if let Some(mapped) = map_to_known_id(key, known_ids) {
                        if let Some(child_map) = child.as_object() {
                            if let Some(level) = parse_level_from_object(
                                child_map,
                                preferred_level_keys,
                                completed_level_array_keys,
                            ) {
                                let entry = out.entry(mapped).or_insert(0);
                                *entry = (*entry).max(level);
                            }
                        }
                    }

                    walk(
                        child,
                        known_ids,
                        preferred_id_keys,
                        preferred_level_keys,
                        completed_level_array_keys,
                        out,
                    );
                }
            }
            _ => {}
        }
    }

    let mut out = HashMap::new();
    walk(
        value,
        known_ids,
        preferred_id_keys,
        preferred_level_keys,
        completed_level_array_keys,
        &mut out,
    );
    out
}

fn map_to_known_id(raw: &str, known_ids: &HashSet<String>) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    if known_ids.contains(raw) {
        return Some(raw.to_string());
    }

    if let Some((_, suffix)) = raw.split_once('_') {
        if known_ids.contains(suffix) {
            return Some(suffix.to_string());
        }
    }

    None
}

async fn get_json<T>(request: reqwest::RequestBuilder) -> Result<T>
where
    T: DeserializeOwned,
{
    let request_template = request
        .try_clone()
        .ok_or_else(|| anyhow!("HTTP request could not be cloned for retries"))?;

    let request_meta = request_template
        .try_clone()
        .and_then(|builder| builder.build().ok())
        .map(|req| (req.method().to_string(), req.url().to_string()));

    if let Some((method, url)) = request_meta.as_ref() {
        debug!(%method, %url, "get_json: sending request");
    } else {
        debug!("get_json: sending request");
    }

    let retry = api_retry_config();
    for attempt in 0..=retry.max_retries {
        api_request_throttle()
            .wait_turn(request_meta.as_ref().map(|(_, url)| url.as_str()))
            .await;

        let request_for_attempt = request_template
            .try_clone()
            .ok_or_else(|| anyhow!("HTTP request could not be cloned for retries"))?;

        let response = match request_for_attempt.send().await {
            Ok(response) => response,
            Err(err) => {
                if should_retry_transport(&err) && attempt < retry.max_retries {
                    let delay = retry.delay_for_attempt(attempt);
                    warn!(
                        attempt = attempt + 1,
                        max_retries = retry.max_retries,
                        retry_in_ms = delay.as_millis() as u64,
                        error = %err,
                        endpoint = request_meta
                            .as_ref()
                            .map(|(_, url)| url.as_str())
                            .unwrap_or("unknown"),
                        "get_json: transport error, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(err).context("HTTP request failed");
            }
        };

        let status = response.status();
        let headers = response.headers().clone();
        if let Some(rate) = extract_rate_limit_info(&headers) {
            debug!(
                limit = rate.limit,
                remaining = rate.remaining,
                reset_unix = rate.reset_unix,
                endpoint = request_meta
                    .as_ref()
                    .map(|(_, url)| url.as_str())
                    .unwrap_or("unknown"),
                "get_json: rate limit headers"
            );
        }
        let body = response
            .text()
            .await
            .context("failed reading HTTP response")?;

        if status.is_success() {
            if let Some((method, url)) = request_meta.as_ref() {
                debug!(%method, %url, status = %status, "get_json: request succeeded");
            }

            return serde_json::from_str(&body).context("failed to parse JSON response");
        }

        let snippet: String = body.chars().take(500).collect();
        let request_id = extract_request_id(&body);

        if let Some((method, url)) = request_meta.as_ref() {
            if let Some(request_id) = request_id.as_ref() {
                warn!(
                    %method,
                    %url,
                    status = %status,
                    request_id,
                    body = %snippet,
                    "get_json: HTTP error"
                );
            } else {
                warn!(
                    %method,
                    %url,
                    status = %status,
                    body = %snippet,
                    "get_json: HTTP error"
                );
            }
        } else if let Some(request_id) = request_id.as_ref() {
            warn!(
                status = %status,
                request_id,
                body = %snippet,
                "get_json: HTTP error"
            );
        } else {
            warn!(status = %status, body = %snippet, "get_json: HTTP error");
        }

        if should_retry_status(status) && attempt < retry.max_retries {
            let delay = retry_delay_for_status(status, &headers, retry, attempt);
            warn!(
                attempt = attempt + 1,
                max_retries = retry.max_retries,
                retry_in_ms = delay.as_millis() as u64,
                status = %status,
                endpoint = request_meta
                    .as_ref()
                    .map(|(_, url)| url.as_str())
                    .unwrap_or("unknown"),
                "get_json: retrying after HTTP error"
            );
            tokio::time::sleep(delay).await;
            continue;
        }

        if let Some(request_id) = request_id {
            return Err(anyhow!(
                "HTTP {} (requestId={}): {}",
                status,
                request_id,
                snippet
            ));
        }
        return Err(anyhow!("HTTP {}: {}", status, snippet));
    }

    Err(anyhow!("HTTP request retries exhausted"))
}

fn should_retry_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn should_retry_transport(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

#[derive(Debug, Clone, Copy)]
struct RateLimitInfo {
    limit: u64,
    remaining: u64,
    reset_unix: u64,
}

fn extract_rate_limit_info(headers: &HeaderMap) -> Option<RateLimitInfo> {
    let limit = headers
        .get("X-RateLimit-Limit")
        .or_else(|| headers.get("x-ratelimit-limit"))
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.parse::<u64>().ok())?;

    let remaining = headers
        .get("X-RateLimit-Remaining")
        .or_else(|| headers.get("x-ratelimit-remaining"))
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.parse::<u64>().ok())?;

    let reset_unix = headers
        .get("X-RateLimit-Reset")
        .or_else(|| headers.get("x-ratelimit-reset"))
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.parse::<u64>().ok())?;

    Some(RateLimitInfo {
        limit,
        remaining,
        reset_unix,
    })
}

fn retry_delay_for_status(
    status: StatusCode,
    headers: &HeaderMap,
    retry: &ApiRetryConfig,
    attempt: usize,
) -> Duration {
    if status == StatusCode::TOO_MANY_REQUESTS {
        if let Some(rate) = extract_rate_limit_info(headers) {
            let now_unix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            if rate.reset_unix > now_unix {
                let until_reset_ms = (rate.reset_unix - now_unix).saturating_mul(1000);
                if until_reset_ms > 0 {
                    return Duration::from_millis(
                        until_reset_ms.min(retry.max_delay.as_millis() as u64),
                    );
                }
            }
        }
    }
    retry.delay_for_attempt(attempt)
}

fn extract_http_status_code_from_error(err: &anyhow::Error) -> Option<u16> {
    let text = err.to_string();
    let start = text.find("HTTP ")? + 5;
    let digits: String = text[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<u16>().ok()
}

fn extract_request_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("meta")
        .and_then(|meta| {
            meta.get("requestId")
                .or_else(|| meta.get("request_id"))
                .or_else(|| meta.get("requestID"))
        })
        .and_then(value_as_string)
}

fn extract_request_id_from_error(err: &anyhow::Error) -> Option<String> {
    let text = err.to_string();
    if let Some(pos) = text.find("requestId=") {
        let rest = &text[pos + "requestId=".len()..];
        let id: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .collect();
        if !id.is_empty() {
            return Some(id);
        }
    }

    if let Some(pos) = text.find("\"requestId\":\"") {
        let rest = &text[pos + "\"requestId\":\"".len()..];
        let id: String = rest.chars().take_while(|ch| *ch != '"').collect();
        if !id.is_empty() {
            return Some(id);
        }
    }

    None
}

fn truncate_for_report(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("...");
    out
}

fn extract_request_id(body: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    parsed
        .get("meta")
        .and_then(|meta| {
            meta.get("requestId")
                .or_else(|| meta.get("request_id"))
                .or_else(|| meta.get("requestID"))
        })
        .and_then(value_as_string)
}

fn has_next_page(payload: &Value, current_page: u32) -> bool {
    let meta = match payload.get("meta") {
        Some(meta) => meta,
        None => return false,
    };

    let pagination = meta.get("pagination").unwrap_or(meta);

    if let Some(next_page) = pagination
        .get("nextPage")
        .or_else(|| pagination.get("next_page"))
        .and_then(value_as_u32)
    {
        return next_page > current_page;
    }

    if let Some(has_next) = pagination
        .get("hasNext")
        .or_else(|| pagination.get("has_next"))
        .and_then(|v| v.as_bool())
    {
        return has_next;
    }

    if let Some(total_pages) = pagination
        .get("totalPages")
        .or_else(|| pagination.get("total_pages"))
        .and_then(value_as_u32)
    {
        return current_page < total_pages;
    }

    if let (Some(total), Some(per_page)) = (
        pagination
            .get("total")
            .or_else(|| pagination.get("totalItems"))
            .and_then(value_as_u32),
        pagination
            .get("perPage")
            .or_else(|| pagination.get("per_page"))
            .and_then(value_as_u32),
    ) {
        return current_page.saturating_mul(per_page) < total;
    }

    false
}

fn extract_inventory_counts(root: &Value) -> HashMap<String, u32> {
    let mut counts = HashMap::new();
    walk_inventory_json(root, &mut counts);
    counts
}

fn walk_inventory_json(value: &Value, counts: &mut HashMap<String, u32>) {
    match value {
        Value::Object(map) => {
            if let Some((item_id, quantity)) = parse_inventory_entry(map) {
                let entry = counts.entry(item_id).or_insert(0);
                *entry = entry.saturating_add(quantity);
            }

            for child in map.values() {
                walk_inventory_json(child, counts);
            }
        }
        Value::Array(items) => {
            for child in items {
                walk_inventory_json(child, counts);
            }
        }
        _ => {}
    }
}

fn parse_inventory_entry(map: &Map<String, Value>) -> Option<(String, u32)> {
    let quantity = ["quantity", "count", "amount", "stack", "stackSize"]
        .iter()
        .find_map(|key| map.get(*key).and_then(value_as_u32))?;

    let item_id = map
        .get("itemId")
        .and_then(value_as_string)
        .or_else(|| map.get("item_id").and_then(value_as_string))
        .or_else(|| {
            map.get("item").and_then(|item| {
                value_as_string(item)
                    .or_else(|| item.get("id").and_then(value_as_string))
                    .or_else(|| item.get("itemId").and_then(value_as_string))
            })
        })
        .or_else(|| {
            if map.contains_key("quantity")
                || map.contains_key("count")
                || map.contains_key("amount")
            {
                map.get("id").and_then(value_as_string)
            } else {
                None
            }
        })?;

    Some((item_id, quantity))
}

fn aggregate_requirements(
    data: &ArcData,
    tracked_crafts: &[TrackedCraft],
    tracked_quests: &[String],
    tracked_hideout: &[TrackedHideout],
    tracked_projects: &[TrackedProject],
) -> HashMap<String, u32> {
    let mut totals = HashMap::new();

    for craft in tracked_crafts {
        if let Some(item) = data.items_by_id.get(&craft.item_id) {
            if let Some(recipe) = item.recipe.as_ref() {
                if !recipe.is_empty() {
                    let output_qty = item.craft_quantity.unwrap_or(1).max(1);
                    let runs = (craft.quantity + output_qty - 1) / output_qty;

                    for (ingredient_id, qty) in recipe {
                        add_requirement(&mut totals, ingredient_id, qty.saturating_mul(runs));
                    }
                    continue;
                }
            }
        }

        add_requirement(&mut totals, &craft.item_id, craft.quantity);
    }

    for quest_id in tracked_quests {
        if let Some(quest) = data.quests.iter().find(|quest| quest.id == *quest_id) {
            for req in &quest.required_item_ids {
                add_requirement(&mut totals, &req.item_id, req.quantity);
            }
        }
    }

    for hideout_entry in tracked_hideout {
        if let Some(module) = data
            .hideout_modules
            .iter()
            .find(|module| module.id == hideout_entry.module_id)
        {
            for level in module
                .levels
                .iter()
                .filter(|level| level.level <= hideout_entry.target_level)
            {
                for req in &level.requirement_item_ids {
                    add_requirement(&mut totals, &req.item_id, req.quantity);
                }
            }
        }
    }

    for project_entry in tracked_projects {
        if let Some(project) = data
            .projects
            .iter()
            .find(|project| project.id == project_entry.project_id)
        {
            for phase in project.phases.iter().filter(|phase| {
                phase.phase >= project_entry.start_phase
                    && phase.phase <= project_entry.target_phase
            }) {
                for req in &phase.requirement_item_ids {
                    add_requirement(&mut totals, &req.item_id, req.quantity);
                }
            }
        }
    }

    totals
}

fn build_dashboard(
    data: &ArcData,
    required_items: &HashMap<String, u32>,
    inventory: &HashMap<String, u32>,
    loadout_counts: &HashMap<String, u32>,
    allow_sell_recommendations: bool,
) -> Dashboard {
    let mut needs = Vec::new();
    let mut keep = Vec::new();
    let mut sell = Vec::new();

    let mut keep_targets: HashMap<String, u32> = required_items.clone();
    for (item_id, loadout_qty) in loadout_counts {
        let entry = keep_targets.entry(item_id.clone()).or_insert(0);
        *entry = entry.saturating_add(*loadout_qty);
    }

    for (item_id, keep_qty) in &keep_targets {
        let have = *inventory.get(item_id).unwrap_or(&0);
        let row = NeedRow {
            name: item_name(data, item_id),
            image_src: item_image_src(data, item_id),
            required: *keep_qty,
            have,
            missing: keep_qty.saturating_sub(have),
        };

        if required_items.contains_key(item_id) && row.missing > 0 {
            needs.push(row.clone());
        }
        keep.push(row);
    }

    if allow_sell_recommendations {
        for (item_id, quantity) in inventory {
            let keep_qty = *keep_targets.get(item_id).unwrap_or(&0);
            if *quantity <= keep_qty {
                continue;
            }

            let unit_value = data
                .items_by_id
                .get(item_id)
                .and_then(|item| item.value)
                .unwrap_or(0);
            let sell_qty = quantity.saturating_sub(keep_qty);

            sell.push(SellRow {
                name: item_name(data, item_id),
                image_src: item_image_src(data, item_id),
                quantity: sell_qty,
                total_value: (sell_qty as u64) * (unit_value as u64),
            });
        }
    }

    needs.sort_by(|a, b| b.missing.cmp(&a.missing).then(a.name.cmp(&b.name)));
    keep.sort_by(|a, b| a.name.cmp(&b.name));
    sell.sort_by(|a, b| b.total_value.cmp(&a.total_value).then(a.name.cmp(&b.name)));

    Dashboard { needs, keep, sell }
}

fn add_requirement(totals: &mut HashMap<String, u32>, item_id: &str, qty: u32) {
    let entry = totals.entry(item_id.to_string()).or_insert(0);
    *entry = entry.saturating_add(qty);
}

fn merge_counts(base: &mut HashMap<String, u32>, other: HashMap<String, u32>) {
    for (item_id, qty) in other {
        let entry = base.entry(item_id).or_insert(0);
        *entry = entry.saturating_add(qty);
    }
}

fn item_name(data: &ArcData, item_id: &str) -> String {
    data.items_by_id
        .get(item_id)
        .map(|item| localized_en(&item.name))
        .unwrap_or_else(|| item_id.to_string())
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

fn quest_name(data: &ArcData, quest_id: &str) -> String {
    data.quests
        .iter()
        .find(|quest| quest.id == quest_id)
        .map(|quest| localized_en(&quest.name))
        .unwrap_or_else(|| quest_id.to_string())
}

fn hideout_name(data: &ArcData, module_id: &str) -> String {
    data.hideout_modules
        .iter()
        .find(|module| module.id == module_id)
        .map(|module| localized_en(&module.name))
        .unwrap_or_else(|| module_id.to_string())
}

fn project_name(data: &ArcData, project_id: &str) -> String {
    data.projects
        .iter()
        .find(|project| project.id == project_id)
        .map(|project| localized_en(&project.name))
        .unwrap_or_else(|| project_id.to_string())
}

fn module_max_level(data: &ArcData, module_id: &str) -> Option<u32> {
    data.hideout_modules
        .iter()
        .find(|module| module.id == module_id)
        .map(|module| {
            let level_max = module
                .levels
                .iter()
                .map(|level| level.level)
                .max()
                .unwrap_or(0);
            level_max.max(module.max_level)
        })
}

fn project_max_phase(data: &ArcData, project_id: &str) -> Option<u32> {
    data.projects
        .iter()
        .find(|project| project.id == project_id)
        .map(|project| {
            project
                .phases
                .iter()
                .map(|phase| phase.phase)
                .max()
                .unwrap_or(1)
        })
}

fn localized_en(value: &HashMap<String, String>) -> String {
    value
        .get("en")
        .cloned()
        .or_else(|| value.values().next().cloned())
        .unwrap_or_else(|| "Unknown".to_string())
}

fn parse_u32_or_default(raw: &str, default: u32) -> u32 {
    raw.trim().parse::<u32>().unwrap_or(default)
}

fn default_start_phase() -> u32 {
    1
}

fn value_as_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(num) => num.as_u64().and_then(|v| u32::try_from(v).ok()),
        Value::String(text) => text.parse::<u32>().ok(),
        _ => None,
    }
}

fn value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        _ => None,
    }
}

fn mask_key(key: &str) -> String {
    if key.trim().is_empty() {
        return "(not set)".to_string();
    }

    if key.len() <= 10 {
        return "(loaded)".to_string();
    }

    let prefix = &key[..6];
    let suffix = &key[key.len().saturating_sub(4)..];
    format!("{prefix}***{suffix}")
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn cache_root_dir() -> PathBuf {
    if let Some(custom) = first_non_empty_env(&["ARC_CACHE_DIR"]) {
        return PathBuf::from(custom);
    }
    PathBuf::from("cache")
}

fn cache_file_path(namespace: &str, key: &str) -> PathBuf {
    cache_root_dir()
        .join(namespace)
        .join(format!("{}.json", short_hash(key)))
}

fn short_hash(value: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn read_cache_typed<T>(namespace: &str, key: &str, max_age: Option<Duration>) -> Option<T>
where
    T: DeserializeOwned,
{
    let path = cache_file_path(namespace, key);
    let content = fs::read_to_string(path).ok()?;
    let envelope: CacheEnvelope = serde_json::from_str(&content).ok()?;

    if let Some(max_age) = max_age {
        let now = now_unix_seconds();
        if now.saturating_sub(envelope.saved_at_unix) > max_age.as_secs() {
            return None;
        }
    }

    serde_json::from_value(envelope.value).ok()
}

fn write_cache_typed<T>(namespace: &str, key: &str, value: &T)
where
    T: Serialize,
{
    let value = match serde_json::to_value(value) {
        Ok(value) => value,
        Err(err) => {
            warn!(error = %err, namespace, "cache_write: failed to serialize value");
            return;
        }
    };
    let envelope = CacheEnvelope {
        saved_at_unix: now_unix_seconds(),
        value,
    };
    let output = match serde_json::to_string(&envelope) {
        Ok(output) => output,
        Err(err) => {
            warn!(error = %err, namespace, "cache_write: failed to encode envelope");
            return;
        }
    };

    let path = cache_file_path(namespace, key);
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            warn!(error = %err, "cache_write: failed to create parent directory");
            return;
        }
    }
    if let Err(err) = fs::write(path, output) {
        warn!(error = %err, "cache_write: failed to write cache file");
    }
}

fn tracked_state_path() -> PathBuf {
    cache_root_dir().join(CACHE_FILE_TRACKED_STATE)
}

fn load_tracked_state() -> Option<PersistedTrackedState> {
    let path = tracked_state_path();
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_tracked_state(state: &PersistedTrackedState) -> Result<()> {
    let path = tracked_state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }
    let content = serde_json::to_string(state).context("failed to encode tracked state")?;
    fs::write(&path, content).with_context(|| format!("failed to write '{}'", path.display()))
}

fn static_cache_ttl() -> Duration {
    Duration::from_secs(
        first_non_empty_env(&["ARC_STATIC_CACHE_TTL_SECONDS"])
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_STATIC_CACHE_TTL_SECONDS),
    )
}

fn startup_user_cache_ttl() -> Duration {
    Duration::from_secs(
        first_non_empty_env(&["ARC_STARTUP_USER_CACHE_TTL_SECONDS"])
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_STARTUP_USER_CACHE_TTL_SECONDS),
    )
}

fn image_prefetch_count() -> usize {
    first_non_empty_env(&["ARC_IMAGE_PREFETCH_COUNT"])
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_IMAGE_PREFETCH_COUNT)
}

async fn get_json_cached<T>(
    request: reqwest::RequestBuilder,
    namespace: &str,
    cache_key: &str,
    ttl: Duration,
) -> Result<T>
where
    T: DeserializeOwned,
{
    if let Some(cached_value) = read_cache_typed::<Value>(namespace, cache_key, Some(ttl)) {
        debug!(namespace, cache_key, "get_json_cached: cache hit");
        return serde_json::from_value(cached_value)
            .context("failed to decode cached JSON payload");
    }

    let fetched_value: Value = get_json(request).await?;
    write_cache_typed(namespace, cache_key, &fetched_value);
    serde_json::from_value(fetched_value).context("failed to decode fetched JSON payload")
}

fn normalize_remote_image_url(source: &str) -> Option<String> {
    let source = source.trim();
    if source.is_empty() || source.starts_with("data:") {
        return None;
    }

    if source.starts_with("http://") || source.starts_with("https://") {
        return Some(source.to_string());
    }
    if source.starts_with('/') {
        return Some(format!("{API_BASE}{source}"));
    }
    if source.starts_with("images/") {
        return Some(format!("{API_BASE}/{source}"));
    }
    None
}

fn image_cache_file_path(url: &str) -> PathBuf {
    cache_root_dir()
        .join(CACHE_NAMESPACE_IMAGES)
        .join(format!("{}.bin", short_hash(url)))
}

fn image_mime_type(url: &str) -> &'static str {
    let lower = url.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
    }
}

fn read_cached_remote_image_data_uri(url: &str) -> Option<String> {
    let path = image_cache_file_path(url);
    let bytes = fs::read(path).ok()?;
    let mime = image_mime_type(url);
    Some(format!("data:{mime};base64,{}", BASE64.encode(bytes)))
}

fn write_cached_remote_image(url: &str, bytes: &[u8]) {
    let path = image_cache_file_path(url);
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            warn!(error = %err, "image_cache: failed to create cache directory");
            return;
        }
    }
    if let Err(err) = fs::write(path, bytes) {
        warn!(error = %err, "image_cache: failed to write image");
    }
}

async fn fetch_and_cache_remote_image(client: &Client, url: &str) -> Result<String> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed downloading image '{url}'"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("image fetch failed with HTTP {status}"));
    }
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("failed reading image bytes '{url}'"))?;
    write_cached_remote_image(url, &bytes);
    let mime = image_mime_type(url);
    Ok(format!("data:{mime};base64,{}", BASE64.encode(&bytes)))
}

async fn cache_remote_item_images(client: &Client, items: &mut [Item]) {
    let mut cache_hits = 0usize;
    let mut fetched = 0usize;
    let prefetch_limit = image_prefetch_count();

    for item in items.iter_mut() {
        let Some(source) = item.image_filename.clone() else {
            continue;
        };
        let Some(url) = normalize_remote_image_url(&source) else {
            continue;
        };

        if let Some(data_uri) = read_cached_remote_image_data_uri(&url) {
            item.image_filename = Some(data_uri);
            cache_hits = cache_hits.saturating_add(1);
            continue;
        }

        if fetched >= prefetch_limit {
            continue;
        }

        match fetch_and_cache_remote_image(client, &url).await {
            Ok(data_uri) => {
                item.image_filename = Some(data_uri);
                fetched = fetched.saturating_add(1);
            }
            Err(err) => {
                debug!(url, error = %err, "image_cache: prefetch failed");
            }
        }
    }

    if cache_hits > 0 || fetched > 0 {
        info!(
            cache_hits,
            fetched, prefetch_limit, "image_cache: refreshed item image sources"
        );
    }
}

fn first_non_empty_env(keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| env::var(key).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

const APP_CSS: &str = r#"
* { box-sizing: border-box; }
body {
  margin: 0;
  font-family: "Segoe UI", "Helvetica Neue", sans-serif;
  background: #0e1117;
  color: #e6edf3;
}

.app-shell {
  max-width: 1400px;
  margin: 0 auto;
  padding: 20px;
}

.header h1 {
  margin: 0 0 4px;
  font-size: 30px;
}

.header p {
  margin: 0 0 14px;
  color: #9fb1c5;
}

.panel {
  background: #171b23;
  border: 1px solid #2b3545;
  border-radius: 12px;
  padding: 14px;
  margin-bottom: 14px;
}

h2, h3 {
  margin: 0 0 10px;
}

.muted {
  color: #96a8bb;
  margin-top: 0;
}

.grid-two {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(400px, 1fr));
  gap: 14px;
}

.grid-three {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  gap: 14px;
}

.dashboard-panel {
  border-color: #35557c;
  box-shadow: 0 0 0 1px rgba(72, 121, 176, 0.15), 0 10px 26px rgba(0, 0, 0, 0.25);
}

.dashboard-priority {
  margin-top: 10px;
}

.dashboard-secondary {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
  gap: 14px;
  margin-top: 14px;
}

.dashboard-card {
  background: #111824;
  border: 1px solid #2a3b52;
  border-radius: 10px;
  padding: 10px;
}

.can-sell-card {
  background: linear-gradient(180deg, #182332 0%, #121d2b 100%);
  border-color: #486d98;
}

.can-sell-card h3 {
  color: #b6dbff;
}

.row {
  display: flex;
  gap: 8px;
  margin-bottom: 10px;
}

.actions {
  display: flex;
  gap: 8px;
  margin-top: 10px;
}

input,
select,
button {
  background: #0f141d;
  border: 1px solid #334155;
  color: #e6edf3;
  border-radius: 8px;
  padding: 8px 10px;
}

input,
select {
  flex: 1;
}

button {
  cursor: pointer;
}

button:disabled {
  opacity: 0.55;
  cursor: default;
}

button.danger {
  border-color: #7f1d1d;
  color: #fecaca;
}

.status {
  color: #86efac;
}

.error {
  color: #fda4af;
}

.table {
  width: 100%;
  border-collapse: collapse;
}

.table th,
.table td {
  text-align: left;
  border-bottom: 1px solid #273244;
  padding: 8px 6px;
  vertical-align: top;
}

.table.compact th,
.table.compact td {
  padding: 6px 4px;
  font-size: 13px;
}

.item-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.item-icon {
  width: 20px;
  height: 20px;
  object-fit: contain;
  border-radius: 3px;
}

.diagnostics-table td {
  word-break: break-word;
}

.diagnostics-report {
  margin: 8px 0 0;
  white-space: pre-wrap;
  background: #0d131d;
  border: 1px solid #2a3b52;
  border-radius: 8px;
  padding: 10px;
  max-height: 260px;
  overflow: auto;
}

@media (max-width: 820px) {
  .app-shell {
    padding: 12px;
  }

  .grid-two,
  .dashboard-secondary {
    grid-template-columns: 1fr;
  }

  .row,
  .actions {
    flex-direction: column;
  }
}
"#;
