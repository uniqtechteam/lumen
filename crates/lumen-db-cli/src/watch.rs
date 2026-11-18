//! `lumen generate --watch` (SPEC-GEN-001 FR-006): write the client, then again each time the
//! schema file changes. The schema's folder is watched, so an editor that saves through a
//! temporary file and a rename is seen. Changes are debounced; a save that leaves the text as it
//! was is skipped; a broken schema prints its problems and the watch goes on; Ctrl-C stops it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use notify::{Event, RecursiveMode, Watcher};

use crate::cmd;
use crate::project::{self, Fail};

/// How long after the last change a run starts.
pub const QUIET: Duration = Duration::from_millis(150);

/// When to run, given the times changes were seen. Pure, so it is tested without sleeping.
#[derive(Debug, Default)]
pub struct Debounce {
    last_change: Option<Instant>,
}

impl Debounce {
    /// A change was seen at `now`.
    pub fn change(&mut self, now: Instant) {
        self.last_change = Some(now);
    }

    /// When the pending run is due, if one is pending.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.last_change.map(|t| t + QUIET)
    }

    /// Whether a run is due at `now`; if so, it is taken.
    pub fn take(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|d| now >= d) {
            self.last_change = None;
            true
        } else {
            false
        }
    }
}

/// Whether `event` touches the file `schema` (by name, in the watched folder).
fn touches(event: &Event, schema: &Path) -> bool {
    let name = schema.file_name();
    event.paths.iter().any(|p| p.file_name() == name)
}

/// One run: read, check, write. The line to print.
fn run(path: &Path, output: Option<&Path>, last: &mut Option<String>) -> Option<String> {
    let started = Instant::now();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Some(format!("cannot read {}; waiting for it", path.display()));
    };
    if last.as_deref() == Some(text.as_str()) {
        return None;
    }
    *last = Some(text);
    let result = project::load(path).and_then(|(_, schema)| cmd::client(path, &schema, output));
    Some(match result {
        Ok(line) => format!("{line} ({} ms)", started.elapsed().as_millis()),
        Err(problems) if problems.starts_with("error") => {
            format!("{problems}\nfix the schema; still watching")
        }
        Err(problem) => format!("error: {problem}\nstill watching"),
    })
}

/// `lumen generate --watch`.
pub async fn generate(given: Option<&Path>, output: Option<&Path>) -> Result<ExitCode, Fail> {
    let path = project::schema_path(given)?;
    let folder: PathBuf = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let schema = path.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        if event.is_ok_and(|e| touches(&e, &schema)) {
            let _ = tx.send(());
        }
    })
    .map_err(|e| format!("cannot watch {}: {e}", folder.display()))?;
    watcher
        .watch(&folder, RecursiveMode::NonRecursive)
        .map_err(|e| format!("cannot watch {}: {e}", folder.display()))?;
    let mut last = None;
    if let Some(line) = run(&path, output, &mut last) {
        eprintln!("{line}");
    }
    eprintln!("watching {} (Ctrl-C to stop)", path.display());
    let mut debounce = Debounce::default();
    loop {
        let wait = debounce.deadline().map(tokio::time::Instant::from_std);
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            seen = rx.recv() => match seen {
                Some(()) => debounce.change(Instant::now()),
                None => break,
            },
            () = async { tokio::time::sleep_until(wait.unwrap_or_else(tokio::time::Instant::now)).await }, if wait.is_some() => {
                if debounce.take(Instant::now())
                    && let Some(line) = run(&path, output, &mut last)
                {
                    eprintln!("{line}");
                }
            }
        }
    }
    drop(watcher);
    eprintln!("stopped watching");
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Debounce, QUIET};

    #[test]
    fn a_burst_of_changes_runs_once_after_the_last() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut d = Debounce::default();
        assert_eq!(d.deadline(), None);
        assert!(!d.take(ms(1000)), "nothing pending");
        // an editor writes a temp file, renames it, touches it: three events
        d.change(ms(0));
        d.change(ms(20));
        d.change(ms(40));
        assert_eq!(d.deadline(), Some(ms(40) + QUIET));
        assert!(!d.take(ms(100)), "too early");
        assert!(d.take(ms(190)), "150 ms after the last change");
        assert!(!d.take(ms(400)), "taken once");
        assert_eq!(d.deadline(), None);
    }

    #[test]
    fn a_change_while_waiting_pushes_the_run_back() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut d = Debounce::default();
        d.change(ms(0));
        d.change(ms(140));
        assert!(!d.take(ms(160)));
        assert!(d.take(ms(290)));
    }
}
