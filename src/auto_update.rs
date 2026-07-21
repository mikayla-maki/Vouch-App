//! Self-update from the rolling GitHub nightly — Zed's mechanism, scaled
//! to one channel: poll the release, compare its `stamp:` against the
//! VOUCH_BUILD_STAMP baked into this binary at bundle time, download the
//! zip, and stage the new .app in a temp directory. The installed bundle
//! is NOT touched until the user asks: the sidebar shows "restart to
//! update", and [`restart_into_update`] does the swap and relaunch in one
//! deliberate step. An update is never something that happened to you
//! mid-session.
//!
//! Deliberately inert outside real installs: dev builds have no stamp,
//! and a binary running outside a .app bundle (cargo run) has nothing to
//! safely replace. All I/O shells out to macOS built-ins (curl, ditto,
//! rsync) — no HTTP stack or unzipper in-process, and TLS is the
//! system's problem.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

const RELEASE_API: &str = "https://api.github.com/repos/mikayla-maki/Vouch-App/releases/tags/nightly";
const CHECK_EVERY: Duration = Duration::from_secs(4 * 60 * 60);

/// A downloaded, validated update waiting for the user to restart into
/// it. Staging lives in the temp dir: if the user quits without
/// restarting, nothing was changed and the next session simply stages
/// again.
struct Staged {
    stamp: u64,
    new_app: PathBuf,
    app_dir: PathBuf,
}

static STAGED: Mutex<Option<Staged>> = Mutex::new(None);

/// The staged-and-ready nightly stamp, if any — what the sidebar's
/// "restart to update" button keys off. `None` until a newer build has
/// been fully downloaded and validated.
pub fn ready() -> Option<u64> {
    STAGED.lock().ok()?.as_ref().map(|s| s.stamp)
}

/// The user said go: swap the staged bundle over the installed one and
/// arrange the relaunch. On `Ok` the caller should quit the app — a
/// detached helper waits for this process to exit and then reopens the
/// (now new) bundle. On `Err` nothing was changed except possibly a
/// partially-updated bundle from a failed rsync, which the next launch's
/// updater heals by re-staging.
pub fn restart_into_update() -> Result<(), String> {
    let staged = STAGED
        .lock()
        .map_err(|_| "updater state poisoned")?
        .take()
        .ok_or("no update staged")?;

    // Trailing slashes: rsync the CONTENTS of the new bundle over the
    // contents of the installed one, deleting what the new build dropped.
    // The running process keeps executing its already-mapped binary; the
    // swap takes effect at the relaunch we're about to cause.
    run(
        "rsync",
        &[
            "-a",
            "--delete",
            &format!("{}/", staged.new_app.display()),
            &format!("{}/", staged.app_dir.display()),
        ],
    )?;
    let _ = std::fs::remove_dir_all(staged.new_app.parent().unwrap_or(&staged.new_app));

    // Relaunch after we're gone: `open` refuses a second instance while
    // this one lives, so a detached shell waits for this pid to exit
    // first. If the helper dies, worst case the user reopens by hand.
    let waiter = format!(
        "while /bin/kill -0 {pid} 2>/dev/null; do /bin/sleep 0.2; done; /usr/bin/open '{app}'",
        pid = std::process::id(),
        app = staged.app_dir.display(),
    );
    Command::new("/bin/sh")
        .args(["-c", &waiter])
        .spawn()
        .map_err(|e| format!("relaunch helper: {e}"))?;
    Ok(())
}

/// Start the updater thread if this build can update itself at all.
pub fn spawn() {
    let Some(my_stamp) = option_env!("VOUCH_BUILD_STAMP").and_then(|s| s.parse::<u64>().ok())
    else {
        return; // dev build: no identity to compare, nothing to do
    };
    let Some(app_dir) = installed_app_dir() else {
        return; // not running from a .app bundle: nothing safe to replace
    };

    std::thread::spawn(move || {
        let mut have = my_stamp;
        // Let launch finish before touching the network.
        std::thread::sleep(Duration::from_secs(60));
        loop {
            match check_and_stage(have, &app_dir) {
                Ok(Some(new_stamp)) => {
                    eprintln!("auto-update: nightly {new_stamp} staged — restart to update");
                    have = new_stamp;
                }
                Ok(None) => {}
                Err(e) => eprintln!("auto-update: {e}"),
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

/// The .app this process runs from, if it is one:
/// `Vouch.app/Contents/MacOS/vouch` → `Vouch.app`.
fn installed_app_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension()? == "app").then(|| app.to_path_buf())
}

fn check_and_stage(have: u64, app_dir: &Path) -> Result<Option<u64>, String> {
    let release = curl_text(RELEASE_API)?;
    let release: serde_json::Value =
        serde_json::from_str(&release).map_err(|e| format!("release json: {e}"))?;

    let stamp = release["body"]
        .as_str()
        .and_then(stamp_from_release_body)
        .ok_or("release body has no readable stamp")?;
    if stamp <= have {
        return Ok(None);
    }

    let url = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str() == Some("Vouch.zip"))
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or("nightly release has no Vouch.zip asset")?
        .to_string();

    eprintln!("auto-update: downloading nightly {stamp}");
    let staging = std::env::temp_dir().join(format!("vouch-update-{stamp}"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("staging dir: {e}"))?;
    let zip = staging.join("Vouch.zip");

    run(
        "curl",
        &[
            "-fsSL",
            "--max-time",
            "600",
            "-o",
            zip.to_str().ok_or("bad zip path")?,
            &url,
        ],
    )?;
    run(
        "ditto",
        &[
            "-x",
            "-k",
            zip.to_str().ok_or("bad zip path")?,
            staging.to_str().ok_or("bad staging path")?,
        ],
    )?;
    let _ = std::fs::remove_file(&zip);

    let new_app = staging.join("Vouch.app");
    if !new_app.join("Contents/MacOS/vouch").exists() {
        return Err("downloaded zip did not contain Vouch.app".into());
    }

    // Ready: publish it for the UI and keep this process's hands off the
    // installed bundle. A newer nightly landing before the user restarts
    // simply replaces the staged one.
    *STAGED.lock().map_err(|_| "updater state poisoned")? = Some(Staged {
        stamp,
        new_app,
        app_dir: app_dir.to_path_buf(),
    });
    Ok(Some(stamp))
}

/// `stamp: <u64>` anywhere in the nightly release's body text.
fn stamp_from_release_body(body: &str) -> Option<u64> {
    body.split("stamp:")
        .nth(1)?
        .trim()
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn curl_text(url: &str) -> Result<String, String> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "60",
            "-H",
            "User-Agent: vouch-app",
            url,
        ])
        .output()
        .map_err(|e| format!("curl spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("curl output: {e}"))
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program} spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_stamp_parses_from_the_body_text() {
        assert_eq!(
            stamp_from_release_body("Nightly build\n\nstamp: 1752900000\n"),
            Some(1_752_900_000)
        );
        assert_eq!(stamp_from_release_body("stamp:42"), Some(42));
        assert_eq!(stamp_from_release_body("no stamp here"), None);
        assert_eq!(stamp_from_release_body("stamp: not-a-number"), None);
    }

    #[test]
    fn nothing_is_ready_and_restart_refuses_until_something_is_staged() {
        assert_eq!(ready(), None);
        assert!(restart_into_update().is_err());
    }
}
