// Always hide the black CMD console window (this is a tray + GUI app).
#![windows_subsystem = "windows"]

//! DayRecap — a lightweight personal AI Reporter & Secretary in the system tray.
//! Tracks active windows, typed/copied work snippets, and read-only Git commits,
//! and generates hourly & daily reports in `.txt`, `.docx` (Word), and `.xlsx` (Excel).
//!
//! One binary, modes:
//!   dayrecap              -> tray daemon (tracker + senses + 8:00 AM / 4:40 PM scheduler), ~5-10 MB RAM
//!   dayrecap --chat       -> native chat & rule manager window (spawned on demand, exits on close)
//!   dayrecap --report [YYYY-MM-DD|today|yesterday] -> generate daily + active Word/Excel reports and exit
//!   dayrecap --rules [YYYY-MM-DD|today|yesterday]  -> generate active Word (.docx) & Excel (.xlsx) rule reports

mod aggregate;
mod attachment;
mod autostart;
mod chat_ui;
mod config;
mod icon;
mod llm;
mod office;
mod paths;
mod platform;
mod privacy;
mod report;
mod rules;
mod scheduler;
mod scrubber;
mod senses;
mod store;
mod tracker;
mod tray;
mod util;

use chrono::{Local, NaiveDate};

fn main() {
    std::panic::set_hook(Box::new(|info| util::log(format!("PANIC: {info}"))));

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--chat") => chat_ui::run(),
        Some("--report") => {
            let date = parse_date(args.get(1).map(String::as_str));
            match report::generate_daily(date) {
                Ok(path) => println!("{}", path.display()),
                Err(e) => {
                    eprintln!("Report failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("--rules") => {
            let date = parse_date(args.get(1).map(String::as_str));
            match report::generate_rule_reports(date) {
                Ok(files) => {
                    for f in files {
                        println!("{}", f.display());
                    }
                }
                Err(e) => {
                    eprintln!("Rule reports failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        _ => tray::run(),
    }
}

fn parse_date(arg: Option<&str>) -> NaiveDate {
    let today = Local::now().date_naive();
    match arg {
        None | Some("today") => today,
        Some("yesterday") => today.pred_opt().unwrap_or(today),
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or(today),
    }
}
