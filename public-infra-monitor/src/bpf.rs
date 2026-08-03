use crate::model::{BpfEventObservation, TargetObservation};

#[derive(Debug, Default)]
pub struct TargetBpfWindow {
    pub retransmits: u64,
    pub establishes: u64,
    pub disconnects: u64,
    pub events: Vec<BpfEventObservation>,
}

#[cfg(feature = "bpf")]
mod imp {
    use std::{
        collections::{HashMap, HashSet},
        ffi::OsStr,
        net::IpAddr,
    };

    use anyhow::{Context, Result};
    use libbpf_rs::{Link, MapCore, MapFlags, Object, ObjectBuilder, TracepointCategory};

    use super::{TargetBpfWindow, TargetObservation};
    use crate::model::{BpfEventObservation, Endpoint, SocketObservation};

    const BPF_OBJECT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/network.bpf.o"));
    const FLOW_KEY_SIZE: usize = 40;
    const EVENT_KEY_SIZE: usize = 44;
    const EVENT_VALUE_SIZE: usize = 16;
    const EVENT_RETRANSMIT: u8 = 1;
    const EVENT_STATE: u8 = 2;
    const TCP_ESTABLISHED: u8 = 1;
    const TCP_CLOSE: u8 = 7;

    pub struct BpfMonitor {
        _links: Vec<Link>,
        object: Object,
        previous_counts: HashMap<Vec<u8>, u64>,
        flow_owners: HashMap<[u8; FLOW_KEY_SIZE], String>,
    }

    impl BpfMonitor {
        pub fn load() -> Result<Self> {
            let object = ObjectBuilder::default()
                .open_memory(BPF_OBJECT)
                .context("open embedded BPF object")?
                .load()
                .context("load network BPF object")?;
            let retransmit = object
                .progs_mut()
                .find(|program| program.name() == OsStr::new("on_retrans"))
                .context("BPF program on_retrans is missing")?
                .attach_tracepoint(
                    TracepointCategory::Custom("tcp".to_owned()),
                    "tcp_retransmit_skb",
                )
                .context("attach tcp_retransmit_skb tracepoint")?;
            let state = object
                .progs_mut()
                .find(|program| program.name() == OsStr::new("on_state"))
                .context("BPF program on_state is missing")?
                .attach_tracepoint(
                    TracepointCategory::Custom("sock".to_owned()),
                    "inet_sock_set_state",
                )
                .context("attach inet_sock_set_state tracepoint")?;

            Ok(Self {
                _links: vec![retransmit, state],
                object,
                previous_counts: HashMap::new(),
                flow_owners: HashMap::new(),
            })
        }

        pub fn sample(
            &mut self,
            targets: &[TargetObservation],
            completed_window: bool,
        ) -> Result<HashMap<String, TargetBpfWindow>> {
            let mut windows: HashMap<String, TargetBpfWindow> = targets
                .iter()
                .map(|target| (target.name.clone(), TargetBpfWindow::default()))
                .collect();
            let rows = self.read_event_rows()?;
            for (key, count, last_ns) in rows {
                let previous = self.previous_counts.insert(key.clone(), count);
                if !completed_window {
                    continue;
                }
                let delta = previous
                    .and_then(|value| count.checked_sub(value))
                    .unwrap_or(count);
                if delta == 0 || key.len() != EVENT_KEY_SIZE {
                    continue;
                }
                let flow: [u8; FLOW_KEY_SIZE] =
                    key[..FLOW_KEY_SIZE].try_into().expect("checked size");
                let Some(owner) = self.flow_owners.get(&flow) else {
                    continue;
                };
                let Some(window) = windows.get_mut(owner) else {
                    continue;
                };
                let kind = key[40];
                let old_state = key[41];
                let new_state = key[42];
                match kind {
                    EVENT_RETRANSMIT => window.retransmits += delta,
                    EVENT_STATE if new_state == TCP_ESTABLISHED => window.establishes += delta,
                    EVENT_STATE if new_state == TCP_CLOSE => window.disconnects += delta,
                    _ => {}
                }
                let (local, remote) = endpoints_from_flow(&flow);
                window.events.push(BpfEventObservation {
                    kind: match kind {
                        EVENT_RETRANSMIT => "retransmit",
                        EVENT_STATE => "state_change",
                        _ => "unknown",
                    }
                    .to_owned(),
                    local,
                    remote,
                    old_state: (kind == EVENT_STATE).then(|| tcp_state_name(old_state).to_owned()),
                    new_state: (kind == EVENT_STATE).then(|| tcp_state_name(new_state).to_owned()),
                    count: delta,
                    last_kernel_monotonic_ns: last_ns,
                });
            }

            self.sync_target_flows(targets)?;
            Ok(windows)
        }

        fn read_event_rows(&self) -> Result<Vec<(Vec<u8>, u64, u64)>> {
            let map = self
                .object
                .maps()
                .find(|map| map.name() == OsStr::new("event_counts"))
                .context("BPF map event_counts is missing")?;
            let mut rows = Vec::new();
            for key in map.keys() {
                if key.len() != EVENT_KEY_SIZE {
                    continue;
                }
                let Some(value) = map.lookup(&key, MapFlags::ANY)? else {
                    continue;
                };
                if value.len() < EVENT_VALUE_SIZE {
                    continue;
                }
                let count = u64::from_ne_bytes(value[0..8].try_into().unwrap());
                let last_ns = u64::from_ne_bytes(value[8..16].try_into().unwrap());
                rows.push((key, count, last_ns));
            }
            Ok(rows)
        }

