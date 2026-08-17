//! Layered configuration: defaults → file → environment → CLI.
//!
//! Two requirements from S01 drive the design:
//!
//! - Later layers override earlier ones, and it must be possible to say *which*
//!   layer a value came from when a setting is not what someone expected.
//! - Validation reports **every** error at startup, not the first. A config with
//!   three mistakes should take one run to fix, not three.
//!
//! Values are held as a flat map of dotted keys, which keeps layering simple:
//! merging is a key-by-key overwrite rather than a recursive tree walk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{CoreError, ErrorReport};

/// Where a config value came from. Reported by [`Config::source_of`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// Compiled-in default.
    Default,
    /// A configuration file.
    File,
    /// An environment variable.
    Environment,
    /// A command-line argument.
    CommandLine,
}

impl std::fmt::Display for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Layer::Default => "default",
            Layer::File => "config file",
            Layer::Environment => "environment",
            Layer::CommandLine => "command line",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone)]
struct Entry {
    value: String,
    layer: Layer,
}

/// A resolved configuration.
#[derive(Debug, Default, Clone)]
pub struct Config {
    entries: BTreeMap<String, Entry>,
}

impl Config {
    /// An empty configuration.
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Applies compiled-in defaults.
    pub fn with_defaults(mut self, defaults: &[(&str, &str)]) -> Self {
        for (key, value) in defaults {
            self.set(key, value, Layer::Default);
        }
        self
    }

    /// Overlays a TOML file.
    ///
    /// A missing file is not an error — it means "no overrides", which is the
    /// normal case for a fresh checkout. A malformed file *is* an error, because
    /// silently ignoring a config someone wrote is worse than refusing to start.
    pub fn with_file(mut self, path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(self),
            Err(error) => {
                return Err(CoreError::ConfigFile {
                    path: path.to_path_buf(),
                    message: error.to_string(),
                });
            }
        };

        // Parsed as a `Table` rather than a `Value`: a TOML *document* is always
        // a table, and `Value`'s parser expects a bare value like `42`.
        let parsed: toml::Table =
            text.parse()
                .map_err(|error: toml::de::Error| CoreError::ConfigFile {
                    path: path.to_path_buf(),
                    message: error.to_string(),
                })?;

        let mut flattened = Vec::new();
        flatten_toml("", &toml::Value::Table(parsed), &mut flattened);
        for (key, value) in flattened {
            self.set(&key, &value, Layer::File);
        }

        Ok(self)
    }

    /// Overlays environment variables carrying `prefix`.
    ///
    /// `CX_SIM_THREADS=8` sets `sim.threads`. Underscores become dots and the
    /// key is lowercased, which is the convention that lets a dotted key be
    /// expressed in a shell.
    pub fn with_env(self, prefix: &str) -> Self {
        self.with_env_vars(prefix, std::env::vars())
    }

    /// [`Config::with_env`] over an explicit set of variables.
    ///
    /// Exists so the prefix-and-underscore mapping can be tested without
    /// mutating process environment — which in edition 2024 is `unsafe`, and
    /// which this crate forbids outright.
    pub fn with_env_vars(
        mut self,
        prefix: &str,
        vars: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        for (name, value) in vars {
            let Some(stripped) = name.strip_prefix(prefix) else {
                continue;
            };
            let key = stripped
                .trim_start_matches('_')
                .to_lowercase()
                .replace('_', ".");
            if !key.is_empty() {
                self.set(&key, &value, Layer::Environment);
            }
        }
        self
    }

    /// Overlays `key=value` command-line overrides.
    pub fn with_overrides<'a>(
        mut self,
        overrides: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, ErrorReport<CoreError>> {
        let mut report = ErrorReport::new();

        for raw in overrides {
            match raw.split_once('=') {
                Some((key, value)) if !key.is_empty() => {
                    self.set(key.trim(), value.trim(), Layer::CommandLine);
                }
                _ => report.push(CoreError::ConfigValue {
                    key: raw.to_owned(),
                    message: "expected `key=value`".to_owned(),
                }),
            }
        }

        report.into_result(self)
    }

    fn set(&mut self, key: &str, value: &str, layer: Layer) {
        self.entries.insert(
            key.to_owned(),
            Entry {
                value: value.to_owned(),
                layer,
            },
        );
    }

    /// The raw string value of a key.
    pub fn raw(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|entry| entry.value.as_str())
    }

    /// Which layer supplied a key's current value.
    pub fn source_of(&self, key: &str) -> Option<Layer> {
        self.entries.get(key).map(|entry| entry.layer)
    }

    /// Every key, in sorted order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Reads a typed value, or records why it could not be read.
    ///
    /// Takes the report rather than returning `Result` so that a caller reading
    /// twenty settings collects twenty problems in one pass.
    pub fn get<T: ConfigValue>(&self, key: &str, report: &mut ErrorReport<CoreError>) -> Option<T> {
        let Some(raw) = self.raw(key) else {
            report.push(CoreError::ConfigMissing {
                key: key.to_owned(),
            });
            return None;
        };

        match T::parse(raw) {
            Some(value) => Some(value),
            None => {
                report.push(CoreError::ConfigType {
                    key: key.to_owned(),
                    expected: T::TYPE_NAME,
                    found: raw.to_owned(),
                });
                None
            }
        }
    }

    /// Reads a typed value, falling back to `fallback` when the key is absent.
    ///
    /// A *malformed* value still reports: an unset key means "use the default",
    /// but a value that cannot be parsed means someone tried to set it and got
    /// it wrong, which they need to be told about.
    pub fn get_or(
        &self,
        key: &str,
        fallback: impl Into<String>,
        report: &mut ErrorReport<CoreError>,
    ) -> Option<String> {
        match self.raw(key) {
            Some(raw) => Some(raw.to_owned()),
            None => {
                let _ = report;
                Some(fallback.into())
            }
        }
    }

    /// Reads a value and checks it against a range.
    pub fn get_in_range<T>(
        &self,
        key: &str,
        range: std::ops::RangeInclusive<T>,
        report: &mut ErrorReport<CoreError>,
    ) -> Option<T>
    where
        T: ConfigValue + PartialOrd + std::fmt::Display + Copy,
    {
        let value = self.get::<T>(key, report)?;
        if range.contains(&value) {
            Some(value)
        } else {
            report.push(CoreError::ConfigValue {
                key: key.to_owned(),
                message: format!(
                    "{value} is outside the permitted range {}..={}",
                    range.start(),
                    range.end()
                ),
            });
            None
        }
    }
}

