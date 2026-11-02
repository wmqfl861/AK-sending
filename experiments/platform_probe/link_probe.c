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
    return ak_m0_probe_version() == 0x00010000 ? 0 : 1;
}
