//! Background scheduler thread (ticks every 30 seconds):
//! 1. **8:00 AM Auto-Wakeup (`work_start_time`)**: For PCs left on 24/7, automatically pops up
//!    the Chatbox at 8:00 AM so you can pick or confirm which rules (`workReportFormatword`,
//!    `ExcelReportFormatExcel`, or both) to activate for today.
//! 2. **Hourly Micro-Digests**: Compresses each completed hour into a compact digest + hourly log.
//! 3. **4:40 PM Auto-Report Trigger (`report_trigger_time`)**: Automatically reads today's
//!    recorded activity, typed/copied notes, and read-only Git commits, predicts the 4:40–5:00 PM
//!    completion window, and writes your `.docx` and `.xlsx` reports.

use crate::{config, paths, report, rules, util};
use chrono::{Local, NaiveDate, NaiveTime, Timelike};
use std::{thread, time::Duration};

struct SchedulerState {
    last_hourly_date: Option<NaiveDate>,
    last_hourly_hour: Option<u32>,
    last_morning_popup_date: Option<NaiveDate>,
    last_auto_report_date: Option<NaiveDate>,
}

pub fn spawn() {
    let _ = thread::Builder::new().name("scheduler".into()).spawn(|| {
        rules::ensure_defaults();
        let mut state = SchedulerState {
            last_hourly_date: None,
            last_hourly_hour: None,
            last_morning_popup_date: None,
            last_auto_report_date: None,
        };
        loop {
            tick(&mut state);
            thread::sleep(Duration::from_secs(30));
        }
    });
}

fn tick(state: &mut SchedulerState) {
    let cfg = config::load();
    let now = Local::now();
    let today = now.date_naive();
    let now_time = now.time();

    // 1. Morning 8:00 AM Auto-Popup (for PCs left on 24/7 or started at work start time)
    if cfg.reports.auto_morning_popup && state.last_morning_popup_date != Some(today) {
        let start_time = parse_hhmm(&cfg.reports.work_start_time)
            .unwrap_or_else(|| NaiveTime::from_hms_opt(8, 0, 0).unwrap());
        let start_window_end = start_time + chrono::Duration::minutes(15);

        if now_time >= start_time && now_time <= start_window_end {
            state.last_morning_popup_date = Some(today);
            util::log("8:00 AM work_start_time reached — opening morning rule selector Chatbox");
            util::spawn_chat();
        }
    }

    // 2. 4:40 PM (16:40) Automatic Rule Report Generation (.docx + .xlsx)
    if state.last_auto_report_date != Some(today) {
        let trigger_time = parse_hhmm(&cfg.reports.report_trigger_time)
            .unwrap_or_else(|| NaiveTime::from_hms_opt(16, 40, 0).unwrap());
        let trigger_window_end = trigger_time + chrono::Duration::minutes(30);

        if now_time >= trigger_time && now_time <= trigger_window_end {
            state.last_auto_report_date = Some(today);
            util::log("4:40 PM report_trigger_time reached — generating active rule reports");
            match report::generate_rule_reports(today) {
                Ok(files) => {
                    for f in &files {
                        util::log(format!("Auto-generated rule report: {}", f.display()));
                    }
                    let _ = report::generate_daily(today);
                    if cfg.reports.open_after_generate {
                        util::open_path(&paths::reports_dir());
                    }
                }
                Err(e) => util::log(format!("Auto report trigger error: {e}")),
            }
        }
    }

    // 3. Hourly Log & Micro-Digests: executes strictly ONCE per completed hour
    if cfg.reports.hourly && now.minute() >= 1 {
        let cur_hour = now.hour();
        let already_ran = state.last_hourly_date == Some(today) && state.last_hourly_hour == Some(cur_hour);
        if !already_ran {
            let previous = state
                .last_hourly_date
                .unwrap_or_else(|| today.pred_opt().unwrap_or(today));
            if previous != today {
                report::catch_up_hourly(&cfg, previous, 24);
            }
            report::catch_up_hourly(&cfg, today, cur_hour);
            state.last_hourly_date = Some(today);
            state.last_hourly_hour = Some(cur_hour);
        }
    }
}

fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}
