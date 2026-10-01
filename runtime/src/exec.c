/*
 * exec.c - the interpreter (milestones M2 and M3).
 *
 * Runs the template of decision D8 (the IR, in JSON) in one thread: nodes
 * in dependency order, fan-outs item by item, in the order of the list
 * (decision D7). Effects go to the I/O layer; this file owns the policies
 * around them: retries of temporary errors with backoff, which depend on
 * the effect (decisions D2, D11, D22), decoding model answers into the
 * prompt's type, and the trace.
 *
 * Every completed call goes to the journal (journal.h) under a key that
 * names its place in the realized graph. Resuming a run executes the graph
 * again and takes each call already in the journal from it, so a crash
 * costs at most the calls that were in flight. Replay takes every call
 * from the journal and calls nothing.
 *
 * Still to come: concurrency and limits (M4), failures as values (M5).
 */
#define _POSIX_C_SOURCE 200809L

#include "calyx_io.h"
#include "calyx_runtime.h"
#include "calyx_verify.h"
#include "journal.h"
#include "json.h"
#include "sha256.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define IR_VERSION 1
#define MAX_GRAPH_DEPTH 64
#define MAX_ATTEMPTS 4

typedef struct {
    cx_arena arena;
    cx_value *models, *tools, *prompts, *graphs;
    int trace;
    double t0;
    cx_buf err;
    int depth;
    cx_journal *journal; /* NULL: run without a journal */
    /* Testing crash recovery: exit abruptly after this many new entries. */
    long crash_after, recorded;
    /* Totals for the summary line. */
    unsigned long model_calls, tool_calls, retries, from_journal;
    unsigned long long input_tokens, output_tokens;
} exec;

typedef struct {
    cx_value *graph;
    cx_value **params;
    cx_value **nodes;
    cx_value *item;
    const char *node; /* name of the node being computed */
    const char *path; /* place of this graph in the realized graph */
    char *instance;   /* place of the node instance being computed */
    int seq;          /* calls made so far by this node instance */
} frame;

static double now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static void sleep_ms(long ms) {
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    while (nanosleep(&ts, &ts) != 0) {
    }
}

/* Records the first error (with the node it happened in) and returns NULL. */
static cx_value *failf(exec *x, frame *f, const char *fmt, ...) {
    if (x->err.len) return NULL;
    if (f && f->node)
        cx_buf_printf(&x->err, "in graph `%s`, node `%s`: ", cx_get_str(f->graph, "name", "?"),
                      f->node);
    char msg[1024];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof msg, fmt, ap);
    va_end(ap);
    cx_buf_puts(&x->err, msg);
    return NULL;
}

static cx_value *at(const cx_value *list, size_t i) {
    if (!list || list->kind != CX_LIST || i >= list->u.list.len) return NULL;
    return list->u.list.items[i];
}

static size_t len_of(const cx_value *list) {
    return list && list->kind == CX_LIST ? list->u.list.len : 0;
}

static size_t index_of(const cx_value *e, const char *key) {
    double d = cx_get_num(e, key, -1);
    return d < 0 ? (size_t)-1 : (size_t)d;
}

static void trace(exec *x, frame *f, const char *fmt, ...) {
    if (!x->trace) return;
    char msg[1024];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof msg, fmt, ap);
    va_end(ap);
    fprintf(stderr, "%7.2fs  %-12s %s\n", now() - x->t0, f && f->node ? f->node : "", msg);
}

/* ----- rendering values into prompts ------------------------------------ */

static void write_plain_num(cx_buf *b, double n) {
    cx_value v = {.kind = CX_NUM, .u.num = n};
    cx_write(b, &v);
}

