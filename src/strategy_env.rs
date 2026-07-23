use anyhow::{Context, Result, bail};
use std::{fs, path::Path, process::Command};

pub fn read_env_file(host: &str, path: &Path) -> Result<String> {
    if host == "local" {
        return fs::read_to_string(path)
            .with_context(|| format!("read env file {}", path.display()));
    }
    if !valid_ssh_host(host) {
        bail!("invalid SSH host alias: {host}");
    }
    let path = path
        .to_str()
        .with_context(|| format!("env path is not UTF-8: {}", path.display()))?;
    let output = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--"])
        .arg(host)
        .arg(format!("cat -- {}", shell_quote(path)))
        .output()
        .with_context(|| format!("run ssh for {host}:{path}"))?;
    if !output.status.success() {
        bail!(
            "read remote env file {host}:{path}: ssh exited with {}",
            output.status
        );
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("remote env file is not UTF-8: {host}:{path}"))
}

fn valid_ssh_host(host: &str) -> bool {
    !host.is_empty()
        && host.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | '@')
        })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::{shell_quote, valid_ssh_host};

    #[test]
    fn validates_ssh_host_aliases() {
        assert!(valid_ssh_host("sg"));
        assert!(valid_ssh_host("ubuntu@10.0.0.1"));
        assert!(!valid_ssh_host(""));
        assert!(!valid_ssh_host("sg; false"));
    }

    #[test]
    fn quotes_remote_paths_for_the_shell() {
        assert_eq!(shell_quote("/tmp/env.sh"), "'/tmp/env.sh'");
        assert_eq!(shell_quote("/tmp/a'b"), "'/tmp/a'\"'\"'b'");
    }
}
