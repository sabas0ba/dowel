//! 構造化ログ。
//!
//! 目的は2つある。
//!
//! 1. **デバッグ** — 依存グラフ、アクショングラフ、併合の経路といった内部状態を
//!    実行しながら観測できること。事後に再現するより安い
//! 2. **段階ごとの所要時間** — cold configure の内訳（docs/20-architecture.md 9節）は
//!    設計判断の根拠になる。計測を後付けにしない
//!
//! 出力先は常に stderr とする。stdout は機械可読な成果物（JSON 診断、
//! `schema dump`、`graph --format=json`）のために空けておく。

use std::io::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum Level {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "silent" => Some(Level::Off),
            "error" => Some(Level::Error),
            "warn" | "warning" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            "trace" => Some(Level::Trace),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// 人間向け。`  12.3ms debug eval  メッセージ`
    Text,
    /// 1行1オブジェクトの JSON。エージェントやログ収集が消費する。
    Json,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Warn as u8);
static FORMAT: AtomicU8 = AtomicU8::new(0);
static COLOR: AtomicU8 = AtomicU8::new(0);
/// プロセス開始からの経過を測る基準点。全スレッドで共有する。
static START: OnceLock<Instant> = OnceLock::new();

fn elapsed_ms() -> f64 {
    START.get().map(|s| s.elapsed().as_nanos() as f64 / 1.0e6).unwrap_or(0.0)
}

/// ログ機構を初期化する。`level` が `None` の場合は環境変数 `DOWEL_LOG` を見る。
pub fn init(level: Option<Level>, format: Format, color: bool) {
    let level = level
        .or_else(|| std::env::var("DOWEL_LOG").ok().and_then(|v| Level::parse(&v)))
        .unwrap_or(Level::Warn);
    LEVEL.store(level as u8, Ordering::Relaxed);
    FORMAT.store(if format == Format::Json { 1 } else { 0 }, Ordering::Relaxed);
    COLOR.store(color as u8, Ordering::Relaxed);
    let _ = START.set(Instant::now());
}

pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        0 => Level::Off,
        1 => Level::Error,
        2 => Level::Warn,
        3 => Level::Info,
        4 => Level::Debug,
        _ => Level::Trace,
    }
}

pub fn enabled(l: Level) -> bool {
    l as u8 <= LEVEL.load(Ordering::Relaxed)
}

fn color_enabled() -> bool {
    COLOR.load(Ordering::Relaxed) == 1
}

fn level_color(l: Level) -> &'static str {
    match l {
        Level::Error => "\x1b[31m",
        Level::Warn => "\x1b[33m",
        Level::Info => "\x1b[32m",
        Level::Debug => "\x1b[36m",
        _ => "\x1b[90m",
    }
}

/// マクロからのみ呼ばれる。直接呼ばないこと。
#[doc(hidden)]
pub fn __log(level: Level, target: &str, args: std::fmt::Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    let target = short_target(target);
    let msg = args.to_string();
    let mut err = std::io::stderr().lock();
    if FORMAT.load(Ordering::Relaxed) == 1 {
        let mut w = crate::json::JsonWriter::new();
        w.begin_object();
        w.field_str("level", level.label());
        w.field_str("target", target);
        w.key("elapsed_ms").str(&format!("{:.3}", elapsed_ms()));
        w.field_str("message", &msg);
        w.end_object();
        let _ = writeln!(err, "{}", w.finish());
    } else if color_enabled() {
        let _ = writeln!(
            err,
            "{:>9.1}ms {}{:<5}\x1b[0m \x1b[90m{:<12}\x1b[0m {}",
            elapsed_ms(),
            level_color(level),
            level.label(),
            target,
            msg
        );
    } else {
        let _ =
            writeln!(err, "{:>9.1}ms {:<5} {:<12} {}", elapsed_ms(), level.label(), target, msg);
    }
}

/// `dowel_eval::interface` のようなモジュールパスから末尾側の見出しを作る。
fn short_target(module_path: &str) -> &str {
    module_path.rsplit("::").next().unwrap_or(module_path)
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::log::__log($crate::log::Level::Error, module_path!(), format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::log::__log($crate::log::Level::Warn, module_path!(), format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::log::__log($crate::log::Level::Info, module_path!(), format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::log::__log($crate::log::Level::Debug, module_path!(), format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => {
        $crate::log::__log($crate::log::Level::Trace, module_path!(), format_args!($($arg)*))
    };
}

/// 段階の所要時間を測る。`Drop` で終了を記録するため、
/// 途中で早期 return しても計測が欠落しない。
pub struct Phase {
    name: &'static str,
    start: Instant,
}

impl Phase {
    pub fn start(name: &'static str) -> Phase {
        __log(Level::Debug, "phase", format_args!("→ {name}"));
        Phase { name, start: Instant::now() }
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        let ms = self.start.elapsed().as_nanos() as f64 / 1.0e6;
        __log(Level::Debug, "phase", format_args!("← {} ({:.2}ms)", self.name, ms));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn レベル文字列を解釈する() {
        assert_eq!(Level::parse("DEBUG"), Some(Level::Debug));
        assert_eq!(Level::parse(" trace "), Some(Level::Trace));
        assert_eq!(Level::parse("off"), Some(Level::Off));
        assert_eq!(Level::parse("verbose"), None);
    }

    #[test]
    fn レベルは順序を持つ() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Trace);
    }

    #[test]
    fn short_target_は末尾のモジュール名を返す() {
        assert_eq!(short_target("dowel_eval::interface"), "interface");
        assert_eq!(short_target("main"), "main");
    }
}
