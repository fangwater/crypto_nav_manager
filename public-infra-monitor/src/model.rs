use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HealthStatus {
    Ok,
    Warn,
    Critical,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub timestamp_unix_ms: u64,
    pub window_secs: Option<f64>,
    pub host: HostObservation,
    pub targets: Vec<TargetObservation>,
    pub system: SystemObservation,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostObservation {
    pub hostname: String,
    pub kernel_release: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Capabilities {
    pub inet_diag: bool,
    pub nic_standard_stats: bool,
    pub ethtool_stats: bool,
    pub runqlat: bool,
    pub bpf: bool,
    pub bpf_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetObservation {
    pub name: String,
    pub venue: String,
    pub expected_cpu: u32,
    pub process: Option<ProcessObservation>,
    pub process_candidates: usize,
    pub sockets: Vec<SocketObservation>,
    pub network: NetworkWindow,
    pub bpf_events: Vec<BpfEventObservation>,
    pub status: HealthStatus,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessObservation {
    pub pid: u32,
    pub executable: String,
    pub cmdline: String,
    pub cwd: Option<String>,
    pub affinity: Vec<u32>,
    pub current_cpu: Option<u32>,
    pub uptime_secs: Option<f64>,
    pub cpu_percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Endpoint {
    pub address: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct SocketObservation {
    pub inode: u64,
    pub family: String,
    pub state: String,
    pub local: Endpoint,
    pub remote: Endpoint,
    pub recv_queue_bytes: u64,
    pub send_queue_bytes: u64,
    pub rtt_us: Option<u32>,
    pub rto_us: Option<u32>,
    pub snd_cwnd: Option<u32>,
    pub last_data_recv_ms: Option<u32>,
    pub bytes_received: Option<u64>,
    pub bytes_sent: Option<u64>,
    pub total_retrans: Option<u32>,
    pub socket_drops: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NetworkWindow {
    pub socket_count: usize,
    pub established_count: usize,
    pub rx_bytes: Option<u64>,
    pub rx_idle_secs: Option<f64>,
    pub tx_bytes: Option<u64>,
    pub retransmits: Option<u64>,
    pub socket_drops: Option<u64>,
    pub reconnects: Option<u64>,
    pub disconnects: Option<u64>,
    pub recv_queue_bytes: u64,
    pub send_queue_bytes: u64,
    pub max_rtt_us: Option<u32>,
    pub max_rto_us: Option<u32>,
    pub max_last_data_recv_ms: Option<u32>,
    pub runqlat_p99_us: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BpfEventObservation {
    pub kind: String,
    pub local: Endpoint,
    pub remote: Endpoint,
    pub old_state: Option<String>,
    pub new_state: Option<String>,
    pub count: u64,
    pub last_kernel_monotonic_ns: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SystemObservation {
    pub interface: String,
    pub status: HealthStatus,
    pub reasons: Vec<String>,
    pub nic: BTreeMap<String, Counter>,
    pub tcp: BTreeMap<String, Counter>,
    pub softnet: SoftnetObservation,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counter {
    pub total: u64,
    pub delta: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SoftnetObservation {
    pub processed: Counter,
    pub dropped: Counter,
    pub time_squeeze: Counter,
    pub per_cpu: Vec<SoftnetCpuObservation>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SoftnetCpuObservation {
    pub cpu: usize,
    pub processed: Counter,
    pub dropped: Counter,
    pub time_squeeze: Counter,
}
