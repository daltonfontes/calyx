/*
 * exec.c - the interpreter (milestones M2 to M4).
 *
 * Runs the template of decision D8 (the IR, in JSON). The programmer never
 * creates threads; the runtime derives the concurrency from the graph:
 *
 *  - Every node instance is a task: a node, or one item of a fan-out. A
 *    task becomes ready when the nodes it reads are done.
 *  - Workers (one per core, at most 8) run ready tasks, highest rank first
 *    (the longest path to the end of the graph, decision D24). Each worker
 *    has its own queue and takes work from the others when it runs out
 *    (work stealing, decision D31).
 *  - Calls to models and tools never block a worker (D31): they go to I/O
 *    threads, at most `limits threads N` at once, the most critical first.
 *    The task stops there, and runs again from the start when the answer
 *    arrives. Calls that already finished are taken from memory, by the
 *    same keys the journal uses, so a call is never made twice and no C
 *    stack has to be kept across the wait.
 *  - Results keep the order of the list (D7); which call finishes first
 *    changes nothing but the time.
 *
 * Effects are policies around the I/O layer: retries of temporary errors
 * with backoff by effect (D2, D11, D22), decoding answers into the prompt's
 * type, `rate` and `budget` limits (D3), and the journal (journal.h): every
 * completed call is recorded, so resuming a run redoes nothing that
 * finished, and replay calls nothing.
 *
 * Locks: `mu` guards the scheduler state (graphs, pending calls, the queue
 * of calls, counters, errors); `jmu` the journal; each worker's `qmu` its
 * queue. Order: mu, then jmu or a qmu. Values are immutable once built, so
 * threads share them without locks; each thread allocates from its own
 * arena, and all are freed when the run ends.
 *
 * Still to come: failures as values (M5).
 */
#define _POSIX_C_SOURCE 200809L

#include "calyx_io.h"
#include "calyx_runtime.h"
#include "calyx_verify.h"
#include "journal.h"
#include "json.h"
#include "sha256.h"

#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define IR_VERSION 1
#define MAX_GRAPH_DEPTH 64
#define MAX_ATTEMPTS 4
#define MAX_WORKERS 8
#define DEFAULT_THREADS 8
#define MAX_RETRY_HINT_S 60.0

/* Returned by evaluation when a call it needs has not finished yet. */
static cx_value pending_marker;
#define PENDING (&pending_marker)

typedef struct exec exec;
typedef struct worker worker;
typedef struct task task;
typedef struct gexec gexec;
typedef struct pending pending;
typedef struct waiter waiter;
typedef struct job job;

enum { T_WAITING, T_QUEUED, T_RUNNING, T_DONE };

struct task {
    gexec *g;
    size_t node;
    long item; /* -1: the node (or a fan-out's list); j: item j */
    double rank;
    unsigned long seq; /* creation order: ties in rank keep it */
    int state;
    int rerun; /* an answer arrived while it was running */
};

struct waiter {
    task *t;
    waiter *next;
};

enum { P_INFLIGHT, P_DONE, P_FAILED };

/* A call (or subgraph) by key: running, or finished with its value. */
struct pending {
    const char *key;
    int state;
    cx_value *value;
    waiter *waiters;
};

/* A running instance of a graph. */
struct gexec {
    cx_value *graph, *nodes_ir;
    size_t n;
    cx_value **params;
    const char *path; /* place in the realized graph */
    int depth;
    cx_value **values; /* per node, once done */
    int *missing;      /* inputs not done yet */
    size_t **users;    /* nodes that read node i */
    size_t *nusers;
    cx_value **lists;  /* fan-out: the list */
    cx_value ***items; /* fan-out: results so far */
    size_t *items_left;
    double *started;
    size_t nodes_left;
    pending *parent; /* the call that started this subgraph; NULL for the root */
};

typedef struct {
    task **a;
    size_t len, cap;
} task_heap;

struct worker {
    exec *x;
    int id;
    pthread_t th;
    cx_arena arena;
    pthread_mutex_t qmu;
    task_heap q;
};

/* A call handed to an I/O thread. */
struct job {
    pending *p;
    int is_model;
    cx_value *spec;   /* the model or the tool */
    cx_value *prompt; /* for models */
    char *req;
    char req_hash[65];
    const char *key;
    const char *graph, *node, *label;
    double rank;
    unsigned long seq;
};

typedef struct {
    job **a;
    size_t len, cap;
} job_heap;

struct exec {
    pthread_mutex_t mu, jmu;
    pthread_cond_t work_cv, io_cv;
    cx_arena arena; /* the main thread's */
    cx_value *models, *tools, *prompts, *graphs;
    int trace;
    double t0;
    cx_buf err;
    int stopping, finished;
    /* One call at a time, chosen only when no task can run: same order always. */
    int deterministic;
    cx_value *result;
    cx_journal *journal;
    long crash_after, recorded;
    /* Pending calls by key: open addressing, capacity a power of two. */
    pending **ptab;
    size_t pcap, pused;
    /* Scheduling. */
    worker *workers;
    int nworkers;
    unsigned next_worker;
    long ready;
    int running;
    int inflight, max_inflight;
    job_heap jobs;
    unsigned long seq;
    pthread_t *io;
    cx_arena *io_arenas;
    int nio;
    /* Limits (D3). */
    double rate, next_start;
    double budget, spent;
    int unpriced_warned;
    /* Totals. */
    unsigned long model_calls, tool_calls, retries, from_journal;
    unsigned long long input_tokens, output_tokens;
};

/* Evaluation context of one task. */
typedef struct {
    exec *x;
    worker *w;
    task *t;
    gexec *g;
    cx_value *item;
    char *instance;    /* key prefix: `path/node` or `path/node[j]` */
    const char *node;  /* node name, for errors */
    const char *label; /* `node` or `node[j]`, for the trace */
} ctx;

/* ----- small helpers ----------------------------------------------------- */

