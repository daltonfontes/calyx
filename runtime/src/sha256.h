/*
 * sha256.h - SHA-256 (FIPS 180-4), for content addresses in the journal:
 * large payloads are stored by hash (decision D20), requests are compared
 * by hash on resume, and the program's IR is identified by its hash
 * (decision D23).
 */
#ifndef CALYX_SHA256_H
#define CALYX_SHA256_H

#include <stddef.h>

/* Writes the lowercase hex digest of `len` bytes at `data` (65 bytes with the NUL). */
void cx_sha256_hex(const void *data, size_t len, char out[65]);

#endif /* CALYX_SHA256_H */