/* Text as is; lists one item per line; records as JSON. */
static void render(cx_buf *b, const cx_value *v) {
    if (!v) return;
    switch (v->kind) {
    case CX_NULL: break;
    case CX_STR: cx_buf_put(b, v->u.str.s, v->u.str.len); break;
    case CX_NUM: write_plain_num(b, v->u.num); break;
    case CX_BOOL: cx_buf_puts(b, v->u.b ? "true" : "false"); break;
    case CX_LIST:
        for (size_t i = 0; i < v->u.list.len; i++) {
            if (i) cx_buf_putc(b, '\n');
            cx_buf_puts(b, "- ");
            render(b, v->u.list.items[i]);
        }
        break;
    case CX_REC: cx_write(b, v); break;
    }
}

/* ----- effects ------------------------------------------------------------ */

static int is_temporary(const char *kind) {
    return strcmp(kind, "Timeout") == 0 || strcmp(kind, "RateLimit") == 0 ||
           strcmp(kind, "Unavailable") == 0 || strcmp(kind, "Network") == 0;
}

/* Calls the I/O layer and parses its answer into the arena. */
static cx_value *io_call(exec *x, char *(*fn)(const char *), const char *req) {
    char *out = fn(req);
    if (!out) return NULL;
    cx_value *v = cx_parse(&x->arena, out, strlen(out), NULL);
    calyx_string_free(out);
    return v;
}

/* Wait before attempt `attempt + 1`: 1 s, 2 s, 4 s (twice that for rate
 * limits). CALYX_RETRY_BASE_MS changes the base, for tests. */
static long backoff_ms(int attempt, const char *kind) {
    long base = 1000;
    const char *env = getenv("CALYX_RETRY_BASE_MS");
    if (env && *env) base = strtol(env, NULL, 10);
    long ms = base << (attempt - 1);
    if (strcmp(kind, "RateLimit") == 0) ms *= 2;
    return ms;
}

/* Model answers sometimes wrap JSON in a ``` fence. */
static void strip_fence(const char **s, size_t *len) {
    const char *p = *s, *end = *s + *len;
    while (p < end && (*p == ' ' || *p == '\n' || *p == '\r' || *p == '\t')) p++;
    while (end > p && (end[-1] == ' ' || end[-1] == '\n' || end[-1] == '\r' || end[-1] == '\t'))
        end--;
    if (end - p >= 6 && memcmp(p, "```", 3) == 0 && memcmp(end - 3, "```", 3) == 0) {
        p += 3;
        while (p < end && *p != '\n') p++; /* language tag */
        end -= 3;
    }
    *s = p;
    *len = (size_t)(end - p);
}

/* The key of the next call of the current node instance: `instance#n`. */
static char *next_key(exec *x, frame *f) {
    size_t n = strlen(f->instance) + 16;
    char *key = cx_alloc(&x->arena, n);
    snprintf(key, n, "%s#%d", f->instance, f->seq++);
    return key;
}

/*
 * Looks the call up in the journal. Returns 1 to go on (with *hit set to
 * the recorded answer, or NULL to make the call), 0 after an error.
 */
static int journal_lookup(exec *x, frame *f, const char *key, const char *req_hash,
                          cx_value **hit) {
    *hit = NULL;
    if (!x->journal) return 1;
    int mismatch = 0;
    *hit = cx_journal_lookup(x->journal, key, req_hash, &mismatch);
    if (mismatch) {
        failf(x, f,
              "call `%s` differs from the one in the journal: the run cannot be resumed "
              "deterministically",
              key);
        return 0;
    }
    if (*hit) {
        x->from_journal++;
        return 1;
    }
    if (cx_journal_get_mode(x->journal) == CX_JOURNAL_REPLAY) {
        failf(x, f, "replay: call `%s` is not in the journal (the run stopped before it)", key);
        return 0;
    }
    return 1;
}

