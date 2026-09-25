//! One redrawable status line. All writes finish before an approval is shown.

use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::presentation::{Style, Tone};

pub(super) enum Update {
    Activity(String),
    Percent(u64),
    Complete(String),
    Notice(String),
    Pause,
}

enum Message {
    Update(Update, Sender<io::Result<()>>),
    Stop,
}

pub(super) struct LiveProgress {
    sender: Sender<Message>,
    worker: Option<JoinHandle<()>>,
}

impl LiveProgress {
    pub(super) fn start(style: Style) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("pkg-progress".into())
            .spawn(move || run(&receiver, style))?;
        Ok(Self {
            sender,
            worker: Some(worker),
        })
    }

    pub(super) fn update(&self, update: Update) -> io::Result<()> {
        let (reply, result) = mpsc::channel();
        self.sender
            .send(Message::Update(update, reply))
            .map_err(io::Error::other)?;
        result.recv().map_err(io::Error::other)?
    }
}

impl Drop for LiveProgress {
    fn drop(&mut self) {
        let _ = self.sender.send(Message::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Line {
    label: String,
    percent: Option<u64>,
    started: Instant,
    updated: Instant,
}

fn run(receiver: &Receiver<Message>, style: Style) {
    let mut line: Option<Line> = None;
    let mut frame = 0;
    let mut last_draw = Instant::now();
    loop {
        match receiver.recv_timeout(Duration::from_millis(120)) {
            Ok(Message::Update(update, reply)) => {
                let result = apply(&mut line, update, style, io::stderr());
                let failed = result.is_err();
                let _ = reply.send(result);
                if failed {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => break,
        }
        if last_draw.elapsed() >= Duration::from_millis(120) {
            if let Some(line) = &line {
                let text = render(line, frame, crate::presentation::terminal_width());
                if write!(io::stderr(), "\r\x1b[2K{text}")
                    .and_then(|()| io::stderr().flush())
                    .is_err()
                {
                    break;
                }
                frame += 1;
            }
            last_draw = Instant::now();
        }
    }
    if line.is_some() {
        let _ = clear();
    }
}

fn clear() -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    write!(stderr, "\r\x1b[2K")?;
    stderr.flush()
}

fn apply(
    line: &mut Option<Line>,
    update: Update,
    style: Style,
    mut writer: impl Write,
) -> io::Result<()> {
    match update {
        Update::Activity(label) => {
            write!(writer, "\r\x1b[2K")?;
            *line = Some(Line {
                label,
                percent: None,
                started: Instant::now(),
                updated: Instant::now(),
            });
        }
        Update::Percent(percent) => {
            if let Some(line) = line {
                line.percent = Some(percent.min(100));
                line.updated = Instant::now();
            }
        }
        Update::Complete(text) => {
            write!(writer, "\r\x1b[2K")?;
            let elapsed = line
                .take()
                .map(|line| format!("  {}", duration(line.started.elapsed())))
                .unwrap_or_default();
            writeln!(
                writer,
                "{} {text}{elapsed}",
                style.paint("✓", Tone::Success)
            )?;
        }
        Update::Notice(text) => {
            write!(writer, "\r\x1b[2K")?;
            *line = None;
            writeln!(writer, "{} {text}", style.paint("!", Tone::Warning))?;
        }
        Update::Pause => {
            write!(writer, "\r\x1b[2K")?;
            *line = None;
        }
    }
    writer.flush()
}

fn duration(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn render(line: &Line, frame: usize, width: usize) -> String {
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][frame % 10];
    let progress = line
        .percent
        .filter(|_| width >= 40)
        .map_or_else(String::new, |percent| {
            let filled = usize::try_from(percent / 10).unwrap_or(10).min(10);
            format!(
                " [{}{}] {percent}%",
                "━".repeat(filled),
                "─".repeat(10 - filled)
            )
        });
    let waiting = if line.updated.elapsed() >= Duration::from_secs(30) {
        if width < 40 {
            ""
        } else if width < 80 {
            " · waiting"
        } else {
            " · waiting for an update"
        }
    } else {
        ""
    };
    let suffix = format!("{progress}  {}{waiting}", duration(line.started.elapsed()));
    let limit = width.saturating_sub(suffix.chars().count() + 4);
    let first = crate::presentation::wrap(&line.label, limit.saturating_sub(1))
        .into_iter()
        .next()
        .unwrap_or_default();
    let label = if first == line.label {
        first
    } else {
        format!("{first}…")
    };
    format!("{spinner} {label}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_replaces_one_line_and_stops_before_the_prompt() {
        let mut line = None;
        let mut output = Vec::new();
        let style = Style::new(true, true);
        apply(
            &mut line,
            Update::Activity("Checking dependencies".into()),
            style,
            &mut output,
        )
        .unwrap();
        assert!(line.is_some());
        apply(
            &mut line,
            Update::Complete("Build plan ready.".into()),
            style,
            &mut output,
        )
        .unwrap();
        assert!(
            line.is_none(),
            "the prompt must have exclusive terminal access"
        );
        apply(
            &mut line,
            Update::Activity("Building just".into()),
            style,
            &mut output,
        )
        .unwrap();
        for percent in 1..100 {
            apply(&mut line, Update::Percent(percent), style, &mut output).unwrap();
        }
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
        apply(&mut line, Update::Pause, style, &mut output).unwrap();
        assert!(
            line.is_none(),
            "failure must stop rendering before the error"
        );
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("ready to use"));
    }

    #[test]
    fn actual_percentage_has_a_bar_but_never_claims_installation_success() {
        let line = Line {
            label: "Building just 1.58.0".into(),
            percent: Some(99),
            started: Instant::now(),
            updated: Instant::now(),
        };
        let text = render(&line, 0, 80);
        assert!(text.contains("99%"));
        assert!(text.contains("Building just 1.58.0"));
        assert!(!text.contains("✓"));
        assert!(text.chars().count() < 80);
    }

    #[test]
    fn unknown_work_has_no_invented_percent() {
        let line = Line {
            label: "Checking dependencies".into(),
            percent: None,
            started: Instant::now(),
            updated: Instant::now(),
        };
        assert!(!render(&line, 0, 40).contains('%'));
    }
}
