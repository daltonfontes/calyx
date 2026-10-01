/*
 * calyx_runtime.h - public interface of the Calyx runtime.
 *
 * M0 only exposes version information and a helper that runs the shared
 * verifier on a source buffer. The interpreter arrives in M2.
 */
#ifndef CALYX_RUNTIME_H
#define CALYX_RUNTIME_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Version of the runtime. Static string; do not free. */
const char *calyx_runtime_version(void);

/*
 * Verifies `len` bytes of Calyx source before running it.
 * Returns 1 if the program is valid, 0 if it has diagnostics, -1 on error.
 * When `diagnostics_json` is not NULL, it receives the JSON diagnostics,
 * to be released with calyx_string_free().
 */
int calyx_runtime_verify(const char *src, size_t len, char **diagnostics_json);

#ifdef __cplusplus
}
#endif

#endif /* CALYX_RUNTIME_H */
