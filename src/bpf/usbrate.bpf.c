#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

char LICENSE[] SEC("license") = "GPL";

struct key_t { __u16 busnum; __u8 devnum; __u8 epnum; __u8 dir_in; __u8 xfer; };

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 4096);
    __type(key, struct key_t);
    __type(value, __u64);
} bytes SEC(".maps");

SEC("kprobe/__usb_hcd_giveback_urb")
int BPF_KPROBE(on_giveback, struct urb *urb)
{
    __u32 len = BPF_CORE_READ(urb, actual_length);
    if (len == 0) return 0;
    __u8 ep = BPF_CORE_READ(urb, ep, desc.bEndpointAddress);
    unsigned int pipe = BPF_CORE_READ(urb, pipe);
    struct key_t k = {
        .busnum = BPF_CORE_READ(urb, dev, bus, busnum),
        .devnum = BPF_CORE_READ(urb, dev, devnum),
        .epnum  = (__u8)(ep & 0x0f),
        .dir_in = (ep & 0x80) ? 1 : 0,
        .xfer   = (__u8)((pipe >> 30) & 0x3),   /* usb_pipetype: 0 iso 1 int 2 ctrl 3 bulk */
    };
    __u64 *cur = bpf_map_lookup_elem(&bytes, &k);
    if (cur) __sync_fetch_and_add(cur, (__u64)len);
    else { __u64 init = len; bpf_map_update_elem(&bytes, &k, &init, BPF_ANY); }
    return 0;
}
