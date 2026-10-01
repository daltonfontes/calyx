#include "json.h"

#include <math.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Out of memory is not recoverable in the interpreter: stop. */
static void *xmalloc(size_t n) {
    void *p = malloc(n ? n : 1);
    if (!p) {
        fputs("calyx: out of memory\n", stderr);
        abort();
    }
    return p;
}

static void *xrealloc(void *p, size_t n) {
    void *q = realloc(p, n ? n : 1);
    if (!q) {
        fputs("calyx: out of memory\n", stderr);
        abort();
    }
    return q;
}

/* ----- arena ------------------------------------------------------------ */

#define BLOCK_SIZE (64 * 1024)

struct cx_block {
    cx_block *next;
    size_t used, cap;
    _Alignas(16) unsigned char data[];
};

void *cx_alloc(cx_arena *a, size_t size) {
    size = (size + 15) & ~(size_t)15;
    cx_block *b = a->head;
    if (!b || b->cap - b->used < size) {
        size_t cap = size > BLOCK_SIZE ? size : BLOCK_SIZE;
        b = xmalloc(sizeof(cx_block) + cap);
        b->used = 0;
        b->cap = cap;
        b->next = a->head;
        a->head = b;
    }
    void *p = b->data + b->used;
    b->used += size;
    return p;
}

char *cx_strndup(cx_arena *a, const char *s, size_t len) {
    char *p = cx_alloc(a, len + 1);
    memcpy(p, s, len);
    p[len] = '\0';
    return p;
}

void cx_arena_free(cx_arena *a) {
    cx_block *b = a->head;
    while (b) {
        cx_block *next = b->next;
        free(b);
        b = next;
    }
    a->head = NULL;
}

/* ----- values ----------------------------------------------------------- */

static cx_value *make(cx_arena *a, cx_kind kind) {
    cx_value *v = cx_alloc(a, sizeof *v);
    memset(v, 0, sizeof *v);
    v->kind = kind;
    return v;
}

cx_value *cx_null(cx_arena *a) { return make(a, CX_NULL); }

cx_value *cx_bool(cx_arena *a, int b) {
    cx_value *v = make(a, CX_BOOL);
    v->u.b = b != 0;
    return v;
}

cx_value *cx_num(cx_arena *a, double n) {
    cx_value *v = make(a, CX_NUM);
    v->u.num = n;
    return v;
}

cx_value *cx_str(cx_arena *a, const char *s, size_t len) {
    cx_value *v = make(a, CX_STR);
    v->u.str.s = cx_strndup(a, s, len);
    v->u.str.len = len;
    return v;
}

cx_value *cx_cstr(cx_arena *a, const char *s) { return cx_str(a, s, strlen(s)); }

cx_value *cx_list(cx_arena *a, cx_value **items, size_t len) {
    cx_value *v = make(a, CX_LIST);
    v->u.list.items = cx_alloc(a, len * sizeof(cx_value *));
    if (len) memcpy(v->u.list.items, items, len * sizeof(cx_value *));
    v->u.list.len = len;
    return v;
}

cx_value *cx_rec(cx_arena *a, const char **keys, cx_value **vals, size_t len) {
    cx_value *v = make(a, CX_REC);
    v->u.rec.keys = cx_alloc(a, (len ? len : 1) * sizeof(char *));
    v->u.rec.vals = cx_alloc(a, (len ? len : 1) * sizeof(cx_value *));
    if (len) {
        memcpy(v->u.rec.keys, keys, len * sizeof(char *));
        memcpy(v->u.rec.vals, vals, len * sizeof(cx_value *));
    }
    v->u.rec.len = len;
    return v;
}

