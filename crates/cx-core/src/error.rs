//! Error types.
//!
//! The policy from `03-conventions.md`: `thiserror` in libraries, `anyhow` only
//! in `apps/`. Loader errors carry file, line, and column, and a malformed
//! definition file must produce a message a content author can act on without
//! reading Rust.
//!
//! Sim crates do not panic in release. Invariant violations report through
//! `cx-diag` and degrade; they do not abort.

use std::fmt;
use std::path::PathBuf;

/// Errors originating in `cx-core`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// A config value was outside its permitted range.
    #[error("config `{key}`: {message}")]
    ConfigValue {
        /// Dotted config key, e.g. `sim.threads`.
        key: String,
        /// What was wrong, in terms a user can act on.
        message: String,
    },

    /// A config value could not be parsed as its declared type.
    #[error("config `{key}`: expected {expected}, found `{found}`")]
    ConfigType {
        /// Dotted config key.
        key: String,
        /// The type that was expected, e.g. `an integer`.
        expected: &'static str,
        /// The text that was found instead.
        found: String,
    },

    /// A config file could not be read or parsed.
    #[error("config file {path}: {message}")]
    ConfigFile {
        /// The file that failed.
        path: PathBuf,
        /// The underlying reason.
        message: String,
    },

    /// A required config key was absent from every layer.
    #[error("config `{key}` is required but was not set in defaults, file, environment, or CLI")]
    ConfigMissing {
        /// Dotted config key.
        key: String,
    },
}

/// Wraps an error with the source location that produced it.
///
/// Content authors are not Rust programmers, so a loader error has to say where
/// in *their* file the problem is. This is the type that carries that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located<E> {
    /// The file the error came from.
    pub path: PathBuf,
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number.
    pub column: u32,
    /// The underlying error.
    pub inner: E,
}

impl<E> Located<E> {
    /// Attaches a location to an error.
    pub fn new(path: impl Into<PathBuf>, line: u32, column: u32, inner: E) -> Self {
        Self {
            path: path.into(),
            line,
            column,
            inner,
        }
    }

    /// Attaches a location with no column information.
    pub fn at_line(path: impl Into<PathBuf>, line: u32, inner: E) -> Self {
        Self::new(path, line, 0, inner)
    }

    /// Replaces the wrapped error, keeping the location.
    pub fn map<F>(self, transform: impl FnOnce(E) -> F) -> Located<F> {
        Located {
            path: self.path,
            line: self.line,
            column: self.column,
            inner: transform(self.inner),
        }
    }
}

impl<E: fmt::Display> fmt::Display for Located<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `path:line:column: message` — the format editors and terminals already
        // know how to turn into a clickable link.
        if self.column > 0 {
            write!(
                f,
                "{}:{}:{}: {}",
                self.path.display(),
                self.line,
                self.column,
                self.inner
            )
        } else {
            write!(f, "{}:{}: {}", self.path.display(), self.line, self.inner)
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for Located<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.inner)
    }
}

/// A batch of errors, reported together.
///
/// S01 requires config validation to report *every* error in a malformed file
/// rather than the first. The same applies to content loading: fixing one typo,
/// re-running, and finding the next one is a miserable authoring loop.
#[derive(Debug, Default)]
pub struct ErrorReport<E> {
    errors: Vec<E>,
}

impl<E> ErrorReport<E> {
    /// An empty report.
    pub const fn new() -> Self {
        Self { errors: Vec::new() }
    }

    /// Records an error and keeps going.
    pub fn push(&mut self, error: E) {
        self.errors.push(error);
    }

    /// Records an error if `result` failed, and returns whether it succeeded.
    pub fn absorb<T>(&mut self, result: Result<T, E>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.push(error);
                None
            }
        }
    }

    /// Whether anything went wrong.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// How many errors were recorded.
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    /// Whether no errors were recorded.
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// The recorded errors.
    pub fn errors(&self) -> &[E] {
        &self.errors
    }

    /// Consumes the report, yielding `Ok(value)` when empty.
    pub fn into_result<T>(self, value: T) -> Result<T, Self> {
        if self.errors.is_empty() {
            Ok(value)
        } else {
            Err(self)
        }
    }

    /// Merges another report into this one.
    pub fn extend(&mut self, other: Self) {
        self.errors.extend(other.errors);
    }
}

impl<E: fmt::Display> fmt::Display for ErrorReport<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} problem(s):", self.errors.len())?;
        for error in &self.errors {
            writeln!(f, "  - {error}")?;
        }
        Ok(())
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for ErrorReport<E> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn located_renders_as_a_clickable_location() {
        let error = Located::new(
            "content/creatures/wolf.toml",
            12,
            5,
            CoreError::ConfigMissing {
                key: "diet".to_owned(),
            },
        );
        let rendered = error.to_string();
        assert!(
            rendered.starts_with("content/creatures/wolf.toml:12:5: "),
            "got {rendered}"
        );
    }

    #[test]
    fn located_omits_an_unknown_column() {
        let error = Located::at_line(
            "a.toml",
            3,
            CoreError::ConfigMissing {
                key: "k".to_owned(),
            },
        );
        assert!(error.to_string().starts_with("a.toml:3: "));
    }

    #[test]
    fn report_collects_every_error_rather_than_the_first() {
        let mut report = ErrorReport::new();
        report.push(CoreError::ConfigMissing {
            key: "a".to_owned(),
        });
        report.push(CoreError::ConfigMissing {
            key: "b".to_owned(),
        });

        assert_eq!(report.len(), 2);
        let rendered = report.to_string();
        assert!(
            rendered.contains("`a`") && rendered.contains("`b`"),
            "got {rendered}"
        );
    }

    #[test]
    fn report_absorbs_failures_and_continues() {
        let mut report: ErrorReport<CoreError> = ErrorReport::new();
        assert_eq!(report.absorb(Ok(7)), Some(7));
        assert_eq!(
            report.absorb::<u32>(Err(CoreError::ConfigMissing { key: "x".into() })),
            None
        );
        assert!(report.has_errors());
        assert!(report.into_result(()).is_err());
    }

    #[test]
    fn empty_report_yields_the_value() {
        let report: ErrorReport<CoreError> = ErrorReport::new();
        assert_eq!(report.into_result(42).ok(), Some(42));
    }
}
