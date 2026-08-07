use anyhow::{Result, bail};
use std::{
    env,
    path::{Path, PathBuf},
};

pub const READ_ONLY_ENV: &str = "CRYPTO_NAV_READ_ONLY";
pub const EXPORT_ROOT_ENV: &str = "CRYPTO_NAV_EXPORT_ROOT";

const DEFAULT_HISTORY_OUTPUT_DIR: &str = "data";
const DEFAULT_MATCHED_OUTPUT_ROOT: &str = "/home/ubuntu/order_data";

pub fn read_only() -> Result<bool> {
    match env::var(READ_ONLY_ENV) {
        Ok(value) => parse_bool(READ_ONLY_ENV, &value),
        Err(env::VarError::NotPresent) => Ok(false),
        Err(env::VarError::NotUnicode(_)) => bail!("{READ_ONLY_ENV} is not valid Unicode"),
    }
}

pub fn history_output_dir(explicit: Option<&Path>) -> Result<PathBuf> {
    let export_root = configured_export_root()?;
    Ok(resolve_history_output_dir(explicit, export_root.as_deref()))
}

pub fn matched_output_root(explicit: Option<&Path>) -> Result<PathBuf> {
    let export_root = configured_export_root()?;
    Ok(resolve_matched_output_root(
        explicit,
        export_root.as_deref(),
    ))
}

fn configured_export_root() -> Result<Option<PathBuf>> {
    let Some(value) = env::var_os(EXPORT_ROOT_ENV) else {
        return Ok(None);
    };
    if value.is_empty() {
        bail!("{EXPORT_ROOT_ENV} must not be empty");
    }
    Ok(Some(PathBuf::from(value)))
}

fn parse_bool(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("{name} must be one of 1/0, true/false, yes/no, or on/off"),
    }
}

fn resolve_history_output_dir(explicit: Option<&Path>, export_root: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| export_root.map(|root| root.join("history")))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_HISTORY_OUTPUT_DIR))
}

fn resolve_matched_output_root(explicit: Option<&Path>, export_root: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| export_root.map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MATCHED_OUTPUT_ROOT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_boolean_values() {
        for value in ["1", "true", "YES", "on"] {
            assert!(parse_bool(READ_ONLY_ENV, value).unwrap());
        }
        for value in ["0", "false", "NO", "off"] {
            assert!(!parse_bool(READ_ONLY_ENV, value).unwrap());
        }
        assert!(parse_bool(READ_ONLY_ENV, "enabled").is_err());
    }

    #[test]
    fn resolves_export_directories_with_cli_precedence() {
        let root = Path::new("/srv/order_data");
        assert_eq!(
            resolve_history_output_dir(None, Some(root)),
            PathBuf::from("/srv/order_data/history")
        );
        assert_eq!(
            resolve_matched_output_root(None, Some(root)),
            PathBuf::from("/srv/order_data")
        );
        assert_eq!(
            resolve_history_output_dir(Some(Path::new("/tmp/history")), Some(root)),
            PathBuf::from("/tmp/history")
        );
        assert_eq!(
            resolve_matched_output_root(Some(Path::new("/tmp/orders")), Some(root)),
            PathBuf::from("/tmp/orders")
        );
    }
}
