//! Usage data: token from Claude Code's credentials file, poll of the OAuth usage endpoint
//! (the one `/usage` reads), pace projection, and the sample history behind the graphs.

use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

pub const FIVE_H: i64 = 5 * 3600;
pub const WEEK: i64 = 7 * 86400;
const URL: &str = "https://api.anthropic.com/api/oauth/usage";
const POLL: i64 = 300; // endpoint is undocumented: be polite
const TICK: Duration = Duration::from_secs(60); // token-file check, no network
const KEEP: i64 = 8 * 86400; // history retention: one weekly window plus a day

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub pct: f64,
    pub resets_at: Option<i64>,
    pub len: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Level {
    Ok,
    Warn,
    Limit,
}

impl Window {
    pub fn start(&self) -> Option<i64> {
        Some(self.resets_at? - self.len)
    }

    /// % at reset if the average burn since the window opened continues.
    /// None until 5% of the window has passed (too little data to mean anything).
    pub fn projected(&self, now: i64) -> Option<f64> {
        let reset = self.resets_at?;
        let elapsed = now - (reset - self.len);
        if now >= reset || (elapsed as f64) < self.len as f64 * 0.05 {
            return None;
        }
        Some(self.pct * self.len as f64 / elapsed as f64)
    }

    /// When 100% is reached at that same rate, if it happens before reset.
    pub fn hits_at(&self, now: i64) -> Option<i64> {
        if self.projected(now)? < 100.0 {
            return None;
        }
        if self.pct >= 100.0 {
            return Some(now);
        }
        let start = self.start()?;
        Some(start + ((now - start) as f64 * 100.0 / self.pct) as i64)
    }

