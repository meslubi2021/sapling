/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::io;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;

use anyhow::Context;
use serde::Deserialize;
use serde::Serialize;

mod chromelike_app;
#[cfg(target_os = "macos")]
mod macos_app;

#[cfg(target_os = "macos")]
pub use macos_app::maybe_become_webview_app;

/// Attempt to open a webview application window and spawn ISL servers to handle it. By default, this function
/// returns without waiting for the webview application. If `browser` is
/// `builtin-webview` on macOS, this function will wait for the webview to
/// close.
///
/// By default, on macOS:
/// - Creates an app bundle with "Sapling" branding.
/// - The current server spawn arguments are written into this app bundle.
/// - The app bundle is launched as new process.
/// - An ISL Server process is spawned by this app process, using the saved server arguments.
/// - The new process uses `webview-sys` to create a browser window, and connect to the server url.
///
/// By default, on Windows and Linux:
/// - An ISL server process is spawned by the current process to get the url for the browser to open.
/// - Try to find a chrome/edge browser and use its `--app` with the url.
pub fn open_isl(opts: ISLSpawnOptions) -> anyhow::Result<()> {
    if should_just_launch_server(&opts) {
        let mut child = opts.spawn_isl_server(false)?;
        child.wait()?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    if opts.browser.is_some() {
        // if --browser=... is passed, use browser instead of macOS app
        chromelike_app::setup_and_spawn_chrome_like(opts)?;
    } else {
        macos_app::setup_and_spawn_app_bundle(opts)?;
    }

    #[cfg(not(target_os = "macos"))]
    chromelike_app::setup_and_spawn_chrome_like(opts)?;

    Ok(())
}

/// Check if the isl spawn options prevent opening a webview/chromelike window.
fn should_just_launch_server(opts: &ISLSpawnOptions) -> bool {
    opts.no_open || opts.kill || opts.no_app
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct ISLSpawnResult {
    port: i32,
    url: String,
    token: String,
    pid: i32,
    was_server_reused: bool,
    log_file_location: String,
    cwd: String,
    command: String,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ISLSpawnOptions {
    pub port: i32,
    pub platform: String,
    pub slcommand: String,
    pub slversion: String,
    /// Path to the repository to open in ISL
    pub repo_cwd: String,
    pub json: bool,
    pub no_open: bool,
    pub foreground: bool,
    pub force: bool,
    pub kill: bool,
    /// Path to the cwd to spawn the isl bundle, from which entrypoint is a valid path.
    pub server_cwd: String,
    pub nodepath: String,
    pub entrypoint: String,
    /// None -> use native app or default chromelike,
    /// "True" -> have node app open OS default browser tab,
    ///  other string path -> launch that path as the browser with --app
    pub browser: Option<String>,
    /// If true, don't spawn the app bundle, just run the server directly and have it open an OS browser tab.
    /// If false (default), spawn with the chromelike --app or in an OS webview application.
    pub no_app: bool,
    pub dev: bool,
}

impl ISLSpawnOptions {
    fn spawn_isl_server(&self, pipe_stdout: bool) -> io::Result<Child> {
        let mut cmd = Command::new(&self.nodepath);
        cmd.current_dir(&self.server_cwd);
        cmd.arg(&self.entrypoint);
        if self.dev {
            // dev mode overrides your port automatically
            cmd.arg("--dev");
        } else {
            cmd.args(["--port", &self.port.to_string()]);
        }
        cmd.args(["--command", &self.slcommand]);
        cmd.args(["--sl-version", &self.slversion]);
        cmd.args(["--cwd", &self.repo_cwd]);
        if self.platform != "browser" {
            cmd.args(["--platform", &self.platform]);
        }
        if self.json {
            cmd.arg("--json");
        }
        if self.no_open {
            cmd.arg("--no-open");
        }
        if self.foreground {
            cmd.arg("--foreground");
        }
        if self.kill {
            cmd.arg("--kill");
        }
        if self.force {
            cmd.arg("--force");
        }
        cmd.stdin(Stdio::null());
        if pipe_stdout {
            cmd.stdout(Stdio::piped());
        }
        cmd.spawn()
    }

    fn spawn_isl_server_json(&self) -> anyhow::Result<ISLSpawnResult> {
        let child = self.spawn_isl_server(true)?;
        let output = child.wait_with_output()?;
        let stdout = String::from_utf8(output.stdout).context("invalid utf-8 from ISL server")?;

        let json = serde_json::from_str::<ISLSpawnResult>(&stdout)
            .context("failed to parse JSON from ISL server")?;
        Ok(json)
    }

    /// Override arguments that make the spawned server compatible with connecting to the webview.
    fn replace_args_for_webview_spawn(self) -> ISLSpawnOptions {
        let mut opts = self.clone();
        opts.json = true;
        // no_open is slightly overloaded: it's used to prevent the app from spawning at all, but also passed
        // into the node server to tell it to open the browser or not.
        // If we've made it to this function call, we assume we've passed the test for opening the app bundle,
        // but if we want to open the app bundle then we shouldn't also open the browser, so we want to forward
        // "true" to the node's no_open option.
        // TODO: it might be better if we move browser opening outside of node and just handle it here instead.
        opts.no_open = true;
        // While we support --foreground for the webview, it does so by spawning the server NOT with --foreground,
        // instead using the JSON to read the log file into the current process, and putting the webview in the foreground.
        opts.foreground = false;
        opts.kill = false;
        opts.platform = "webview".to_owned();
        opts
    }

    /// Override arguments that make the spawned server compatible with connecting to a chromelike browser via --app
    fn replace_args_for_chromelike_spawn(self) -> ISLSpawnOptions {
        let mut opts = self.clone();
        opts.json = true;
        // See replace_args_for_webview_spawn above
        opts.no_open = true;
        opts.foreground = false;
        opts.kill = false;
        opts.platform = "chromelike_app".to_owned();
        opts
    }
}