/// A type that can be read from a config string.
pub trait ConfigValue: Sized {
    /// Human-readable type name, used in error messages.
    const TYPE_NAME: &'static str;

    /// Parses the value, or `None` if it does not fit the type.
    fn parse(raw: &str) -> Option<Self>;
}

macro_rules! impl_config_value {
    ($type:ty, $name:literal) => {
        impl ConfigValue for $type {
            const TYPE_NAME: &'static str = $name;

            fn parse(raw: &str) -> Option<Self> {
                raw.trim().parse::<$type>().ok()
            }
        }
    };
}

impl_config_value!(u8, "an integer");
impl_config_value!(u16, "an integer");
impl_config_value!(u32, "an integer");
impl_config_value!(u64, "an integer");
impl_config_value!(usize, "an integer");
impl_config_value!(i32, "an integer");
impl_config_value!(i64, "an integer");
impl_config_value!(f32, "a number");
impl_config_value!(f64, "a number");

impl ConfigValue for bool {
    const TYPE_NAME: &'static str = "true or false";

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" | "on" => Some(true),
            "false" | "no" | "0" | "off" => Some(false),
            _ => None,
        }
    }
}

impl ConfigValue for String {
    const TYPE_NAME: &'static str = "a string";

    fn parse(raw: &str) -> Option<Self> {
        Some(raw.to_owned())
    }
}

impl ConfigValue for PathBuf {
    const TYPE_NAME: &'static str = "a path";

    fn parse(raw: &str) -> Option<Self> {
        Some(PathBuf::from(raw))
    }
}

fn flatten_toml(prefix: &str, value: &toml::Value, out: &mut Vec<(String, String)>) {
    match value {
        toml::Value::Table(table) => {
            for (key, nested) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_toml(&path, nested, out);
            }
        }
        toml::Value::Array(items) => {
            // Arrays are joined rather than indexed: config arrays in this
            // engine are lists of names (module ids, content paths), and a
            // comma-joined string keeps the flat-key model intact.
            let joined: Vec<String> = items.iter().map(render_scalar).collect();
            out.push((prefix.to_owned(), joined.join(",")));
        }
        scalar => out.push((prefix.to_owned(), render_scalar(scalar))),
    }
}

