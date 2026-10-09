//! "XP" menu, last in the menu bar (fork `Avgilles/photocraft`, not upstream): the workflow each
//! AI action of the selection task bar runs, and the `xp-photocraft` bridge's ComfyUI server.
//!
//! Everything goes through the bridge's CLI (`workflows`, `assign`, `connect`); PhotoCraft keeps
//! no address, no command and no key. The server field is the only place the address shows.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use egui::containers::menu::MenuConfig;
use egui::{Id, TextEdit};
use serde::Deserialize;

use crate::{selection_taskbar, widgets};

const SERVER_ID: &str = "xp-menu-server";
/// The actions the bridge assigns workflows to, with their task bar labels.
const ACTIONS: [(&str, &str); 2] = [("fill", "Generative fill"), ("remove", "Remove (AI)")];

/// The bridge's last `workflows@1` answer (`workflows` and `assign` both print it).
static LISTING: Mutex<Option<Listing>> = Mutex::new(None);
static QUERYING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Default, Deserialize)]
struct Listing {
    ok: bool,
    #[serde(default)]
    workflows: Vec<Choice>,
    #[serde(default)]
    assign: BTreeMap<String, String>,
    /// The saved ComfyUI server (the bridge owns where it is saved).
    #[serde(default)]
    server: String,
    #[serde(default)]
    error: String,
}

#[derive(Clone, Deserialize)]
struct Choice {
    id: String,
    label: String,
}

/// The "XP" title and its menu, styled like the bar's other menus. Returns the title and the
/// server to test and save when "Test & save" was clicked (the caller runs it: it needs the app).
pub fn menu(ui: &mut egui::Ui, color: egui::Color32) -> (egui::Response, Option<String>) {
    let title = ui.add(egui::Button::new(egui::RichText::new("XP").color(color)));
    let bar = MenuConfig::find(ui);
    let config = MenuConfig::new().close_behavior(bar.close_behavior).style(bar.style.clone());
    let mut connect = None;
    egui::Popup::menu(&title)
        // Like Help's search field: a click in the server field must not close the menu.
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .style(config.style.clone())
        .info(egui::UiStackInfo::new(egui::UiKind::Menu).with_tag_value(MenuConfig::MENU_CONFIG_TAG, config))
        .show(|ui| {
            ui.set_min_width(280.0);
            connect = body(ui);
        });
    (title, connect)
}

fn body(ui: &mut egui::Ui) -> Option<String> {
    let ctx = ui.ctx().clone();
    let listing = LISTING.lock().ok().and_then(|l| l.clone());
    match &listing {
        None => {
            query(&ctx, &["workflows"]);
            ui.weak("Asking the bridge…");
        }
        Some(l) if !l.ok => {
            ui.colored_label(ui.visuals().error_fg_color, &l.error);
        }
        Some(l) => workflow_menus(ui, &ctx, l),
    }
    if ui.add_enabled(!QUERYING.load(Ordering::Relaxed), egui::Button::new("Reload Workflows")).clicked() {
        query(&ctx, &["workflows"]);
    }
    ui.separator();
    widgets::section_label(ui, "ComfyUI server");
    let saved = listing.as_ref().map(|l| l.server.clone()).unwrap_or_default();
    let mut url: String = ctx.data(|d| d.get_temp(Id::new(SERVER_ID))).unwrap_or(saved);
    let mut connect = None;
    ui.horizontal(|ui| {
        ui.add(TextEdit::singleline(&mut url).hint_text("http://127.0.0.1:8188").desired_width(170.0));
        if ui.add_enabled(!url.trim().is_empty(), egui::Button::new("Test & save")).clicked() {
            connect = Some(url.trim().to_string());
            ui.close();
        }
    });
    if listing.is_some() {
        ctx.data_mut(|d| d.insert_temp(Id::new(SERVER_ID), url));
    }
    connect
}

/// One submenu per action, "Generative fill: <workflow>", listing the workflows to pick from.
fn workflow_menus(ui: &mut egui::Ui, ctx: &egui::Context, listing: &Listing) {
    for (action, label) in ACTIONS {
        let current = listing.assign.get(action).map(String::as_str).unwrap_or_default();
        let shown = listing.workflows.iter().find(|c| c.id == current).map_or(current, |c| c.label.as_str());
        ui.menu_button(format!("{label}: {shown}"), |ui| {
            for choice in &listing.workflows {
                if ui.selectable_label(choice.id == current, &choice.label).clicked() {
                    query(ctx, &["assign", action, &choice.id]);
                    ui.close();
                }
            }
        });
    }
}

/// Ask the bridge in the background (it takes a few hundred ms to start); the answer replaces
/// [`LISTING`] and repaints. One question at a time.
fn query(ctx: &egui::Context, args: &[&str]) {
    if QUERYING.swap(true, Ordering::SeqCst) {
        return;
    }
    let (ctx, args): (egui::Context, Vec<String>) = (ctx.clone(), args.iter().map(|a| a.to_string()).collect());
    std::thread::spawn(move || {
        let listing = match selection_taskbar::bridge_command().args(&args).stderr(std::process::Stdio::null()).output() {
            Ok(out) => serde_json::from_slice(out.stdout.trim_ascii())
                .unwrap_or_else(|_| Listing { error: "the bridge gave no workflow list (update xp-photocraft)".into(), ..Listing::default() }),
            Err(e) => Listing { error: format!("could not start `xp-photocraft` ({e})"), ..Listing::default() },
        };
        if let Ok(mut slot) = LISTING.lock() {
            *slot = Some(listing);
        }
        QUERYING.store(false, Ordering::SeqCst);
        ctx.request_repaint();
    });
}
