//! Writes every Rust panic to `panic.log` in the app log folder, so a crash report can be read without
//! disassembling the binary. On macOS the folder is `~/Library/Logs/ai.qiaomu.subcast`.
//!
//! Release binaries are stripped, so a backtrace prints only `<unknown>`. The panic location (file:line:col)
//! survives the strip: it is a string inside the binary. So each line keeps the time, the app version, the
//! thread, the message and the location. The default hook still runs after, so stderr keeps its copy.
//!
//! A panic inside a panic hook aborts the process. So every step here ignores its own errors.
use std::io::Write;
use std::path::PathBuf;

/// Past this size the log moves to `panic.log.1` (the older copy is dropped), so it never grows unbounded.
const MAX_BYTES: u64 = 1024 * 1024;

/// The text a panic was raised with (`panic!("…")` gives `&str` or `String`).
pub fn message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload.downcast_ref::<&str>().copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(non-text payload)")
}

/// Installs the hook. Call once, early in `setup`, with the app log folder and the app version.
pub fn install(dir: PathBuf, version: String) {
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("panic.log");
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = message(info.payload());
        let at = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
        let thread = std::thread::current();
        let line = format!(
            "{} v{version} [{}] {msg} @ {at}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            thread.name().unwrap_or("unnamed"),
        );
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
            let _ = std::fs::rename(&path, path.with_extension("log.1"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = f.write_all(line.as_bytes());
        }
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    #[test]
    fn panic_lands_in_the_log_with_its_location() {
        let dir = std::env::temp_dir().join(format!("subcast-panic-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        super::install(dir.clone(), "9.9.9".into());
        let line = line!() + 1;
        let _ = std::panic::catch_unwind(|| panic!("boom {}", 42));
        let log = std::fs::read_to_string(dir.join("panic.log")).unwrap();
        assert!(log.contains("v9.9.9"), "{log}");
        assert!(log.contains("boom 42"), "{log}");
        assert!(log.contains(&format!("panic_log.rs:{line}:")), "{log}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
