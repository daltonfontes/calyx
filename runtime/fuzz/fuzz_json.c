/* The JSON parser on any bytes: never a crash, and what parses writes
 * back to text that parses to an equal value. */
#include <assert.h>
#include <stddef.h>
#include <stdint.h>

#include "json.h"

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {
    cx_arena a = {0};
    const char *err = NULL;
    cx_value *v = cx_parse(&a, (const char *)data, size, &err);
    if (v) {
        cx_buf b = {0};
        cx_write(&b, v);
        cx_value *again = cx_parse(&a, b.data ? b.data : "", b.len, &err);
        assert(again && "written JSON parses");
        assert(cx_equal(v, again) && "and is the same value");
        (void)cx_get(v, "key");
        (void)cx_get_str(v, "k", "");
        (void)cx_get_num(v, "n", 0);
        cx_buf_free(&b);
    }
    cx_arena_free(&a);
    return 0;
}
