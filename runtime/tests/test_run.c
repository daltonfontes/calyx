/* The interpreter from C: a small IR, run with a fake model. */
#include "calyx_runtime.h"

#define _POSIX_C_SOURCE 200809L
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int failures;

#define EXPECT(cond, msg)                                                    \
    do {                                                                     \
        if (!(cond)) {                                                       \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, msg);    \
            failures++;                                                      \
        }                                                                    \
    } while (0)

/*
 * model m = "fake"
 * prompt echo(x: Text) -> Text: """Diga {x}"""
 * graph g(x: Text) -> List[Text]:
 *     both = for each w in [x, "mundo"]: m(echo(w))
 *     return both
 */
static const char *IR =
    "{\"version\":1,"
    "\"models\":[{\"name\":\"m\",\"id\":\"fake\",\"max_output\":null}],"
    "\"tools\":[],"
    "\"prompts\":[{\"name\":\"echo\",\"params\":[\"x\"],"
    "\"parts\":[{\"lit\":\"Diga \"},{\"path\":[\"x\"]}],\"schema\":null,\"wrapped\":false}],"
    "\"graphs\":[{\"name\":\"g\",\"params\":[\"x\"],\"param_types\":[\"Text\"],\"ret\":\"List[Text]\","
    "\"nodes\":[{\"name\":\"both\",\"type\":\"List[Text]\",\"effect\":\"llm\","
    "\"over\":{\"k\":\"list\",\"items\":[{\"k\":\"param\",\"i\":0},{\"k\":\"text\",\"v\":\"mundo\"}]},"
    "\"value\":{\"k\":\"model\",\"model\":0,\"prompt\":0,\"args\":[{\"k\":\"item\"}]}}],"
    "\"output\":0}]}";

int main(void) {
    char *out = calyx_run(IR, "g", "{\"x\":\"oi\"}", NULL);
    EXPECT(out && strcmp(out,
                         "{\"ok\":[\"[resposta falsa para: Diga oi]\","
                         "\"[resposta falsa para: Diga mundo]\"]}") == 0,
           "fan-out over a list, in order");
    if (out) printf("%s\n", out);
    calyx_run_free(out);

    out = calyx_run(IR, "nope", "{}", NULL);
    EXPECT(out && strstr(out, "no graph `nope`"), "unknown graph");
    calyx_run_free(out);

    out = calyx_run("{\"version\":99}", "g", "{}", NULL);
    EXPECT(out && strstr(out, "\"error\""), "IR from another version");
    calyx_run_free(out);

    /* A run with a journal, then a replay of it that calls nothing. */
    char dir[] = "/tmp/calyx-test-run-XXXXXX";
    if (!mkdtemp(dir)) {
        perror("mkdtemp");
        return 1;
    }
    char opts[512];
    snprintf(opts, sizeof opts, "{\"journal\":\"%s/run\",\"mode\":\"new\"}", dir);
    char *first = calyx_run(IR, "g", "{\"x\":\"oi\"}", opts);
    snprintf(opts, sizeof opts, "{\"journal\":\"%s/run\",\"mode\":\"replay\"}", dir);
    char *again = calyx_run(IR, "g", "{\"x\":\"oi\"}", opts);
    EXPECT(first && again && strcmp(first, again) == 0, "replay gives the same result");
    calyx_run_free(first);
    calyx_run_free(again);
    /* A new run never overwrites an existing journal. */
    snprintf(opts, sizeof opts, "{\"journal\":\"%s/run\",\"mode\":\"new\"}", dir);
    out = calyx_run(IR, "g", "{\"x\":\"oi\"}", opts);
    EXPECT(out && strstr(out, "cannot create the journal"), "existing journal is kept");
    calyx_run_free(out);
    char cmd[600];
    snprintf(cmd, sizeof cmd, "rm -rf %s", dir);
    if (system(cmd) != 0) fprintf(stderr, "warning: could not remove %s\n", dir);

    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("interpreter: all tests passed\n");
    return 0;
}
