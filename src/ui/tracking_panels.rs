use std::sync::Arc;

use dioxus::prelude::*;

use crate::{
    ArcData, NeedRow, TrackedCraft, TrackedHideout, TrackedProject, hideout_name, item_name,
    localized_en, module_max_level, parse_u32_or_default, project_max_phase, project_name,
    quest_name,
};

#[component]
pub fn TrackingPanels(
    show_planning_workspace: bool,
    data_snapshot: Option<Arc<ArcData>>,
    craft_pick: Signal<String>,
    craft_qty: Signal<String>,
    tracked_crafts: Signal<Vec<TrackedCraft>>,
    crafts_snapshot: Vec<TrackedCraft>,
    quest_pick: Signal<String>,
    tracked_quests: Signal<Vec<String>>,
    quests_snapshot: Vec<String>,
    hideout_pick: Signal<String>,
    hideout_level: Signal<String>,
    tracked_hideout: Signal<Vec<TrackedHideout>>,
    hideout_snapshot: Vec<TrackedHideout>,
    project_pick: Signal<String>,
    project_phase: Signal<String>,
    tracked_projects: Signal<Vec<TrackedProject>>,
    projects_snapshot: Vec<TrackedProject>,
    required_rows_filtered: Vec<NeedRow>,
) -> Element {
    if !show_planning_workspace {
        return rsx! {
            div { class: "panel subtle-panel",
                h2 { "Planning Workspace Hidden" }
                p { class: "muted", "Use 'Show Planning Workspace' to edit tracked crafts, quests, hideout upgrades, and projects." }
            }
        };
    }

    rsx! {
        div { class: "grid-two",
            div { class: "panel",
                h2 { "Track Crafts" }
                p { class: "muted", "Pick a craftable item and how many outputs you want to build." }
                p { class: "field-label", "Craft item" }
                div { class: "row",
                    select {
                        value: "{craft_pick.read()}",
                        "aria-label": "Craft item selection",
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
                        "aria-label": "Craft quantity",
                        oninput: move |evt| craft_qty.set(evt.value()),
                    }
                    button {
                        onclick: {
                            let craft_pick = craft_pick;
                            let craft_qty = craft_qty;
                            let mut tracked_crafts = tracked_crafts;
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
                    class: "table", "aria-label": "Tracked craft goals",
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
                            tr { key: "{craft.item_id}",
                                td { "{data_snapshot.as_ref().map(|d| item_name(d, &craft.item_id)).unwrap_or_else(|| craft.item_id.clone())}" }
                                td { "{craft.quantity}" }
                                td {
                                    button {
                                        class: "danger",
                                        onclick: {
                                            let mut tracked_crafts = tracked_crafts;
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
                p { class: "field-label", "Quest" }
                div { class: "row",
                    select {
                        value: "{quest_pick.read()}",
                        "aria-label": "Quest selection",
                        onchange: move |evt| quest_pick.set(evt.value()),
                        option { value: "", "Select quest..." }
                        for quest in data_snapshot.as_ref().map(|d| d.quests.clone()).unwrap_or_default() {
                            option { value: "{quest.id}", "{localized_en(&quest.name)}" }
                        }
                    }
                    button {
                        onclick: {
                            let quest_pick = quest_pick;
                            let mut tracked_quests = tracked_quests;
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
                    class: "table", "aria-label": "Tracked quests",
                    thead { tr { th { "Quest" } th { "" } } }
                    tbody {
                        if quests_snapshot.is_empty() {
                            tr { td { colspan: "2", class: "muted", "No tracked quests." } }
                        }
                        for (idx, quest_id) in quests_snapshot.iter().enumerate() {
                            tr { key: "{quest_id}",
                                td { "{data_snapshot.as_ref().map(|d| quest_name(d, quest_id)).unwrap_or_else(|| quest_id.clone())}" }
                                td {
                                    button {
                                        class: "danger",
                                        onclick: {
                                            let mut tracked_quests = tracked_quests;
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
                p { class: "field-label", "Hideout module" }
                div { class: "row",
                    select {
                        value: "{hideout_pick.read()}",
                        "aria-label": "Hideout module selection",
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
                        "aria-label": "Hideout target level",
                        oninput: move |evt| hideout_level.set(evt.value()),
                    }
                    button {
                        onclick: {
                            let hideout_pick = hideout_pick;
                            let hideout_level = hideout_level;
                            let mut tracked_hideout = tracked_hideout;
                            let data_snapshot = data_snapshot.clone();
                            move |_| {
                                let module_id = hideout_pick.read().trim().to_string();
                                if module_id.is_empty() {
                                    return;
                                }

                                let mut level = parse_u32_or_default(&hideout_level.read(), 1).max(1);
                                if let Some(data) = data_snapshot.as_ref()
                                    && let Some(max_level) = module_max_level(data, &module_id)
                                {
                                    level = level.min(max_level.max(1));
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
                    class: "table", "aria-label": "Tracked hideout upgrades",
                    thead { tr { th { "Module" } th { "Target" } th { "" } } }
                    tbody {
                        if hideout_snapshot.is_empty() {
                            tr { td { colspan: "3", class: "muted", "No tracked hideout upgrades." } }
                        }
                        for (idx, entry) in hideout_snapshot.iter().enumerate() {
                            tr { key: "{entry.module_id}",
                                td { "{data_snapshot.as_ref().map(|d| hideout_name(d, &entry.module_id)).unwrap_or_else(|| entry.module_id.clone())}" }
                                td { "L{entry.target_level}" }
                                td {
                                    button {
                                        class: "danger",
                                        onclick: {
                                            let mut tracked_hideout = tracked_hideout;
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
                p { class: "field-label", "Project" }
                div { class: "row",
                    select {
                        value: "{project_pick.read()}",
                        "aria-label": "Project selection",
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
                        "aria-label": "Project target phase",
                        oninput: move |evt| project_phase.set(evt.value()),
                    }
                    button {
                        onclick: {
                            let project_pick = project_pick;
                            let project_phase = project_phase;
                            let mut tracked_projects = tracked_projects;
                            let data_snapshot = data_snapshot.clone();
                            move |_| {
                                let project_id = project_pick.read().trim().to_string();
                                if project_id.is_empty() {
                                    return;
                                }

                                let mut phase = parse_u32_or_default(&project_phase.read(), 1).max(1);
                                if let Some(data) = data_snapshot.as_ref()
                                    && let Some(max_phase) = project_max_phase(data, &project_id)
                                {
                                    phase = phase.min(max_phase.max(1));
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
                    class: "table", "aria-label": "Tracked projects",
                    thead { tr { th { "Project" } th { "Target" } th { "" } } }
                    tbody {
                        if projects_snapshot.is_empty() {
                            tr { td { colspan: "3", class: "muted", "No tracked projects." } }
                        }
                        for (idx, entry) in projects_snapshot.iter().enumerate() {
                            tr { key: "{entry.project_id}",
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
                                            let mut tracked_projects = tracked_projects;
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
            table { class: "table", "aria-label": "All required items",
                thead { tr { th { "Item" } th { "Required" } th { "Have" } th { "Missing" } } }
                tbody {
                    if required_rows_filtered.is_empty() {
                        tr { td { colspan: "4", class: "muted", "No requirements tracked yet." } }
                    }
                    for row in required_rows_filtered.iter() {
                        tr { key: "{row.name}-{row.required}-{row.missing}",
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
    }
}
