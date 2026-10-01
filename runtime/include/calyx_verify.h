/*
 * calyx_verify.h - C interface of the Calyx verifier.
 *
 * The verifier is written in Rust (compiler/calyx-check) and linked into the
 * runtime as a static library, so the runtime checks graphs generated at run
 * time with exactly the same code as `calyx check` (decision D10).
 */
#ifndef CALYX_VERIFY_H
#define CALYX_VERIFY_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Checks `len` bytes of UTF-8 Calyx source at `src`.
 *
 * Returns a newly allocated, NUL-terminated JSON array of diagnostics
 * ("[]" when the program is valid), or NULL if `src` is NULL or not UTF-8.
 * The result must be released with calyx_string_free().
 */
char *calyx_verify(const unsigned char *src, size_t len);

/* Releases a string returned by this library. NULL is ignored. */
void calyx_string_free(char *s);

/* Version of the verifier. Static string; do not free. */
const char *calyx_verifier_version(void);

#ifdef __cplusplus
}
#endif

#endif /* CALYX_VERIFY_H */
