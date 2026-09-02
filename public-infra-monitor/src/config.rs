use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

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
    #[serde(default)]
    pub notifications: NotificationConfig,
    pub targets: Vec<TargetConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    pub name: String,
    pub venue: String,
    pub expected_cpu: u32,
    #[serde(default)]
    pub match_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub address: String,
    pub queue_capacity: usize,
    pub request_timeout_ms: u64,
    pub repeat_interval_secs: u64,
    pub alert_samples: u32,
    pub recovery_samples: u32,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            address: "127.0.0.1:18100".to_owned(),
            queue_capacity: 32,
            request_timeout_ms: 250,
            repeat_interval_secs: 15 * 60,
            alert_samples: 3,
            recovery_samples: 6,
        }
    }
}

impl NotificationConfig {
    pub fn socket_address(&self) -> Result<SocketAddr> {
        let address = self.address.parse::<SocketAddr>().map_err(|error| {
            anyhow::anyhow!("invalid notification address {}: {error}", self.address)
        })?;
        if !address.ip().is_loopback() {
            bail!("notification address must be loopback: {address}");
        }
        Ok(address)
    }
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
        if self.notifications.enabled {
            self.notifications.socket_address()?;
            if !(1..=4_096).contains(&self.notifications.queue_capacity) {
                bail!("notification queue_capacity must be in [1, 4096]");
            }
            if !(1..=5_000).contains(&self.notifications.request_timeout_ms) {
                bail!("notification request_timeout_ms must be in [1, 5000]");
            }
            if self.notifications.repeat_interval_secs == 0 {
                bail!("notification repeat_interval_secs must be greater than zero");
            }
            if !(1..=100).contains(&self.notifications.alert_samples) {
                bail!("notification alert_samples must be in [1, 100]");
            }
            if !(1..=100).contains(&self.notifications.recovery_samples) {
                bail!("notification recovery_samples must be in [1, 100]");
            }
        }

        let mut venues: HashMap<&str, Vec<&TargetConfig>> = HashMap::new();
        for target in &self.targets {
            if !names.insert(&target.name) {
                bail!("duplicate target name {}", target.name);
            }
            if target.match_args.iter().any(String::is_empty) {
                bail!("target {} has an empty match argument", target.name);
            }
            venues.entry(&target.venue).or_default().push(target);
        }
        for (venue, targets) in venues {
            if targets.len() == 1 {
                continue;
            }
            let mut selectors = HashSet::new();
            for target in targets {
                if target.match_args.is_empty() {
                    bail!("targets sharing venue {venue} must define match_args");
                }
                if !selectors.insert(&target.match_args) {
                    bail!("duplicate match_args for venue {venue}");
                }
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
    fn rejects_duplicate_venues_without_match_args() {
        let config = MonitorConfig {
            listen: default_listen(),
            sample_interval_secs: 10,
            interface: "eth0".to_owned(),
            executable: default_executable(),
            bpf_enabled: true,
            thresholds: Thresholds::default(),
            notifications: NotificationConfig::default(),
            targets: vec![
                TargetConfig {
                    name: "a".to_owned(),
                    venue: "same".to_owned(),
                    expected_cpu: 1,
                    match_args: Vec::new(),
                },
                TargetConfig {
                    name: "b".to_owned(),
                    venue: "same".to_owned(),
                    expected_cpu: 2,
                    match_args: Vec::new(),
                },
            ],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn accepts_duplicate_venues_with_distinct_match_args() {
        let mut config: MonitorConfig =
            serde_json::from_str(include_str!("../config.example.json")).unwrap();
        config
            .targets
            .retain(|target| target.venue == "binance-futures");

        assert_eq!(config.targets.len(), 2);
        config.validate().unwrap();
    }

    #[test]
    fn rejects_duplicate_venue_selectors() {
        let mut config: MonitorConfig =
            serde_json::from_str(include_str!("../config.example.json")).unwrap();
        let duplicate = config
            .targets
            .iter()
            .find(|target| target.name == "spp_bn_fu_market")
            .unwrap()
            .clone();
        config.targets.push(TargetConfig {
            name: "duplicate".to_owned(),
            ..duplicate
        });

        assert!(config.validate().is_err());
    }

    #[test]
    fn example_config_validates() {
        let config: MonitorConfig =
            serde_json::from_str(include_str!("../config.example.json")).unwrap();

        config.validate().unwrap();
    }

    #[test]
    fn rejects_non_loopback_notification_address() {
        let notifications = NotificationConfig {
            address: "192.0.2.1:18100".to_owned(),
            ..NotificationConfig::default()
        };

        assert!(notifications.socket_address().is_err());
    }
}
