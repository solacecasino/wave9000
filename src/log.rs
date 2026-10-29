//! Messages for the Log panel, from any thread. Also written to stderr.

use std::collections::VecDeque;
use std::sync::Mutex;

const KEEP: usize = 500;

static LINES: Mutex<VecDeque<(bool, String)>> = Mutex::new(VecDeque::new());

pub fn info(text: impl Into<String>) {
    push(false, text.into());
}

pub fn error(text: impl Into<String>) {
    push(true, text.into());
}

fn push(error: bool, text: String) {
    eprintln!("wave9000: {text}");
    let mut lines = LINES.lock().unwrap_or_else(|e| e.into_inner());
    if lines.len() >= KEEP {
        lines.pop_front();
    }
    lines.push_back((error, text));
}

/// Calls `f` with each line, oldest first; true marks errors.
pub fn each(mut f: impl FnMut(bool, &str)) {
    for (e, t) in LINES.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        f(*e, t);
    }
}
