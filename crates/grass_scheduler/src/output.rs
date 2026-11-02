//! Terminal output that a parent can label, silence or send to a file.
//!
//! Codes print through [`say!`](crate::say) instead of `println!`. On its own
//! an App prints exactly as before. When a parent runs several Apps in one
//! process (coupled sub-Apps), it gives each one an [`Output`] — a `[dem] `
//! prefix by default — and every line that App prints while it runs goes
//! there. Lines from different codes stay readable instead of interleaving
//! unlabeled.

use std::cell::RefCell;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::rc::Rc;

/// Where one App's terminal output goes.
#[derive(Clone)]
pub struct Output(Rc<RefCell<Sink>>);

enum Sink {
    Stdout { prefix: String },
    Quiet,
    File(BufWriter<File>),
}

impl Output {
    /// Print to stdout with `prefix` in front of every line.
    pub fn prefixed(prefix: impl Into<String>) -> Self {
        Self(Rc::new(RefCell::new(Sink::Stdout {
            prefix: prefix.into(),
        })))
    }

    /// Print to stdout unchanged.
    pub fn stdout() -> Self {
        Self::prefixed("")
    }

    /// Drop every line.
    pub fn quiet() -> Self {
        Self(Rc::new(RefCell::new(Sink::Quiet)))
    }

    /// Write every line to a file, created (or truncated) now.
    pub fn file(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self(Rc::new(RefCell::new(Sink::File(BufWriter::new(
            file,
        ))))))
    }

    /// Run `f` with this as the current output: every [`say!`](crate::say)
    /// inside it, on this thread, goes here. Scopes nest; the previous output
    /// is restored afterwards, even if `f` panics.
    pub fn scope<R>(&self, f: impl FnOnce() -> R) -> R {
        struct Restore(Option<Output>);
        impl Drop for Restore {
            fn drop(&mut self) {
                CURRENT.with(|c| *c.borrow_mut() = self.0.take());
            }
        }
        let previous = CURRENT.with(|c| c.borrow_mut().replace(self.clone()));
        let _restore = Restore(previous);
        f()
    }

    fn write_line(&self, line: &str) {
        match &mut *self.0.borrow_mut() {
            Sink::Stdout { prefix } => {
                let stdout = std::io::stdout();
                let mut out = stdout.lock();
                for part in line.split('\n') {
                    let _ = writeln!(out, "{prefix}{part}");
                }
            }
            Sink::Quiet => {}
            Sink::File(file) => {
                let _ = writeln!(file, "{line}");
                let _ = file.flush();
            }
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Output>> = const { RefCell::new(None) };
}

/// Print one line to the current [`Output`], or to stdout when none is set.
/// Used by [`say!`](crate::say); call that instead.
pub fn say_line(line: std::fmt::Arguments<'_>) {
    let current = CURRENT.with(|c| c.borrow().clone());
    match current {
        Some(output) => output.write_line(&line.to_string()),
        None => println!("{line}"),
    }
}

/// `println!` for simulation codes: goes to the running App's [`Output`].
///
/// ```
/// grass_scheduler::say!("step {} of {}", 1, 10);
/// grass_scheduler::say!();
/// ```
#[macro_export]
macro_rules! say {
    () => {
        $crate::output::say_line(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::output::say_line(format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_sends_lines_to_a_file_and_restores() {
        let path = std::env::temp_dir().join(format!("grass_output_{}.txt", std::process::id()));
        let out = Output::file(&path).unwrap();
        out.scope(|| {
            crate::say!("a {}", 1);
            Output::quiet().scope(|| crate::say!("hidden"));
            crate::say!("b");
        });
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "a 1\nb\n");
        assert!(CURRENT.with(|c| c.borrow().is_none()));
        let _ = std::fs::remove_file(path);
    }
}
