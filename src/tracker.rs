//! Background tracking thread.
//!
//! Every `poll_seconds` it asks the OS which window is in front and how long the
//! user has been idle. A line is written to disk **only when the activity changes**
//! (run-length encoding), so a full day is typically 50-150 KB.
//!
//! The in-progress span is checkpointed to `current.tsv` every 30 s, so:
//!   * the chat/report always sees up-to-date data, and
//!   * a crash or power loss loses at most ~30 s.

use crate::{
    aggregate::IDLE,
    config::{self, Config},
    platform, privacy,
    store::{self, Span},
    util,
};
use chrono::{Duration as CDur, Local, NaiveDate, NaiveDateTime, NaiveTime};
use std::{
    sync::{
        mpsc::{self, RecvTimeoutError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const CHECKPOINT_EVERY_SECS: i64 = 5;

/// State shared between the tray (UI thread) and the tracker thread.
#[derive(Default)]
pub struct Shared {
    paused_until: Mutex<Option<NaiveDateTime>>,
}

impl Shared {
    pub fn pause_for(&self, minutes: i64) {
        *self.paused_until.lock().unwrap() = Some(Local::now().naive_local() + CDur::minutes(minutes));
    }

    pub fn pause_until_tomorrow(&self) {
        let tomorrow = Local::now().date_naive() + CDur::days(1);
        *self.paused_until.lock().unwrap() = Some(tomorrow.and_time(NaiveTime::MIN));
    }

    pub fn resume(&self) {
        *self.paused_until.lock().unwrap() = None;
    }

    /// `Some(until)` while paused. Expired pauses are cleared automatically.
    pub fn paused_until(&self) -> Option<NaiveDateTime> {
        let mut guard = self.paused_until.lock().unwrap();
        if guard.is_some_and(|until| Local::now().naive_local() >= until) {
            *guard = None;
        }
        *guard
    }
}

/// Handle used to stop the tracker cleanly (flushes the open span to disk).
pub struct Tracker {
    stop_tx: mpsc::Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl Tracker {
    pub fn stop(mut self) {
        let _ = self.stop_tx.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub fn spawn(shared: Arc<Shared>) -> Tracker {
    let (stop_tx, stop_rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("tracker".into())
        .spawn(move || Loop::new(shared).run(stop_rx))
        .expect("failed to start tracker thread");
    Tracker { stop_tx, handle: Some(handle) }
}

struct Current {
    date: NaiveDate,
    start: NaiveDateTime,
    app: String,
    title: String,
}

struct Loop {
    shared: Arc<Shared>,
    cfg: Config,
    cfg_mtime: Option<std::time::SystemTime>,
    cur: Option<Current>,
    last_tick: NaiveDateTime,
    last_checkpoint: NaiveDateTime,
    senses: crate::senses::SenseCollector,
}

impl Loop {
    fn new(shared: Arc<Shared>) -> Self {
        // Recover a span left behind by a crash / power loss.
        if let Some((date, span)) = store::read_checkpoint() {
            let _ = store::append(date, &span);
            store::clear_checkpoint();
        }
        let cfg = config::load();
        crate::rules::ensure_defaults();
        store::cleanup(cfg.tracking.retention_days);
        let now = Local::now().naive_local();
        Self {
            shared,
            cfg,
            cfg_mtime: config::modified(),
            cur: None,
            last_tick: now,
            last_checkpoint: now,
            senses: crate::senses::SenseCollector::default(),
        }
    }

    fn poll(&self) -> i64 {
        self.cfg.tracking.poll_seconds.clamp(1, 60) as i64
    }

    fn run(mut self, stop_rx: mpsc::Receiver<()>) {
        util::log("tracker started");
        self.tick(Local::now().naive_local());
        let mut ticks: u64 = 0;
        loop {
            match stop_rx.recv_timeout(Duration::from_secs(self.poll() as u64)) {
                Err(RecvTimeoutError::Timeout) => {}
                _ => break, // stop requested or tray gone
            }
            ticks += 1;
            if ticks % 10 == 0 {
                self.reload_config_if_changed();
            }
            self.tick(Local::now().naive_local());
        }
        let now = Local::now().naive_local();
        self.close(now);
        store::clear_checkpoint();
        util::log("tracker stopped");
    }

    fn reload_config_if_changed(&mut self) {
        let mtime = config::modified();
        if mtime != self.cfg_mtime {
            self.cfg = config::load();
            self.cfg_mtime = mtime;
            util::log("config reloaded");
        }
    }

    fn tick(&mut self, now: NaiveDateTime) {
        // Sleep, hibernate or a clock change: end the span where we last saw it.
        let gap = (now - self.last_tick).num_seconds();
        if gap < 0 || gap > (self.poll() * 3).max(30) {
            let last = self.last_tick;
            self.close(last);
            store::clear_checkpoint();
        }
        self.last_tick = now;

        // Midnight: finish yesterday's span and continue it in today's file.
        if let Some(c) = &self.cur {
            if c.date != now.date() {
                let (app, title) = (c.app.clone(), c.title.clone());
                self.close(now);
                self.cur = Some(Current {
                    date: now.date(),
                    start: now.date().and_time(NaiveTime::MIN),
                    app,
                    title,
                });
            }
        }

        match self.observe(now) {
            None => {
                // Paused: record nothing at all.
                if self.cur.is_some() {
                    self.close(now);
                    store::clear_checkpoint();
                }
            }
            Some((app, title, since)) => {
                if app != IDLE {
                    self.senses
                        .poll(&self.cfg.tracking, now.date(), now.time(), &app, &title);
                }
                let same = self
                    .cur
                    .as_ref()
                    .is_some_and(|c| c.app == app && c.title == title);
                if !same {
                    let cut = match &self.cur {
                        Some(c) => since.clamp(c.start, now),
                        None => now,
                    };
                    self.close(cut);
                    self.cur = Some(Current { date: cut.date(), start: cut, app, title });
                    self.checkpoint(now);
                }
            }
        }

        if (now - self.last_checkpoint).num_seconds() >= CHECKPOINT_EVERY_SECS {
            self.checkpoint(now);
        }
    }

    /// What is the user doing right now? `None` = paused.
    /// The third value is when the activity started (used to back-date idle time).
    fn observe(&self, now: NaiveDateTime) -> Option<(String, String, NaiveDateTime)> {
        if self.shared.paused_until().is_some() {
            return None;
        }
        let idle = platform::idle_seconds();
        if idle >= self.cfg.tracking.idle_minutes.max(1) * 60 {
            return Some((IDLE.into(), String::new(), now - CDur::seconds(idle as i64)));
        }
        match platform::foreground() {
            Some(fg) if !fg.app.eq_ignore_ascii_case("LockApp.exe") => {
                let (app, title) = privacy::apply(&self.cfg.tracking, &fg.app, &fg.title);
                Some((app, title, now))
            }
            _ => Some((IDLE.into(), String::new(), now)),
        }
    }

    /// Write the current span (ending at `end`) to disk and clear it.
    fn close(&mut self, end: NaiveDateTime) {
        if let Some(c) = self.cur.take() {
            if let Some(span) = to_span(&c, end) {
                if let Err(e) = store::append(c.date, &span) {
                    util::log(format!("write failed: {e}"));
                }
            }
        }
    }

    fn checkpoint(&mut self, now: NaiveDateTime) {
        self.last_checkpoint = now;
        match self.cur.as_ref().and_then(|c| to_span(c, now).map(|s| (c.date, s))) {
            Some((date, span)) => store::write_checkpoint(date, &span),
            None => {}
        }
    }
}

/// Convert to a storable span, clamping the end to the span's own day.
fn to_span(c: &Current, end: NaiveDateTime) -> Option<Span> {
    let end_time = if end.date() != c.date {
        NaiveTime::from_hms_opt(23, 59, 59).unwrap()
    } else {
        end.time()
    };
    let start_time = c.start.time();
    (end_time > start_time).then(|| Span {
        start: start_time,
        end: end_time,
        app: c.app.clone(),
        title: c.title.clone(),
    })
}
