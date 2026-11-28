//! Turns a panic inside a Tauri command into an ordinary error, so the interface shows a message instead of
//! hanging or losing the app. The panic hook (`panic_log.rs`) still runs first, so each one lands in
//! `panic.log` with its file:line.
//!
//! Two layers, because Tauri runs the two kinds of command differently:
//! - Sync commands run inside the IPC callback on the main thread. An unwind there crosses into WebKit and
//!   aborts the process. [`catch_sync`] wraps the whole generated handler and rejects the call instead.
//! - Async commands run in a task Tauri spawns and never joins. A panic there drops the reply, and the
//!   interface's promise never settles. No hook reaches that task, so each async command wraps its own body
//!   in [`guarded`].
use futures_util::FutureExt;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use tauri::{ipc::Invoke, Runtime};

/// What the interface shows. Fixed text, so `tr()` finds its translation; the details are in `panic.log`.
pub const INTERNAL_ERROR: &str = "出了点内部错误，这次操作没有完成。详情已记在 panic.log 里。";

/// Runs an async command's body; a panic becomes `Err(INTERNAL_ERROR)`.
pub async fn guarded<T>(body: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    AssertUnwindSafe(body).catch_unwind().await.unwrap_or_else(|_| Err(INTERNAL_ERROR.into()))
}

/// Wraps a generated `invoke_handler`: a panic in a sync command rejects that call.
pub fn catch_sync<R: Runtime>(
    handler: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke| {
        let resolver = invoke.resolver.clone();
        match catch_unwind(AssertUnwindSafe(|| handler(invoke))) {
            Ok(found) => found,
            Err(_) => {
                // Tauri's reject panics ("resolver consumed") if the command had already replied: catch that too.
                let _ = catch_unwind(AssertUnwindSafe(|| resolver.reject(INTERNAL_ERROR)));
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn guarded_passes_results_through_and_turns_panics_into_errors() {
        assert_eq!(guarded(async { Ok::<_, String>(7) }).await, Ok(7));
        assert_eq!(guarded(async { Err::<i32, _>("nope".to_string()) }).await, Err("nope".into()));
        let panicked = guarded(async {
            if std::hint::black_box(true) { panic!("boom") }
            Ok::<i32, String>(1)
        }).await;
        assert_eq!(panicked, Err(INTERNAL_ERROR.into()));
    }

    /// Every async command must wrap its body, or a panic leaves the interface waiting forever.
    #[test]
    fn every_async_command_is_guarded() {
        let src = include_str!("lib.rs");
        let mut missing = Vec::new();
        for part in src.split("#[tauri::command]").skip(1) {
            let head = part.trim_start();
            if !head.starts_with("async fn") { continue; }
            let open = head.find('{').unwrap();
            if !head[open + 1..].trim_start().starts_with("guard::guarded(async move {") {
                missing.push(head.lines().next().unwrap_or_default().to_string());
            }
        }
        assert!(missing.is_empty(), "async commands without guard::guarded: {missing:#?}");
    }
}
