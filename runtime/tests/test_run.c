/* The interpreter from C: a small IR, run with a fake model. */
#include "calyx_runtime.h"

#include <stdio.h>
#include <string.h>

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
    char *out = calyx_run(IR, "g", "{\"x\":\"oi\"}", 0);
    EXPECT(out && strcmp(out,
                         "{\"ok\":[\"[resposta falsa para: Diga oi]\","
                         "\"[resposta falsa para: Diga mundo]\"]}") == 0,
           "fan-out over a list, in order");
    if (out) printf("%s\n", out);
    calyx_run_free(out);

    out = calyx_run(IR, "nope", "{}", 0);
    EXPECT(out && strstr(out, "no graph `nope`"), "unknown graph");
    calyx_run_free(out);

    out = calyx_run("{\"version\":99}", "g", "{}", 0);
    EXPECT(out && strstr(out, "\"error\""), "IR from another version");
    calyx_run_free(out);

    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("interpreter: all tests passed\n");
    return 0;
}
