//! Reads user settings from config.toml.

use super::constants::{CONFIG_TOML, DEFAULT_HISTORY_DAYS, RTK_DATA_DIR};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub tracking: TrackingConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub filters: FilterConfig,
    #[serde(default)]
    pub tee: crate::core::tee::TeeConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    pub hooks: HooksConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub posthook: PosthookConfig,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct HooksConfig {
    /// Commands to exclude from auto-rewrite (e.g. ["curl", "playwright"]).
    /// Survives `rtk init -g` re-runs since config.toml is user-owned.
    #[serde(default)]
    pub exclude_commands: Vec<String>,

    /// Wrapper prefixes that should be transparently stripped before routing
    /// to a filter, then re-prepended on the rewrite. For example, with
    /// `transparent_prefixes = ["docker exec mycontainer"]`, the command
    /// `docker exec mycontainer git status` rewrites to
    /// `docker exec mycontainer rtk git status` instead of passing through
    /// unrewritten.
    ///
    /// Useful for any per-project env wrapper that sits in front of every
    /// command — e.g. `docker exec mycontainer`, `direnv exec .`, `poetry run`,
    /// or `bundle exec`.
    ///
    /// Matching is literal, not pattern-based. Configure the exact concrete
    /// prefix you actually use, such as `docker exec mycontainer`.
    ///
    /// Extends the built-in `SHELL_PREFIX_BUILTINS` list (`noglob`, `command`,
    /// `builtin`, `exec`, `nocorrect`) with user- or organization-specific
    /// wrappers. Matching is strict: a configured prefix `"foo bar"` matches
    /// a command that starts with `"foo bar "` (or strictly equals `"foo bar"`),
    /// not anything else.
    #[serde(default)]
    pub transparent_prefixes: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TrackingConfig {
    pub enabled: bool,
    pub history_days: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_path: Option<PathBuf>,
}

impl Default for TrackingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            history_days: DEFAULT_HISTORY_DAYS as u32,
            database_path: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DisplayConfig {
    pub colors: bool,
    pub emoji: bool,
    pub max_width: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            colors: true,
            emoji: true,
            max_width: 120,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FilterConfig {
    pub ignore_dirs: Vec<String>,
    pub ignore_files: Vec<String>,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            ignore_dirs: vec![
                ".git".into(),
                "node_modules".into(),
                "target".into(),
                "__pycache__".into(),
                ".venv".into(),
                "vendor".into(),
            ],
            ignore_files: vec!["*.lock".into(), "*.min.js".into(), "*.min.css".into()],
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TelemetryConfig {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_given: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_date: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LimitsConfig {
    /// Max total grep results to show (default: 200)
    pub grep_max_results: usize,
    /// Max matches per file in grep output (default: 25)
    pub grep_max_per_file: usize,
    /// Max staged/modified files shown in git status (default: 15)
    pub status_max_files: usize,
    /// Max untracked files shown in git status (default: 10)
    pub status_max_untracked: usize,
    /// Max chars for parser passthrough fallback (default: 2000)
    pub passthrough_max_chars: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            grep_max_results: 200,
            grep_max_per_file: 25,
            status_max_files: 15,
            status_max_untracked: 10,
            passthrough_max_chars: 2000,
        }
    }
}

/// PostToolUse output-filter settings (`[posthook]` in config.toml).
#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PosthookConfig {
    pub enabled: bool,
    /// Per-tool on/off toggles: where output comes from.
    pub tools: PosthookTools,
    /// Globs matched against `tool_input.file_path` / `tool_input.url`,
    /// e.g. ["**/*.min.js"]. Matching paths pass through unfiltered.
    pub exclude_paths: Vec<String>,
    /// Per-format converter selection: what the content is.
    /// Part 1 accepted values: "auto" | "off" (unknown strings = "auto").
    pub formats: PosthookFormats,
    /// Translate edits made against compressed JSON views back to raw bytes.
    pub lens: bool,
}

impl Default for PosthookConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tools: PosthookTools::default(),
            exclude_paths: Vec::new(),
            formats: PosthookFormats::default(),
            lens: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PosthookTools {
    pub read: bool,
    pub grep: bool,
    pub webfetch: bool,
    pub websearch: bool,
    pub glob: bool,
    /// Generic floor for Bash commands NOT rewritten by RTK.
    pub bash: bool,
}

impl Default for PosthookTools {
    fn default() -> Self {
        Self {
            read: true,
            grep: true,
            webfetch: true,
            websearch: true,
            glob: false,
            bash: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PosthookFormats {
    pub json: String,
    pub web: String,
    pub lockfile: String,
    pub term: String,
}

impl Default for PosthookFormats {
    fn default() -> Self {
        Self {
            json: "auto".into(),
            web: "auto".into(),
            lockfile: "auto".into(),
            term: "auto".into(),
        }
    }
}

/// Get posthook config. Falls back to defaults if config can't be loaded.
pub fn posthook() -> PosthookConfig {
    Config::load().map(|c| c.posthook).unwrap_or_default()
}

/// Get limits config. Falls back to defaults if config can't be loaded.
pub fn limits() -> LimitsConfig {
    Config::load().map(|c| c.limits).unwrap_or_default()
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = get_config_path()?;

        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = get_config_path()?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    pub fn create_default() -> Result<PathBuf> {
        let config = Config::default();
        config.save()?;
        get_config_path()
    }
}

fn get_config_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    Ok(config_dir.join(RTK_DATA_DIR).join(CONFIG_TOML))
}

pub fn show_config() -> Result<()> {
    let path = get_config_path()?;
    println!("Config: {}", path.display());
    println!();

    if path.exists() {
        let config = Config::load()?;
        println!("{}", toml::to_string_pretty(&config)?);
    } else {
        println!("(default config, file not created)");
        println!();
        let config = Config::default();
        println!("{}", toml::to_string_pretty(&config)?);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hooks_config_deserialize() {
        let toml = r#"
[hooks]
exclude_commands = ["curl", "gh"]
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert_eq!(config.hooks.exclude_commands, vec!["curl", "gh"]);
    }

    #[test]
    fn test_hooks_config_default_empty() {
        let config = Config::default();
        assert!(config.hooks.exclude_commands.is_empty());
        assert!(config.hooks.transparent_prefixes.is_empty());
    }

    #[test]
    fn test_hooks_config_transparent_prefixes_deserialize() {
        let toml = r#"
[hooks]
transparent_prefixes = ["direnv exec .", "nix develop --command"]
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert_eq!(
            config.hooks.transparent_prefixes,
            vec!["direnv exec .", "nix develop --command"]
        );
    }

    #[test]
    fn test_hooks_config_transparent_prefixes_missing_is_empty() {
        // Older configs that predate this field must still parse.
        let toml = r#"
[hooks]
exclude_commands = ["curl"]
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert_eq!(config.hooks.exclude_commands, vec!["curl"]);
        assert!(config.hooks.transparent_prefixes.is_empty());
    }

    #[test]
    fn test_config_without_hooks_section_is_valid() {
        let toml = r#"
[tracking]
enabled = true
history_days = 90
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert!(config.hooks.exclude_commands.is_empty());
    }

    #[test]
    fn test_old_toml_without_consent_fields() {
        let toml = r#"
[telemetry]
enabled = true
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert!(config.telemetry.enabled);
        assert!(config.telemetry.consent_given.is_none());
        assert!(config.telemetry.consent_date.is_none());
    }

    #[test]
    fn test_telemetry_default_disabled() {
        let config = Config::default();
        assert!(!config.telemetry.enabled);
        assert!(config.telemetry.consent_given.is_none());
    }

    #[test]
    fn test_posthook_defaults_when_section_missing() {
        // Older configs that predate [posthook] must still parse, default-on.
        let toml = r#"
[tracking]
enabled = true
history_days = 90
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert!(config.posthook.enabled);
        assert!(config.posthook.tools.read);
        assert!(config.posthook.tools.grep);
        assert!(config.posthook.tools.webfetch);
        assert!(config.posthook.tools.websearch);
        assert!(!config.posthook.tools.glob);
        assert!(config.posthook.exclude_paths.is_empty());
        assert!(config.posthook.tools.bash);
        assert!(config.posthook.lens);
        assert_eq!(config.posthook.formats.json, "auto");
        assert_eq!(config.posthook.formats.web, "auto");
        assert_eq!(config.posthook.formats.lockfile, "auto");
        assert_eq!(config.posthook.formats.term, "auto");
    }

    #[test]
    fn test_posthook_partial_section_fills_defaults() {
        let toml = r#"
[posthook]
enabled = false
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert!(!config.posthook.enabled);
        assert!(config.posthook.tools.read, "missing tools keep defaults");
        assert_eq!(config.posthook.formats.json, "auto");
    }

    #[test]
    fn test_posthook_roundtrip() {
        let toml = r#"
[posthook]
enabled = true
exclude_paths = ["**/*.min.js"]

[posthook.tools]
read = true
grep = false
webfetch = true
websearch = true
glob = true

[posthook.formats]
json = "off"
web = "auto"
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert!(!config.posthook.tools.grep);
        assert!(config.posthook.tools.glob);
        assert_eq!(config.posthook.exclude_paths, vec!["**/*.min.js"]);
        assert_eq!(config.posthook.formats.json, "off");

        let serialized = toml::to_string_pretty(&config).expect("serialize");
        let reparsed: Config = toml::from_str(&serialized).expect("reparse");
        assert!(!reparsed.posthook.tools.grep);
        assert_eq!(reparsed.posthook.formats.json, "off");
    }

    #[test]
    fn test_posthook_inline_tools_table() {
        // Spec template uses the inline-table form.
        let toml = r#"
[posthook]
tools = { read = true, grep = true, webfetch = true, websearch = true, glob = false }
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert!(config.posthook.tools.read);
        assert!(!config.posthook.tools.glob);
    }

    #[test]
    fn test_telemetry_consent_roundtrip() {
        let toml = r#"
[telemetry]
enabled = true
consent_given = true
consent_date = "2026-04-10T12:00:00Z"
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert_eq!(config.telemetry.consent_given, Some(true));
        assert_eq!(
            config.telemetry.consent_date.as_deref(),
            Some("2026-04-10T12:00:00Z")
        );
    }
}