static int journal_record(exec *x, frame *f, const char *key, const char *effect,
                          const char *req_hash, const cx_value *ok) {
    if (!x->journal || cx_journal_get_mode(x->journal) == CX_JOURNAL_REPLAY) return 1;
    cx_buf b = {0};
    cx_write(&b, ok);
    int written = cx_journal_record(x->journal, key, effect, req_hash, b.data, b.len);
    cx_buf_free(&b);
    if (!written) {
        failf(x, f, "cannot write the journal");
        return 0;
    }
    if (x->crash_after > 0 && ++x->recorded >= x->crash_after) {
        /* Simulates `kill -9` right after an entry reached the journal. */
        fprintf(stderr, "calyx: CALYX_CRASH_AFTER=%ld reached, exiting abruptly\n", x->crash_after);
        _exit(137);
    }
    return 1;
}

/* Turns a model's answer into a value of the prompt's type. */
static cx_value *decode_answer(exec *x, cx_value *prompt, cx_value *ok, const char **why) {
    cx_value *t = cx_get(ok, "text");
    cx_value *schema = cx_get(prompt, "schema");
    if (!schema || schema->kind == CX_NULL) return t;
    const char *s = t ? t->u.str.s : "";
    size_t len = t ? t->u.str.len : 0;
    strip_fence(&s, &len);
    *why = NULL;
    cx_value *v = cx_parse(&x->arena, s, len, why);
    if (v && cx_get_bool(prompt, "wrapped", 0)) {
        v = cx_get(v, "value");
        if (!v) *why = "missing `value`";
    }
    return v;
}

static cx_value *eval(exec *x, frame *f, cx_value *e);

static cx_value *call_model(exec *x, frame *f, cx_value *e) {
    cx_value *model = at(x->models, index_of(e, "model"));
    cx_value *prompt = at(x->prompts, index_of(e, "prompt"));
    if (!model || !prompt) return failf(x, f, "invalid IR: unknown model or prompt");
    cx_value *args_e = cx_get(e, "args");
    cx_value *params = cx_get(prompt, "params");
    size_t n = len_of(args_e);
    cx_value **args = cx_alloc(&x->arena, (n ? n : 1) * sizeof *args);
    for (size_t i = 0; i < n; i++)
        if (!(args[i] = eval(x, f, at(args_e, i)))) return NULL;

    /* The prompt's text, with {paths} replaced by the arguments. */
    cx_buf text = {0};
    cx_value *parts = cx_get(prompt, "parts");
    for (size_t i = 0; i < len_of(parts); i++) {
        cx_value *part = at(parts, i);
        cx_value *lit = cx_get(part, "lit");
        if (lit) {
            render(&text, lit);
            continue;
        }
        cx_value *path = cx_get(part, "path");
        cx_value *v = NULL;
        const char *root = at(path, 0) ? at(path, 0)->u.str.s : "";
        for (size_t p = 0; p < len_of(params); p++)
            if (strcmp(at(params, p)->u.str.s, root) == 0 && p < n) v = args[p];
        for (size_t k = 1; v && k < len_of(path); k++) v = cx_get(v, at(path, k)->u.str.s);
        render(&text, v);
    }

    cx_value *schema = cx_get(prompt, "schema");
    int has_schema = schema && schema->kind != CX_NULL;
    cx_buf req = {0};
    cx_buf_puts(&req, "{\"model\":");
    cx_write(&req, cx_get(model, "id"));
    cx_buf_puts(&req, ",\"prompt\":");
    cx_buf_json_str(&req, text.data ? text.data : "", text.len);
    cx_buf_puts(&req, ",\"schema\":");
    cx_write(&req, has_schema ? schema : NULL);
    cx_buf_puts(&req, ",\"max_output\":");
    cx_write(&req, cx_get(model, "max_output"));
    cx_buf_puts(&req, ",\"timeout_ms\":300000}"); /* llm: 5 min per attempt (D22) */
    cx_buf_free(&text);

    const char *model_id = cx_get_str(model, "id", "?");
    const char *prompt_name = cx_get_str(prompt, "name", "?");
    char *key = next_key(x, f);
    char req_hash[65];
    cx_sha256_hex(req.data, req.len, req_hash);
    cx_value *hit;
    if (!journal_lookup(x, f, key, req_hash, &hit)) {
        cx_buf_free(&req);
        return NULL;
    }
    cx_value *result = NULL;
    char last[1024] = "";
    if (hit) {
        cx_buf_free(&req);
        const char *why = NULL;
        result = decode_answer(x, prompt, hit, &why);
        trace(x, f, "llm   %s(%s)  from the journal", model_id, prompt_name);
        if (!result) return failf(x, f, "the journal's answer for `%s` does not decode", key);
        return result;
    }
    for (int attempt = 1; attempt <= MAX_ATTEMPTS && !result; attempt++) {
        if (attempt > 1) x->retries++;
        x->model_calls++;
        cx_value *ans = io_call(x, calyx_io_model_call, req.data);
        cx_value *ok = cx_get(ans, "ok");
        const char *kind;
        if (ok) {
            double in = cx_get_num(ok, "input_tokens", 0), out = cx_get_num(ok, "output_tokens", 0);
            x->input_tokens += (unsigned long long)in;
            x->output_tokens += (unsigned long long)out;
            trace(x, f, "llm   %s(%s)  %.2f s  %.0f -> %.0f tokens%s", model_id, prompt_name,
                  cx_get_num(ok, "ms", 0) / 1000.0, in, out, attempt > 1 ? "  (retry)" : "");
            const char *why = NULL;
            cx_value *v = decode_answer(x, prompt, ok, &why);
            if (v) {
                /* Only answers that decode are recorded. */
                if (!journal_record(x, f, key, "llm", req_hash, ok)) break;
                result = v;
                break;
            }
            /* The answer does not match the prompt's type: ask again. */
            kind = "Decode";
            snprintf(last, sizeof last, "Decode: the answer is not valid JSON (%s)",
                     why ? why : "?");
        } else {
            cx_value *err = cx_get(ans, "error");
            kind = cx_get_str(err, "kind", "Unavailable");
            snprintf(last, sizeof last, "%s: %s", kind, cx_get_str(err, "message", "no answer"));
            trace(x, f, "llm   %s(%s)  failed: %s", model_id, prompt_name, last);
            if (!is_temporary(kind)) break;
        }
        if (attempt < MAX_ATTEMPTS) sleep_ms(backoff_ms(attempt, kind));
    }
    cx_buf_free(&req);
    if (!result)
        return x->err.len ? NULL
                          : failf(x, f, "model `%s` with prompt `%s` failed: %s", model_id,
                                  prompt_name, last);
    return result;
}

