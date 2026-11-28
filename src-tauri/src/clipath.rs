//! Whether the folder holding the `subcast` command is on the PATH people's terminals use, and putting it there.
//!
//! The app's own PATH says nothing about that: an app opened from Finder gets the system's minimal PATH, never the
//! one the listener's shell builds from ~/.zshrc and friends. So the shell is asked (a login, interactive shell,
//! the kind a new terminal window opens). When the folder is missing (~/.local/bin isn't on a stock Mac's PATH,
//! and SubCast's folder on Windows is on nobody's), one marked line goes into the shell's startup file, or the
//! folder into the Windows user PATH. Nothing else in those files is touched.
use std::path::{Path, PathBuf};
use parking_lot::Mutex;

/// The marker on the line SubCast adds, so it is added once and can be found again.
#[cfg(unix)]
const MARK: &str = "# Added by SubCast: the subcast command";

static SHELL_PATH: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || std::fs::canonicalize(a).ok().zip(std::fs::canonicalize(b).ok()).is_some_and(|(x, y)| x == y)
}

/// The folders a new terminal window searches (cached; cleared after SubCast changes them).
fn terminal_path() -> Vec<PathBuf> {
    if let Some(p) = SHELL_PATH.lock().clone() { return p; }
    let found = read_terminal_path().unwrap_or_default();
    *SHELL_PATH.lock() = Some(found.clone());
    found
}

/// Runs `cmd` for at most `secs`, returning its standard output.
fn output_within(mut cmd: std::process::Command, secs: u64) -> Option<String> {
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed().as_secs() >= secs => { let _ = child.kill(); let _ = child.wait(); return None; }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}

/// The listener's login shell: $SHELL, which an app opened from Finder may not have, else the account's record.
#[cfg(unix)]
fn shell() -> String {
    if let Some(s) = std::env::var("SHELL").ok().filter(|s| !s.trim().is_empty()) { return s; }
    // SAFETY: getpwuid returns a pointer into static storage or null; pw_shell is a C string when non-null.
    let from_account = unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() { None }
        else { std::ffi::CStr::from_ptr((*pw).pw_shell).to_str().ok().map(str::to_string) }
    };
    from_account.filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into())
}

#[cfg(unix)]
fn read_terminal_path() -> Option<Vec<PathBuf>> {
    let mut cmd = std::process::Command::new(shell());
    // Interactive and login: reads ~/.zprofile and ~/.zshrc (or bash's and fish's equivalents) like a terminal does.
    cmd.args(["-ilc", "printf '\\n__SUBCAST_PATH__%s\\n' \"$PATH\""]);
    let out = output_within(cmd, 6)?;
    let line = out.lines().find_map(|l| l.strip_prefix("__SUBCAST_PATH__"))?;
    // fish prints its PATH list space-separated.
    let parts: Vec<&str> = if line.contains(':') { line.split(':').collect() } else { line.split(' ').collect() };
    Some(parts.into_iter().filter(|s| !s.is_empty()).map(PathBuf::from).collect())
}

#[cfg(windows)]
fn powershell(script: &str, dir: Option<&Path>) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]).creation_flags(0x08000000);
    if let Some(d) = dir { cmd.env("SUBCAST_DIR", d); }
    output_within(cmd, 15)
}

#[cfg(windows)]
fn read_terminal_path() -> Option<Vec<PathBuf>> {
    // New terminals take the user PATH plus the machine's from the registry; this process only has a copy made
    // when it started.
    let out = powershell("[Environment]::GetEnvironmentVariable('Path','User') + ';' + [Environment]::GetEnvironmentVariable('Path','Machine')", None)?;
    Some(out.trim().split(';').filter(|s| !s.is_empty()).map(PathBuf::from).collect())
}

/// Whether a new terminal finds commands in `dir`.
pub fn in_path(dir: &Path) -> bool {
    let process = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|x| same_dir(&x, dir)));
    process || terminal_path().iter().any(|x| same_dir(x, dir))
}

