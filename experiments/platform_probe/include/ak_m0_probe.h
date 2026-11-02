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
#ifdef __cplusplus
}
#endif
#endif
