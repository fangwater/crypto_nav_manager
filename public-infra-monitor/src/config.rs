use std::collections::HashSet;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_sample_interval")]
    pub sample_interval_secs: u64,
    pub interface: String,
    #[serde(default = "default_executable")]
    pub executable: String,
    #[serde(default = "default_true")]
    pub bpf_enabled: bool,
    #[serde(default)]
    pub thresholds: Thresholds,
    pub targets: Vec<TargetConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    pub name: String,
    pub venue: String,
    pub expected_cpu: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Thresholds {
    pub recv_queue_warn_bytes: u64,
    pub recv_queue_critical_bytes: u64,
    pub retrans_warn: u64,
    pub retrans_critical: u64,
    pub socket_drop_warn: u64,
    pub socket_drop_critical: u64,
    pub reconnect_warn: u64,
    pub reconnect_critical: u64,
    pub softnet_drop_warn: u64,
    pub softnet_time_squeeze_warn: u64,
    pub rx_idle_warn_secs: u64,
    pub rx_idle_critical_secs: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            recv_queue_warn_bytes: 64 * 1024,
            recv_queue_critical_bytes: 1024 * 1024,
            retrans_warn: 1,
            retrans_critical: 10,
            socket_drop_warn: 1,
            socket_drop_critical: 10,
            reconnect_warn: 2,
            reconnect_critical: 10,
            softnet_drop_warn: 1,
            softnet_time_squeeze_warn: 1,
            rx_idle_warn_secs: 30,
            rx_idle_critical_secs: 120,
        }
    }
}

impl MonitorConfig {
    pub fn validate(&self) -> Result<()> {
        if self.sample_interval_secs == 0 {
            bail!("sample_interval_secs must be greater than zero");
        }
        if self.interface.is_empty() {
            bail!("interface must not be empty");
        }
        if self.targets.is_empty() {
            bail!("at least one target is required");
        }

        let mut names = HashSet::new();
        let mut venues = HashSet::new();
        for target in &self.targets {
            if !names.insert(&target.name) {
                bail!("duplicate target name {}", target.name);
            }
            if !venues.insert(&target.venue) {
                bail!("duplicate target venue {}", target.venue);
            }
        }
        Ok(())
    }
}

fn default_listen() -> String {
    "127.0.0.1:9918".to_owned()
}

const fn default_sample_interval() -> u64 {
    10
}

fn default_executable() -> String {
    "spread_pbs".to_owned()
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_venues() {
        let config = MonitorConfig {
            listen: default_listen(),
            sample_interval_secs: 10,
            interface: "eth0".to_owned(),
            executable: default_executable(),
            bpf_enabled: true,
            thresholds: Thresholds::default(),
            targets: vec![
                TargetConfig {
                    name: "a".to_owned(),
                    venue: "same".to_owned(),
                    expected_cpu: 1,
                },
                TargetConfig {
                    name: "b".to_owned(),
                    venue: "same".to_owned(),
                    expected_cpu: 2,
                },
            ],
        };
        assert!(config.validate().is_err());
    }
}
