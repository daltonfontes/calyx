#ifndef CALYX_FUZZ_STUBS_H
#define CALYX_FUZZ_STUBS_H
#include <stddef.h>
/* The answers the next calls get, in order (not copied: kept by the caller). */
void fuzz_set_answers(const char *const *answers, size_t n);
#endif
