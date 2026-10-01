/*
 * calyx_runtime.h - public interface of the Calyx runtime.
 *
 * The interpreter runs a compiled program (its IR, in JSON, as printed by
 * `calyx check --ir-json`), and a helper runs the shared verifier on source.
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

/* Flag for calyx_run(): one line per node and effect on stderr. */
#define CALYX_RUN_TRACE 1

/*
 * Runs `graph` of a compiled program. `args_json` is a JSON object mapping
 * each parameter to its value. Returns {"ok": value} or {"error": message}
 * as JSON, to be released with calyx_run_free(). Effects go through the
 * I/O layer (calyx_io.h), configured by calyx.toml.
 */
char *calyx_run(const char *ir_json, const char *graph, const char *args_json, int flags);
void calyx_run_free(char *s);

#ifdef __cplusplus
}
#endif

#endif /* CALYX_RUNTIME_H */