int cx_equal(const cx_value *x, const cx_value *y) {
    if (!x || !y) return x == y;
    if (x->kind != y->kind) return 0;
    switch (x->kind) {
    case CX_NULL: return 1;
    case CX_BOOL: return x->u.b == y->u.b;
    case CX_NUM: return x->u.num == y->u.num;
    case CX_STR:
        return x->u.str.len == y->u.str.len && memcmp(x->u.str.s, y->u.str.s, x->u.str.len) == 0;
    case CX_LIST:
        if (x->u.list.len != y->u.list.len) return 0;
        for (size_t i = 0; i < x->u.list.len; i++)
            if (!cx_equal(x->u.list.items[i], y->u.list.items[i])) return 0;
        return 1;
    case CX_REC:
        if (x->u.rec.len != y->u.rec.len) return 0;
        for (size_t i = 0; i < x->u.rec.len; i++)
            if (!cx_equal(x->u.rec.vals[i], cx_get(y, x->u.rec.keys[i]))) return 0;
        return 1;
    }
    return 0;
}

cx_value *cx_get(const cx_value *rec, const char *key) {
    if (!rec || rec->kind != CX_REC) return NULL;
    for (size_t i = 0; i < rec->u.rec.len; i++)
        if (strcmp(rec->u.rec.keys[i], key) == 0) return rec->u.rec.vals[i];
    return NULL;
}

const char *cx_get_str(const cx_value *rec, const char *key, const char *dflt) {
    cx_value *v = cx_get(rec, key);
    return v && v->kind == CX_STR ? v->u.str.s : dflt;
}

double cx_get_num(const cx_value *rec, const char *key, double dflt) {
    cx_value *v = cx_get(rec, key);
    return v && v->kind == CX_NUM ? v->u.num : dflt;
}

int cx_get_bool(const cx_value *rec, const char *key, int dflt) {
    cx_value *v = cx_get(rec, key);
    return v && v->kind == CX_BOOL ? v->u.b : dflt;
}

/* ----- text buffer ------------------------------------------------------ */

void cx_buf_put(cx_buf *b, const char *s, size_t len) {
    if (b->len + len + 1 > b->cap) {
        size_t cap = b->cap ? b->cap : 256;
        while (cap < b->len + len + 1) cap *= 2;
        b->data = xrealloc(b->data, cap);
        b->cap = cap;
    }
    memcpy(b->data + b->len, s, len);
    b->len += len;
    b->data[b->len] = '\0';
}

void cx_buf_puts(cx_buf *b, const char *s) { cx_buf_put(b, s, strlen(s)); }

void cx_buf_putc(cx_buf *b, char c) { cx_buf_put(b, &c, 1); }

void cx_buf_printf(cx_buf *b, const char *fmt, ...) {
    char small[256];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(small, sizeof small, fmt, ap);
    va_end(ap);
    if (n < 0) return;
    if ((size_t)n < sizeof small) {
        cx_buf_put(b, small, (size_t)n);
        return;
    }
    char *big = xmalloc((size_t)n + 1);
    va_start(ap, fmt);
    vsnprintf(big, (size_t)n + 1, fmt, ap);
    va_end(ap);
    cx_buf_put(b, big, (size_t)n);
    free(big);
}

void cx_buf_json_str(cx_buf *b, const char *s, size_t len) {
    cx_buf_putc(b, '"');
    for (size_t i = 0; i < len; i++) {
        unsigned char c = (unsigned char)s[i];
        switch (c) {
        case '"': cx_buf_puts(b, "\\\""); break;
        case '\\': cx_buf_puts(b, "\\\\"); break;
        case '\n': cx_buf_puts(b, "\\n"); break;
        case '\r': cx_buf_puts(b, "\\r"); break;
        case '\t': cx_buf_puts(b, "\\t"); break;
        default:
            if (c < 0x20)
                cx_buf_printf(b, "\\u%04x", c);
            else
                cx_buf_putc(b, (char)c);
        }
    }
    cx_buf_putc(b, '"');
}

void cx_buf_free(cx_buf *b) {
    free(b->data);
    b->data = NULL;
    b->len = b->cap = 0;
}

char *cx_buf_take(cx_buf *b) {
    if (!b->data) cx_buf_put(b, "", 0);
    char *s = b->data;
    b->data = NULL;
    b->len = b->cap = 0;
    return s;
}

