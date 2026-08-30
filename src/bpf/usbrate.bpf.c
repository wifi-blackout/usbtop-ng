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

/* Single-slot counter of URBs whose bytes were lost because `bytes` was full
 * (see the giveback handler below). Userspace reads slot 0 each poll and warns
 * when it grows, so the bounded map-full loss surfaces instead of silently
 * under-reporting. */
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, __u64);
} dropped SEC(".maps");

SEC("kprobe/__usb_hcd_giveback_urb")
int BPF_KPROBE(on_giveback, struct urb *urb)
{
    __u32 len = BPF_CORE_READ(urb, actual_length);
    if (len == 0) return 0;
    __u8 ep = BPF_CORE_READ(urb, ep, desc.bEndpointAddress);
    unsigned int pipe = BPF_CORE_READ(urb, pipe);
    struct key_t k = {
        /* busnum/devnum are kernel `int`s (see vmlinux.h): CO-RE reads the
         * full 4 bytes, then this narrows the *value* into key_t's u16/u8 --
         * endian-safe, unlike declaring the vmlinux.h fields narrow. */
        .busnum = BPF_CORE_READ(urb, dev, bus, busnum),
        .devnum = BPF_CORE_READ(urb, dev, devnum),
        .epnum  = (__u8)(ep & 0x0f),
        /* Direction from the pipe (usb_pipein), not the descriptor's address
         * bit: endpoint 0's bEndpointAddress is always 0x00, so a descriptor-
         * bit direction would file every control IN transfer as OUT. The pipe
         * carries the URB's real direction for every transfer type, matching
         * what the usbmon backends account. */
        .dir_in = (__u8)((pipe >> 7) & 0x1),
        .xfer   = (__u8)((pipe >> 30) & 0x3),   /* usb_pipetype: 0 iso 1 int 2 ctrl 3 bulk */
    };
    __u64 *cur = bpf_map_lookup_elem(&bytes, &k);
    if (cur) { __sync_fetch_and_add(cur, (__u64)len); return 0; }
    /* First sight of this key. BPF_NOEXIST so two CPUs racing the same new
     * key don't clobber each other's first URB: the loser gets a non-zero
     * return, re-looks up the now-present entry, and adds to it. A genuine
     * map-full failure also lands here; the re-lookup misses and the bytes
     * are dropped -- the same bounded, documented map-full loss. */
    __u64 init = len;
    if (bpf_map_update_elem(&bytes, &k, &init, BPF_NOEXIST) != 0) {
        cur = bpf_map_lookup_elem(&bytes, &k);
        if (cur) {
            __sync_fetch_and_add(cur, (__u64)len);
        } else {
            /* Not a first-insert race (that re-lookup would have hit): the map
             * is genuinely full, so this URB's bytes have nowhere to go. Count
             * the drop so userspace can warn rather than under-report silently. */
            __u32 zero = 0;
            __u64 *d = bpf_map_lookup_elem(&dropped, &zero);
            if (d) __sync_fetch_and_add(d, (__u64)1);
        }
    }
    return 0;
}
