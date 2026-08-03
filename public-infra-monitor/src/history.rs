use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::model::{HealthStatus, Snapshot, TargetObservation};

const HISTORY_VERSION: u32 = 1;
pub const BUCKET_SECS: u64 = 60;
pub const RETENTION_HOURS: u64 = 24;
const MAX_BUCKETS: usize = (RETENTION_HOURS * 60 * 60 / BUCKET_SECS) as usize;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryFile {
    version: u32,
    bucket_secs: u64,
    retention_hours: u64,
    updated_at_unix_ms: u64,
    targets: BTreeMap<String, TargetHistory>,
}

impl Default for HistoryFile {
    fn default() -> Self {
        Self {
            version: HISTORY_VERSION,
            bucket_secs: BUCKET_SECS,
            retention_hours: RETENTION_HOURS,
            updated_at_unix_ms: 0,
            targets: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetHistory {
    pub name: String,
    pub venue: String,
    pub points: VecDeque<HistoryBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryBucket {
    pub bucket_start_unix_ms: u64,
    pub status: HealthStatus,
    pub samples: u32,
    pub rx_bytes: u64,
    pub reconnects: u64,
    pub disconnects: u64,
    pub retransmits: u64,
    pub socket_drops: u64,
    pub max_rx_idle_secs: f64,
    pub max_recv_queue_bytes: u64,
    pub min_established_count: usize,
    pub max_socket_count: usize,
}

impl HistoryBucket {
    fn from_target(bucket_start_unix_ms: u64, target: &TargetObservation) -> Self {
        let network = &target.network;
        Self {
            bucket_start_unix_ms,
            status: target.status,
            samples: 1,
            rx_bytes: network.rx_bytes.unwrap_or_default(),
            reconnects: network.reconnects.unwrap_or_default(),
            disconnects: network.disconnects.unwrap_or_default(),
            retransmits: network.retransmits.unwrap_or_default(),
            socket_drops: network.socket_drops.unwrap_or_default(),
            max_rx_idle_secs: network.rx_idle_secs.unwrap_or_default(),
            max_recv_queue_bytes: network.recv_queue_bytes,
            min_established_count: network.established_count,
            max_socket_count: network.socket_count,
        }
    }

    fn aggregate(&mut self, target: &TargetObservation) {
        let network = &target.network;
        self.status = worse_status(self.status, target.status);
        self.samples = self.samples.saturating_add(1);
        self.rx_bytes = self
            .rx_bytes
            .saturating_add(network.rx_bytes.unwrap_or_default());
        self.reconnects = self
            .reconnects
            .saturating_add(network.reconnects.unwrap_or_default());
        self.disconnects = self
            .disconnects
            .saturating_add(network.disconnects.unwrap_or_default());
        self.retransmits = self
            .retransmits
            .saturating_add(network.retransmits.unwrap_or_default());
        self.socket_drops = self
            .socket_drops
            .saturating_add(network.socket_drops.unwrap_or_default());
        self.max_rx_idle_secs = self
            .max_rx_idle_secs
            .max(network.rx_idle_secs.unwrap_or_default());
        self.max_recv_queue_bytes = self.max_recv_queue_bytes.max(network.recv_queue_bytes);
        self.min_established_count = self.min_established_count.min(network.established_count);
        self.max_socket_count = self.max_socket_count.max(network.socket_count);
    }
}

#[derive(Debug)]
pub struct HistoryStore {
    path: PathBuf,
    data: HistoryFile,
    last_persisted_bucket_ms: Option<u64>,
}

impl HistoryStore {
    pub fn load(path: PathBuf) -> Result<Self> {
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::new(path));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("read history {}", path.display()));
            }
        };
        let mut data: HistoryFile = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse history {}", path.display()))?;
        if data.version != HISTORY_VERSION {
            bail!(
                "history {} has unsupported version {}",
                path.display(),
                data.version
            );
        }
        if data.bucket_secs != BUCKET_SECS {
            bail!(
                "history {} has unsupported bucket size {}",
                path.display(),
                data.bucket_secs
            );
        }
        data.retention_hours = RETENTION_HOURS;
        prune(&mut data);
        let last_persisted_bucket_ms = latest_bucket(&data);
        Ok(Self {
            path,
            data,
            last_persisted_bucket_ms,
        })
    }

    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            data: HistoryFile::default(),
            last_persisted_bucket_ms: None,
        }
    }

    pub fn record(&mut self, snapshot: &Snapshot) -> bool {
        if snapshot.window_secs.is_none() {
            return false;
        }

        let bucket_start_unix_ms =
            snapshot.timestamp_unix_ms / (BUCKET_SECS * 1_000) * (BUCKET_SECS * 1_000);
        for target in &snapshot.targets {
            let history = self
                .data
                .targets
                .entry(target.name.clone())
                .or_insert_with(|| TargetHistory {
                    name: target.name.clone(),
                    venue: target.venue.clone(),
                    points: VecDeque::new(),
                });
            history.venue.clone_from(&target.venue);
            match history.points.back_mut() {
                Some(bucket) if bucket.bucket_start_unix_ms == bucket_start_unix_ms => {
                    bucket.aggregate(target);
                }
                Some(bucket) if bucket.bucket_start_unix_ms > bucket_start_unix_ms => {
                    continue;
                }
                _ => history
                    .points
                    .push_back(HistoryBucket::from_target(bucket_start_unix_ms, target)),
            }
        }
        self.data.updated_at_unix_ms = snapshot.timestamp_unix_ms;
        prune(&mut self.data);

        self.last_persisted_bucket_ms != Some(bucket_start_unix_ms)
    }

    pub fn persist_if_due(&mut self) -> Result<bool> {
        let latest = latest_bucket(&self.data);
        if latest == self.last_persisted_bucket_ms {
            return Ok(false);
        }
        self.persist()?;
        self.last_persisted_bucket_ms = latest;
        Ok(true)
    }

    pub fn persist_latest(&mut self) -> Result<()> {
        self.persist()?;
        self.last_persisted_bucket_ms = latest_bucket(&self.data);
        Ok(())
    }

    pub fn response(&self, hours: u64) -> HistoryResponse {
        let hours = hours.clamp(1, RETENTION_HOURS);
        let generated_at_unix_ms = now_unix_ms();
        let from_unix_ms =
            generated_at_unix_ms.saturating_sub(hours.saturating_mul(60 * 60 * 1_000));
        let targets = self
            .data
            .targets
            .values()
            .map(|target| TargetHistoryResponse {
                name: target.name.clone(),
                venue: target.venue.clone(),
                points: target
                    .points
                    .iter()
                    .filter(|point| point.bucket_start_unix_ms >= from_unix_ms)
                    .cloned()
                    .collect(),
            })
            .collect();
        HistoryResponse {
            generated_at_unix_ms,
            from_unix_ms,
            bucket_secs: BUCKET_SECS,
            retention_hours: RETENTION_HOURS,
            targets,
        }
    }

    fn persist(&self) -> Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("create history directory {}", parent.display()))?;

        let temp_path = temp_path(&self.path);
        let bytes = serde_json::to_vec(&self.data).context("serialize history")?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp_path)
            .with_context(|| format!("open history temp file {}", temp_path.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write history temp file {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync history temp file {}", temp_path.display()))?;
        drop(file);
        fs::rename(&temp_path, &self.path).with_context(|| {
            format!(
                "replace history {} with {}",
                self.path.display(),
                temp_path.display()
            )
        })?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("sync history directory {}", parent.display()))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryResponse {
    pub generated_at_unix_ms: u64,
    pub from_unix_ms: u64,
    pub bucket_secs: u64,
    pub retention_hours: u64,
    pub targets: Vec<TargetHistoryResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetHistoryResponse {
    pub name: String,
    pub venue: String,
    pub points: Vec<HistoryBucket>,
}

fn prune(data: &mut HistoryFile) {
    let cutoff_unix_ms = now_unix_ms().saturating_sub(RETENTION_HOURS * 60 * 60 * 1_000);
    for target in data.targets.values_mut() {
        while target
            .points
            .front()
            .is_some_and(|point| point.bucket_start_unix_ms < cutoff_unix_ms)
        {
            target.points.pop_front();
        }
        while target.points.len() > MAX_BUCKETS {
            target.points.pop_front();
        }
    }
    data.targets.retain(|_, target| !target.points.is_empty());
}

fn latest_bucket(data: &HistoryFile) -> Option<u64> {
    data.targets
        .values()
        .filter_map(|target| target.points.back())
        .map(|point| point.bucket_start_unix_ms)
        .max()
}

fn worse_status(left: HealthStatus, right: HealthStatus) -> HealthStatus {
    if status_rank(right) > status_rank(left) {
        right
    } else {
        left
    }
}

const fn status_rank(status: HealthStatus) -> u8 {
    match status {
        HealthStatus::Ok => 0,
        HealthStatus::Unknown => 1,
        HealthStatus::Warn => 2,
        HealthStatus::Critical => 3,
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(".tmp");
    PathBuf::from(value)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Capabilities, HostObservation, NetworkWindow, SystemObservation, TargetObservation,
    };

    fn snapshot(
        timestamp_unix_ms: u64,
        status: HealthStatus,
        rx_bytes: u64,
        reconnects: u64,
        disconnects: u64,
    ) -> Snapshot {
        Snapshot {
            timestamp_unix_ms,
            window_secs: Some(10.0),
            host: HostObservation {
                hostname: "test".to_owned(),
                kernel_release: "test".to_owned(),
            },
            targets: vec![TargetObservation {
                name: "spp_test".to_owned(),
                venue: "test-venue".to_owned(),
                expected_cpu: 1,
                process: None,
                process_candidates: 1,
                sockets: Vec::new(),
                network: NetworkWindow {
                    socket_count: 2,
                    established_count: 2,
                    rx_bytes: Some(rx_bytes),
                    rx_idle_secs: Some(0.0),
                    reconnects: Some(reconnects),
                    disconnects: Some(disconnects),
                    ..NetworkWindow::default()
                },
                bpf_events: Vec::new(),
                status,
                reasons: Vec::new(),
            }],
            system: SystemObservation::default(),
            capabilities: Capabilities::default(),
        }
    }

    #[test]
    fn aggregates_samples_in_the_same_minute() {
        let path = std::env::temp_dir().join("public-infra-history-aggregate.json");
        let minute = now_unix_ms() / (BUCKET_SECS * 1_000) * (BUCKET_SECS * 1_000);
        let mut store = HistoryStore::new(path);
        store.record(&snapshot(minute + 1, HealthStatus::Ok, 100, 0, 0));
        store.record(&snapshot(minute + 10_001, HealthStatus::Warn, 50, 2, 1));

        let response = store.response(24);
        let point = &response.targets[0].points[0];
        assert_eq!(point.samples, 2);
        assert_eq!(point.status, HealthStatus::Warn);
        assert_eq!(point.rx_bytes, 150);
        assert_eq!(point.reconnects, 2);
        assert_eq!(point.disconnects, 1);
    }

    #[test]
    fn retains_exactly_twenty_four_hours() {
        let path = std::env::temp_dir().join("public-infra-history-retention.json");
        let end = now_unix_ms() / (BUCKET_SECS * 1_000) * (BUCKET_SECS * 1_000);
        let start = end.saturating_sub(MAX_BUCKETS as u64 * BUCKET_SECS * 1_000);
        let mut store = HistoryStore::new(path);
        for minute in 0..=(MAX_BUCKETS as u64) {
            store.record(&snapshot(
                start + minute * BUCKET_SECS * 1_000,
                HealthStatus::Ok,
                1,
                0,
                0,
            ));
        }

        let points = &store.response(24).targets[0].points;
        assert_eq!(points.len(), MAX_BUCKETS);
        assert_eq!(points[0].bucket_start_unix_ms, start + BUCKET_SECS * 1_000);
    }

    #[test]
    fn atomically_persists_and_reloads() {
        let unique = format!(
            "public-infra-history-{}-{}",
            std::process::id(),
            now_unix_ms()
        );
        let directory = std::env::temp_dir().join(unique);
        let path = directory.join("history.json");
        let minute = now_unix_ms() / (BUCKET_SECS * 1_000) * (BUCKET_SECS * 1_000);
        let mut store = HistoryStore::new(path.clone());
        store.record(&snapshot(minute, HealthStatus::Warn, 42, 1, 0));
        store.persist_if_due().unwrap();
        store.persist_latest().unwrap();

        assert!(path.exists());
        assert!(!temp_path(&path).exists());
        let loaded = HistoryStore::load(path.clone()).unwrap();
        let point = &loaded.response(24).targets[0].points[0];
        assert_eq!(point.rx_bytes, 42);
        assert_eq!(point.reconnects, 1);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reads_network_window_fields_without_socket_details() {
        let network = NetworkWindow {
            rx_bytes: Some(7),
            ..NetworkWindow::default()
        };
        assert_eq!(network.rx_bytes, Some(7));
    }
}
