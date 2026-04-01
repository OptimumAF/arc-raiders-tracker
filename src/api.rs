use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    ArcData, HideoutModule, Item, ItemsResponse, Project, ProjectsResponse, Quest, QuestsResponse,
    TrackedHideout, TrackedProject, UserProfileInfo, extract_http_status_code_from_error,
    extract_inventory_counts, extract_known_ids, extract_progress_level_map,
    extract_request_id_from_error, extract_request_id_from_payload, get_json, get_json_cached,
    has_next_page, localized_en, merge_counts, parse_user_profile, read_cache_typed,
    read_cached_remote_image_data_uri, short_hash, truncate_for_report, unwrap_data_ref,
    write_cache_typed, write_cached_remote_image,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct UserSyncResult {
    pub profile: Option<UserProfileInfo>,
    pub stash_counts: Option<HashMap<String, u32>>,
    pub loadout_counts: Option<HashMap<String, u32>>,
    pub tracked_quests: Option<Vec<String>>,
    pub tracked_hideout: Option<Vec<TrackedHideout>>,
    pub tracked_projects: Option<Vec<TrackedProject>>,
    pub quests_synced: bool,
    pub hideout_synced: bool,
    pub projects_synced: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ApiDiagnosticRow {
    pub endpoint: String,
    pub status_code: String,
    pub request_id: Option<String>,
    pub detail: String,
    pub ok: bool,
}

pub(crate) async fn fetch_static_data(client: &Client) -> Result<ArcData> {
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
    let items_url = format!("{}/api/items", crate::API_BASE);
    let quests_url = format!("{}/api/quests", crate::API_BASE);
    let hideout_url = format!("{}/api/hideout", crate::API_BASE);
    let projects_url = format!("{}/api/projects?season=1,2", crate::API_BASE);
    let static_ttl = crate::static_cache_ttl();
    let mut items_resp: ItemsResponse = get_json_cached(
        client.get(items_url),
        crate::CACHE_NAMESPACE_STATIC,
        "items",
        static_ttl,
    )
    .await
    .context("failed to load items dataset")?;
    let quests_resp: QuestsResponse = get_json_cached(
        client.get(quests_url),
        crate::CACHE_NAMESPACE_STATIC,
        "quests",
        static_ttl,
    )
    .await
    .context("failed to load quests dataset")?;
    let hideout_resp: crate::HideoutResponse = get_json_cached(
        client.get(hideout_url),
        crate::CACHE_NAMESPACE_STATIC,
        "hideout",
        static_ttl,
    )
    .await
    .context("failed to load hideout dataset")?;
    let projects_resp: ProjectsResponse = get_json_cached(
        client.get(projects_url),
        crate::CACHE_NAMESPACE_STATIC,
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
    if let Some(configured) = crate::first_non_empty_env(&[
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

    let candidate = PathBuf::from(crate::LOCAL_DATA_DEFAULT_DIR);
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

pub(crate) fn local_item_image_data_uri(images_dir: &Path, item_id: &str) -> Option<String> {
    let candidate = images_dir.join(format!("{item_id}.png"));
    if !candidate.exists() {
        return None;
    }
    let bytes = fs::read(&candidate).ok()?;
    Some(format!("data:image/png;base64,{}", BASE64.encode(bytes)))
}

pub(crate) async fn fetch_stash_inventory(
    client: &Client,
    app_key: &str,
    user_key: &str,
) -> Result<HashMap<String, u32>> {
    fetch_stash_inventory_with_cache(client, app_key, user_key, None).await
}

pub(crate) async fn fetch_stash_inventory_with_cache(
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
    if let Some(ttl) = cache_ttl
        && let Some(cached) = read_cache_typed::<HashMap<String, u32>>(
            crate::CACHE_NAMESPACE_USER,
            &cache_key,
            Some(ttl),
        )
    {
        info!(
            unique_items = cached.len(),
            total_items = cached.values().sum::<u32>(),
            "fetch_stash_inventory: cache hit"
        );
        return Ok(cached);
    }

    info!("fetch_stash_inventory: start");
    let mut all_counts: HashMap<String, u32> = HashMap::new();
    let mut page = 1u32;

    loop {
        let url = format!(
            "{}/api/v2/user/stash?locale=en&page={page}&per_page=500&sort=slot",
            crate::API_BASE
        );

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
        write_cache_typed(crate::CACHE_NAMESPACE_USER, &cache_key, &all_counts);
    }
    Ok(all_counts)
}

pub(crate) async fn get_user_json_value(
    client: &Client,
    app_key: &str,
    user_key: &str,
    path_with_query: &str,
    cache_ttl: Option<Duration>,
) -> Result<Value> {
    debug!(path = path_with_query, "get_user_json_value: request");
    let cache_key = format!("{}_{}", short_hash(user_key), short_hash(path_with_query));
    if let Some(ttl) = cache_ttl
        && let Some(cached) =
            read_cache_typed::<Value>(crate::CACHE_NAMESPACE_USER, &cache_key, Some(ttl))
    {
        debug!(path = path_with_query, "get_user_json_value: cache hit");
        return Ok(cached);
    }

    let url = format!("{}{}", crate::API_BASE, path_with_query);
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
                write_cache_typed(crate::CACHE_NAMESPACE_USER, &cache_key, &value);
            }
            Ok(value)
        }
        Err(err) => {
            if cache_ttl.is_some()
                && let Some(stale) =
                    read_cache_typed::<Value>(crate::CACHE_NAMESPACE_USER, &cache_key, None)
            {
                warn!(
                    path = path_with_query,
                    error = %err,
                    "get_user_json_value: using stale cache after fetch failure"
                );
                return Ok(stale);
            }
            Err(err)
        }
    }
}

pub(crate) async fn run_api_diagnostics<F>(
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

pub(crate) fn build_api_diagnostics_report(rows: &[ApiDiagnosticRow]) -> String {
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

pub(crate) async fn sync_user_progress(
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
            crate::CACHE_NAMESPACE_USER,
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

fn normalize_remote_image_url(source: &str) -> Option<String> {
    let source = source.trim();
    if source.is_empty() || source.starts_with("data:") {
        return None;
    }

    if source.starts_with("http://") || source.starts_with("https://") {
        return Some(source.to_string());
    }
    if source.starts_with('/') {
        return Some(format!("{}{source}", crate::API_BASE));
    }
    if source.starts_with("images/") {
        return Some(format!("{}/{}", crate::API_BASE, source));
    }
    None
}

pub(crate) fn image_mime_type(url: &str) -> &'static str {
    let lower = url.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
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
    let prefetch_limit = crate::image_prefetch_count();

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