static int listed(const cx_value *list, const char *kind) {
    for (size_t i = 0; i < len_of(list); i++)
        if (at(list, i)->kind == CX_STR && strcmp(at(list, i)->u.str.s, kind) == 0) return 1;
    return 0;
}

static cx_value *call_tool(exec *x, frame *f, cx_value *e) {
    cx_value *tool = at(x->tools, index_of(e, "tool"));
    if (!tool) return failf(x, f, "invalid IR: unknown tool");
    const char *name = cx_get_str(tool, "name", "?");
    const char *effect = cx_get_str(tool, "effect", "read");
    cx_value *params = cx_get(tool, "params");
    cx_value *args_e = cx_get(e, "args");

    cx_buf req = {0};
    cx_buf_puts(&req, "{\"tool\":");
    cx_buf_json_str(&req, name, strlen(name));
    cx_buf_puts(&req, ",\"args\":{");
    for (size_t i = 0; i < len_of(args_e); i++) {
        cx_value *v = eval(x, f, at(args_e, i));
        if (!v) {
            cx_buf_free(&req);
            return NULL;
        }
        if (i) cx_buf_putc(&req, ',');
        cx_write(&req, at(params, i));
        cx_buf_putc(&req, ':');
        cx_write(&req, v);
    }
    cx_buf_puts(&req, "},\"max_output\":");
    cx_write(&req, cx_get(tool, "max_output"));
    cx_buf_printf(&req, ",\"timeout_ms\":%.0f}", cx_get_num(tool, "timeout_ms", 30000));

    char *key = next_key(x, f);
    char req_hash[65];
    cx_sha256_hex(req.data, req.len, req_hash);
    cx_value *hit;
    if (!journal_lookup(x, f, key, req_hash, &hit)) {
        cx_buf_free(&req);
        return NULL;
    }
    /* A `write once` effect is never repeated (decision D2). */
    int write_once = strcmp(effect, "write once") == 0;
    if (!hit && write_once && x->journal) {
        if (cx_journal_uncertain(x->journal, key)) {
            cx_buf_free(&req);
            /* The `on_uncertain` policies (verify, pause, accept_loss) arrive in M6. */
            return failf(x, f,
                         "`%s` (write once) started before the interruption and its outcome is "
                         "unknown; it is not repeated",
                         name);
        }
        cx_journal_begin(x->journal, key, req_hash);
    }
    cx_value *retry_on = cx_get(tool, "retry_on");
    cx_value *result = NULL;
    char last[1024] = "";
    for (int attempt = 1; attempt <= MAX_ATTEMPTS && !result; attempt++) {
        cx_value *ok = hit;
        const char *kind = "Unavailable";
        if (!ok) {
            if (attempt > 1) x->retries++;
            x->tool_calls++;
            cx_value *ans = io_call(x, calyx_io_tool_call, req.data);
            ok = cx_get(ans, "ok");
            if (!ok) {
                cx_value *err = cx_get(ans, "error");
                kind = cx_get_str(err, "kind", "Unavailable");
                snprintf(last, sizeof last, "%s: %s", kind,
                         cx_get_str(err, "message", "no answer"));
                trace(x, f, "%-5s %s  failed: %s", effect, name, last);
            } else {
                trace(x, f, "%-5s %s  %.2f s%s%s", effect, name,
                      cx_get_num(ok, "ms", 0) / 1000.0,
                      cx_get_bool(ok, "truncated", 0) ? "  (output cut at max_output)" : "",
                      attempt > 1 ? "  (retry)" : "");
                if (!journal_record(x, f, key, effect, req_hash, ok)) break;
            }
        } else {
            trace(x, f, "%-5s %s  from the journal", effect, name);
        }
        if (ok) {
            if (cx_get_bool(tool, "returns_text", 1)) {
                result = cx_get(ok, "text");
                break;
            }
            cx_value *j = cx_get(ok, "json");
            if (j && j->kind != CX_NULL) {
                result = j;
                break;
            }
            cx_value *t = cx_get(ok, "text");
            result = t ? cx_parse(&x->arena, t->u.str.s, t->u.str.len, NULL) : NULL;
            if (!result) snprintf(last, sizeof last, "Decode: the tool's output is not JSON");
            break;
        }
        /* Tools repeat only the errors they declare in `retry_on`. */
        if (write_once || !listed(retry_on, kind)) break;
        if (attempt < MAX_ATTEMPTS) sleep_ms(backoff_ms(attempt, kind));
    }
    cx_buf_free(&req);
    if (!result) return x->err.len ? NULL : failf(x, f, "tool `%s` failed: %s", name, last);
    return result;
}