static double now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static void sleep_s(double s) {
    if (s <= 0) return;
    struct timespec ts = {(time_t)s, (long)((s - (double)(time_t)s) * 1e9)};
    while (nanosleep(&ts, &ts) != 0) {
    }
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

static char *fmt(cx_arena *a, const char *f, ...) {
    va_list ap;
    va_start(ap, f);
    int n = vsnprintf(NULL, 0, f, ap);
    va_end(ap);
    char *s = cx_alloc(a, (size_t)n + 1);
    va_start(ap, f);
    vsnprintf(s, (size_t)n + 1, f, ap);
    va_end(ap);
    return s;
}

static void trace(exec *x, const char *label, const char *f, ...) {
    if (!x->trace) return;
    char msg[1024];
    va_list ap;
    va_start(ap, f);
    vsnprintf(msg, sizeof msg, f, ap);
    va_end(ap);
    fprintf(stderr, "%7.2fs  %-14s %s\n", now() - x->t0, label ? label : "", msg);
}

/* Records the first error and stops the run. Call with `mu` held. */
static void fail_locked(exec *x, const char *graph, const char *node, const char *msg) {
    if (x->err.len) return;
    if (graph && node) cx_buf_printf(&x->err, "in graph `%s`, node `%s`: ", graph, node);
    cx_buf_puts(&x->err, msg);
    x->stopping = 1;
    pthread_cond_broadcast(&x->work_cv);
    pthread_cond_broadcast(&x->io_cv);
}

static cx_value *failf(ctx *c, const char *f, ...) {
    char msg[1024];
    va_list ap;
    va_start(ap, f);
    vsnprintf(msg, sizeof msg, f, ap);
    va_end(ap);
    pthread_mutex_lock(&c->x->mu);
    fail_locked(c->x, cx_get_str(c->g->graph, "name", "?"), c->node, msg);
    pthread_mutex_unlock(&c->x->mu);
    return NULL;
}

/* ----- rendering values into prompts ------------------------------------ */

/* Text as is; lists one item per line; records as JSON. */
static void render(cx_buf *b, const cx_value *v) {
    if (!v) return;
    switch (v->kind) {
    case CX_NULL: break;
    case CX_STR: cx_buf_put(b, v->u.str.s, v->u.str.len); break;
    case CX_NUM:
    case CX_REC: cx_write(b, v); break;
    case CX_BOOL: cx_buf_puts(b, v->u.b ? "true" : "false"); break;
    case CX_LIST:
        for (size_t i = 0; i < v->u.list.len; i++) {
            if (i) cx_buf_putc(b, '\n');
            cx_buf_puts(b, "- ");
            render(b, v->u.list.items[i]);
        }
        break;
    }
}

/* ----- heaps: highest rank first, then creation order --------------------- */

#define HEAP_FUNCS(NAME, ITEM)                                                          \
    static int NAME##_before(ITEM *a, ITEM *b) {                                        \
        return a->rank > b->rank || (a->rank == b->rank && a->seq < b->seq);            \
    }                                                                                   \
    static void NAME##_push(NAME *h, ITEM *it) {                                        \
        if (h->len == h->cap) {                                                         \
            h->cap = h->cap ? h->cap * 2 : 16;                                          \
            h->a = realloc(h->a, h->cap * sizeof *h->a);                                \
            if (!h->a) abort();                                                         \
        }                                                                               \
        size_t i = h->len++;                                                            \
        h->a[i] = it;                                                                   \
        while (i > 0 && NAME##_before(h->a[i], h->a[(i - 1) / 2])) {                    \
            ITEM *tmp = h->a[i];                                                        \
            h->a[i] = h->a[(i - 1) / 2];                                                \
            h->a[(i - 1) / 2] = tmp;                                                    \
            i = (i - 1) / 2;                                                            \
        }                                                                               \
    }                                                                                   \
    static ITEM *NAME##_pop(NAME *h) {                                                  \
        if (!h->len) return NULL;                                                       \
        ITEM *top = h->a[0];                                                            \
        h->a[0] = h->a[--h->len];                                                       \
        for (size_t i = 0;;) {                                                          \
            size_t l = 2 * i + 1, r = l + 1, m = i;                                     \
            if (l < h->len && NAME##_before(h->a[l], h->a[m])) m = l;                   \
            if (r < h->len && NAME##_before(h->a[r], h->a[m])) m = r;                   \
            if (m == i) break;                                                          \
            ITEM *tmp = h->a[i];                                                        \
            h->a[i] = h->a[m];                                                          \
            h->a[m] = tmp;                                                              \
            i = m;                                                                      \
        }                                                                               \
        return top;                                                                     \
    }

HEAP_FUNCS(task_heap, task)
HEAP_FUNCS(job_heap, job)

/* ----- pending calls ---------------------------------------------------- */

static size_t hash_str(const char *s) {
    size_t h = (size_t)1469598103934665603ULL;
    for (; *s; s++) h = (h ^ (unsigned char)*s) * (size_t)1099511628211ULL;
    return h;
}

/* With `mu` held. */
static pending *ptab_get(exec *x, const char *key) {
    if (!x->pcap) return NULL;
    for (size_t i = hash_str(key) & (x->pcap - 1);; i = (i + 1) & (x->pcap - 1)) {
        if (!x->ptab[i]) return NULL;
        if (strcmp(x->ptab[i]->key, key) == 0) return x->ptab[i];
    }
}

static void ptab_insert(exec *x, pending *p) {
    if ((x->pused + 1) * 2 > x->pcap) {
        size_t cap = x->pcap ? x->pcap * 2 : 256;
        pending **old = x->ptab;
        size_t old_cap = x->pcap;
        x->ptab = calloc(cap, sizeof *x->ptab);
        if (!x->ptab) abort();
        x->pcap = cap;
        for (size_t i = 0; i < old_cap; i++) {
            if (!old[i]) continue;
            size_t j = hash_str(old[i]->key) & (cap - 1);
            while (x->ptab[j]) j = (j + 1) & (cap - 1);
            x->ptab[j] = old[i];
        }
        free(old);
    }
    size_t j = hash_str(p->key) & (x->pcap - 1);
    while (x->ptab[j]) j = (j + 1) & (x->pcap - 1);
    x->ptab[j] = p;
    x->pused++;
}

static pending *new_pending(exec *x, cx_arena *a, const char *key, int state, cx_value *value) {
    pending *p = cx_alloc(a, sizeof *p);
    p->key = cx_strndup(a, key, strlen(key));
    p->state = state;
    p->value = value;
    p->waiters = NULL;
    ptab_insert(x, p);
    return p;
}

static void add_waiter(cx_arena *a, pending *p, task *t) {
    waiter *w = cx_alloc(a, sizeof *w);
    w->t = t;
    w->next = p->waiters;
    p->waiters = w;
}

/* ----- scheduling (all with `mu` held) ------------------------------------ */

/* Queues a task on `w` (the current worker) or, from an I/O thread, on the
 * next worker in turn. */
static void push_task(exec *x, worker *w, task *t) {
    t->state = T_QUEUED;
    if (!w) w = &x->workers[x->next_worker++ % (unsigned)x->nworkers];
    pthread_mutex_lock(&w->qmu);
    task_heap_push(&w->q, t);
    pthread_mutex_unlock(&w->qmu);
    x->ready++;
    pthread_cond_signal(&x->work_cv);
}

static task *new_task(exec *x, cx_arena *a, gexec *g, size_t node, long item) {
    task *t = cx_alloc(a, sizeof *t);
    t->g = g;
    t->node = node;
    t->item = item;
    t->rank = cx_get_num(at(g->nodes_ir, node), "rank", 0);
    t->seq = x->seq++;
    t->state = T_WAITING;
    t->rerun = 0;
    return t;
}

/* An answer arrived: tasks waiting for it run again. */
static void wake(exec *x, worker *w, pending *p) {
    for (waiter *it = p->waiters; it; it = it->next) {
        task *t = it->t;
        if (t->state == T_WAITING)
            push_task(x, w, t);
        else if (t->state == T_RUNNING)
            t->rerun = 1;
    }
    p->waiters = NULL;
}

/* Nothing is ready, running or in flight, yet the graph is not done. */
static void check_stuck(exec *x) {
    if (x->ready <= 0 && x->running == 0 && x->inflight == 0 && x->jobs.len == 0 &&
        !x->finished && !x->stopping)
        fail_locked(x, NULL, NULL, "internal error: the run stopped with nothing left to do");
}

static void graph_done(exec *x, worker *w, gexec *g) {
    size_t out = index_of(g->graph, "output");
    cx_value *v = out < g->n ? g->values[out] : NULL;
    if (!v) v = cx_null(&w->arena);
    if (g->parent) {
        g->parent->state = P_DONE;
        g->parent->value = v;
        wake(x, w, g->parent);
    } else {
        x->result = v;
        x->finished = 1;
        pthread_cond_broadcast(&x->work_cv);
        pthread_cond_broadcast(&x->io_cv);
    }
}

static void complete_node(exec *x, worker *w, gexec *g, size_t i, cx_value *v) {
    g->values[i] = v;
    cx_value *node = at(g->nodes_ir, i);
    if (x->trace && strcmp(cx_get_str(node, "effect", "pure"), "pure") != 0) {
        if (g->lists[i])
            trace(x, cx_get_str(node, "name", "?"), "done  %zu item(s)  %.2f s",
                  len_of(g->lists[i]), now() - g->started[i]);
        else
            trace(x, cx_get_str(node, "name", "?"), "done  %.2f s", now() - g->started[i]);
    }
    for (size_t k = 0; k < g->nusers[i]; k++) {
        size_t u = g->users[i][k];
        if (--g->missing[u] == 0) push_task(x, w, new_task(x, &w->arena, g, u, -1));
    }
    if (--g->nodes_left == 0) graph_done(x, w, g);
}

/* Starts a graph: its nodes without inputs are ready at once. */
static void start_graph(exec *x, worker *w, cx_arena *a, cx_value *graph, cx_value **params,
                        const char *path, int depth, pending *parent) {
    gexec *g = cx_alloc(a, sizeof *g);
    memset(g, 0, sizeof *g);
    g->graph = graph;
    g->nodes_ir = cx_get(graph, "nodes");
    g->n = len_of(g->nodes_ir);
    size_t n = g->n ? g->n : 1;
    g->params = params;
    g->path = path;
    g->depth = depth;
    g->parent = parent;
#define ZEROED(field) (g->field = cx_alloc(a, n * sizeof *g->field), memset(g->field, 0, n * sizeof *g->field))
    ZEROED(values);
    ZEROED(missing);
    ZEROED(users);
    ZEROED(nusers);
    ZEROED(lists);
    ZEROED(items);
    ZEROED(items_left);
    ZEROED(started);
#undef ZEROED
    g->nodes_left = g->n;
    /* Who reads whom, from each node's inputs. */
    for (size_t i = 0; i < g->n; i++) {
        cx_value *inputs = cx_get(at(g->nodes_ir, i), "inputs");
        g->missing[i] = (int)len_of(inputs);
        for (size_t k = 0; k < len_of(inputs); k++) {
            size_t j = (size_t)at(inputs, k)->u.num;
            if (j < g->n) g->nusers[j]++;
        }
    }
    for (size_t i = 0; i < g->n; i++) {
        g->users[i] = cx_alloc(a, (g->nusers[i] ? g->nusers[i] : 1) * sizeof(size_t));
        g->nusers[i] = 0;
    }
    for (size_t i = 0; i < g->n; i++) {
        cx_value *inputs = cx_get(at(g->nodes_ir, i), "inputs");
        for (size_t k = 0; k < len_of(inputs); k++) {
            size_t j = (size_t)at(inputs, k)->u.num;
            if (j < g->n) g->users[j][g->nusers[j]++] = i;
        }
    }
    if (g->n == 0) {
        graph_done(x, w, g);
        return;
    }
    for (size_t i = 0; i < g->n; i++)
        if (g->missing[i] == 0) push_task(x, w, new_task(x, a, g, i, -1));
}

/* ----- effects: decoding ----------------------------------------------- */

static int is_temporary(const char *kind) {
    return strcmp(kind, "Timeout") == 0 || strcmp(kind, "RateLimit") == 0 ||
           strcmp(kind, "Unavailable") == 0 || strcmp(kind, "Network") == 0;
}

static int listed(const cx_value *list, const char *kind) {
    for (size_t i = 0; i < len_of(list); i++)
        if (at(list, i)->kind == CX_STR && strcmp(at(list, i)->u.str.s, kind) == 0) return 1;
    return 0;
}

/* Wait before attempt `attempt + 1`: 1 s, 2 s, 4 s (twice that for rate
 * limits). CALYX_RETRY_BASE_MS changes the base, for tests. */
static double backoff_s(int attempt, const char *kind) {
    double base = 1.0;
    const char *env = getenv("CALYX_RETRY_BASE_MS");
    if (env && *env) base = strtod(env, NULL) / 1000.0;
    double s = base * (double)(1 << (attempt - 1));
    return strcmp(kind, "RateLimit") == 0 ? 2 * s : s;
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

/* A model's answer as a value of the prompt's type, or NULL. */
static cx_value *decode_model(cx_arena *a, cx_value *prompt, cx_value *ok, const char **why) {
    cx_value *t = cx_get(ok, "text");
    cx_value *schema = cx_get(prompt, "schema");
    if (!schema || schema->kind == CX_NULL) return t;
    const char *s = t ? t->u.str.s : "";
    size_t len = t ? t->u.str.len : 0;
    strip_fence(&s, &len);
    *why = NULL;
    cx_value *v = cx_parse(a, s, len, why);
    if (v && cx_get_bool(prompt, "wrapped", 0)) {
        v = cx_get(v, "value");
        if (!v) *why = "missing `value`";
    }
    return v;
}

/* A tool's answer: its text, its structured content, or its text as JSON. */
static cx_value *decode_tool(cx_arena *a, cx_value *tool, cx_value *ok) {
    if (cx_get_bool(tool, "returns_text", 1)) return cx_get(ok, "text");
    cx_value *j = cx_get(ok, "json");
    if (j && j->kind != CX_NULL) return j;
    cx_value *t = cx_get(ok, "text");
    return t ? cx_parse(a, t->u.str.s, t->u.str.len, NULL) : NULL;
}

/* Tokens and cost of an answer, for the totals and the budget. With `mu` held. */
static void account(exec *x, const char *model_id, cx_value *ok) {
    x->input_tokens += (unsigned long long)cx_get_num(ok, "input_tokens", 0);
    x->output_tokens += (unsigned long long)cx_get_num(ok, "output_tokens", 0);
    cx_value *cost = cx_get(ok, "cost_usd");
    if (cost && cost->kind == CX_NUM) {
        x->spent += cost->u.num;
    } else if (x->budget > 0 && !x->unpriced_warned) {
        x->unpriced_warned = 1;
        fprintf(stderr,
                "calyx: warning: no price for model `%s` in calyx.toml; its calls do not count "
                "toward the budget\n",
                model_id);
    }
}

/* ----- effects: in the I/O threads --------------------------------------- */

static int journal_record(exec *x, job *j, const char *effect, cx_value *ok) {
    if (!x->journal || cx_journal_get_mode(x->journal) == CX_JOURNAL_REPLAY) return 1;
    cx_buf b = {0};
    cx_write(&b, ok);
    pthread_mutex_lock(&x->jmu);
    int written = cx_journal_record(x->journal, j->key, effect, j->req_hash, b.data, b.len);
    if (written && x->crash_after > 0 && ++x->recorded >= x->crash_after) {
        /* Simulates `kill -9` right after this entry reached the journal. The
         * journal stays locked, so no other call is recorded after it. */
        fprintf(stderr, "calyx: CALYX_CRASH_AFTER=%ld reached, exiting abruptly\n", x->crash_after);
        _exit(137);
    }
    pthread_mutex_unlock(&x->jmu);
    cx_buf_free(&b);
    return written;
}

static cx_value *io_call(cx_arena *a, char *(*fn)(const char *), const char *req) {
    char *out = fn(req);
    if (!out) return NULL;
    cx_value *v = cx_parse(a, out, strlen(out), NULL);
    calyx_string_free(out);
    return v;
}

/*
 * Runs one call, with its retries, in an I/O thread. Returns the decoded
 * value, or NULL with the reason in `why` ("stopped" if the run stopped).
 */
static cx_value *run_job(exec *x, cx_arena *a, job *j, char *why, size_t why_len) {
    const char *name = cx_get_str(j->spec, j->is_model ? "id" : "name", "?");
    const char *effect = j->is_model ? "llm" : cx_get_str(j->spec, "effect", "read");
    const char *prompt_name = j->is_model ? cx_get_str(j->prompt, "name", "?") : "";
    int write_once = strcmp(effect, "write once") == 0;
    cx_value *retry_on = j->is_model ? NULL : cx_get(j->spec, "retry_on");

    if (write_once && x->journal) {
        pthread_mutex_lock(&x->jmu);
        cx_journal_begin(x->journal, j->key, j->req_hash);
        pthread_mutex_unlock(&x->jmu);
    }
    for (int attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
        pthread_mutex_lock(&x->mu);
        if (x->stopping) {
            pthread_mutex_unlock(&x->mu);
            snprintf(why, why_len, "stopped");
            return NULL;
        }
        if (j->is_model && x->budget > 0 && x->spent >= x->budget) {
            pthread_mutex_unlock(&x->mu);
            snprintf(why, why_len,
                     "the budget of %.2f USD is used up (%.4f USD spent); the run can continue "
                     "with a larger budget",
                     x->budget, x->spent);
            return NULL;
        }
        /* `rate`: calls start at most `rate` per second (D3). */
        double wait = 0;
        if (x->rate > 0) {
            double t = now(), start = x->next_start > t ? x->next_start : t;
            x->next_start = start + 1.0 / x->rate;
            wait = start - t;
        }
        if (attempt > 1) x->retries++;
        if (j->is_model)
            x->model_calls++;
        else
            x->tool_calls++;
        pthread_mutex_unlock(&x->mu);
        sleep_s(wait);

        cx_value *ans = io_call(a, j->is_model ? calyx_io_model_call : calyx_io_tool_call, j->req);
        cx_value *ok = cx_get(ans, "ok");
        const char *kind = "Unavailable";
        double hint = 0;
        if (ok) {
            const char *perr = NULL;
            cx_value *v = j->is_model ? decode_model(a, j->prompt, ok, &perr)
                                      : decode_tool(a, j->spec, ok);
            if (j->is_model) {
                pthread_mutex_lock(&x->mu);
                account(x, name, ok);
                pthread_mutex_unlock(&x->mu);
                trace(x, j->label, "llm   %s(%s)  %.2f s  %.0f -> %.0f tokens%s", name,
                      prompt_name, cx_get_num(ok, "ms", 0) / 1000.0,
                      cx_get_num(ok, "input_tokens", 0), cx_get_num(ok, "output_tokens", 0),
                      attempt > 1 ? "  (retry)" : "");
            } else {
                trace(x, j->label, "%-5s %s  %.2f s%s%s", effect, name,
                      cx_get_num(ok, "ms", 0) / 1000.0,
                      cx_get_bool(ok, "truncated", 0) ? "  (output cut at max_output)" : "",
                      attempt > 1 ? "  (retry)" : "");
            }
            if (v) {
                /* Only answers that decode are recorded. */
                if (!journal_record(x, j, effect, ok)) {
                    snprintf(why, why_len, "cannot write the journal");
                    return NULL;
                }
                return v;
            }
            if (!j->is_model) {
                snprintf(why, why_len, "tool `%s` failed: Decode: the output is not JSON", name);
                return NULL;
            }
            /* The answer does not match the prompt's type: ask again. */
            kind = "Decode";
            snprintf(why, why_len,
                     "model `%s` with prompt `%s` failed: Decode: the answer is not valid JSON "
                     "(%s)",
                     name, prompt_name, perr ? perr : "?");
        } else {
            cx_value *err = cx_get(ans, "error");
            kind = cx_get_str(err, "kind", "Unavailable");
            const char *message = cx_get_str(err, "message", "no answer");
            if (j->is_model)
                snprintf(why, why_len, "model `%s` with prompt `%s` failed: %s: %s", name,
                         prompt_name, kind, message);
            else
                snprintf(why, why_len, "tool `%s` failed: %s: %s", name, kind, message);
            trace(x, j->label, "%-5s %s  failed: %s: %s", effect, name, kind, message);
            /* Models repeat temporary errors; tools only what `retry_on` lists;
             * `write once` never repeats (D2). */
            int retry = j->is_model ? is_temporary(kind) : (!write_once && listed(retry_on, kind));
            if (!retry) return NULL;
            /* Wait at least what the provider asked for (up to a minute). */
            hint = cx_get_num(err, "retry_after_ms", 0) / 1000.0;
            if (hint > MAX_RETRY_HINT_S) hint = MAX_RETRY_HINT_S;
        }
        if (attempt < MAX_ATTEMPTS) {
            double wait = backoff_s(attempt, kind);
            sleep_s(hint > wait ? hint : wait);
        }
    }
    return NULL;
}

typedef struct {
    exec *x;
    int id;
} io_arg;

static void *io_main(void *arg) {
    io_arg *ia = arg;
    exec *x = ia->x;
    cx_arena *a = &x->io_arenas[ia->id];
    for (;;) {
        pthread_mutex_lock(&x->mu);
        while ((x->jobs.len == 0 || (x->deterministic && (x->ready > 0 || x->running > 0))) &&
               !x->stopping && !x->finished)
            pthread_cond_wait(&x->io_cv, &x->mu);
        if (x->stopping || x->finished) {
            pthread_mutex_unlock(&x->mu);
            break;
        }
        job *j = job_heap_pop(&x->jobs);
        x->inflight++;
        if (x->inflight > x->max_inflight) x->max_inflight = x->inflight;
        pthread_mutex_unlock(&x->mu);

        char why[1024] = "";
        cx_value *v = run_job(x, a, j, why, sizeof why);

        pthread_mutex_lock(&x->mu);
        x->inflight--;
        if (v) {
            j->p->state = P_DONE;
            j->p->value = v;
        } else {
            j->p->state = P_FAILED;
            if (strcmp(why, "stopped") != 0) fail_locked(x, j->graph, j->node, why);
        }
        wake(x, NULL, j->p);
        check_stuck(x);
        pthread_mutex_unlock(&x->mu);
        free(j->req);
        free(j);
    }
    return NULL;
}

/* ----- effects: in the workers ------------------------------------------- */

/*
 * The value of a call: from memory or the journal if it finished, or
 * PENDING after handing it to an I/O thread. Takes the request text.
 */
static cx_value *request(ctx *c, cx_value *e, int is_model, cx_value *spec, cx_value *prompt,
                         cx_buf *req) {
    exec *x = c->x;
    const char *key = fmt(&c->w->arena, "%s#%zu", c->instance, index_of(e, "id"));
    char hash[65];
    cx_sha256_hex(req->data, req->len, hash);
    const char *name = cx_get_str(spec, is_model ? "id" : "name", "?");
    const char *effect = is_model ? "llm" : cx_get_str(spec, "effect", "read");
    const char *graph = cx_get_str(c->g->graph, "name", "?");
    cx_value *result = NULL;

    pthread_mutex_lock(&x->mu);
    pending *p = ptab_get(x, key);
    if (p) {
        if (p->state == P_DONE) result = p->value;
        if (p->state == P_INFLIGHT) {
            add_waiter(&c->w->arena, p, c->t);
            result = PENDING;
        }
        pthread_mutex_unlock(&x->mu);
        cx_buf_free(req);
        return result;
    }
    if (x->journal) {
        int mismatch = 0, uncertain = 0;
        pthread_mutex_lock(&x->jmu);
        cx_value *hit = cx_journal_lookup(x->journal, key, hash, &mismatch);
        if (!hit && strcmp(effect, "write once") == 0)
            uncertain = cx_journal_uncertain(x->journal, key);
        int replay = cx_journal_get_mode(x->journal) == CX_JOURNAL_REPLAY;
        pthread_mutex_unlock(&x->jmu);
        char msg[512] = "";
        if (mismatch)
            snprintf(msg, sizeof msg,
                     "call `%s` differs from the one in the journal: the run cannot be resumed "
                     "deterministically",
                     key);
        else if (!hit && replay)
            snprintf(msg, sizeof msg,
                     "replay: call `%s` is not in the journal (the run stopped before it)", key);
        else if (uncertain)
            /* The `on_uncertain` policies (verify, pause, accept_loss) arrive in M6. */
            snprintf(msg, sizeof msg,
                     "`%s` (write once) started before the interruption and its outcome is "
                     "unknown; it is not repeated",
                     name);
        if (msg[0]) {
            fail_locked(x, graph, c->node, msg);
            pthread_mutex_unlock(&x->mu);
            cx_buf_free(req);
            return NULL;
        }
        if (hit) {
            const char *why = NULL;
            cx_value *v = is_model ? decode_model(&c->w->arena, prompt, hit, &why)
                                   : decode_tool(&c->w->arena, spec, hit);
            if (!v) {
                fail_locked(x, graph, c->node, "the journal's answer does not decode");
            } else {
                x->from_journal++;
                if (is_model) account(x, name, hit);
                new_pending(x, &c->w->arena, key, P_DONE, v);
                trace(x, c->label, "%-5s %s%s%s%s  from the journal", effect, name,
                      is_model ? "(" : "", is_model ? cx_get_str(prompt, "name", "?") : "",
                      is_model ? ")" : "");
            }
            pthread_mutex_unlock(&x->mu);
            cx_buf_free(req);
            return v;
        }
    }
    /* A new call: an I/O thread makes it; this task waits for the answer. */
    p = new_pending(x, &c->w->arena, key, P_INFLIGHT, NULL);
    add_waiter(&c->w->arena, p, c->t);
    job *j = calloc(1, sizeof *j);
    if (!j) abort();
    j->p = p;
    j->is_model = is_model;
    j->spec = spec;
    j->prompt = prompt;
    j->req = cx_buf_take(req);
    memcpy(j->req_hash, hash, sizeof hash);
    j->key = p->key;
    j->graph = graph;
    j->node = c->node;
    j->label = c->label;
    j->rank = c->t->rank;
    j->seq = x->seq++;
    job_heap_push(&x->jobs, j);
    pthread_cond_signal(&x->io_cv);
    pthread_mutex_unlock(&x->mu);
    return PENDING;
}

static cx_value *eval(ctx *c, cx_value *e);

/* Evaluates every expression of `list` into `out`, so independent calls
 * start together. Returns NULL on error or PENDING from the caller. */
#define EVAL_ALL(c, list, out)                                                          \
    do {                                                                                \
        int waiting_ = 0;                                                               \
        for (size_t i_ = 0; i_ < len_of(list); i_++) {                                  \
            (out)[i_] = eval((c), at((list), i_));                                      \
            if (!(out)[i_]) return NULL;                                                \
            if ((out)[i_] == PENDING) waiting_ = 1;                                     \
        }                                                                               \
        if (waiting_) return PENDING;                                                   \
    } while (0)

static cx_value *call_model(ctx *c, cx_value *e) {
    exec *x = c->x;
    cx_value *model = at(x->models, index_of(e, "model"));
    cx_value *prompt = at(x->prompts, index_of(e, "prompt"));
    if (!model || !prompt) return failf(c, "invalid IR: unknown model or prompt");
    cx_value *args_e = cx_get(e, "args");
    cx_value *params = cx_get(prompt, "params");
    size_t n = len_of(args_e);
    cx_value **args = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *args);
    EVAL_ALL(c, args_e, args);

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
    cx_buf req = {0};
    cx_buf_puts(&req, "{\"model\":");
    cx_write(&req, cx_get(model, "id"));
    cx_buf_puts(&req, ",\"prompt\":");
    cx_buf_json_str(&req, text.data ? text.data : "", text.len);
    cx_buf_puts(&req, ",\"schema\":");
    cx_write(&req, schema && schema->kind != CX_NULL ? schema : NULL);
    cx_buf_puts(&req, ",\"max_output\":");
    cx_write(&req, cx_get(model, "max_output"));
    cx_buf_puts(&req, ",\"timeout_ms\":300000}"); /* llm: 5 min per attempt (D22) */
    cx_buf_free(&text);
    return request(c, e, 1, model, prompt, &req);
}

static cx_value *call_tool(ctx *c, cx_value *e) {
    cx_value *tool = at(c->x->tools, index_of(e, "tool"));
    if (!tool) return failf(c, "invalid IR: unknown tool");
    const char *name = cx_get_str(tool, "name", "?");
    cx_value *params = cx_get(tool, "params");
    cx_value *args_e = cx_get(e, "args");
    size_t n = len_of(args_e);
    cx_value **args = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *args);
    EVAL_ALL(c, args_e, args);
    cx_buf req = {0};
    cx_buf_puts(&req, "{\"tool\":");
    cx_buf_json_str(&req, name, strlen(name));
    cx_buf_puts(&req, ",\"args\":{");
    for (size_t i = 0; i < n; i++) {
        if (i) cx_buf_putc(&req, ',');
        cx_write(&req, at(params, i));
        cx_buf_putc(&req, ':');
        cx_write(&req, args[i]);
    }
    cx_buf_puts(&req, "},\"max_output\":");
    cx_write(&req, cx_get(tool, "max_output"));
    cx_buf_printf(&req, ",\"timeout_ms\":%.0f}", cx_get_num(tool, "timeout_ms", 30000));
    return request(c, e, 0, tool, NULL, &req);
}