static void write_num(cx_buf *b, double n) {
    if (!isfinite(n)) {
        cx_buf_puts(b, "null");
    } else if (n == floor(n) && fabs(n) < 9007199254740992.0) {
        cx_buf_printf(b, "%lld", (long long)n);
    } else {
        cx_buf_printf(b, "%.17g", n);
    }
}

void cx_write(cx_buf *b, const cx_value *v) {
    if (!v) {
        cx_buf_puts(b, "null");
        return;
    }
    switch (v->kind) {
    case CX_NULL: cx_buf_puts(b, "null"); break;
    case CX_BOOL: cx_buf_puts(b, v->u.b ? "true" : "false"); break;
    case CX_NUM: write_num(b, v->u.num); break;
    case CX_STR: cx_buf_json_str(b, v->u.str.s, v->u.str.len); break;
    case CX_LIST:
        cx_buf_putc(b, '[');
        for (size_t i = 0; i < v->u.list.len; i++) {
            if (i) cx_buf_putc(b, ',');
            cx_write(b, v->u.list.items[i]);
        }
        cx_buf_putc(b, ']');
        break;
    case CX_REC:
        cx_buf_putc(b, '{');
        for (size_t i = 0; i < v->u.rec.len; i++) {
            if (i) cx_buf_putc(b, ',');
            cx_buf_json_str(b, v->u.rec.keys[i], strlen(v->u.rec.keys[i]));
            cx_buf_putc(b, ':');
            cx_write(b, v->u.rec.vals[i]);
        }
        cx_buf_putc(b, '}');
        break;
    }
}

/* ----- parser ----------------------------------------------------------- */

typedef struct {
    cx_arena *a;
    const char *p, *end;
    const char *err;
    int depth;
} parser;

#define MAX_DEPTH 256

static void skip_ws(parser *ps) {
    while (ps->p < ps->end &&
           (*ps->p == ' ' || *ps->p == '\n' || *ps->p == '\r' || *ps->p == '\t'))
        ps->p++;
}

static cx_value *fail(parser *ps, const char *msg) {
    if (!ps->err) ps->err = msg;
    return NULL;
}

static int hex4(parser *ps, uint32_t *out) {
    if (ps->end - ps->p < 4) return 0;
    uint32_t v = 0;
    for (int i = 0; i < 4; i++) {
        char c = *ps->p++;
        v <<= 4;
        if (c >= '0' && c <= '9')
            v |= (uint32_t)(c - '0');
        else if (c >= 'a' && c <= 'f')
            v |= (uint32_t)(c - 'a' + 10);
        else if (c >= 'A' && c <= 'F')
            v |= (uint32_t)(c - 'A' + 10);
        else
            return 0;
    }
    *out = v;
    return 1;
}

static void put_utf8(cx_buf *b, uint32_t cp) {
    char out[4];
    if (cp < 0x80) {
        out[0] = (char)cp;
        cx_buf_put(b, out, 1);
    } else if (cp < 0x800) {
        out[0] = (char)(0xC0 | (cp >> 6));
        out[1] = (char)(0x80 | (cp & 0x3F));
        cx_buf_put(b, out, 2);
    } else if (cp < 0x10000) {
        out[0] = (char)(0xE0 | (cp >> 12));
        out[1] = (char)(0x80 | ((cp >> 6) & 0x3F));
        out[2] = (char)(0x80 | (cp & 0x3F));
        cx_buf_put(b, out, 3);
    } else {
        out[0] = (char)(0xF0 | (cp >> 18));
        out[1] = (char)(0x80 | ((cp >> 12) & 0x3F));
        out[2] = (char)(0x80 | ((cp >> 6) & 0x3F));
        out[3] = (char)(0x80 | (cp & 0x3F));
        cx_buf_put(b, out, 4);
    }
}