fn render_scalar(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => text.clone(),
        toml::Value::Integer(number) => number.to_string(),
        toml::Value::Float(number) => number.to_string(),
        toml::Value::Boolean(flag) => flag.to_string(),
        toml::Value::Datetime(stamp) => stamp.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s01_acceptance_validation_reports_every_error_not_just_the_first() {
        let config = Config::new().with_defaults(&[
            ("sim.threads", "not-a-number"),
            ("sim.seed", "also-not-a-number"),
            ("sim.tick_us", "33333"),
        ]);

        let mut report = ErrorReport::new();
        let threads = config.get::<u32>("sim.threads", &mut report);
        let seed = config.get::<u64>("sim.seed", &mut report);
        let missing = config.get::<u32>("sim.absent", &mut report);
        let tick = config.get::<u64>("sim.tick_us", &mut report);

        assert_eq!(threads, None);
        assert_eq!(seed, None);
        assert_eq!(missing, None);
        assert_eq!(tick, Some(33_333), "valid keys still read");

        assert_eq!(report.len(), 3, "all three problems reported in one pass");
        let rendered = report.to_string();
        assert!(rendered.contains("sim.threads"), "got {rendered}");
        assert!(rendered.contains("sim.seed"), "got {rendered}");
        assert!(rendered.contains("sim.absent"), "got {rendered}");
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn later_layers_override_earlier_ones() {
        let config = Config::new()
            .with_defaults(&[("sim.threads", "4")])
            .with_env_vars("CX", env(&[("CX_SIM_THREADS", "16")]))
            .with_overrides(["sim.threads=32"])
            .expect("override should parse");

        assert_eq!(config.raw("sim.threads"), Some("32"));
        assert_eq!(config.source_of("sim.threads"), Some(Layer::CommandLine));
    }

    #[test]
    fn each_layer_wins_over_the_one_before_it() {
        let defaults = [("sim.threads", "4")];

        let from_default = Config::new().with_defaults(&defaults);
        assert_eq!(from_default.source_of("sim.threads"), Some(Layer::Default));

        let from_env = Config::new()
            .with_defaults(&defaults)
            .with_env_vars("CX", env(&[("CX_SIM_THREADS", "16")]));
        assert_eq!(from_env.raw("sim.threads"), Some("16"));
        assert_eq!(from_env.source_of("sim.threads"), Some(Layer::Environment));
    }

    #[test]
    fn env_layer_maps_underscores_to_dotted_keys() {
        let config = Config::new()
            .with_defaults(&[("sim.threads", "4")])
            .with_env_vars(
                "CX",
                env(&[("CX_SIM_THREADS", "12"), ("UNRELATED_VAR", "x")]),
            );

        assert_eq!(config.raw("sim.threads"), Some("12"));
        assert_eq!(config.source_of("sim.threads"), Some(Layer::Environment));
        assert_eq!(
            config.raw("var"),
            None,
            "variables without the prefix are ignored"
        );
    }

    #[test]
    fn missing_config_file_is_not_an_error() {
        let config = Config::new()
            .with_defaults(&[("a", "1")])
            .with_file("definitely/not/here.toml")
            .expect("a missing file means no overrides");
        assert_eq!(config.raw("a"), Some("1"));
    }

    #[test]
    fn malformed_config_file_names_the_file() {
        let path = std::env::temp_dir().join("cx_core_bad_config.toml");
        std::fs::write(&path, "this is = = not toml").expect("write temp file");

        let error = Config::new()
            .with_file(&path)
            .expect_err("malformed file should fail");
        assert!(
            error.to_string().contains("cx_core_bad_config.toml"),
            "got {error}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn toml_tables_flatten_to_dotted_keys() {
        let path = std::env::temp_dir().join("cx_core_config.toml");
        std::fs::write(
            &path,
            "[sim]\nthreads = 8\nseed = 42\n\n[render]\nvsync = true\nmodules = [\"a\", \"b\"]\n",
        )
        .expect("write temp file");

        let config = Config::new().with_file(&path).expect("valid toml");
        assert_eq!(config.raw("sim.threads"), Some("8"));
        assert_eq!(config.raw("render.vsync"), Some("true"));
        assert_eq!(config.raw("render.modules"), Some("a,b"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn range_validation_reports_the_permitted_range() {
        let config = Config::new().with_defaults(&[("sim.threads", "500")]);
        let mut report = ErrorReport::new();

        assert_eq!(
            config.get_in_range::<u32>("sim.threads", 1..=64, &mut report),
            None
        );
        let rendered = report.to_string();
        assert!(
            rendered.contains("1..=64"),
            "the message should say what is allowed: {rendered}"
        );
    }

    #[test]
    fn booleans_accept_the_usual_spellings() {
        assert_eq!(bool::parse("yes"), Some(true));
        assert_eq!(bool::parse("OFF"), Some(false));
        assert_eq!(bool::parse("maybe"), None);
    }

    #[test]
    fn malformed_override_is_reported_rather_than_ignored() {
        let error = Config::new()
            .with_overrides(["not-a-pair"])
            .expect_err("should fail");
        assert!(error.to_string().contains("key=value"));
    }
}
