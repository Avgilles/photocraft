//! Contextual task bar under the selection (fork `Avgilles/photocraft`, not upstream).
//!
//! Classical actions run engine commands. Generative Fill and Remove (AI) run the external
//! `xp-photocraft` bridge, which edits this document back through the control protocol, so
//! PhotoCraft itself holds no network code and no key. The app must be started with
//! `--control` and the bridge's work folder as automation roots (see the bridge's README).
//! The menu bar's "XP" menu (`xp_menu`) sets the ComfyUI server and the workflow of each action.

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
/// The bridge's report, in the temp folder; named without that folder, which holds the user name.
const LOG_NAME: &str = "xp-photocraft-bar.log";

/// One bridge run at a time; the bar shows it until the process exits.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

struct Running {
    child: Child,
    label: &'static str,
    busy: &'static str,
    started: Instant,
}

pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    // A run leaves a placeholder layer and the app free: the bar stays, only the AI waits.
    let running = poll_running(app, ctx);
    // A modal (Unsaved changes, TIFF options, a job's progress…) is no app dialog: check egui's.
    let modal = ctx.memory(|m| m.top_modal_layer()).is_some();
    if app.ui.view.hides_chrome() || !app.ui.dialogs.is_empty() || app.jobs.focus.is_some() || modal {
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
    let mut clicked: Option<&'static str> = None;
    Area::new(Id::new("selection-taskbar")).order(Order::Foreground).fixed_pos(at).constrain_to(canvas).show(ctx, |ui| {
        egui::Frame::new().fill(t.card).stroke(Stroke::new(1.0, t.card_border)).corner_radius(t.radius).inner_margin(6).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut prompt).hint_text("Describe what to generate…").desired_width(200.0));
                let has_prompt = !prompt.trim().is_empty();
                let busy = "one generation at a time: wait for the current one";
                if ui.add_enabled_ui(has_prompt && !running, |ui| widgets::primary_button(ui, "Generative fill", 0.0)).inner.on_disabled_hover_text(if running { busy } else { "describe what to generate first" }).clicked() {
                    clicked = Some("fill");
                }
                ui.separator();
                for (label, id) in [("Remove", "remove"), ("Remove (AI)", "remove-ai"), ("Inverse", "inverse"), ("Mask", "mask"), ("Deselect", "deselect")] {
                    let ai = id == "remove-ai";
                    if ui.add_enabled_ui(!(ai && running), |ui| widgets::secondary_button(ui, label, 0.0)).inner.on_disabled_hover_text(busy).clicked() {
                        clicked = Some(id);
                    }
                }
            });
        });
    });
    ctx.data_mut(|d| d.insert_temp(Id::new(PROMPT_ID), prompt.clone()));
    match clicked {
        Some("fill") => spawn(app, "Generative fill", "generating…", &["fill", "--prompt", prompt.trim()]),
        Some("remove-ai") => spawn(app, "Remove (AI)", "generating…", &["remove"]),
        // Empty params open Content-Aware Fill's dialog; its default sampling runs it at once.
        Some("remove") => run(app, ctx, "edit.contentAwareFill", json!({"sampling": "auto"})),
        Some("inverse") => run(app, ctx, "select.inverse", json!({})),
        Some("mask") => run(app, ctx, "layer.layerMask.revealSelection", json!({})),
        Some("deselect") => run(app, ctx, "select.deselect", json!({})),
        _ => {}
    }
}

fn run(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str, params: serde_json::Value) {
    if let Err(error) = menus::invoke(app, ctx, id, params) {
        report(app, error);
    }
}

/// The bridge's report goes to a file, not a pipe: a pipe nobody reads until the end blocks
/// the bridge once its buffer fills, and the file survives for diagnosis.
fn log_path() -> std::path::PathBuf {
    std::env::temp_dir().join(LOG_NAME)
}

/// The bridge, with no stdin and no console window.
pub(crate) fn bridge_command() -> Command {
    let mut command = Command::new(BRIDGE);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashing over the canvas.
        command.creation_flags(0x0800_0000);
    }
    command
}

/// Run the bridge in the background; the status bar shows it until it exits (one at a time).
pub(crate) fn spawn(app: &mut PhotocraftApp, label: &'static str, busy: &'static str, args: &[&str]) {
    let log = match std::fs::File::create(log_path()) {
        Ok(f) => f,
        Err(e) => return report(app, format!("{label}: cannot write {LOG_NAME} in the temp folder ({e})")),
    };
    let mut command = bridge_command();
    command.args(args).stdout(Stdio::null()).stderr(Stdio::from(log));
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
                report(app, failed(label, last));
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

/// The status line of a failed run: the report's last line, and where the full report is.
fn failed(label: &str, last: Option<&str>) -> String {
    format!("{label} failed: {} (full report: {LOG_NAME} in the temp folder)", last.unwrap_or("no report"))
}

fn report(app: &mut PhotocraftApp, message: String) {
    app.ui.status = message;
    app.ui.status_error = true;
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use serde_json::json;

    use super::failed;
    use crate::PhotocraftApp;

    /// A dirty document with a rectangle selected: the bar shows under it.
    fn app_with_selection() -> Harness<'static, PhotocraftApp> {
        let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(64).build_eframe(|cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
            app.run("file.new", json!({"width": 400, "height": 300})).unwrap();
            app.run("layer.new.layer", json!({})).unwrap();
            app.run("select.rect", json!({"x": 100, "y": 100, "width": 120, "height": 60})).unwrap();
            app
        });
        h.run_steps(6);
        h
    }

    #[test]
    fn the_bar_shows_under_a_selection() {
        let h = app_with_selection();
        assert!(h.query_by_label("Generative fill").is_some());
    }

    #[test]
    fn a_modal_prompt_hides_the_bar() {
        let mut h = app_with_selection();
        // File › Close on a dirty document: "Unsaved changes", an egui modal (not an app dialog).
        assert!(crate::discard_ui::intercept(h.state_mut(), "file.close", &json!({})));
        h.run_steps(4);
        assert!(h.query_by_label("Generative fill").is_none(), "nothing to click above a modal prompt");
    }

    #[test]
    fn a_failure_names_the_report_without_its_folder() {
        let shown = failed("Generative fill", Some("xp-photocraft fill: ComfyUI server unreachable"));
        // The temp folder holds the user name on Windows: the status bar is filmed and shared.
        assert!(!shown.contains(&*std::env::temp_dir().to_string_lossy()), "{shown}");
        assert!(shown.contains("xp-photocraft-bar.log"), "{shown}");
    }
}
