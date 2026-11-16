#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod i18n;
mod names;
mod ops;
mod store;
mod ui;

use std::path::PathBuf;

use eframe::egui;

const USAGE: &str = "\
claude-code-ln - manage Claude Code session \"soft links\" between organizations
in the Claude desktop app

Usage: claude-code-ln [--root <dir>] [--projects <dir>]

  --root <dir>      session store (default: claude-code-sessions in the desktop
                    app's data directory)
  --projects <dir>  Claude Code transcripts (default: ~/.claude/projects)
";

fn main() -> eframe::Result {
    // A release build has no console on Windows; borrow the parent's for --help and errors.
    #[cfg(all(windows, not(debug_assertions)))]
    {
        unsafe extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        // SAFETY: plain Win32 call; ATTACH_PARENT_PROCESS fails harmlessly without a parent console.
        unsafe { AttachConsole(u32::MAX) };
    }
    let mut root = store::default_root();
    let mut projects = store::default_projects_dir();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--root") => root = args.next().map(PathBuf::from).or(root),
            Some("--projects") => projects = args.next().map(PathBuf::from).or(projects),
            Some("-h" | "--help") => {
                print!("{USAGE}");
                return Ok(());
            }
            _ => {
                eprint!("unknown argument: {}\n\n{USAGE}", arg.to_string_lossy());
                std::process::exit(2);
            }
        }
    }
    let Some(root) = root else {
        eprintln!("Cannot locate the desktop app's data directory; pass --root.");
        std::process::exit(2);
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Claude Code Session Links")
            .with_app_id("claude-code-ln")
            .with_icon(egui::IconData {
                rgba: include_bytes!("../packaging/icon-128.rgba").to_vec(),
                width: 128,
                height: 128,
            })
            .with_inner_size([1280.0, 760.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "claude-code-ln",
        options,
        Box::new(move |_cc| Ok(Box::new(ui::App::new(root, projects)))),
    )
}
