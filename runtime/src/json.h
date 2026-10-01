/*
 * json.h - values of the interpreter.
 *
 * Calyx values are immutable trees: text, numbers, booleans, lists and
 * records. They have the same shape as JSON, which is also how they travel
 * to the I/O layer and back, so one representation serves both.
 *
 * Every value of an execution lives in one arena and is released at once
 * when the execution ends: no reference counting and no collector.
 */
#ifndef CALYX_JSON_H
#define CALYX_JSON_H

#include <stddef.h>

/* ----- arena ------------------------------------------------------------ */

typedef struct cx_block cx_block;

typedef struct {
    cx_block *head;
} cx_arena;

void *cx_alloc(cx_arena *a, size_t size);
char *cx_strndup(cx_arena *a, const char *s, size_t len);
void cx_arena_free(cx_arena *a);

/* ----- values ----------------------------------------------------------- */

typedef enum { CX_NULL, CX_BOOL, CX_NUM, CX_STR, CX_LIST, CX_REC } cx_kind;

typedef struct cx_value cx_value;

struct cx_value {
    cx_kind kind;
    union {
        int b;
        double num;
        struct {
            const char *s; /* NUL-terminated; len excludes the NUL */
            size_t len;
        } str;
        struct {
            cx_value **items;
            size_t len;
        } list;
        struct {
            const char **keys;
            cx_value **vals;
            size_t len;
        } rec;
    } u;
};

cx_value *cx_null(cx_arena *a);
cx_value *cx_bool(cx_arena *a, int b);
cx_value *cx_num(cx_arena *a, double n);
cx_value *cx_str(cx_arena *a, const char *s, size_t len);
cx_value *cx_cstr(cx_arena *a, const char *s);
cx_value *cx_list(cx_arena *a, cx_value **items, size_t len);
/* A record; keys and values are copied (the keys must outlive it). */
cx_value *cx_rec(cx_arena *a, const char **keys, cx_value **vals, size_t len);

/* Deep equality: same kind and same contents (records: same fields, any order). */
int cx_equal(const cx_value *x, const cx_value *y);

/* Field of a record, or NULL. */
cx_value *cx_get(const cx_value *rec, const char *key);
/* Shorthands that return a default when the field is missing or of another kind. */
const char *cx_get_str(const cx_value *rec, const char *key, const char *dflt);
double cx_get_num(const cx_value *rec, const char *key, double dflt);
int cx_get_bool(const cx_value *rec, const char *key, int dflt);

/*
 * Parses JSON text into the arena. Returns NULL on invalid input and, when
 * `err` is not NULL, a static message.
 */
cx_value *cx_parse(cx_arena *a, const char *text, size_t len, const char **err);

/* ----- text buffer ------------------------------------------------------ */

typedef struct {
    char *data;
    size_t len, cap;
} cx_buf;

void cx_buf_put(cx_buf *b, const char *s, size_t len);
void cx_buf_puts(cx_buf *b, const char *s);
void cx_buf_putc(cx_buf *b, char c);
void cx_buf_printf(cx_buf *b, const char *fmt, ...);
/* Text as a JSON string literal, with quotes and escapes. */
void cx_buf_json_str(cx_buf *b, const char *s, size_t len);
void cx_buf_free(cx_buf *b);
/* Takes ownership of the text (malloc'd, NUL-terminated); resets the buffer. */
char *cx_buf_take(cx_buf *b);

/* Writes a value as compact JSON. */
void cx_write(cx_buf *b, const cx_value *v);

#endif /* CALYX_JSON_H */
