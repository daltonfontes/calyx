/*
 * Checks that the C runtime calls the Rust verifier through the static
 * library: one verifier, used by both `calyx check` and the runtime.
 */
#include "calyx_runtime.h"
#include "calyx_verify.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;

#define EXPECT(cond, msg)                                    \
    do {                                                     \
        if (!(cond)) {                                       \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__,    \
                    __LINE__, msg);                          \
            failures++;                                      \
        }                                                    \
    } while (0)

static void test_valid_program(void) {
    const char *src = "graph g() -> Text:\n    return \"oi\"\n";
    char *json = NULL;
    int r = calyx_runtime_verify(src, strlen(src), &json);
    EXPECT(r == 1, "valid program should be accepted");
    EXPECT(json && strcmp(json, "[]") == 0, "valid program should have no diagnostics");
    calyx_string_free(json);
}

static void test_invalid_program(void) {
    const char *src = "node x = f(a);\n";
    char *json = NULL;
    int r = calyx_runtime_verify(src, strlen(src), &json);
    EXPECT(r == 0, "invalid program should be rejected");
    EXPECT(json && strstr(json, "\"code\":\"E0004\""), "diagnostic E0004 expected");
    calyx_string_free(json);
}

static void test_invalid_utf8(void) {
    const char src[] = {(char)0xff, (char)0xfe};
    EXPECT(calyx_runtime_verify(src, sizeof src, NULL) == -1, "invalid UTF-8 should fail");
}

static void test_versions(void) {
    EXPECT(strcmp(calyx_verifier_version(), calyx_runtime_version()) == 0,
           "runtime and verifier versions should match");
}

int main(void) {
    test_valid_program();
    test_invalid_program();
    test_invalid_utf8();
    test_versions();
    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return EXIT_FAILURE;
    }
    printf("runtime: all tests passed (verifier %s)\n", calyx_verifier_version());
    return EXIT_SUCCESS;
}
