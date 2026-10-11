/* A journal as a resumed run finds it on disk: torn, corrupted or written
 * by hand. Opening it and looking calls up must never crash. */
#define _POSIX_C_SOURCE 200809L
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "journal.h"
#include "json.h"

static char dir[64];

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {
    if (!dir[0]) {
        snprintf(dir, sizeof dir, "/tmp/calyx-fuzz-journal-%d", (int)getpid());
        cx_mkdirs(dir);
    }
    char path[128];
    snprintf(path, sizeof path, "%s/" CX_JOURNAL_FILE, dir);
    FILE *f = fopen(path, "wb");
    if (!f) return 0;
    /* A header that matches, then the fuzzer's lines. */
    fputs("{\"type\":\"run\",\"version\":1,\"ir_hash\":\"h\",\"graph\":\"g\"}\n", f);
    fwrite(data, 1, size, f);
    fclose(f);
    cx_arena a = {0};
    cx_buf err = {0};
    for (int mode = CX_JOURNAL_RESUME; mode <= CX_JOURNAL_REPLAY; mode++) {
        cx_journal *j = cx_journal_open(&a, dir, (cx_journal_mode)mode, "h", NULL, NULL, &err);
        if (!j) continue;
        int mismatch = 0;
        (void)cx_journal_lookup(j, "g/a#0", "x", &mismatch);
        (void)cx_journal_lookup(j, "", "", &mismatch);
        (void)cx_journal_uncertain(j, "g/a#0");
        cx_journal_close(j);
    }
    cx_buf_free(&err);
    cx_arena_free(&a);
    return 0;
}