/* Parses a string literal (at the opening quote) into `b`. */
static int parse_string_into(parser *ps, cx_buf *b) {
    ps->p++; /* opening quote */
    while (ps->p < ps->end) {
        const char *start = ps->p;
        while (ps->p < ps->end && *ps->p != '"' && *ps->p != '\\' &&
               (unsigned char)*ps->p >= 0x20)
            ps->p++;
        cx_buf_put(b, start, (size_t)(ps->p - start));
        if (ps->p >= ps->end) break;
        char c = *ps->p++;
        if (c == '"') return 1;
        if (c != '\\') {
            fail(ps, "control character in string");
            return 0;
        }
        if (ps->p >= ps->end) break;
        char e = *ps->p++;
        switch (e) {
        case '"': cx_buf_putc(b, '"'); break;
        case '\\': cx_buf_putc(b, '\\'); break;
        case '/': cx_buf_putc(b, '/'); break;
        case 'b': cx_buf_putc(b, '\b'); break;
        case 'f': cx_buf_putc(b, '\f'); break;
        case 'n': cx_buf_putc(b, '\n'); break;
        case 'r': cx_buf_putc(b, '\r'); break;
        case 't': cx_buf_putc(b, '\t'); break;
        case 'u': {
            uint32_t cp;
            if (!hex4(ps, &cp)) {
                fail(ps, "invalid \\u escape");
                return 0;
            }
            if (cp >= 0xD800 && cp <= 0xDBFF) {
                uint32_t lo;
                if (ps->end - ps->p >= 6 && ps->p[0] == '\\' && ps->p[1] == 'u') {
                    ps->p += 2;
                    if (!hex4(ps, &lo) || lo < 0xDC00 || lo > 0xDFFF) {
                        fail(ps, "invalid surrogate pair");
                        return 0;
                    }
                    cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                } else {
                    cp = 0xFFFD;
                }
            } else if (cp >= 0xDC00 && cp <= 0xDFFF) {
                cp = 0xFFFD;
            }
            put_utf8(b, cp);
            break;
        }
        default: fail(ps, "invalid escape"); return 0;
        }
    }
    fail(ps, "unterminated string");
    return 0;
}

static cx_value *parse_value(parser *ps);

static cx_value *parse_string(parser *ps) {
    cx_buf b = {0};
    if (!parse_string_into(ps, &b)) {
        cx_buf_free(&b);
        return NULL;
    }
    cx_value *v = cx_str(ps->a, b.data ? b.data : "", b.len);
    cx_buf_free(&b);
    return v;
}

static cx_value *parse_number(parser *ps) {
    const char *start = ps->p;
    if (ps->p < ps->end && (*ps->p == '-' || *ps->p == '+')) ps->p++;
    while (ps->p < ps->end &&
           ((*ps->p >= '0' && *ps->p <= '9') || *ps->p == '.' || *ps->p == 'e' ||
            *ps->p == 'E' || *ps->p == '-' || *ps->p == '+'))
        ps->p++;
    char tmp[64];
    size_t n = (size_t)(ps->p - start);
    if (n == 0 || n >= sizeof tmp) return fail(ps, "invalid number");
    memcpy(tmp, start, n);
    tmp[n] = '\0';
    char *endp;
    double d = strtod(tmp, &endp);
    if (*endp) return fail(ps, "invalid number");
    return cx_num(ps->a, d);
}

/* Growable array of pointers, copied into the arena at the end. */
typedef struct {
    void **items;
    size_t len, cap;
} ptrvec;

static void ptrvec_push(ptrvec *v, void *p) {
    if (v->len == v->cap) {
        v->cap = v->cap ? v->cap * 2 : 8;
        v->items = xrealloc(v->items, v->cap * sizeof(void *));
    }
    v->items[v->len++] = p;
}

