//! Contextual task bar under the selection (fork `Avgilles/photocraft`, not upstream).
//!
//! Classical actions run engine commands. Generative Fill and Remove (AI) run the external
//! `xp-photocraft` bridge, which edits this document back through the control protocol, so
//! PhotoCraft itself holds no network code and no key. The app must be started with
//! `--control` and the bridge's work folder as automation roots (see the bridge's README).
//! "Connection…" sets the ComfyUI server: the bridge tests it and saves it in its own file
//! (`%LOCALAPPDATA%\xp-photocraft\connection.json`), so the address never enters PhotoCraft's prefs.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use egui::{Area, Id, Order, Stroke, TextEdit};
use serde_json::json;

use crate::canvas::ViewXform;
use crate::{PhotocraftApp, menus, theme::Tokens, widgets};

/// The bridge executable, resolved on PATH (`uv tool install` puts it there).
const BRIDGE: &str = "xp-photocraft";
const PROMPT_ID: &str = "selection-taskbar-prompt";
/// The connection panel: `None` closed, `Some(url being edited)` open.
const SERVER_ID: &str = "selection-taskbar-server";

/// One bridge run at a time; the bar shows it until the process exits.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

struct Running {
    child: Child,
    label: &'static str,
    busy: &'static str,
    started: Instant,
}

pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if poll_running(app, ctx) {
        return;
    }
    if app.ui.view.hides_chrome() || !app.ui.dialogs.is_empty() || app.jobs.focus.is_some() {
        return;
    }
    let Some(bounds) = app.session.active().and_then(|st| st.doc.selection.as_ref()).map(|s| s.content_bounds()) else { return };
    if bounds.is_empty() {
        return;
    }
    let Some(xf) = ViewXform::active(app) else { return };
    // ponytail: anchored to the selection's bottom centre (unrotated view); follows pan and zoom.
    let below = xf.to_screen(bounds.x0 as f32 + bounds.width() as f32 / 2.0, bounds.y0 as f32 + bounds.height() as f32);
    let canvas = app.last_canvas_rect.shrink(8.0);
    let at = egui::pos2((below.x - 260.0).clamp(canvas.left(), (canvas.right() - 520.0).max(canvas.left())), (below.y + 14.0).clamp(canvas.top(), (canvas.bottom() - 44.0).max(canvas.top())));
    let t = Tokens::get(ctx);
    let mut prompt: String = ctx.data(|d| d.get_temp(Id::new(PROMPT_ID))).unwrap_or_default();
    let mut server: Option<String> = ctx.data(|d| d.get_temp(Id::new(SERVER_ID))).unwrap_or_default();
    let mut clicked: Option<&'static str> = None;
    Area::new(Id::new("selection-taskbar")).order(Order::Foreground).fixed_pos(at).constrain_to(canvas).show(ctx, |ui| {
        egui::Frame::new().fill(t.card).stroke(Stroke::new(1.0, t.card_border)).corner_radius(t.radius).inner_margin(6).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut prompt).hint_text("Describe what to generate…").desired_width(200.0));
                let has_prompt = !prompt.trim().is_empty();
                if ui.add_enabled_ui(has_prompt, |ui| widgets::primary_button(ui, "Generative fill", 0.0)).inner.clicked() {
                    clicked = Some("fill");
                }
                ui.separator();
                for (label, id) in [("Remove", "remove"), ("Remove (AI)", "remove-ai"), ("Inverse", "inverse"), ("Mask", "mask"), ("Deselect", "deselect")] {
                    if widgets::secondary_button(ui, label, 0.0).clicked() {
                        clicked = Some(id);
                    }
                }
                ui.separator();
                if widgets::secondary_button(ui, "Connection…", 0.0).clicked() {
                    clicked = Some("connection");
                }
            });
            if let Some(url) = server.as_mut() {
                ui.horizontal(|ui| {
                    ui.label("ComfyUI server");
                    ui.add(TextEdit::singleline(url).hint_text("http://127.0.0.1:8188").desired_width(260.0));
                    if ui.add_enabled_ui(!url.trim().is_empty(), |ui| widgets::primary_button(ui, "Test & save", 0.0)).inner.clicked() {
                        clicked = Some("connect");
                    }
                    if widgets::secondary_button(ui, "Close", 0.0).clicked() {
                        clicked = Some("close");
                    }
                });
            }
        });
    });
    ctx.data_mut(|d| d.insert_temp(Id::new(PROMPT_ID), prompt.clone()));
    let url = server.clone().unwrap_or_default();
    let panel = match clicked {
        Some("connection") => if server.is_some() { None } else { Some(saved_server()) },
        Some("connect" | "close") => None,
        _ => server,
    };
    ctx.data_mut(|d| d.insert_temp(Id::new(SERVER_ID), panel));
    match clicked {
        Some("fill") => spawn(app, "Generative fill", "running on ComfyUI…", &["fill", "--prompt", prompt.trim()]),
        Some("remove-ai") => spawn(app, "Remove (AI)", "running on ComfyUI…", &["remove"]),
        Some("connect") => spawn(app, "Connection", "testing the server…", &["connect", url.trim()]),
        Some("remove") => run(app, ctx, "edit.contentAwareFill"),
        Some("inverse") => run(app, ctx, "select.inverse"),
        Some("mask") => run(app, ctx, "layer.layerMask.revealSelection"),
        Some("deselect") => run(app, ctx, "select.deselect"),
        _ => {}
    }
}

