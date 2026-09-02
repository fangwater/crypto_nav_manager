# public-infra-monitor

Independent, read-only process network monitor for the market-data host. The hot
path stays out of eBPF: Rust samples process-owned TCP sockets with
`NETLINK_SOCK_DIAG` and parses `INET_DIAG_INFO`/`INET_DIAG_SKMEMINFO`.
C eBPF is reserved for low-frequency retransmit and socket-state events.

## Data paths

- `/proc`: exact executable plus `--venue` and optional contiguous `match_args`
  process discovery, PID, cwd, affinity, current CPU, uptime and window CPU
  usage. Targets may share a venue only when each has distinct non-empty
  `match_args`.
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
- `GET /v1/history?hours=24`
- `GET /metrics`

Completed sampling windows are aggregated into one-minute buckets and retained
for 24 hours (at most 1,440 buckets per target). Once per minute the daemon
atomically replaces `/var/lib/public-infra-monitor/history.json`; it does not
create daily or append-only files.

The frontend presents each target separately and folds those stored minute
buckets into five-minute worst-state intervals for the 24-hour overview.

## Notifications

The monitor can reuse the host-local `notification_server` rather than owning a
Telegram bot or token. When enabled, a bounded worker queue sends JSON requests
to `POST /v1/notify` on `127.0.0.1:18100`; configuration rejects non-loopback
addresses. Sampling never waits for delivery. Queue saturation and delivery
failures are exposed by the `public_infra_notifications_*_total` metrics.

Missing processes, missing established sockets and sustained RX silence notify
immediately. Queue, retransmit, drop, reconnect and host-network degradation must
persist for three consecutive windows. Escalation is immediate, an unchanged
fault repeats after 15 minutes, and recovery requires six consecutive healthy
windows. A recovered non-immediate incident cannot re-arm for 15 minutes.
Isolated disconnects remain visible in history but do not notify.
CPU-affinity-only warnings do not notify.

If the local notification API requires authentication, provide its bearer token
as `PUBLIC_INFRA_NOTIFICATION_TOKEN` in the monitor service environment. Do not
put the Telegram bot token in this monitor's configuration.

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