/* A subgraph runs as tasks of its own; this task waits for its result. */
static cx_value *call_graph(ctx *c, cx_value *e) {
    exec *x = c->x;
    cx_value *g = at(x->graphs, index_of(e, "graph"));
    if (!g) return failf(c, "invalid IR: unknown graph");
    cx_value *args_e = cx_get(e, "args");
    size_t n = len_of(args_e);
    if (n != len_of(cx_get(g, "params")))
        return failf(c, "graph `%s` expects %zu argument(s)", cx_get_str(g, "name", "?"),
                     len_of(cx_get(g, "params")));
    cx_value **args = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *args);
    EVAL_ALL(c, args_e, args);
    const char *key = fmt(&c->w->arena, "%s#%zu", c->instance, index_of(e, "id"));
    cx_value *result = PENDING;
    pthread_mutex_lock(&x->mu);
    pending *p = ptab_get(x, key);
    if (!p) {
        if (c->g->depth + 1 >= MAX_GRAPH_DEPTH) {
            fail_locked(x, cx_get_str(c->g->graph, "name", "?"), c->node,
                        "graphs nested too deeply");
            pthread_mutex_unlock(&x->mu);
            return NULL;
        }
        p = new_pending(x, &c->w->arena, key, P_INFLIGHT, NULL);
        add_waiter(&c->w->arena, p, c->t);
        start_graph(x, c->w, &c->w->arena, g, args,
                    fmt(&c->w->arena, "%s/%s", key, cx_get_str(g, "name", "?")),
                    c->g->depth + 1, p);
    } else if (p->state == P_INFLIGHT) {
        add_waiter(&c->w->arena, p, c->t);
    }
    if (p->state == P_DONE) result = p->value;
    if (p->state == P_FAILED) result = NULL;
    pthread_mutex_unlock(&x->mu);
    return result;
}