/// The startup file a new terminal of this shell reads, and the line that adds `dir` to its PATH.
#[cfg(unix)]
fn startup_file(dir: &Path) -> Option<(PathBuf, String)> {
    let home = dirs::home_dir()?;
    let shown = match dir.strip_prefix(&home) { Ok(rel) => format!("$HOME/{}", rel.display()), Err(_) => dir.display().to_string() };
    let name = shell().rsplit('/').next().unwrap_or("").to_string();
    Some(match name.as_str() {
        "zsh" => (home.join(".zprofile"), format!("export PATH=\"{shown}:$PATH\"")),
        // bash reads only the first of these that exists.
        "bash" => {
            let file = [".bash_profile", ".bash_login", ".profile"].iter().map(|f| home.join(f)).find(|p| p.exists()).unwrap_or_else(|| home.join(".bash_profile"));
            (file, format!("export PATH=\"{shown}:$PATH\""))
        }
        "fish" => (home.join(".config/fish/conf.d/subcast.fish"), format!("fish_add_path -g \"{}\"", dir.display())),
        _ => (home.join(".profile"), format!("export PATH=\"{shown}:$PATH\"")),
    })
}

/// Puts `dir` on the PATH of new terminals when it isn't. Returns where it was added (a file, or "the user PATH"),
/// or None when nothing was needed or it couldn't be done.
pub fn ensure(dir: &Path) -> Option<String> {
    if in_path(dir) { return None; }
    let added = add(dir)?;
    *SHELL_PATH.lock() = None;
    Some(added)
}

#[cfg(unix)]
fn add(dir: &Path) -> Option<String> {
    let (file, line) = startup_file(dir)?;
    let old = std::fs::read_to_string(&file).unwrap_or_default();
    if old.contains(MARK) { return None; }
    if let Some(parent) = file.parent() { std::fs::create_dir_all(parent).ok()?; }
    let sep = if old.is_empty() || old.ends_with('\n') { "" } else { "\n" };
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&file).ok()?;
    write!(f, "{sep}\n{MARK}\n{line}\n").ok()?;
    Some(file.display().to_string())
}

#[cfg(windows)]
fn add(dir: &Path) -> Option<String> {
    let script = "$d = $env:SUBCAST_DIR; $p = [Environment]::GetEnvironmentVariable('Path','User'); \
        if (-not (($p -split ';') -contains $d)) { \
          $n = if ([string]::IsNullOrEmpty($p)) { $d } else { $p.TrimEnd(';') + ';' + $d }; \
          [Environment]::SetEnvironmentVariable('Path', $n, 'User') }; 'ok'";
    powershell(script, Some(dir)).filter(|o| o.trim() == "ok")?;
    Some("PATH".into())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn the_terminal_path_is_read_and_contains_the_system_folders() {
        let p = super::read_terminal_path().unwrap_or_default();
        assert!(p.is_empty() || p.iter().any(|d| d == std::path::Path::new("/usr/bin")), "{p:?}");
    }
}

#[cfg(all(test, unix))]
mod add_tests {
    /// Changes HOME for the whole test process: run alone (`cargo test --lib clipath -- --ignored`).
    #[test]
    #[ignore]
    fn a_fresh_account_gets_one_line_and_then_finds_the_folder() {
        let home = std::env::temp_dir().join(format!("subcast-home-{}", std::process::id()));
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        // SAFETY: this ignored test runs on its own; nothing else reads the environment meanwhile.
        unsafe { std::env::set_var("HOME", &home); std::env::set_var("SHELL", "/bin/zsh"); }
        assert!(!super::in_path(&bin));
        assert!(super::ensure(&bin).is_some_and(|f| f.ends_with(".zprofile")));
        assert!(super::in_path(&bin), "a new login shell should now see it");
        assert!(super::ensure(&bin).is_none(), "added once");
        let text = std::fs::read_to_string(home.join(".zprofile")).unwrap();
        assert_eq!(text.matches(super::MARK).count(), 1);
        assert!(text.contains("export PATH=\"$HOME/.local/bin:$PATH\""), "{text}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
