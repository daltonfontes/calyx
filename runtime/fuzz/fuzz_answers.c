/* The interpreter running answers.clyx (its IR in answers.ir.json, made by
 * `calyx check --ir-json`), with every model and tool answer from the
 * fuzzer: the input is the answers, in call order, separated by 0x1e. Any
 * answer, valid or not, must give a result or a failure, never a crash. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "calyx_runtime.h"
#include "stubs.h"

static char *ir;

static char *slurp(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    char *s = malloc((size_t)n + 1);
    if (s && fread(s, 1, (size_t)n, f) != (size_t)n) n = 0;
    if (s) s[n] = 0;
    fclose(f);
    return s;
}

int LLVMFuzzerInitialize(int *argc, char ***argv) {
    (void)argc;
    (void)argv;
    const char *path = getenv("CALYX_FUZZ_IR");
    ir = slurp(path ? path : "answers.ir.json");
    if (!ir) {
        fprintf(stderr, "fuzz_answers: no IR (set CALYX_FUZZ_IR)\n");
        exit(1);
    }
    return 0;
}

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {
    char *text = malloc(size + 1);
    memcpy(text, data, size);
    text[size] = 0;
    const char *parts[64];
    size_t n = 0;
    for (char *p = text; n < 64;) {
        parts[n++] = p;
        char *sep = memchr(p, 0x1e, (size_t)(text + size - p));
        if (!sep) break;
        *sep = 0;
        p = sep + 1;
    }
    fuzz_set_answers(parts, n);
    char *out = calyx_run(ir, "main", "{\"q\":\"A1\"}", "{\"deterministic\":true}");
    calyx_run_free(out);
    free(text);
    return 0;
}
