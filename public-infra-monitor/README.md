# public-infra-monitor

Independent, read-only process network monitor for the market-data host. The hot
path stays out of eBPF: Rust samples process-owned TCP sockets with
`NETLINK_SOCK_DIAG` and parses `INET_DIAG_INFO`/`INET_DIAG_SKMEMINFO`.
C eBPF is reserved for low-frequency retransmit and socket-state events.

## Data paths

- `/proc`: exact executable plus `--venue` process discovery, PID, cwd,
  affinity, current CPU, uptime and window CPU usage.
- `NETLINK_SOCK_DIAG`: tuple, state, queues, RTT, RTO, cwnd, last receive,
  bytes, retransmits and socket drops. Socket inode is joined against
  `/proc/<pid>/fd`; `/proc/<pid>/net/tcp` is not used as ownership data.
- sysfs/procfs: NIC statistics, TCP/IP counters and per-CPU softnet counters.
- C CO-RE: `tcp_retransmit_skb` and `inet_sock_set_state` only. A Rust-owned
  flow allowlist filters events in the kernel and an LRU hash aggregates them.
  There is no packet payload access, ring buffer, XDP or TC attachment.

## Build

The network-only build needs the Rust toolchain:

```sh
cargo build --release --manifest-path public-infra-monitor/Cargo.toml
```

The production BPF build vendors libbpf through `libbpf-rs` and links the
system libelf/zlib. It needs `clang`, `bpftool`, `make`, `pkg-config`
and the libelf/zlib development files at build time:

```sh
cargo build --release --features bpf --manifest-path public-infra-monitor/Cargo.toml
```

`build.rs` generates `vmlinux.h` from `/sys/kernel/btf/vmlinux` and embeds
the resulting CO-RE object in the Rust binary. Set
`PUBLIC_INFRA_VMLINUX_BTF` when building against another reference BTF file.

## Run

A bounded network-only window does not load BPF:

```sh
public-infra-monitor --config public-infra-monitor/config.example.json \
  --once --no-bpf --window-secs 10
```

Daemon mode serves:

- `GET /healthz`
- `GET /v1/snapshot`
- `GET /metrics`

The frontend route is `/nav/#/market-data`. In development, Vite proxies
`/market-data-api/` to the loopback listener. For nginx, apply the reviewed
`deploy/nginx-public-infra-monitor.patch` so the same path is proxied to
`127.0.0.1:9918`; the monitor itself should remain loopback-only.

The snapshot reports capability gaps explicitly. Version 0.1 exposes standard
sysfs NIC statistics but does not yet implement ENA vendor counters through
ethtool netlink, and leaves runqlat unset. Scheduler tracing should remain an
on-demand bounded window because `sched_switch` is a hot tracepoint.

The example systemd unit grants `CAP_BPF`, `CAP_PERFMON` and
`CAP_DAC_READ_SEARCH`. Some older kernels may require `CAP_SYS_ADMIN`; add it
only after validating the target kernel. A BPF load/attach failure is
reported in `capabilities.bpf_reason` while INET_DIAG monitoring continues.
`CAP_DAC_READ_SEARCH` is limited to the monitor service because some tracefs
mounts expose tracepoint ID files as `0440 root:root`; libbpf must read those
IDs before opening the perf events.
