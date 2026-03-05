use dioxus::prelude::*;

use crate::{NeedRow, SellRow};

fn pct(value: u32, max_value: u32) -> u32 {
    if max_value == 0 {
        return 0;
    }
    ((value as f64 / max_value as f64) * 100.0).round() as u32
}

#[component]
fn SummaryCharts(
    need_item_types: u32,
    keep_item_types: u32,
    sell_item_types: u32,
    missing_total: u32,
    keep_total_qty: u32,
    sell_total_qty: u32,
) -> Element {
    let max_type = need_item_types
        .max(keep_item_types)
        .max(sell_item_types)
        .max(1);
    let max_qty = missing_total.max(keep_total_qty).max(sell_total_qty).max(1);

    rsx! {
        div { class: "summary-charts",
            div { class: "chart-card", role: "img", "aria-label": "Item type counts for need, keep, and sell categories",
                h3 { "Category Item Types" }
                div { class: "chart-row",
                    span { "Need" }
                    div { class: "chart-bar-track",
                        div { class: "chart-bar chart-need", style: "width: {pct(need_item_types, max_type)}%;" }
                    }
                    strong { "{need_item_types}" }
                }
                div { class: "chart-row",
                    span { "Keep" }
                    div { class: "chart-bar-track",
                        div { class: "chart-bar chart-keep", style: "width: {pct(keep_item_types, max_type)}%;" }
                    }
                    strong { "{keep_item_types}" }
                }
                div { class: "chart-row",
                    span { "Sell" }
                    div { class: "chart-bar-track",
                        div { class: "chart-bar chart-sell", style: "width: {pct(sell_item_types, max_type)}%;" }
                    }
                    strong { "{sell_item_types}" }
                }
            }
            div { class: "chart-card", role: "img", "aria-label": "Quantity totals for missing, keep, and sell categories",
                h3 { "Quantity Snapshot" }
                div { class: "chart-row",
                    span { "Missing" }
                    div { class: "chart-bar-track",
                        div { class: "chart-bar chart-need", style: "width: {pct(missing_total, max_qty)}%;" }
                    }
                    strong { "{missing_total}" }
                }
                div { class: "chart-row",
                    span { "Keep Qty" }
                    div { class: "chart-bar-track",
                        div { class: "chart-bar chart-keep", style: "width: {pct(keep_total_qty, max_qty)}%;" }
                    }
                    strong { "{keep_total_qty}" }
                }
                div { class: "chart-row",
                    span { "Sell Qty" }
                    div { class: "chart-bar-track",
                        div { class: "chart-bar chart-sell", style: "width: {pct(sell_total_qty, max_qty)}%;" }
                    }
                    strong { "{sell_total_qty}" }
                }
            }
        }
    }
}

#[component]
pub fn DashboardPanel(
    suppress_sell_recommendations: bool,
    requirements_data_issue: String,
    dashboard_filter: String,
    on_dashboard_filter_input: EventHandler<FormEvent>,
    sell_total_qty: u32,
    sell_total_value: u64,
    missing_total: u32,
    need_item_types: u32,
    keep_item_types: u32,
    sell_item_types: u32,
    keep_total_qty: u32,
    sell_rows: Vec<SellRow>,
    need_rows: Vec<NeedRow>,
    keep_rows: Vec<NeedRow>,
) -> Element {
    rsx! {
        div { class: "panel dashboard-panel",
            h2 { "Dashboard" }
            p { class: "muted", "Compares scanned inventory against all tracked requirements. Prioritizing what you can sell first." }
            div { class: "dashboard-toolbar",
                input {
                    value: "{dashboard_filter}",
                    placeholder: "Filter items in dashboard and requirements...",
                    "aria-label": "Filter dashboard items",
                    oninput: move |evt| on_dashboard_filter_input.call(evt),
                }
            }
            div { class: "stats-grid",
                div { class: "stat-chip",
                    p { class: "dash-num", "Sell Qty" }
                    strong { "{sell_total_qty}" }
                }
                div { class: "stat-chip",
                    p { class: "dash-num", "Sell Value" }
                    strong { "{sell_total_value}" }
                }
                div { class: "stat-chip",
                    p { class: "dash-num", "Missing Items" }
                    strong { "{missing_total}" }
                }
            }

            SummaryCharts {
                need_item_types,
                keep_item_types,
                sell_item_types,
                missing_total,
                keep_total_qty,
                sell_total_qty,
            }

            div { class: "dashboard-priority",
                div { class: "dashboard-card can-sell-card",
                    h3 { "Can Sell" }
                    if suppress_sell_recommendations {
                        p {
                            class: "muted",
                            if requirements_data_issue.is_empty() {
                                "Sell suggestions are paused because full progress data is not available."
                            } else {
                                "{requirements_data_issue}"
                            }
                        }
                    }
                    table { class: "table compact", "aria-label": "Items that can be sold",
                        thead { tr { th { "Item" } th { "Qty" } th { "Value" } } }
                        tbody {
                            if suppress_sell_recommendations {
                                tr { td { colspan: "3", class: "muted", "Paused to avoid inaccurate sell recommendations." } }
                            } else if sell_rows.is_empty() {
                                tr { td { colspan: "3", class: "muted", "No excess items to suggest selling." } }
                            }
                            for row in sell_rows.iter().take(40) {
                                tr { key: "{row.name}",
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
                    table { class: "table compact", "aria-label": "Items still needed",
                        thead { tr { th { "Item" } th { "Missing" } th { "Have/Need" } } }
                        tbody {
                            if need_rows.is_empty() {
                                tr { td { colspan: "3", class: "muted", "No missing items based on current tracking." } }
                            }
                            for row in need_rows.iter().take(20) {
                                tr { key: "{row.name}",
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
                    table { class: "table compact", "aria-label": "Items to keep in stash",
                        thead { tr { th { "Item" } th { "Need" } th { "Have" } } }
                        tbody {
                            if keep_rows.is_empty() {
                                tr { td { colspan: "3", class: "muted", "No tracked requirement items yet." } }
                            }
                            for row in keep_rows.iter().take(20) {
                                tr { key: "{row.name}",
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
    }
}