static cx_value *run_graph(exec *x, frame *caller, size_t gi, cx_value **args, size_t nargs,
                           const char *path);

static cx_value *eval(exec *x, frame *f, cx_value *e) {
    const char *k = cx_get_str(e, "k", "");
    if (strcmp(k, "text") == 0) return cx_get(e, "v");
    if (strcmp(k, "int") == 0 || strcmp(k, "float") == 0) return cx_get(e, "v");
    if (strcmp(k, "param") == 0) return f->params[index_of(e, "i")];
    if (strcmp(k, "node") == 0) {
        cx_value *v = f->nodes[index_of(e, "i")];
        return v ? v : failf(x, f, "invalid IR: node used before it was computed");
    }
    if (strcmp(k, "item") == 0) return f->item;
    if (strcmp(k, "field") == 0) {
        cx_value *base = eval(x, f, cx_get(e, "base"));
        if (!base) return NULL;
        const char *name = cx_get_str(e, "name", "");
        cx_value *v = cx_get(base, name);
        if (!v) {
            cx_buf shown = {0};
            cx_write(&shown, base);
            if (shown.len > 200) shown.len = 200, shown.data[200] = '\0';
            failf(x, f, "the value has no field `%s`: %s", name, shown.data);
            cx_buf_free(&shown);
        }
        return v;
    }
    if (strcmp(k, "list") == 0) {
        cx_value *items = cx_get(e, "items");
        size_t n = len_of(items);
        cx_value **vs = cx_alloc(&x->arena, (n ? n : 1) * sizeof *vs);
        for (size_t i = 0; i < n; i++)
            if (!(vs[i] = eval(x, f, at(items, i)))) return NULL;
        return cx_list(&x->arena, vs, n);
    }
    if (strcmp(k, "interp") == 0) {
        cx_buf b = {0};
        cx_value *parts = cx_get(e, "parts");
        for (size_t i = 0; i < len_of(parts); i++) {
            cx_value *part = at(parts, i);
            cx_value *lit = cx_get(part, "lit");
            cx_value *v = lit ? lit : eval(x, f, cx_get(part, "expr"));
            if (!v) {
                cx_buf_free(&b);
                return NULL;
            }
            render(&b, v);
        }
        cx_value *s = cx_str(&x->arena, b.data ? b.data : "", b.len);
        cx_buf_free(&b);
        return s;
    }
    if (strcmp(k, "model") == 0) return call_model(x, f, e);
    if (strcmp(k, "tool") == 0) return call_tool(x, f, e);
    if (strcmp(k, "graph") == 0) {
        cx_value *args_e = cx_get(e, "args");
        size_t n = len_of(args_e);
        cx_value **args = cx_alloc(&x->arena, (n ? n : 1) * sizeof *args);
        for (size_t i = 0; i < n; i++)
            if (!(args[i] = eval(x, f, at(args_e, i)))) return NULL;
        /* A subgraph's calls are keyed under the call that started it. */
        char *key = next_key(x, f);
        cx_value *g = at(x->graphs, index_of(e, "graph"));
        size_t len = strlen(key) + strlen(cx_get_str(g, "name", "?")) + 2;
        char *path = cx_alloc(&x->arena, len);
        snprintf(path, len, "%s/%s", key, cx_get_str(g, "name", "?"));
        return run_graph(x, f, index_of(e, "graph"), args, n, path);
    }
    return failf(x, f, "invalid IR: unknown expression `%s`", k);
}

