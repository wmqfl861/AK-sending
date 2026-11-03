#include "include/ak_m0_probe.h"
/* Link-only on iOS. This executable is not signed, installed, or device-tested. */
int main(int argc, char **argv) {
    (void)argv;
    if (argc == 42) {
        int64_t handle = ak_m0_probe_start();
        if (handle > 0) {
            (void)ak_m0_probe_poll((uint64_t)handle);
            (void)ak_m0_probe_cancel((uint64_t)handle);
            (void)ak_m0_probe_release((uint64_t)handle);
        }
    }
    if (argc == 43) {
        int64_t h = ak_m0_lan_prepare(0x7f000001, 42424);
        (void)ak_m0_lan_poll((uint64_t)h);
        (void)ak_m0_lan_read((uint64_t)h, 0, UINT32_MAX);
        (void)ak_m0_lan_push((uint64_t)h, 0, 0);
        (void)ak_m0_lan_configure((uint64_t)h, 0x7f000001, 42425, 0);
        (void)ak_m0_lan_start((uint64_t)h);
        (void)ak_m0_lan_cancel((uint64_t)h);
        (void)ak_m0_lan_release((uint64_t)h);
    }
    if (ak_m0_lan_version() != 1) return 1;
    return ak_m0_probe_version() == 0x00010000 ? 0 : 1;
}
