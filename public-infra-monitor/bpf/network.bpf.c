// SPDX-License-Identifier: GPL-2.0
#include "vmlinux.h"

#define SEC(name) __attribute__((section(name), used))
#define __always_inline inline __attribute__((always_inline))
#define __uint(name, value) int (*name)[value]
#define __type(name, value) typeof(value) *name

#define BPF_MAP_TYPE_HASH 1
#define BPF_MAP_TYPE_LRU_HASH 9
#define BPF_NOEXIST 1

static void *(*bpf_map_lookup_elem)(const void *map, const void *key) = (void *)1;
static long (*bpf_map_update_elem)(const void *map, const void *key,
                                   const void *value, __u64 flags) = (void *)2;
static __u64 (*bpf_ktime_get_ns)(void) = (void *)5;
static long (*bpf_probe_read_kernel)(void *dst, __u32 size,
                                     const void *unsafe_ptr) = (void *)113;

enum event_kind {
    EVENT_RETRANSMIT = 1,
    EVENT_STATE = 2,
};

struct flow_key {
    __u8 family;
    __u8 pad[3];
    __u16 sport;
    __u16 dport;
    __u8 saddr[16];
    __u8 daddr[16];
};

struct event_key {
    struct flow_key flow;
    __u8 kind;
    __u8 old_state;
    __u8 new_state;
    __u8 pad;
};

struct event_value {
    __u64 count;
    __u64 last_ns;
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 8192);
    __type(key, struct flow_key);
    __type(value, __u8);
} target_flows SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 4096);
    __type(key, struct event_key);
    __type(value, struct event_value);
} event_counts SEC(".maps");

static __always_inline int copy_flow(struct flow_key *key, __u16 family,
                                     __u16 sport, __u16 dport,
                                     const __u8 saddr[4],
                                     const __u8 daddr[4],
                                     const __u8 saddr_v6[16],
                                     const __u8 daddr_v6[16])
{
    __builtin_memset(key, 0, sizeof(*key));
    key->family = family;
    key->sport = sport;
    key->dport = dport;
    if (family == 2) {
        if (bpf_probe_read_kernel(key->saddr, 4, saddr) != 0 ||
            bpf_probe_read_kernel(key->daddr, 4, daddr) != 0)
            return -1;
        return 0;
    }
    if (family == 10) {
        if (bpf_probe_read_kernel(key->saddr, 16, saddr_v6) != 0 ||
            bpf_probe_read_kernel(key->daddr, 16, daddr_v6) != 0)
            return -1;
        return 0;
    }
    return -1;
}

static __always_inline int is_target(const struct flow_key *flow)
{
    return bpf_map_lookup_elem(&target_flows, flow) != 0;
}

static __always_inline void count_event(const struct event_key *key)
{
    struct event_value *value = bpf_map_lookup_elem(&event_counts, key);
    __u64 now = bpf_ktime_get_ns();

    if (value) {
        __sync_fetch_and_add(&value->count, 1);
        value->last_ns = now;
        return;
    }

    struct event_value initial = {
        .count = 1,
        .last_ns = now,
    };
    if (bpf_map_update_elem(&event_counts, key, &initial, BPF_NOEXIST) != 0) {
        value = bpf_map_lookup_elem(&event_counts, key);
        if (value) {
            __sync_fetch_and_add(&value->count, 1);
            value->last_ns = now;
        }
    }
}

SEC("tracepoint/tcp/tcp_retransmit_skb")
int on_retrans(struct trace_event_raw_tcp_retransmit_skb *ctx)
{
    struct event_key key = {};
    if (copy_flow(&key.flow, ctx->family, ctx->sport, ctx->dport,
                  ctx->saddr, ctx->daddr, ctx->saddr_v6,
                  ctx->daddr_v6) != 0)
        return 0;
    if (!is_target(&key.flow))
        return 0;

    key.kind = EVENT_RETRANSMIT;
    count_event(&key);
    return 0;
}

SEC("tracepoint/sock/inet_sock_set_state")
int on_state(struct trace_event_raw_inet_sock_set_state *ctx)
{
    if (ctx->protocol != 6)
        return 0;

    struct event_key key = {};
    if (copy_flow(&key.flow, ctx->family, ctx->sport, ctx->dport,
                  ctx->saddr, ctx->daddr, ctx->saddr_v6,
                  ctx->daddr_v6) != 0)
        return 0;
    if (!is_target(&key.flow))
        return 0;

    key.kind = EVENT_STATE;
    key.old_state = ctx->oldstate;
    key.new_state = ctx->newstate;
    count_event(&key);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