/* Sets the place of the node instance about to run: `path/node` or `path/node[j]`. */
static void enter(exec *x, frame *f, long item) {
    size_t n = strlen(f->path) + strlen(f->node) + 32;
    f->instance = cx_alloc(&x->arena, n);
    if (item < 0)
        snprintf(f->instance, n, "%s/%s", f->path, f->node);
    else
        snprintf(f->instance, n, "%s/%s[%ld]", f->path, f->node, item);
    f->seq = 0;
}

static cx_value *run_graph(exec *x, frame *caller, size_t gi, cx_value **args, size_t nargs,
                           const char *path) {
    cx_value *g = at(x->graphs, gi);
    if (!g) return failf(x, caller, "invalid IR: unknown graph");
    if (x->depth >= MAX_GRAPH_DEPTH)
        return failf(x, caller, "graphs nested more than %d deep", MAX_GRAPH_DEPTH);
    if (nargs != len_of(cx_get(g, "params")))
        return failf(x, caller, "graph `%s` expects %zu argument(s)", cx_get_str(g, "name", "?"),
                     len_of(cx_get(g, "params")));
    cx_value *nodes = cx_get(g, "nodes");
    size_t n = len_of(nodes);
    frame f = {g, args, cx_alloc(&x->arena, (n ? n : 1) * sizeof(cx_value *)), NULL, NULL, path,
               NULL, 0};
    memset(f.nodes, 0, (n ? n : 1) * sizeof(cx_value *));
    x->depth++;
    /* The IR lists nodes so that each comes after its inputs. */
    for (size_t i = 0; i < n; i++) {
        cx_value *node = at(nodes, i);
        f.node = cx_get_str(node, "name", "?");
        double started = now();
        cx_value *over_e = cx_get(node, "over");
        cx_value *v;
        enter(x, &f, -1);
        if (over_e && over_e->kind != CX_NULL) {
            cx_value *over = eval(x, &f, over_e);
            if (!over) break;
            if (over->kind != CX_LIST) {
                failf(x, &f, "`for each` over a value that is not a list");
                break;
            }
            size_t m = over->u.list.len;
            cx_value **items = cx_alloc(&x->arena, (m ? m : 1) * sizeof *items);
            int ok = 1;
            /* One instance per item; results keep the order of the list (D7). */
            for (size_t j = 0; j < m && ok; j++) {
                f.item = over->u.list.items[j];
                enter(x, &f, (long)j);
                ok = (items[j] = eval(x, &f, cx_get(node, "value"))) != NULL;
            }
            f.item = NULL;
            if (!ok) break;
            v = cx_list(&x->arena, items, m);
            trace(x, &f, "done  %zu item(s)  %.2f s", m, now() - started);
        } else {
            v = eval(x, &f, cx_get(node, "value"));
            if (!v) break;
            if (strcmp(cx_get_str(node, "effect", "pure"), "pure") != 0)
                trace(x, &f, "done  %.2f s", now() - started);
        }
        f.nodes[i] = v;
    }
    x->depth--;
    if (x->err.len) return NULL;
    size_t out = index_of(g, "output");
    return out < n ? f.nodes[out] : cx_null(&x->arena);
}