static cx_value *eval(ctx *c, cx_value *e) {
    const char *k = cx_get_str(e, "k", "");
    if (strcmp(k, "text") == 0 || strcmp(k, "int") == 0 || strcmp(k, "float") == 0)
        return cx_get(e, "v");
    if (strcmp(k, "param") == 0) return c->g->params[index_of(e, "i")];
    if (strcmp(k, "node") == 0) {
        size_t i = index_of(e, "i");
        cx_value *v = i < c->g->n ? c->g->values[i] : NULL;
        return v ? v : failf(c, "invalid IR: node used before it was computed");
    }
    if (strcmp(k, "item") == 0) return c->item;
    if (strcmp(k, "field") == 0) {
        cx_value *base = eval(c, cx_get(e, "base"));
        if (!base || base == PENDING) return base;
        const char *name = cx_get_str(e, "name", "");
        cx_value *v = cx_get(base, name);
        if (!v) {
            cx_buf shown = {0};
            cx_write(&shown, base);
            if (shown.len > 200) shown.len = 200, shown.data[200] = '\0';
            failf(c, "the value has no field `%s`: %s", name, shown.data);
            cx_buf_free(&shown);
        }
        return v;
    }
    if (strcmp(k, "list") == 0) {
        cx_value *items = cx_get(e, "items");
        size_t n = len_of(items);
        cx_value **vs = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *vs);
        EVAL_ALL(c, items, vs);
        return cx_list(&c->w->arena, vs, n);
    }
    if (strcmp(k, "interp") == 0) {
        cx_value *parts = cx_get(e, "parts");
        size_t n = len_of(parts);
        cx_value **vs = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *vs);
        int waiting = 0;
        for (size_t i = 0; i < n; i++) {
            cx_value *lit = cx_get(at(parts, i), "lit");
            vs[i] = lit ? lit : eval(c, cx_get(at(parts, i), "expr"));
            if (!vs[i]) return NULL;
            if (vs[i] == PENDING) waiting = 1;
        }
        if (waiting) return PENDING;
        cx_buf b = {0};
        for (size_t i = 0; i < n; i++) render(&b, vs[i]);
        cx_value *s = cx_str(&c->w->arena, b.data ? b.data : "", b.len);
        cx_buf_free(&b);
        return s;
    }
    if (strcmp(k, "model") == 0) return call_model(c, e);
    if (strcmp(k, "tool") == 0) return call_tool(c, e);
    if (strcmp(k, "graph") == 0) return call_graph(c, e);
    return failf(c, "invalid IR: unknown expression `%s`", k);
}

