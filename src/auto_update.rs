//! Self-update from the rolling GitHub nightly — Zed's mechanism, scaled
//! to one channel: poll the release, compare its `stamp:` against the
//! VOUCH_BUILD_STAMP baked into this binary at bundle time, download the
//! zip, and rsync the new .app over this one. The swap takes effect on
//! the next launch; the running process is untouched.
//!
//! Deliberately inert outside real installs: dev builds have no stamp,
//! and a binary running outside a .app bundle (cargo run) has nothing to
//! safely replace. All I/O shells out to macOS built-ins (curl, ditto,
//! rsync) — no HTTP stack or unzipper in-process, and TLS is the
//! system's problem.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const RELEASE_API: &str = "https://api.github.com/repos/mikayla-maki/Vouch-App/releases/tags/nightly";
const CHECK_EVERY: Duration = Duration::from_secs(4 * 60 * 60);

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
        let mut installed = my_stamp;
        // Let launch finish before touching the network.
        std::thread::sleep(Duration::from_secs(60));
        loop {
            match check_and_install(installed, &app_dir) {
                Ok(Some(new_stamp)) => {
                    eprintln!(
                        "auto-update: nightly {new_stamp} installed — takes effect next launch"
                    );
                    installed = new_stamp;
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

fn check_and_install(installed: u64, app_dir: &Path) -> Result<Option<u64>, String> {
    let release = curl_text(RELEASE_API)?;
    let release: serde_json::Value =
        serde_json::from_str(&release).map_err(|e| format!("release json: {e}"))?;

    let stamp = release["body"]
        .as_str()
        .and_then(|body| body.split("stamp:").nth(1))
        .map(|rest| rest.trim())
        .and_then(|rest| {
            rest.split_whitespace()
                .next()
                .and_then(|s| s.parse::<u64>().ok())
        })
        .ok_or("release body has no readable stamp")?;
    if stamp <= installed {
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

    let new_app = staging.join("Vouch.app");
    if !new_app.join("Contents/MacOS/vouch").exists() {
        return Err("downloaded zip did not contain Vouch.app".into());
    }

    // Trailing slashes: rsync the CONTENTS of the new bundle over the
    // contents of the installed one, deleting what the new build dropped.
    run(
        "rsync",
        &[
            "-a",
            "--delete",
            &format!("{}/", new_app.display()),
            &format!("{}/", app_dir.display()),
        ],
    )?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(Some(stamp))
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