static char *error_json(const char *msg) {
    cx_buf b = {0};
    cx_buf_puts(&b, "{\"error\":");
    cx_buf_json_str(&b, msg, strlen(msg));
    cx_buf_putc(&b, '}');
    return cx_buf_take(&b);
}

char *calyx_run(const char *ir_json, const char *graph, const char *args_json,
                const char *options_json) {
    if (!ir_json || !graph || !args_json) return error_json("calyx_run: missing argument");
    exec x;
    memset(&x, 0, sizeof x);
    x.t0 = now();
    char *out = NULL;
    const char *perr = NULL;

    cx_value *options = options_json ? cx_parse(&x.arena, options_json, strlen(options_json), NULL)
                                     : NULL;
    if (options_json && (!options || options->kind != CX_REC)) {
        out = error_json("the options must be a JSON object");
        goto done;
    }
    x.trace = cx_get_bool(options, "trace", 0);
    const char *crash = getenv("CALYX_CRASH_AFTER");
    x.crash_after = crash ? strtol(crash, NULL, 10) : 0;

    cx_value *ir = cx_parse(&x.arena, ir_json, strlen(ir_json), &perr);
    cx_value *args = cx_parse(&x.arena, args_json, strlen(args_json), NULL);
    if (!ir || cx_get_num(ir, "version", -1) != IR_VERSION) {
        out = error_json("the program's IR is invalid or from another version of calyx");
        goto done;
    }
    if (!args || args->kind != CX_REC) {
        out = error_json("the arguments must be a JSON object");
        goto done;
    }
    x.models = cx_get(ir, "models");
    x.tools = cx_get(ir, "tools");
    x.prompts = cx_get(ir, "prompts");
    x.graphs = cx_get(ir, "graphs");

    size_t gi = (size_t)-1;
    for (size_t i = 0; i < len_of(x.graphs); i++)
        if (strcmp(cx_get_str(at(x.graphs, i), "name", ""), graph) == 0) gi = i;
    if (gi == (size_t)-1) {
        cx_buf b = {0};
        cx_buf_printf(&b, "the program has no graph `%s`", graph);
        out = error_json(b.data);
        cx_buf_free(&b);
        goto done;
    }
    cx_value *g = at(x.graphs, gi);
    cx_value *params = cx_get(g, "params");
    size_t np = len_of(params);
    cx_value **vals = cx_alloc(&x.arena, (np ? np : 1) * sizeof *vals);
    for (size_t i = 0; i < np; i++) {
        vals[i] = cx_get(args, at(params, i)->u.str.s);
        if (!vals[i]) {
            cx_buf b = {0};
            cx_buf_printf(&b, "missing argument `%s` for graph `%s`", at(params, i)->u.str.s, graph);
            out = error_json(b.data);
            cx_buf_free(&b);
            goto done;
        }
    }

    /* The journal: the program is identified by the hash of its IR (D23). */
    const char *dir = cx_get_str(options, "journal", NULL);
    if (dir) {
        const char *mode_s = cx_get_str(options, "mode", "new");
        cx_journal_mode mode = strcmp(mode_s, "resume") == 0   ? CX_JOURNAL_RESUME
                               : strcmp(mode_s, "replay") == 0 ? CX_JOURNAL_REPLAY
                                                               : CX_JOURNAL_NEW;
        char ir_hash[65];
        cx_sha256_hex(ir_json, strlen(ir_json), ir_hash);
        cx_buf header = {0};
        cx_buf_printf(&header, "{\"type\":\"run\",\"version\":%d,\"ir_hash\":\"%s\",\"graph\":",
                      CX_JOURNAL_VERSION, ir_hash);
        cx_buf_json_str(&header, graph, strlen(graph));
        cx_buf_puts(&header, ",\"args\":");
        cx_write(&header, args);
        cx_buf_puts(&header, ",\"program\":");
        cx_write(&header, cx_get(options, "program"));
        cx_buf_printf(&header, ",\"started\":%lld}", (long long)time(NULL));
        cx_buf err = {0};
        x.journal = cx_journal_open(&x.arena, dir, mode, ir_hash, header.data, &err);
        cx_buf_free(&header);
        if (!x.journal) {
            out = error_json(err.data ? err.data : "cannot open the journal");
            cx_buf_free(&err);
            goto done;
        }
    }

    if (x.trace) fprintf(stderr, "%7.2fs  running graph `%s`\n", 0.0, graph);
    cx_value *result = run_graph(&x, NULL, gi, vals, np, graph);
    if (x.trace) {
        fprintf(stderr,
                "%7.2fs  %s: %lu model call(s) (%llu -> %llu tokens), %lu tool call(s), %lu "
                "retry(ies)",
                now() - x.t0, result ? "finished" : "failed", x.model_calls, x.input_tokens,
                x.output_tokens, x.tool_calls, x.retries);
        if (x.journal) fprintf(stderr, ", %lu taken from the journal", x.from_journal);
        fputc('\n', stderr);
    }
    if (!result) {
        out = error_json(x.err.len ? x.err.data : "unknown failure");
        if (x.journal) cx_journal_end(x.journal, NULL, x.err.len ? x.err.data : "unknown failure");
    } else {
        cx_buf b = {0};
        cx_write(&b, result);
        if (x.journal) cx_journal_end(x.journal, b.data, NULL);
        cx_buf ok = {0};
        cx_buf_puts(&ok, "{\"ok\":");
        cx_buf_put(&ok, b.data, b.len);
        cx_buf_putc(&ok, '}');
        cx_buf_free(&b);
        out = cx_buf_take(&ok);
    }
done:
    cx_journal_close(x.journal);
    cx_buf_free(&x.err);
    cx_arena_free(&x.arena);
    return out;
}

void calyx_run_free(char *s) { free(s); }
