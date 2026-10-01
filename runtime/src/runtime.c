#include "calyx_runtime.h"
#include "calyx_verify.h"

#include <string.h>

#define CALYX_RUNTIME_VERSION "0.2.0"

const char *calyx_runtime_version(void) { return CALYX_RUNTIME_VERSION; }

int calyx_runtime_verify(const char *src, size_t len, char **diagnostics_json) {
    if (diagnostics_json) *diagnostics_json = NULL;
    char *json = calyx_verify((const unsigned char *)src, len);
    if (!json) return -1;

    int valid = strcmp(json, "[]") == 0;
    if (diagnostics_json) {
        *diagnostics_json = json;
    } else {
        calyx_string_free(json);
    }
    return valid;
}
