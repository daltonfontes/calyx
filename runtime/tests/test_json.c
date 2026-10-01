/* Unit tests of the interpreter's values (src/json.c). */
#include "json.h"

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

/* Parses and writes back; compares with `expected`. */
static void round_trip(const char *in, const char *expected) {
    cx_arena a = {0};
    const char *err = NULL;
    cx_value *v = cx_parse(&a, in, strlen(in), &err);
    cx_buf b = {0};
    cx_write(&b, v);
    if (!v || strcmp(b.data, expected) != 0) {
        fprintf(stderr, "FAIL round trip of %s: got %s (%s)\n", in, v ? b.data : "NULL",
                err ? err : "");
        failures++;
    }
    cx_buf_free(&b);
    cx_arena_free(&a);
}

static void rejects(const char *in) {
    cx_arena a = {0};
    const char *err = NULL;
    cx_value *v = cx_parse(&a, in, strlen(in), &err);
    if (v || !err) {
        fprintf(stderr, "FAIL accepted invalid JSON: %s\n", in);
        failures++;
    }
    cx_arena_free(&a);
}

int main(void) {
    round_trip(" { \"a\" : [1, 2.5, -3e2, true, false, null] } ",
               "{\"a\":[1,2.5,-300,true,false,null]}");
    round_trip("\"linha\\nnova \\\"aspas\\\" \\u00e9 \\ud83d\\ude00\"",
               "\"linha\\nnova \\\"aspas\\\" é 😀\"");
    round_trip("[]", "[]");
    round_trip("{}", "{}");
    round_trip("\"\\u0001\"", "\"\\u0001\"");

    rejects("");
    rejects("[1,]");
    rejects("{\"a\" 1}");
    rejects("\"sem fim");
    rejects("1 2");
    rejects("tru");

    cx_arena a = {0};
    const char *text = "{\"n\":3,\"s\":\"x\",\"b\":true}";
    cx_value *v = cx_parse(&a, text, strlen(text), NULL);
    EXPECT(cx_get_num(v, "n", 0) == 3, "number field");
    EXPECT(strcmp(cx_get_str(v, "s", ""), "x") == 0, "text field");
    EXPECT(cx_get_bool(v, "b", 0) == 1, "bool field");
    EXPECT(cx_get(v, "missing") == NULL, "missing field");
    cx_arena_free(&a);

    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("values: all tests passed\n");
    return 0;
}
