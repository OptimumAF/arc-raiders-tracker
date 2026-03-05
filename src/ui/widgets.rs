use dioxus::prelude::*;

use crate::{OperationProgress, Toast, ToastKind};

fn toast_kind_label(kind: &ToastKind) -> &'static str {
    match kind {
        ToastKind::Info => "Info",
        ToastKind::Success => "Success",
        ToastKind::Warning => "Warning",
        ToastKind::Error => "Error",
    }
}

fn toast_kind_class(kind: &ToastKind) -> &'static str {
    match kind {
        ToastKind::Info => "toast-info",
        ToastKind::Success => "toast-success",
        ToastKind::Warning => "toast-warning",
        ToastKind::Error => "toast-error",
    }
}

fn progress_percent(progress: &OperationProgress) -> u32 {
    if progress.indeterminate || progress.total == 0 {
        return 0;
    }
    let pct = (progress.current as f64 / progress.total as f64) * 100.0;
    pct.clamp(0.0, 100.0) as u32
}

#[component]
pub fn ProgressPanel(progress: OperationProgress) -> Element {
    let percent = progress_percent(&progress);
    let fill_style = if progress.indeterminate {
        "width: 40%;".to_string()
    } else {
        format!("width: {percent}%;")
    };

    rsx! {
        div { class: "progress-panel",
            div { class: "progress-top",
                strong { "{progress.label}" }
                if !progress.indeterminate && progress.total > 0 {
                    span { class: "muted", "{progress.current}/{progress.total}" }
                }
            }
            if !progress.detail.is_empty() {
                p { class: "muted progress-detail", "{progress.detail}" }
            }
            div { class: "progress-track",
                div {
                    class: if progress.indeterminate { "progress-fill progress-indeterminate" } else { "progress-fill" },
                    style: "{fill_style}"
                }
            }
        }
    }
}

#[component]
pub fn ToastViewport(toasts: Vec<Toast>) -> Element {
    if toasts.is_empty() {
        return rsx! {};
    }

    rsx! {
        div { class: "toast-viewport",
            for toast in toasts.iter().rev().take(5) {
                div { class: "toast {toast_kind_class(&toast.kind)}",
                    p { class: "dash-num", "{toast_kind_label(&toast.kind)}" }
                    p { "{toast.message}" }
                }
            }
        }
    }
}
