use std::{
    collections::{HashMap, HashSet},
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod ui;

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
use ui::{ApiPanel, DashboardPanel, ToastViewport, TrackingPanels};

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
];
const CACHE_NAMESPACE_STATIC: &str = "static_api";
const CACHE_NAMESPACE_USER: &str = "user_api";
const CACHE_NAMESPACE_IMAGES: &str = "images";
const CACHE_FILE_TRACKED_STATE: &str = "tracked_state.json";
static API_REQUEST_THROTTLE: OnceLock<ApiRequestThrottle> = OnceLock::new();
static API_RETRY_CONFIG: OnceLock<ApiRetryConfig> = OnceLock::new();
static SELL_FILTER_CONFIG: OnceLock<SellFilterConfig> = OnceLock::new();

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

#[derive(Debug, Clone, Default, PartialEq)]
struct ArcData {
    items_by_id: HashMap<String, Item>,
    craftable_items: Vec<Item>,
    quests: Vec<Quest>,
    hideout_modules: Vec<HideoutModule>,
    projects: Vec<Project>,
    local_images_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Item {
    id: String,
    #[serde(default, rename = "type")]
    item_type: Option<String>,
    #[serde(default)]
    is_weapon: bool,
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

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ItemRequirement {
    item_id: String,
    quantity: u32,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Quest {
    id: String,
    #[serde(default)]
    name: HashMap<String, String>,
    #[serde(default)]
    required_item_ids: Vec<ItemRequirement>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct HideoutLevel {
    level: u32,
    #[serde(default)]
    requirement_item_ids: Vec<ItemRequirement>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ProjectPhase {
    phase: u32,
    #[serde(default)]
    requirement_item_ids: Vec<ItemRequirement>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Default, PartialEq)]
struct NeedRow {
    name: String,
    image_src: String,
    required: u32,
    have: u32,
    missing: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct SellRow {
    name: String,
    image_src: String,
    quantity: u32,
    total_value: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Dashboard {
    needs: Vec<NeedRow>,
    keep: Vec<NeedRow>,
    sell: Vec<SellRow>,
}

#[derive(Debug, Clone, Default, PartialEq)]
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
    #[serde(default = "default_theme_preference")]
    theme_preference: String,
    #[serde(default = "default_show_planning_workspace")]
    show_planning_workspace: bool,
    saved_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEnvelope {
    saved_at_unix: u64,
    value: Value,
}

#[derive(Debug)]
struct SellFilterConfig {
    exclude_weapons: bool,
    excluded_types: HashSet<String>,
}

impl SellFilterConfig {
    fn from_env() -> Self {
        let exclude_weapons = first_non_empty_env(&["ARC_SELL_EXCLUDE_WEAPONS"])
            .and_then(|value| parse_env_bool(&value))
            .unwrap_or(DEFAULT_SELL_EXCLUDE_WEAPONS);

        let excluded_types = first_non_empty_env(&["ARC_SELL_EXCLUDE_TYPES"])
            .map(|raw| parse_csv_lower_set(&raw))
            .filter(|set| !set.is_empty())
            .unwrap_or_else(|| {
                DEFAULT_SELL_EXCLUDE_TYPES
                    .iter()
                    .map(|value| value.to_ascii_lowercase())
                    .collect()
            });

        info!(
            exclude_weapons,
            excluded_type_count = excluded_types.len(),
            "sell_filter: configured can-sell exclusions"
        );

        Self {
            exclude_weapons,
            excluded_types,
        }
    }
}

fn sell_filter_config() -> &'static SellFilterConfig {
    SELL_FILTER_CONFIG.get_or_init(SellFilterConfig::from_env)
}

#[component]
fn App() -> Element {
    let persisted_state = load_tracked_state().unwrap_or_default();
    let initial_tracked_crafts = persisted_state.tracked_crafts.clone();
    let initial_tracked_quests = persisted_state.tracked_quests.clone();
    let initial_tracked_hideout = persisted_state.tracked_hideout.clone();
    let initial_tracked_projects = persisted_state.tracked_projects.clone();
    let initial_theme_preference = normalize_theme_preference(&persisted_state.theme_preference);
    let initial_show_planning_workspace = persisted_state.show_planning_workspace;

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
        let theme_preference = theme_preference.clone();
        let show_planning_workspace = show_planning_workspace.clone();
        use_effect(move || {
            let snapshot = PersistedTrackedState {
                tracked_crafts: tracked_crafts.read().clone(),
                tracked_quests: tracked_quests.read().clone(),
                tracked_hideout: tracked_hideout.read().clone(),
                tracked_projects: tracked_projects.read().clone(),
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
                    button {
                        class: "ghost",
                        onclick: {
                            let mut theme_preference = theme_preference.clone();
                            move |_| {
                                let next = next_theme_preference(theme_preference.read().as_str());
                                theme_preference.set(next);
                            }
                        },
                        "{theme_button_text}"
                    }
                    button {
                        class: "ghost",
                        onclick: {
                            let mut show_planning_workspace = show_planning_workspace.clone();
                            move |_| {
                                let current = *show_planning_workspace.read();
                                show_planning_workspace.set(!current);
                            }
                        },
                        if *show_planning_workspace.read() {
                            "Hide Planning Workspace"
                        } else {
                            "Show Planning Workspace"
                        }
                    }
                }
            }

            main { class: "main-content",
                ApiPanel {
                    app_key_masked: app_key_masked.clone(),
                    user_key: user_key.read().clone(),
                    on_user_key_input: move |evt: FormEvent| user_key.set(evt.value()),
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

async fn run_api_diagnostics<F>(
    client: &Client,
    app_key: &str,
    user_key: &str,
    mut on_step: F,
) -> Result<Vec<ApiDiagnosticRow>>
where
    F: FnMut(usize, usize, &str),
{
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
    let total = endpoints.len();
    for (index, endpoint) in endpoints.iter().enumerate() {
        match get_user_json_value(client, app_key, user_key, endpoint, None).await {
            Ok(payload) => rows.push(ApiDiagnosticRow {
                endpoint: (*endpoint).to_string(),
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
                    endpoint: (*endpoint).to_string(),
                    status_code,
                    request_id: extract_request_id_from_error(&err),
                    detail: truncate_for_report(&err.to_string(), 220),
                    ok: false,
                });
            }
        }
        on_step(index + 1, total, endpoint);
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
            if is_excluded_from_sell(data, item_id) {
                continue;
            }

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

fn is_excluded_from_sell(data: &ArcData, item_id: &str) -> bool {
    let Some(item) = data.items_by_id.get(item_id) else {
        return false;
    };

    let config = sell_filter_config();
    let normalized_type = item
        .item_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);

    if config.exclude_weapons {
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
        return config.excluded_types.contains(item_type);
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

fn default_theme_preference() -> String {
    "system".to_string()
}

fn default_show_planning_workspace() -> bool {
    true
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

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn enqueue_toast(mut toasts: Signal<Vec<Toast>>, kind: ToastKind, message: String) {
    let id = now_unix_millis();
    toasts.write().push(Toast { id, kind, message });

    spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        toasts.write().retain(|toast| toast.id != id);
    });
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

fn normalize_theme_preference(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "dark" => "dark".to_string(),
        "light" => "light".to_string(),
        _ => "system".to_string(),
    }
}

fn next_theme_preference(current: &str) -> String {
    match current {
        "system" => "dark".to_string(),
        "dark" => "light".to_string(),
        _ => "system".to_string(),
    }
}

fn theme_preference_label(theme: &str) -> &'static str {
    match theme {
        "dark" => "Dark",
        "light" => "Light",
        _ => "System",
    }
}

fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" => Some(true),
        "0" | "false" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

fn parse_csv_lower_set(raw: &str) -> HashSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

const APP_CSS: &str = include_str!("../assets/tailwind.css");
