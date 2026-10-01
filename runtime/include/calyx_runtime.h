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

/*
 * Runs `graph` of a compiled program. `args_json` is a JSON object mapping
 * each parameter to its value. `options_json` may be NULL or an object:
 *
 *   "trace": true        one line per node and call on stderr
 *   "journal": "dir"     the run's directory (journal.jsonl, blobs/)
 *   "mode": "new" | "resume" | "replay"
 *                        new run; resume one, taking finished calls from
 *                        its journal; or replay it from the journal only
 *   "program": "path"    recorded in the journal, to find the source again
 *
 * Returns {"ok": value} or {"error": message} as JSON, to be released with
 * calyx_run_free(). Effects go through the I/O layer (calyx_io.h).
 */
char *calyx_run(const char *ir_json, const char *graph, const char *args_json,
                const char *options_json);
void calyx_run_free(char *s);

#ifdef __cplusplus
}
#endif

#endif /* CALYX_RUNTIME_H */