        fn sync_target_flows(&mut self, targets: &[TargetObservation]) -> Result<()> {
            let desired: HashMap<[u8; FLOW_KEY_SIZE], String> = targets
                .iter()
                .flat_map(|target| {
                    target
                        .sockets
                        .iter()
                        .filter_map(|socket| flow_key(socket).map(|key| (key, target.name.clone())))
                })
                .collect();
            let map = self
                .object
                .maps_mut()
                .find(|map| map.name() == OsStr::new("target_flows"))
                .context("BPF map target_flows is missing")?;
            let existing: HashSet<Vec<u8>> = map.keys().collect();
            for key in &existing {
                if key
                    .as_slice()
                    .try_into()
                    .map(|key: &[u8; FLOW_KEY_SIZE]| !desired.contains_key(key))
                    .unwrap_or(true)
                {
                    map.delete(key)?;
                }
            }
            for key in desired.keys() {
                if !existing.contains(key.as_slice()) {
                    map.update(key, &[1], MapFlags::ANY)?;
                }
            }

            if self.flow_owners.len() > 16_384 {
                self.flow_owners.retain(|key, _| desired.contains_key(key));
            }
            self.flow_owners.extend(desired);
            Ok(())
        }
    }

    fn flow_key(socket: &SocketObservation) -> Option<[u8; FLOW_KEY_SIZE]> {
        let local: IpAddr = socket.local.address.parse().ok()?;
        let remote: IpAddr = socket.remote.address.parse().ok()?;
        let mut key = [0_u8; FLOW_KEY_SIZE];
        match (local, remote) {
            (IpAddr::V4(local), IpAddr::V4(remote)) => {
                key[0] = libc::AF_INET as u8;
                key[8..12].copy_from_slice(&local.octets());
                key[24..28].copy_from_slice(&remote.octets());
            }
            (IpAddr::V6(local), IpAddr::V6(remote)) => {
                key[0] = libc::AF_INET6 as u8;
                key[8..24].copy_from_slice(&local.octets());
                key[24..40].copy_from_slice(&remote.octets());
            }
            _ => return None,
        }
        key[4..6].copy_from_slice(&socket.local.port.to_ne_bytes());
        key[6..8].copy_from_slice(&socket.remote.port.to_ne_bytes());
        Some(key)
    }

    fn endpoints_from_flow(flow: &[u8; FLOW_KEY_SIZE]) -> (Endpoint, Endpoint) {
        let local_port = u16::from_ne_bytes(flow[4..6].try_into().unwrap());
        let remote_port = u16::from_ne_bytes(flow[6..8].try_into().unwrap());
        let (local_address, remote_address) = match flow[0] as libc::c_int {
            libc::AF_INET => (
                std::net::Ipv4Addr::from(<[u8; 4]>::try_from(&flow[8..12]).unwrap()).to_string(),
                std::net::Ipv4Addr::from(<[u8; 4]>::try_from(&flow[24..28]).unwrap()).to_string(),
            ),
            libc::AF_INET6 => (
                std::net::Ipv6Addr::from(<[u8; 16]>::try_from(&flow[8..24]).unwrap()).to_string(),
                std::net::Ipv6Addr::from(<[u8; 16]>::try_from(&flow[24..40]).unwrap()).to_string(),
            ),
            _ => ("unknown".to_owned(), "unknown".to_owned()),
        };
        (
            Endpoint {
                address: local_address,
                port: local_port,
            },
            Endpoint {
                address: remote_address,
                port: remote_port,
            },
        )
    }

    const fn tcp_state_name(state: u8) -> &'static str {
        match state {
            1 => "ESTABLISHED",
            2 => "SYN_SENT",
            3 => "SYN_RECV",
            4 => "FIN_WAIT1",
            5 => "FIN_WAIT2",
            6 => "TIME_WAIT",
            7 => "CLOSE",
            8 => "CLOSE_WAIT",
            9 => "LAST_ACK",
            10 => "LISTEN",
            11 => "CLOSING",
            12 => "NEW_SYN_RECV",
            _ => "UNKNOWN",
        }
    }

    pub use BpfMonitor as Monitor;
}

#[cfg(not(feature = "bpf"))]
mod imp {
    use std::collections::HashMap;

    use anyhow::{Result, bail};

    use super::{TargetBpfWindow, TargetObservation};

    pub struct BpfMonitor;

    impl BpfMonitor {
        pub fn load() -> Result<Self> {
            bail!("binary was built without the bpf feature")
        }

        pub fn sample(
            &mut self,
            _targets: &[TargetObservation],
            _completed_window: bool,
        ) -> Result<HashMap<String, TargetBpfWindow>> {
            Ok(HashMap::new())
        }
    }

    pub use BpfMonitor as Monitor;
}

pub use imp::Monitor as BpfMonitor;
