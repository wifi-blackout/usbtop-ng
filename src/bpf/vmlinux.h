/* SPDX-License-Identifier: GPL-2.0 */
#ifndef __VMLINUX_H__
#define __VMLINUX_H__

/*
 * Minimal, hand-written CO-RE type header for usbrate.bpf.c.
 *
 * This is NOT a `bpftool btf dump ... format c` snapshot of a running
 * kernel's BTF (that dump is ~166k lines and pins the header to whatever
 * kernel produced it). It declares only the structs and fields the BPF
 * program actually reads. CO-RE (`BPF_CORE_READ` / `preserve_access_index`)
 * relocates every access against the *target* kernel's BTF at load time by
 * field name, so the field *offsets* implied by the layout below need not
 * match the real kernel struct layout.
 *
 * Each field's declared *width* still must, though: `BPF_CORE_READ` reads
 * exactly `sizeof(the locally declared type)` bytes at the relocated offset.
 * Declaring a field narrower than the kernel's real field (e.g. `__u16` for
 * a kernel `int`) reads only its low bytes -- correct only by little-endian
 * accident, wrong on a big-endian target. So `busnum`/`devnum` are `int`
 * here because the kernel defines them as `int` (verified against this
 * host's `/sys/kernel/btf/vmlinux`); the BPF program narrows the read *value*
 * into `key_t`'s `__u16`/`__u8` fields afterward, which is endian-safe.
 */

typedef signed char __s8;
typedef unsigned char __u8;
typedef short __s16;
typedef unsigned short __u16;
typedef int __s32;
typedef unsigned int __u32;
typedef long long __s64;
typedef unsigned long long __u64;

typedef __u16 __be16;
typedef __u16 __sum16;
typedef __u32 __be32;
typedef __u32 __wsum;
typedef __u64 __be64;
typedef __u32 __le32;

/*
 * `struct pt_regs` is the kprobe entry context on x86-64: bpf_tracing.h's
 * BPF_KPROBE macro reads its named registers (di/si/dx/cx/r8/r9 for the
 * first six arguments) whenever `__VMLINUX_H__` is defined, so -- unlike
 * the CO-RE structs below -- this one has to have the real kernel field
 * names and layout, not just the fields we happen to read.
 */
struct pt_regs {
	unsigned long r15;
	unsigned long r14;
	unsigned long r13;
	unsigned long r12;
	unsigned long bp;
	unsigned long bx;
	unsigned long r11;
	unsigned long r10;
	unsigned long r9;
	unsigned long r8;
	unsigned long ax;
	unsigned long cx;
	unsigned long dx;
	unsigned long si;
	unsigned long di;
	unsigned long orig_ax;
	unsigned long ip;
	unsigned long cs;
	unsigned long flags;
	unsigned long sp;
	unsigned long ss;
};

/*
 * `bpf/bpf_helper_defs.h` declares every BPF helper function, including
 * ones this program never calls, so every struct type any helper prototype
 * mentions (even only as an opaque pointer) needs at least a forward
 * declaration here for the header to parse standalone.
 */
struct __sk_buff;
struct bpf_fib_lookup;
struct bpf_map;
struct bpf_perf_event_data;
struct bpf_perf_event_value;
struct bpf_pidns_info;
struct bpf_redir_neigh;
struct bpf_sock;
struct bpf_sock_addr;
struct bpf_sock_ops;
struct bpf_sock_tuple;
struct bpf_spin_lock;
struct bpf_sysctl;
struct bpf_tcp_sock;
struct bpf_tunnel_key;
struct bpf_xfrm_state;
struct linux_binprm;
struct sk_msg_md;
struct sk_reuseport_md;
struct sockaddr;
struct tcphdr;
struct seq_file;
struct task_struct;
struct path;
struct inode;
struct socket;
struct file;
struct udp6_sock;
struct unix_sock;
struct mptcp_sock;
struct sock;
struct iphdr;
struct ipv6hdr;
struct in6_addr;
struct xdp_md;
struct btf_ptr;

/* Map-type and map-update-flag constants this program's `.maps` section and
 * `bpf_map_update_elem()` call reference, matching UAPI `linux/bpf.h`'s
 * `enum bpf_map_type` / `enum` values (libbpf reads the numeric value via
 * BTF, so the value -- not just the name -- must match the real kernel).
 */
enum bpf_map_type {
	BPF_MAP_TYPE_HASH = 1,
};

enum {
	BPF_ANY = 0,
	BPF_NOEXIST = 1,
};

#pragma clang attribute push (__attribute__((preserve_access_index)), apply_to = record)

struct usb_endpoint_descriptor {
	__u8 bEndpointAddress;
};

struct usb_host_endpoint {
	struct usb_endpoint_descriptor desc;
};

struct usb_bus {
	int busnum;
};

struct usb_device {
	struct usb_bus *bus;
	int devnum;
};

struct urb {
	unsigned int pipe;
	struct usb_device *dev;
	struct usb_host_endpoint *ep;
	__u32 actual_length;
};

#pragma clang attribute pop

#endif /* __VMLINUX_H__ */