/* ----- workers -------------------------------------------------------------- */

static void run_task(worker *w, task *t) {
    exec *x = w->x;
    gexec *g = t->g;
    cx_value *node = at(g->nodes_ir, t->node);
    const char *name = cx_get_str(node, "name", "?");
    ctx c = {x, w, t, g, NULL, NULL, name, name};
    if (t->item < 0) {
        c.instance = fmt(&w->arena, "%s/%s", g->path, name);
    } else {
        c.instance = fmt(&w->arena, "%s/%s[%ld]", g->path, name, t->item);
        c.label = fmt(&w->arena, "%s[%ld]", name, t->item);
        c.item = at(g->lists[t->node], (size_t)t->item);
    }
    cx_value *over_e = cx_get(node, "over");
    int fan_out = over_e && over_e->kind != CX_NULL;
    cx_value *v = eval(&c, t->item < 0 && fan_out ? over_e : cx_get(node, "value"));
    if (v && v != PENDING && t->item < 0 && fan_out && v->kind != CX_LIST)
        v = failf(&c, "`for each` over a value that is not a list");

    pthread_mutex_lock(&x->mu);
    x->running--;
    if (x->deterministic) pthread_cond_signal(&x->io_cv);
    if (v == PENDING) {
        /* Wait for the answers it asked for; run again if one already came. */
        if (t->rerun) {
            t->rerun = 0;
            push_task(x, w, t);
        } else {
            t->state = T_WAITING;
        }
    } else {
        t->state = T_DONE;
        if (v && !x->stopping) {
            if (t->item < 0 && fan_out) {
                /* The list is ready: one task per item. */
                size_t m = v->u.list.len;
                g->lists[t->node] = v;
                if (m == 0) {
                    complete_node(x, w, g, t->node, v);
                } else {
                    g->items[t->node] = cx_alloc(&w->arena, m * sizeof(cx_value *));
                    g->items_left[t->node] = m;
                    for (size_t j = 0; j < m; j++)
                        push_task(x, w, new_task(x, &w->arena, g, t->node, (long)j));
                }
            } else if (t->item >= 0) {
                g->items[t->node][t->item] = v;
                if (--g->items_left[t->node] == 0) {
                    size_t m = len_of(g->lists[t->node]);
                    complete_node(x, w, g, t->node, cx_list(&w->arena, g->items[t->node], m));
                }
            } else {
                complete_node(x, w, g, t->node, v);
            }
        }
    }
    check_stuck(x);
    pthread_mutex_unlock(&x->mu);
}