static cx_value *parse_list(parser *ps) {
    ps->p++; /* [ */
    ptrvec items = {0};
    skip_ws(ps);
    if (ps->p < ps->end && *ps->p == ']') {
        ps->p++;
        return cx_list(ps->a, NULL, 0);
    }
    for (;;) {
        cx_value *v = parse_value(ps);
        if (!v) goto bad;
        ptrvec_push(&items, v);
        skip_ws(ps);
        if (ps->p < ps->end && *ps->p == ',') {
            ps->p++;
            continue;
        }
        if (ps->p < ps->end && *ps->p == ']') {
            ps->p++;
            break;
        }
        fail(ps, "expected `,` or `]`");
        goto bad;
    }
    cx_value *list = cx_list(ps->a, (cx_value **)items.items, items.len);
    free(items.items);
    return list;
bad:
    free(items.items);
    return NULL;
}

static cx_value *parse_record(parser *ps) {
    ps->p++; /* { */
    ptrvec keys = {0}, vals = {0};
    skip_ws(ps);
    if (ps->p < ps->end && *ps->p == '}') {
        ps->p++;
        goto done;
    }
    for (;;) {
        skip_ws(ps);
        if (ps->p >= ps->end || *ps->p != '"') {
            fail(ps, "expected a field name");
            goto bad;
        }
        cx_value *k = parse_string(ps);
        if (!k) goto bad;
        skip_ws(ps);
        if (ps->p >= ps->end || *ps->p != ':') {
            fail(ps, "expected `:`");
            goto bad;
        }
        ps->p++;
        cx_value *v = parse_value(ps);
        if (!v) goto bad;
        ptrvec_push(&keys, (void *)k->u.str.s);
        ptrvec_push(&vals, v);
        skip_ws(ps);
        if (ps->p < ps->end && *ps->p == ',') {
            ps->p++;
            continue;
        }
        if (ps->p < ps->end && *ps->p == '}') {
            ps->p++;
            break;
        }
        fail(ps, "expected `,` or `}`");
        goto bad;
    }
done:;
    cx_value *r = make(ps->a, CX_REC);
    r->u.rec.len = keys.len;
    r->u.rec.keys = cx_alloc(ps->a, keys.len * sizeof(char *));
    r->u.rec.vals = cx_alloc(ps->a, vals.len * sizeof(cx_value *));
    if (keys.len) {
        memcpy(r->u.rec.keys, keys.items, keys.len * sizeof(char *));
        memcpy(r->u.rec.vals, vals.items, vals.len * sizeof(cx_value *));
    }
    free(keys.items);
    free(vals.items);
    return r;
bad:
    free(keys.items);
    free(vals.items);
    return NULL;
}

static int match_word(parser *ps, const char *w) {
    size_t n = strlen(w);
    if ((size_t)(ps->end - ps->p) >= n && memcmp(ps->p, w, n) == 0) {
        ps->p += n;
        return 1;
    }
    return 0;
}

static cx_value *parse_value(parser *ps) {
    if (++ps->depth > MAX_DEPTH) return fail(ps, "nested too deeply");
    skip_ws(ps);
    cx_value *v;
    if (ps->p >= ps->end) {
        v = fail(ps, "unexpected end of input");
    } else {
        switch (*ps->p) {
        case '{': v = parse_record(ps); break;
        case '[': v = parse_list(ps); break;
        case '"': v = parse_string(ps); break;
        case 't': v = match_word(ps, "true") ? cx_bool(ps->a, 1) : fail(ps, "invalid literal"); break;
        case 'f': v = match_word(ps, "false") ? cx_bool(ps->a, 0) : fail(ps, "invalid literal"); break;
        case 'n': v = match_word(ps, "null") ? cx_null(ps->a) : fail(ps, "invalid literal"); break;
        default:
            v = (*ps->p == '-' || (*ps->p >= '0' && *ps->p <= '9')) ? parse_number(ps)
                                                                    : fail(ps, "unexpected character");
        }
    }
    ps->depth--;
    return v;
}

cx_value *cx_parse(cx_arena *a, const char *text, size_t len, const char **err) {
    parser ps = {a, text, text + len, NULL, 0};
    cx_value *v = parse_value(&ps);
    if (v) {
        skip_ws(&ps);
        if (ps.p != ps.end) v = fail(&ps, "unexpected text after the value");
    }
    if (err) *err = v ? NULL : ps.err;
    return v;
}