fn run(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str) {
    if let Err(error) = menus::invoke(app, ctx, id, json!({})) {
        report(app, error);
    }
}

/// The bridge's report goes to a file, not a pipe: a pipe nobody reads until the end blocks
/// the bridge once its buffer fills, and the file survives for diagnosis.
fn log_path() -> std::path::PathBuf {
    std::env::temp_dir().join("xp-photocraft-bar.log")
}

/// The URL the bridge saved last (`connection.json`), or empty: the field then shows the default.
fn saved_server() -> String {
    let Some(base) = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("HOME")) else { return String::new() };
    let path = PathBuf::from(base).join("xp-photocraft").join("connection.json");
    let Ok(text) = std::fs::read_to_string(path) else { return String::new() };
    serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v.get("comfy_url")?.as_str().map(str::to_string)).unwrap_or_default()
}

fn spawn(app: &mut PhotocraftApp, label: &'static str, busy: &'static str, args: &[&str]) {
    let log = match std::fs::File::create(log_path()) {
        Ok(f) => f,
        Err(e) => return report(app, format!("{label}: cannot write {} ({e})", log_path().display())),
    };
    let mut command = Command::new(BRIDGE);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashing over the canvas.
        command.creation_flags(0x0800_0000);
    }
    match command.spawn() {
        Ok(child) => {
            if let Ok(mut slot) = RUNNING.lock() {
                *slot = Some(Running { child, label, busy, started: Instant::now() });
            }
            app.ui.status = format!("{label}: {busy}");
            app.ui.status_error = false;
        }
        Err(e) => report(app, format!("{label}: could not start `{BRIDGE}` ({e}). Install it with `uv tool install`.")),
    }
}

/// Shows the running bridge (and hides the bar) until it exits; true while it runs.
fn poll_running(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
    let Ok(mut slot) = RUNNING.lock() else { return false };
    let Some(running) = slot.as_mut() else { return false };
    match running.child.try_wait() {
        Ok(None) => {
            let elapsed = running.started.elapsed().as_secs();
            app.ui.status = format!("{}: {} {elapsed} s", running.label, running.busy);
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
            true
        }
        Ok(Some(status)) => {
            let label = running.label;
            slot.take();
            drop(slot);
            // Lossy: on Windows a redirected Python writes its report in the console code page.
            let report_text = std::fs::read(log_path()).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            let last = report_text.lines().rev().find(|l| !l.trim().is_empty()).map(str::trim);
            if status.success() {
                app.ui.status = last.map_or_else(|| format!("{label}: done"), str::to_string);
                app.ui.status_error = false;
            } else {
                report(app, format!("{label} failed: {} (full report: {})", last.unwrap_or("no report"), log_path().display()));
            }
            false
        }
        Err(e) => {
            slot.take();
            drop(slot);
            report(app, format!("lost track of the bridge: {e}"));
            false
        }
    }
}

fn report(app: &mut PhotocraftApp, message: String) {
    app.ui.status = message;
    app.ui.status_error = true;
}