/* Takes the best task from this worker's queue, or steals one. */
static task *take(worker *w) {
    exec *x = w->x;
    for (int k = 0; k < x->nworkers; k++) {
        worker *v = &x->workers[(w->id + k) % x->nworkers];
        pthread_mutex_lock(&v->qmu);
        task *t = task_heap_pop(&v->q);
        pthread_mutex_unlock(&v->qmu);
        if (t) return t;
    }
    return NULL;
}

static void *worker_main(void *arg) {
    worker *w = arg;
    exec *x = w->x;
    for (;;) {
        task *t = take(w);
        pthread_mutex_lock(&x->mu);
        if (t) x->ready--;
        if (x->stopping || x->finished) {
            pthread_mutex_unlock(&x->mu);
            break;
        }
        if (t) {
            t->state = T_RUNNING;
            x->running++;
            if (t->g->started[t->node] == 0) t->g->started[t->node] = now();
            pthread_mutex_unlock(&x->mu);
            run_task(w, t);
            continue;
        }
        while (x->ready <= 0 && !x->stopping && !x->finished) {
            check_stuck(x);
            if (x->stopping) break;
            pthread_cond_wait(&x->work_cv, &x->mu);
        }
        pthread_mutex_unlock(&x->mu);
    }
    return NULL;
}

