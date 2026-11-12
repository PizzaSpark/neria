//! Signed-in downloads in the main window: "Sign in to …" on a link's problem, the list of
//! signed-in sites in Settings → Network (Sign out), and `cookies.txt` import.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use neria_core::{cookies, probe};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{Accounts, AppWindow, SiteItem, Theme, show_toast};

static OPEN: AtomicBool = AtomicBool::new(false);

fn ago(at: u64) -> String {
    let days = crate::now_secs().saturating_sub(at) / 86_400;
    match days {
        0 => "signed in today".into(),
        1 => "signed in yesterday".into(),
        d => format!("signed in {d} days ago"),
    }
}

/// Fill Settings → Network's list of signed-in sites.
pub fn refresh(ui: &AppWindow) {
    let items: Vec<SiteItem> = cookies::list()
        .into_iter()
        .map(|(site, at)| SiteItem {
            name: cookies::site_name(&site).into(),
            site: site.into(),
            detail: ago(at).into(),
        })
        .collect();
    ui.global::<Accounts>().set_sites(ModelRc::from(Rc::new(VecModel::from(items))));
}

/// The link's problem: offer "Sign in to …" when signing in may help.
pub fn show_for_problem(ui: &AppWindow, url: &str, problem: Option<&probe::LinkProblem>) {
    let acc = ui.global::<Accounts>();
    let site = problem.filter(|p| p.sign_in).and_then(|_| cookies::site_of(url));
    match site {
        Some(site) => {
            acc.set_problem_name(cookies::site_name(&site).into());
            acc.set_problem_again(cookies::signed_in(&site));
            acc.set_problem_site(site.into());
        }
        None => acc.set_problem_site("".into()),
    }
}

/// Look the current link up again (after signing in or importing cookies).
fn reprobe(ui: &AppWindow) {
    if ui.get_probe_state() == 3 {
        let url = ui.get_url();
        ui.invoke_url_edited(url);
    }
}

/// Open the sign-in window for `site` (its own process) and pick up the result.
pub fn start(ui: &AppWindow, site: &str) {
    let Some(site) = cookies::site_of_host(site.trim().trim_start_matches("https://").trim_start_matches("http://"))
    else {
        show_toast(ui, "That doesn't look like a site. Type its address, e.g. vimeo.com.");
        return;
    };
    if OPEN.swap(true, Ordering::SeqCst) {
        show_toast(ui, "A sign-in window is already open.");
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        OPEN.store(false, Ordering::SeqCst);
        return;
    };
    let dark = ui.global::<Theme>().get_dark();
    let child = neria_core::proc::command(exe).args(["--sign-in", &site, if dark { "dark" } else { "light" }]).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => {
            OPEN.store(false, Ordering::SeqCst);
            show_toast(ui, "Couldn't open the sign-in window.");
            return;
        }
    };
    ui.global::<Accounts>().set_busy(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let saved = child.wait().is_ok_and(|s| s.success());
        OPEN.store(false, Ordering::SeqCst);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.global::<Accounts>().set_busy(false);
            refresh(&ui);
            let name = cookies::site_name(&site);
            if saved {
                show_toast(&ui, &format!("Signed in to {name}. Neria uses it for {name} links only."));
                reprobe(&ui);
            } else {
                show_toast(&ui, &format!("Not signed in to {name}, nothing was kept."));
            }
        });
    });
}

pub fn install(ui: &AppWindow) {
    refresh(ui);
    let acc = ui.global::<Accounts>();
    acc.on_sign_in({
        let weak = ui.as_weak();
        move |site| {
            let ui = weak.unwrap();
            start(&ui, &site);
            ui.global::<Accounts>().set_other_site("".into());
        }
    });
    acc.on_sign_out({
        let weak = ui.as_weak();
        move |site| {
            let ui = weak.unwrap();
            let _ = cookies::sign_out(&site);
            // The sign-in window's own browser profile for the site goes too.
            let _ = std::fs::remove_dir_all(crate::signin::profile_dir().join(site.as_str()));
            refresh(&ui);
            show_toast(&ui, &format!("Signed out of {}.", cookies::site_name(&site)));
        }
    });
    acc.on_import_file({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let Some(file) = rfd::FileDialog::new().add_filter("cookies.txt", &["txt"]).pick_file() else { return };
            match cookies::import(&file) {
                Ok(sites) => {
                    refresh(&ui);
                    let names: Vec<String> = sites.iter().map(|s| cookies::site_name(s)).collect();
                    let list = if names.len() > 3 {
                        format!("{} and {} more", names[..3].join(", "), names.len() - 3)
                    } else {
                        names.join(", ")
                    };
                    show_toast(&ui, &format!("Cookies kept for {list}. Each site only gets its own."));
                    reprobe(&ui);
                }
                Err(e) => show_toast(&ui, &format!("Couldn't import it: {e}")),
            }
        }
    });
}