    pub fn level(&self, now: i64) -> Level {
        if self.pct >= 90.0 {
            Level::Limit
        } else if self.hits_at(now).is_some() {
            Level::Warn
        } else {
            Level::Ok
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Usage {
    pub five: Window,
    pub week: Window,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    NoLogin,
    Expired,
    Auth(u16),
    RateLimited,
    Offline(String),
    ApiChanged(String),
}

impl Problem {
    /// One terse line for the widget. Detail goes to widget.log.
    pub fn label(&self) -> &'static str {
        match self {
            Problem::NoLogin => "NOT LOGGED IN - right-click",
            Problem::Expired => "STALE - right-click to log in",
            Problem::Auth(_) => "AUTH FAILED - right-click",
            Problem::RateLimited => "RATE LIMITED - retrying",
            Problem::Offline(_) => "OFFLINE - retrying",
            Problem::ApiChanged(_) => "API CHANGED - see widget.log",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub t: i64,
    pub five: f64,
    pub week: f64,
}

#[derive(Default)]
pub struct Shared {
    pub last: Option<(Usage, i64)>,
    pub problem: Option<Problem>,
    pub samples: Vec<Sample>,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("usage-widget");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn log(msg: &str) {
    let path = data_dir().join("widget.log");
    // Plain truncate at 512 KB, not rotation. Only start lines and errors go here.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 512 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}

/// Starts `claude auth login`, which opens the browser and writes the credentials file this
/// widget reads. Output goes to login.log. The poll thread sees the new token within a minute.
pub fn login() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", "claude", "auth", "login"]).creation_flags(CREATE_NO_WINDOW);
    if let Ok(f) = std::fs::File::create(data_dir().join("login.log")) {
        if let Ok(err) = f.try_clone() {
            cmd.stderr(err);
        }
        cmd.stdout(f);
    }
    match cmd.spawn() {
        Ok(_) => log("login: started claude auth login"),
        Err(e) => log(&format!("login: could not start claude: {e}")),
    }
}

pub fn parse(body: &str) -> Result<Usage, Problem> {
    let bad = |m: String| Problem::ApiChanged(m);
    let v: Value = serde_json::from_str(body).map_err(|e| bad(format!("not JSON: {e}")))?;
    let win = |key: &str, len: i64| -> Result<Window, Problem> {
        let w = match v.get(key) {
            None => return Err(bad(format!("missing {key}"))),
            Some(Value::Null) => return Ok(Window { pct: 0.0, resets_at: None, len }), // no usage yet this window
            Some(w) => w,
        };
        let pct = w
            .get("utilization")
            .and_then(Value::as_f64)
            .filter(|p| *p >= 0.0)
            .ok_or_else(|| bad(format!("{key}.utilization not a number >= 0")))?;
        let resets_at = match w.get("resets_at") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(
                chrono::DateTime::parse_from_rfc3339(s)
                    .map_err(|e| bad(format!("{key}.resets_at {s:?}: {e}")))?
                    .timestamp(),
            ),
            Some(other) => return Err(bad(format!("{key}.resets_at is {other}"))),
        };
        Ok(Window { pct, resets_at, len })
    };
    Ok(Usage { five: win("five_hour", FIVE_H)?, week: win("seven_day", WEEK)? })
}

/// Access token Claude Code keeps refreshed. Read-only: never refreshed here, because a
/// rotated refresh token written by two programs can log Claude Code out.
fn token(now: i64) -> Result<String, Problem> {
    let home = std::env::var_os("USERPROFILE").ok_or(Problem::NoLogin)?;
    let path = PathBuf::from(home).join(".claude").join(".credentials.json");
    let text = std::fs::read_to_string(path).map_err(|_| Problem::NoLogin)?;
    let v: Value = serde_json::from_str(&text).map_err(|_| Problem::NoLogin)?;
    let oauth = v.get("claudeAiOauth").ok_or(Problem::NoLogin)?;
    let tok = oauth.get("accessToken").and_then(Value::as_str).ok_or(Problem::NoLogin)?;
    let expires_ms = oauth.get("expiresAt").and_then(Value::as_i64).unwrap_or(i64::MAX);
    if expires_ms / 1000 <= now + 30 {
        return Err(Problem::Expired);
    }
    Ok(tok.to_string())
}

fn fetch(agent: &ureq::Agent, tok: &str) -> Result<Usage, Problem> {
    let res = agent
        .get(URL)
        .header("Authorization", &format!("Bearer {tok}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .call();
    match res {
        Ok(mut r) => {
            let body = r.body_mut().read_to_string().map_err(|e| Problem::Offline(e.to_string()))?;
            parse(&body)
        }
        Err(ureq::Error::StatusCode(c @ (401 | 403))) => Err(Problem::Auth(c)),
        Err(ureq::Error::StatusCode(429)) => Err(Problem::RateLimited),
        Err(ureq::Error::StatusCode(c)) => Err(Problem::Offline(format!("HTTP {c}"))),
        Err(e) => Err(Problem::Offline(e.to_string())),
    }
}

fn history_path() -> PathBuf {
    data_dir().join("history.csv")
}

/// Loads samples newer than KEEP and rewrites the file if anything was dropped.
fn load_history(now: i64) -> Vec<Sample> {
    let text = std::fs::read_to_string(history_path()).unwrap_or_default();
    let all: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    let keep: Vec<Sample> = all
        .iter()
        .filter_map(|l| {
            let mut f = l.split(',');
            Some(Sample { t: f.next()?.parse().ok()?, five: f.next()?.parse().ok()?, week: f.next()?.parse().ok()? })
        })
        .filter(|s| now - s.t <= KEEP)
        .collect();
    if keep.len() != all.len() {
        let out: String = keep.iter().map(|s| format!("{},{},{}\n", s.t, s.five, s.week)).collect();
        let _ = std::fs::write(history_path(), out);
    }
    keep
}

fn append_history(s: &Sample) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(history_path()) {
        let _ = writeln!(f, "{},{},{}", s.t, s.five, s.week);
    }
}

/// Starts the poll thread. Send `()` on the returned channel to force a fetch now.
pub fn spawn(ctx: eframe::egui::Context) -> (Arc<Mutex<Shared>>, mpsc::Sender<()>) {
    let shared = Arc::new(Mutex::new(Shared { samples: load_history(now()), ..Default::default() }));
    let (tx, rx) = mpsc::channel::<()>();
    let out = shared.clone();
    std::thread::spawn(move || {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .timeout_global(Some(Duration::from_secs(20)))
            .user_agent("usage-widget/0.1")
            .build()
            .into();
        let mut forced = true;
        let mut last_try = 0i64;
        let mut last_tok = String::new();
        loop {
            let t = now();
            let result = match token(t) {
                Err(p) => Some(Err(p)),
                // A new token (Claude Code refreshed it, or a fresh login) means retry now.
                Ok(tok) if forced || t - last_try >= POLL || tok != last_tok => {
                    last_try = t;
                    let r = fetch(&agent, &tok);
                    last_tok = tok;
                    Some(r)
                }
                Ok(_) => None,
            };
            if let Some(r) = result {
                let mut s = out.lock().unwrap_or_else(|e| e.into_inner());
                match r {
                    Ok(u) => {
                        let sample = Sample { t, five: u.five.pct, week: u.week.pct };
                        append_history(&sample);
                        s.samples.push(sample);
                        s.samples.retain(|x| t - x.t <= KEEP);
                        s.last = Some((u, t));
                        s.problem = None;
                    }
                    Err(p) => {
                        if s.problem.as_ref() != Some(&p) {
                            log(&format!("poll: {p:?}"));
                        }
                        s.problem = Some(p);
                    }
                }
                drop(s);
                ctx.request_repaint();
            }
            forced = match rx.recv_timeout(TICK) {
                Ok(()) => true,
                Err(mpsc::RecvTimeoutError::Timeout) => false,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
        }
    });
    (shared, tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from a real response, 2026-10-29.
    const BODY: &str = r#"{"five_hour":{"utilization":25.0,"resets_at":"2026-10-28T23:59:59.908707+00:00","limit_dollars":null},
        "seven_day":{"utilization":17.0,"resets_at":"2026-11-03T12:59:59.908730+00:00"},"seven_day_opus":null,"extra_usage":{"is_enabled":false}}"#;

    #[test]
    fn parses_real_response() {
        let u = parse(BODY).unwrap();
        assert_eq!(u.five.pct, 25.0);
        assert_eq!(u.five.resets_at, Some(1_793_231_999));
        assert_eq!(u.week.pct, 17.0);
        assert_eq!(u.week.len, WEEK);
    }

    #[test]
    fn null_window_is_idle_but_missing_or_wrong_type_is_loud() {
        let u = parse(r#"{"five_hour":null,"seven_day":{"utilization":3,"resets_at":null}}"#).unwrap();
        assert_eq!(u.five, Window { pct: 0.0, resets_at: None, len: FIVE_H });
        assert_eq!(u.week.resets_at, None);
        assert!(matches!(parse(r#"{"seven_day":null}"#), Err(Problem::ApiChanged(_))));
        assert!(matches!(parse(r#"{"five_hour":{"utilization":"25"},"seven_day":null}"#), Err(Problem::ApiChanged(_))));
        assert!(matches!(parse(r#"{"five_hour":{"utilization":-1},"seven_day":null}"#), Err(Problem::ApiChanged(_))));
        assert!(matches!(parse("<html>"), Err(Problem::ApiChanged(_))));
    }

    #[test]
    fn projection_uses_average_burn_since_window_start() {
        let reset = 1_000_000;
        let w = |pct| Window { pct, resets_at: Some(reset), len: FIVE_H };
        let half = reset - FIVE_H / 2;
        // 25% at halfway -> 50% at reset, never hits
        assert_eq!(w(25.0).projected(half), Some(50.0));
        assert_eq!(w(25.0).hits_at(half), None);
        assert_eq!(w(25.0).level(half), Level::Ok);
        // 60% at halfway -> hits 100% at 5/6 of the window
        assert_eq!(w(60.0).hits_at(half), Some(reset - FIVE_H + FIVE_H * 5 / 6));
        assert_eq!(w(60.0).level(half), Level::Warn);
        assert_eq!(w(95.0).level(half), Level::Limit);
        // too early, after reset, no reset time: no projection
        assert_eq!(w(10.0).projected(reset - FIVE_H + 60), None);
        assert_eq!(w(10.0).projected(reset), None);
        assert_eq!(Window { pct: 10.0, resets_at: None, len: FIVE_H }.projected(half), None);
    }
}