/* ----- entry point ------------------------------------------------------- */

static char *error_json(const char *msg) {
    cx_buf b = {0};
    cx_buf_puts(&b, "{\"error\":");
    cx_buf_json_str(&b, msg, strlen(msg));
    cx_buf_putc(&b, '}');
    return cx_buf_take(&b);
}

static int cpu_count(void) {
    long n = sysconf(_SC_NPROCESSORS_ONLN);
    return n < 1 ? 1 : n > MAX_WORKERS ? MAX_WORKERS : (int)n;
}

char *calyx_run(const char *ir_json, const char *graph, const char *args_json,
                const char *options_json) {
    if (!ir_json || !graph || !args_json) return error_json("calyx_run: missing argument");
    exec *x = calloc(1, sizeof *x);
    if (!x) return error_json("out of memory");
    pthread_mutex_init(&x->mu, NULL);
    pthread_mutex_init(&x->jmu, NULL);
    pthread_cond_init(&x->work_cv, NULL);
    pthread_cond_init(&x->io_cv, NULL);
    x->t0 = now();
    char *out = NULL;
    cx_buf msg = {0};

    cx_value *options =
        options_json ? cx_parse(&x->arena, options_json, strlen(options_json), NULL) : NULL;
    cx_value *ir = cx_parse(&x->arena, ir_json, strlen(ir_json), NULL);
    cx_value *args = cx_parse(&x->arena, args_json, strlen(args_json), NULL);
    if (options_json && (!options || options->kind != CX_REC)) {
        out = error_json("the options must be a JSON object");
        goto done;
    }
    if (!ir || cx_get_num(ir, "version", -1) != IR_VERSION) {
        out = error_json("the program's IR is invalid or from another version of calyx");
        goto done;
    }
    if (!args || args->kind != CX_REC) {
        out = error_json("the arguments must be a JSON object");
        goto done;
    }
    x->trace = cx_get_bool(options, "trace", 0);
    const char *crash = getenv("CALYX_CRASH_AFTER");
    x->crash_after = crash ? strtol(crash, NULL, 10) : 0;
    x->models = cx_get(ir, "models");
    x->tools = cx_get(ir, "tools");
    x->prompts = cx_get(ir, "prompts");
    x->graphs = cx_get(ir, "graphs");

    size_t gi = (size_t)-1;
    for (size_t i = 0; i < len_of(x->graphs); i++)
        if (strcmp(cx_get_str(at(x->graphs, i), "name", ""), graph) == 0) gi = i;
    if (gi == (size_t)-1) {
        cx_buf_printf(&msg, "the program has no graph `%s`", graph);
        out = error_json(msg.data);
        goto done;
    }
    cx_value *g = at(x->graphs, gi);
    cx_value *params = cx_get(g, "params");
    size_t np = len_of(params);
    cx_value **vals = cx_alloc(&x->arena, (np ? np : 1) * sizeof *vals);
    for (size_t i = 0; i < np; i++) {
        vals[i] = cx_get(args, at(params, i)->u.str.s);
        if (!vals[i]) {
            cx_buf_printf(&msg, "missing argument `%s` for graph `%s`", at(params, i)->u.str.s,
                          graph);
            out = error_json(msg.data);
            goto done;
        }
    }

    /* Limits of the graph that runs (D3). */
    cx_value *limits = cx_get(g, "limits");
    double threads = cx_get_num(limits, "threads", DEFAULT_THREADS);
    x->rate = cx_get_num(limits, "rate_per_s", 0);
    cx_value *budget = cx_get(limits, "budget");
    if (budget && budget->kind == CX_REC) {
        if (strcmp(cx_get_str(budget, "unit", ""), "USD") == 0)
            x->budget = cx_get_num(budget, "amount", 0);
        else
            fprintf(stderr, "calyx: warning: budgets in %s are not enforced yet, only USD\n",
                    cx_get_str(budget, "unit", "?"));
    }
    double budget_override = cx_get_num(options, "budget_usd", 0);
    if (budget_override > 0) x->budget = budget_override;
    x->deterministic = cx_get_bool(options, "deterministic", 0);
    x->nworkers = x->deterministic ? 1 : cpu_count();
    x->nio = x->deterministic ? 1 : threads < 1 ? 1 : threads > 256 ? 256 : (int)threads;

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
        x->journal = cx_journal_open(&x->arena, dir, mode, ir_hash, header.data, &msg);
        cx_buf_free(&header);
        if (!x->journal) {
            out = error_json(msg.data ? msg.data : "cannot open the journal");
            goto done;
        }
    }

    if (x->trace)
        fprintf(stderr, "%7.2fs  running graph `%s` (%d worker(s), up to %d call(s) at once)\n",
                0.0, graph, x->nworkers, x->nio);
    x->workers = calloc((size_t)x->nworkers, sizeof *x->workers);
    x->io = calloc((size_t)x->nio, sizeof *x->io);
    x->io_arenas = calloc((size_t)x->nio, sizeof *x->io_arenas);
    io_arg *io_args = calloc((size_t)x->nio, sizeof *io_args);
    if (!x->workers || !x->io || !x->io_arenas || !io_args) abort();
    for (int i = 0; i < x->nworkers; i++) {
        x->workers[i].x = x;
        x->workers[i].id = i;
        pthread_mutex_init(&x->workers[i].qmu, NULL);
    }
    pthread_mutex_lock(&x->mu);
    start_graph(x, &x->workers[0], &x->arena, g, vals, graph, 0, NULL);
    pthread_mutex_unlock(&x->mu);
    for (int i = 0; i < x->nio; i++) {
        io_args[i] = (io_arg){x, i};
        pthread_create(&x->io[i], NULL, io_main, &io_args[i]);
    }
    for (int i = 0; i < x->nworkers; i++)
        pthread_create(&x->workers[i].th, NULL, worker_main, &x->workers[i]);
    for (int i = 0; i < x->nworkers; i++) pthread_join(x->workers[i].th, NULL);
    /* Calls in flight finish (and reach the journal) before returning. */
    for (int i = 0; i < x->nio; i++) pthread_join(x->io[i], NULL);
    free(io_args);

    cx_value *result = x->err.len ? NULL : x->result;
    if (x->trace) {
        fprintf(stderr,
                "%7.2fs  %s: %lu model call(s) (%llu -> %llu tokens), %lu tool call(s), %lu "
                "retry(ies), at most %d at once",
                now() - x->t0, result ? "finished" : "failed", x->model_calls, x->input_tokens,
                x->output_tokens, x->tool_calls, x->retries, x->max_inflight);
        if (x->journal) fprintf(stderr, ", %lu taken from the journal", x->from_journal);
        if (x->spent > 0) fprintf(stderr, ", %.4f USD", x->spent);
        fputc('\n', stderr);
    }
    if (!result) {
        const char *e = x->err.len ? x->err.data : "unknown failure";
        out = error_json(e);
        if (x->journal) cx_journal_end(x->journal, NULL, e);
    } else {
        cx_buf b = {0};
        cx_write(&b, result);
        if (x->journal) cx_journal_end(x->journal, b.data, NULL);
        cx_buf ok = {0};
        cx_buf_puts(&ok, "{\"ok\":");
        cx_buf_put(&ok, b.data, b.len);
        cx_buf_putc(&ok, '}');
        cx_buf_free(&b);
        out = cx_buf_take(&ok);
    }

done:
    cx_journal_close(x->journal);
    for (size_t i = 0; i < x->jobs.len; i++) {
        free(x->jobs.a[i]->req);
        free(x->jobs.a[i]);
    }
    free(x->jobs.a);
    if (x->workers) {
        for (int i = 0; i < x->nworkers; i++) {
            free(x->workers[i].q.a);
            cx_arena_free(&x->workers[i].arena);
            pthread_mutex_destroy(&x->workers[i].qmu);
        }
        free(x->workers);
    }
    if (x->io_arenas)
        for (int i = 0; i < x->nio; i++) cx_arena_free(&x->io_arenas[i]);
    free(x->io_arenas);
    free(x->io);
    free(x->ptab);
    cx_buf_free(&msg);
    cx_buf_free(&x->err);
    cx_arena_free(&x->arena);
    pthread_mutex_destroy(&x->mu);
    pthread_mutex_destroy(&x->jmu);
    pthread_cond_destroy(&x->work_cv);
    pthread_cond_destroy(&x->io_cv);
    free(x);
    return out;
}

void calyx_run_free(char *s) { free(s); }
