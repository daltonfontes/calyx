/* Unit tests of the interpreter's values (src/json.c) and hashes (src/sha256.c). */
#include "json.h"
#include "sha256.h"

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

    /* FIPS 180-4 test vectors, plus inputs around the padding boundary. */
    char hex[65];
    cx_sha256_hex("", 0, hex);
    EXPECT(strcmp(hex, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855") == 0,
           "sha256 of empty input");
    cx_sha256_hex("abc", 3, hex);
    EXPECT(strcmp(hex, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad") == 0,
           "sha256 of abc");
    const char *two = "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    cx_sha256_hex(two, strlen(two), hex);
    EXPECT(strcmp(hex, "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1") == 0,
           "sha256 of a 56-byte input (two padding blocks)");

    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("values and hashes: all tests passed\n");
    return 0;
}
