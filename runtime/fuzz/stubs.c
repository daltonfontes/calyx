/*
 * stubs.c - the Rust side of the runtime, replaced for fuzzing.
 *
 * The C interpreter calls the Rust layer for effects (models, tools), the
 * PostgreSQL journal, telemetry, sandboxes and delivery checks. The fuzz
 * targets link the C sources alone, with these: no database, no
 * telemetry, and model and tool answers taken from the fuzzer's input
 * (fuzz_set_answers), in the order the calls are made.
 */
#define _POSIX_C_SOURCE 200809L
#include <stdlib.h>
#include <string.h>

#include "stubs.h"

static const char *const *answers;
static size_t nanswers, next;

void fuzz_set_answers(const char *const *a, size_t n) {
    answers = a;
    nanswers = n;
    next = 0;
}

/* The next answer (the whole {"ok": ...} or {"error": ...} text, maybe
 * invalid); once they run out, an error that is not retried. */
static char *answer(void) {
    size_t i = __atomic_fetch_add(&next, 1, __ATOMIC_SEQ_CST);
    if (i < nanswers) return strdup(answers[i]);
    return strdup("{\"error\":{\"kind\":\"ToolError\",\"message\":\"no more answers\"}}");
}

char *calyx_io_model_call(const char *req) {
    (void)req;
    return answer();
}

char *calyx_io_tool_call(const char *req) {
    (void)req;
    return answer();
}

void calyx_string_free(char *s) { free(s); }
const char *calyx_runtime_version(void) { return "fuzz"; }

int calyx_pg_enabled(void) { return 0; }
int calyx_pg_create(const char *run) {
    (void)run;
    return 0;
}
int calyx_pg_append(const char *run, const char *line, size_t len) {
    (void)run;
    (void)line;
    (void)len;
    return 0;
}
int calyx_pg_flush(void) { return 0; }
char *calyx_pg_read(const char *run) {
    (void)run;
    return NULL;
}
int calyx_pg_status(const char *run, const char *status) {
    (void)run;
    (void)status;
    return 0;
}
int calyx_pg_blob_put(const char *hash, const char *data, size_t len) {
    (void)hash;
    (void)data;
    (void)len;
    return 0;
}
char *calyx_pg_blob_get(const char *hash) {
    (void)hash;
    return NULL;
}
int calyx_pg_entity(const char *entity, const char *khash, const char *key, int exclusive,
                    char *(*step)(void *, const char *), void *ud) {
    (void)entity;
    (void)khash;
    (void)key;
    (void)exclusive;
    (void)step;
    (void)ud;
    return 0;
}
int calyx_pg_file_append(const char *run, const char *name, const char *line, size_t len) {
    (void)run;
    (void)name;
    (void)line;
    (void)len;
    return 0;
}
char *calyx_pg_file_read(const char *run, const char *name) {
    (void)run;
    (void)name;
    return NULL;
}

char *calyx_sandbox_fork(const char *src, const char *key, const char *journal_dir) {
    (void)src;
    (void)key;
    (void)journal_dir;
    return NULL;
}

char *calyx_delivery_check(const char *run, const char *line) {
    (void)run;
    (void)line;
    return strdup("{\"ok\":null}");
}

int calyx_telemetry_enabled(void) { return 0; }
void calyx_telemetry_begin(const char *r, const char *g, const char *m) {
    (void)r;
    (void)g;
    (void)m;
}
void calyx_telemetry_event(const char *j) { (void)j; }
void calyx_telemetry_end(const char *s, const char *e) {
    (void)s;
    (void)e;
}
