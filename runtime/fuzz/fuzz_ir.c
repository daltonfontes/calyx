/* The interpreter given any IR (graphs a model wrote are checked by the
 * verifier first, but the loader must not trust its input either): a
 * result or an error, never a crash. Models and tools answer nothing. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "calyx_runtime.h"
#include "stubs.h"

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {
    char *ir = malloc(size + 1);
    memcpy(ir, data, size);
    ir[size] = 0;
    fuzz_set_answers(NULL, 0);
    char *out = calyx_run(ir, "main", "{\"q\":\"x\",\"n\":2,\"items\":[\"a\",\"b\"]}",
                          "{\"deterministic\":true}");
    calyx_run_free(out);
    free(ir);
    return 0;
}
