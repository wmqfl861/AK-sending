#ifndef AK_M0_PROBE_H
#define AK_M0_PROBE_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/* Synthetic loopback only. No file-transfer API. One loaded copy per process. */
/* status: running=0, passed=1, failed=-1, cancelled=-2, invalid=-3,
   busy=-4, exhausted=-5. Positive start result is an opaque handle.
   Cancel returns cancelled only if it wins before completion.
   Release returns busy until worker exits. Release before unloading the library. */
uint32_t ak_m0_probe_version(void);
int64_t ak_m0_probe_start(void);
int32_t ak_m0_probe_poll(uint64_t handle);
int32_t ak_m0_probe_cancel(uint64_t handle);
int32_t ak_m0_probe_release(uint64_t handle);
/* Independent LAN ABI v1. IPv4 is numeric network order (a<<24|b<<16|c<<8|d).
   Ports 1024..65535; RFC1918 or 127.0.0.1 only. prepare does not open sockets.
   READY=11, PAIRED=12, START=13, CONNECTING=14, ACTIVE=15, LISTENING=16.
   terminal pass=1, fail=-1, cancelled=-2, invalid=-3, busy=-4,
   exhausted=-5, bad_input=-6, connection_failed=-7, expired=-8, content=-9.
   Read kind 0=local DER, 1=local SHA256, 2=peer SHA256; UINT32_MAX=length.
   Push only ordered public DER bytes (<=4096). Configure freezes peer/role.
   Start requires LOCAL fingerprint verification + one-shot synthetic consent.
   No loaded library may be unloaded until both loopback/LAN slots released. */
uint32_t ak_m0_lan_version(void);
int64_t ak_m0_lan_prepare(uint32_t ip, uint32_t port);
int32_t ak_m0_lan_poll(uint64_t handle);
int32_t ak_m0_lan_read(uint64_t handle, uint32_t kind, uint32_t index);
int32_t ak_m0_lan_push(uint64_t handle, uint32_t index, uint32_t byte);
int32_t ak_m0_lan_configure(uint64_t handle, uint32_t ip, uint32_t port, uint32_t role);
int32_t ak_m0_lan_start(uint64_t handle);
int32_t ak_m0_lan_cancel(uint64_t handle);
int32_t ak_m0_lan_release(uint64_t handle);
#ifdef __cplusplus
}
#endif
#endif
