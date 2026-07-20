//! Progress bars for the foreground daemon.
//!
//! Invariant: presentation only — nothing here touches protocol, state, or
//! transfer semantics, and everything degrades to a no-op when stdout is not
//! a terminal (CI, pipes) so daemons behave identically headless. Log lines
//! and bars coexist: tracing output is routed through a writer that suspends
//! the bar area for the duration of each write.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

/// Cheap-to-clone handle over the terminal's progress area.
///
/// `None` inside means disabled: not a TTY, `start` not in the foreground, or
/// tests — every method silently no-ops.
#[derive(Debug, Clone, Default)]
pub struct Ui {
    mp: Option<MultiProgress>,
    colors: bool,
}

impl Ui {
    /// Enables bars only when stdout (and stderr, where bars draw) are real
    /// terminals; honors `NO_COLOR`.
    pub fn detect() -> Self {
        let tty = std::io::stdout().is_terminal() && std::io::stderr().is_terminal();
        if !tty {
            return Self::disabled();
        }
        let colors = std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
        Self {
            mp: Some(MultiProgress::new()),
            colors,
        }
    }

    pub fn disabled() -> Self {
        Self {
            mp: None,
            colors: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.mp.is_some()
    }

    fn bar_style(&self) -> ProgressStyle {
        let template = if self.colors {
            "{msg:32!} {bytes:>10} / {total_bytes:<10} {bytes_per_sec:>12} [{bar:28.cyan/blue}] {percent:>3}%"
        } else {
            "{msg:32!} {bytes:>10} / {total_bytes:<10} {bytes_per_sec:>12} [{bar:28}] {percent:>3}%"
        };
        // The template literal is valid; fall back to the default style
        // rather than panicking if it ever regresses.
        ProgressStyle::with_template(template)
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> ")
    }

    fn spinner_style(&self) -> ProgressStyle {
        let template = if self.colors {
            "{spinner.green} {msg}"
        } else {
            "{spinner} {msg}"
        };
        ProgressStyle::with_template(template).unwrap_or_else(|_| ProgressStyle::default_spinner())
    }

    /// A meter for one pull: drives a bar when enabled and always feeds the
    /// `status` transfer rows.
    pub fn pull_meter(&self, path: &str, total_bytes: u64) -> Arc<Meter> {
        let bar = self.mp.as_ref().map(|mp| {
            let bar = mp.add(ProgressBar::new(total_bytes));
            bar.set_style(self.bar_style());
            bar.set_message(format!("⇣ {path}"));
            bar
        });
        Arc::new(Meter {
            path: path.to_string(),
            total: total_bytes,
            done: AtomicU64::new(0),
            started: Instant::now(),
            bar: Mutex::new(bar),
        })
    }

    /// A compact spinner shown while a large local edit is re-chunked.
    pub fn publish_spinner(&self, path: &str) -> PublishSpinner {
        let bar = self.mp.as_ref().map(|mp| {
            let bar = mp.add(ProgressBar::new_spinner());
            bar.set_style(self.spinner_style());
            bar.set_message(format!("publishing {path}…"));
            bar.enable_steady_tick(Duration::from_millis(120));
            bar
        });
        PublishSpinner { bar }
    }

    /// A `tracing` writer that suspends the bar area for each log write, so
    /// log lines never tear through a rendering bar.
    pub fn tracing_writer(&self) -> UiWriter {
        UiWriter {
            mp: self.mp.clone(),
        }
    }
}

/// Byte progress for one active pull. Written by the transfer task, read by
/// both the bar and `status`.
#[derive(Debug)]
pub struct Meter {
    path: String,
    total: u64,
    done: AtomicU64,
    started: Instant,
    bar: Mutex<Option<ProgressBar>>,
}

impl Meter {
    pub fn inc(&self, bytes: u64) {
        let now = self.done.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if let Ok(guard) = self.bar.lock()
            && let Some(bar) = guard.as_ref()
        {
            bar.set_position(now.min(self.total));
        }
    }

    /// Announces the chunk count once the manifest is resolved.
    pub fn set_chunks(&self, chunks: usize) {
        if let Ok(guard) = self.bar.lock()
            && let Some(bar) = guard.as_ref()
        {
            bar.set_message(format!("⇣ {} · {chunks} chunks", self.path));
        }
    }

    /// Completes and removes the bar (the log line is the durable record).
    pub fn finish(&self) {
        if let Ok(mut guard) = self.bar.lock()
            && let Some(bar) = guard.take()
        {
            bar.finish_and_clear();
        }
    }

    pub fn bytes_done(&self) -> u64 {
        self.done.load(Ordering::Relaxed).min(self.total)
    }

    pub fn bytes_total(&self) -> u64 {
        self.total
    }

    pub fn percent(&self) -> u8 {
        if self.total == 0 {
            return 100;
        }
        ((self.bytes_done() * 100) / self.total) as u8
    }

    /// Average transfer rate since the pull started, in bytes/second.
    pub fn rate(&self) -> u64 {
        let secs = self.started.elapsed().as_secs_f64();
        if secs <= f64::EPSILON {
            return 0;
        }
        (self.bytes_done() as f64 / secs) as u64
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Spinner handle for a local publish; clears itself when dropped.
#[derive(Debug)]
pub struct PublishSpinner {
    bar: Option<ProgressBar>,
}

impl Drop for PublishSpinner {
    fn drop(&mut self) {
        if let Some(bar) = self.bar.take() {
            bar.finish_and_clear();
        }
    }
}

/// `MakeWriter` bridging tracing output through the progress area.
#[derive(Debug, Clone)]
pub struct UiWriter {
    mp: Option<MultiProgress>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for UiWriter {
    type Writer = SuspendingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        SuspendingWriter {
            mp: self.mp.clone(),
        }
    }
}

/// Writes to stderr; when bars are active, the bar area is suspended for the
/// duration of the write so output lines stay whole.
#[derive(Debug)]
pub struct SuspendingWriter {
    mp: Option<MultiProgress>,
}

impl Write for SuspendingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match &self.mp {
            Some(mp) => mp.suspend(|| std::io::stderr().write(buf)),
            None => std::io::stderr().write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &self.mp {
            Some(mp) => mp.suspend(|| std::io::stderr().flush()),
            None => std::io::stderr().flush(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_ui_is_a_noop_but_meters_still_count() {
        let ui = Ui::disabled();
        assert!(!ui.is_enabled());
        let meter = ui.pull_meter("a/b.txt", 1000);
        meter.set_chunks(4);
        meter.inc(250);
        assert_eq!(meter.bytes_done(), 250);
        assert_eq!(meter.percent(), 25);
        meter.inc(10_000); // over-increment clamps to total
        assert_eq!(meter.bytes_done(), 1000);
        assert_eq!(meter.percent(), 100);
        meter.finish();
        let _spinner = ui.publish_spinner("big.bin");
    }

    #[test]
    fn zero_total_meter_reports_complete() {
        let meter = Ui::disabled().pull_meter("empty", 0);
        assert_eq!(meter.percent(), 100);
        assert_eq!(meter.rate(), 0);
    }
}
