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
 * M5 adds loops, `match`, `if`, operators, records, `try` and agents. A
 * failure is local first: the task's context carries it, so `try` can turn
 * it into a value; only a failure nobody catches stops the run. An agent is
 * the ReAct cycle (D5): model with tools, tool calls in parallel, their
 * results as observations, until the model answers or a limit is reached.
 * Every turn and every tool call is a call with its own key, so agents are
 * journaled, resumed and replayed like everything else.
 */
#define _POSIX_C_SOURCE 200809L
/* macOS hides sysconf(_SC_NPROCESSORS_ONLN) and friends under strict POSIX. */
#define _DARWIN_C_SOURCE

#include "calyx_io.h"
#include "calyx_runtime.h"
#include "calyx_verify.h"
#include "journal.h"
#include "json.h"
#include "sha256.h"

#include <ctype.h>
#include <fcntl.h>
#include <pthread.h>
#include <sys/file.h>
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
typedef struct agent_progress agent_progress;

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

/* Where an agent got to: the turns it finished, with their observations.
 * The agent's step is evaluated again each time one of its calls answers;
 * without this, each evaluation would walk every turn from the first, with
 * a conversation that grows, and an agent's cost would grow with the cube
 * of its turns. With `mu` held. */
struct agent_progress {
    char *key;
    long turn;
    char *msgs;
    size_t len;
    char *previous;
    int repeats;
    agent_progress *next;
};

/* A call (or subgraph) by key: running, or finished with its value. */
struct pending {
    const char *key;
    int state;
    cx_value *value;
    const char *error; /* P_FAILED: why */
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
    int failed;      /* a node failed; the parent call already knows */
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

/* Kinds of call. */
enum { CALL_TOOL, CALL_MODEL, CALL_CHAT, CALL_ENTITY };

/* A call handed to an I/O thread. */
struct job {
    pending *p;
    int is_model; /* CALL_MODEL or CALL_CHAT */
    int raw;      /* CALL_CHAT: the answer is kept as the provider sent it */
    const char *note;
    cx_value *spec;   /* the model or the tool */
    cx_value *prompt; /* for models */
    char *req;
    char req_hash[65];
    const char *key;
    const char *graph, *node, *label;
    double rank;
    unsigned long seq;
    /* `write once` begun in an earlier run, with no answer in the journal:
     * it may or may not have happened (D2). */
    int uncertain;
    /* A message to an entity (D15): `spec` is the entity, `prompt` the
     * handler; `send` if it changes the state. */
    int entity, send;
};

typedef struct {
    job **a;
    size_t len, cap;
} job_heap;

struct exec {
    pthread_mutex_t mu, jmu;
    pthread_cond_t work_cv, io_cv;
    cx_arena arena; /* the main thread's */
    cx_value *models, *tools, *prompts, *graphs, *entities, *defs, *routers;
    /* Identifies this run in entities' records of applied messages: the
     * run's directory name, the same when it is resumed. */
    const char *run_id;
    /* The run's directory (with the journal): where `receive` keeps its
     * deadlines (`waits.jsonl`) and finds delivered messages (`inbox.jsonl`);
     * with the journal in PostgreSQL, they are rows of `calyx_files`. */
    const char *run_dir;
    /* `receive`s waiting for a message: the run stops, to be resumed. */
    int waiting;
    const char *wait_desc;
    /* Branches that lost a race (D12), by key prefix: their subgraphs'
     * tasks and their calls not started yet are dropped. */
    const char **cancelled;
    size_t ncancelled, cancelled_cap;
    /* Saga (D12): for each write with `compensate` that was sent, how to
     * undo it (the call's key, and `{"tool": i, "args": [...]}`); the
     * same goes to the journal as `owe:<key>`, before the write. */
    struct owe {
        const char *key;
        char *json;
    } *owes;
    size_t nowes, owes_cap;
    int trace;
    double t0;
    cx_buf err;
    int stopping, finished;
    /* One call at a time, chosen only when no task can run: same order always. */
    int deterministic;
    cx_value *result;
    cx_journal *journal;
    long crash_after, recorded;
    /* `--uncertain done|retry|failed`: what a person decided, when resuming,
     * about `write once` calls of unknown outcome. NULL: apply each tool's
     * `on_uncertain`. */
    const char *decision;
    /* `--uncertain done=<value>`: the answer of the uncertain call. */
    cx_value *decision_value;
    /* Pending calls by key: open addressing, capacity a power of two. */
    pending **ptab;
    size_t pcap, pused;
    /* Agents' progress by key, in buckets (with `mu`). */
    agent_progress *agents[256];
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
    char *instance;    /* `path/node` or `path/node[j]` */
    const char *scope; /* key prefix: the instance, plus loop turns */
    const char *node;  /* node name, for errors */
    const char *label; /* `node` or `node[j]`, for the trace */
    cx_value **locals; /* loop values and `case` fields */
    cx_value *estate;  /* in an entity's handler: the state */
    const char *failure; /* a failure not reported yet; `try` may catch it */
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

/* A failure of this evaluation: `try` may catch it; otherwise the task
 * reports it when evaluation ends. */
static cx_value *failf(ctx *c, const char *f, ...) {
    char msg[1024];
    va_list ap;
    va_start(ap, f);
    vsnprintf(msg, sizeof msg, f, ap);
    va_end(ap);
    if (!c->failure) c->failure = cx_strndup(&c->w->arena, msg, strlen(msg));
    return NULL;
}

/* A failure nothing may catch (a broken journal, an invalid IR): stops the run. */
static cx_value *fatalf(ctx *c, const char *f, ...) {
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
    p->error = NULL;
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
/* Nothing can run: the run waits for messages (D21), or it is a bug. */
static void check_stuck(exec *x) {
    if (x->ready <= 0 && x->running == 0 && x->inflight == 0 && x->jobs.len == 0 &&
        !x->finished && !x->stopping)
        fail_locked(x, NULL, NULL,
                    x->waiting ? x->wait_desc
                               : "internal error: the run stopped with nothing left to do");
}

/* Is `key` inside a branch that lost a race? (`mu` held) */
static int is_cancelled(exec *x, const char *key) {
    for (size_t i = 0; i < x->ncancelled; i++) {
        size_t n = strlen(x->cancelled[i]);
        if (strncmp(key, x->cancelled[i], n) == 0 && key[n] == '#') return 1;
    }
    return 0;
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

/*
 * Does `v` fit `schema`? The subset the compiler emits: type, enum,
 * properties, required, items, maxItems, minimum and anyOf. Models do not
 * always respect the schema they were given (a variant without its
 * fields); the answer must have the prompt's type before anyone uses it.
 */
static int conforms(const cx_value *v, const cx_value *schema, const char **why) {
    if (!schema || schema->kind != CX_REC) return 1;
    cx_value *any = cx_get(schema, "anyOf");
    if (any) {
        for (size_t i = 0; i < len_of(any); i++)
            if (conforms(v, at(any, i), why)) return 1;
        *why = "the answer matches none of the variants of its type";
        return 0;
    }
    const char *type = cx_get_str(schema, "type", NULL);
    if (type) {
        int ok = strcmp(type, "string") == 0    ? v->kind == CX_STR
                 : strcmp(type, "boolean") == 0 ? v->kind == CX_BOOL
                 : strcmp(type, "array") == 0   ? v->kind == CX_LIST
                 : strcmp(type, "object") == 0  ? v->kind == CX_REC
                 : strcmp(type, "number") == 0  ? v->kind == CX_NUM
                 : strcmp(type, "integer") == 0 ? v->kind == CX_NUM && v->u.num == (double)(long long)v->u.num
                                                : 1;
        if (!ok) {
            *why = "a value of the answer has the wrong type";
            return 0;
        }
    }
    cx_value *options = cx_get(schema, "enum");
    if (options) {
        int found = 0;
        for (size_t i = 0; i < len_of(options); i++) found |= cx_equal(v, at(options, i));
        if (!found) {
            *why = "a value of the answer is not one of the allowed ones";
            return 0;
        }
    }
    cx_value *min = cx_get(schema, "minimum");
    if (min && v->kind == CX_NUM && v->u.num < min->u.num) {
        *why = "a number of the answer is below its minimum";
        return 0;
    }
    if (v->kind == CX_LIST) {
        cx_value *max = cx_get(schema, "maxItems");
        if (max && (double)v->u.list.len > max->u.num) {
            *why = "a list of the answer is longer than its `max`";
            return 0;
        }
        for (size_t i = 0; i < v->u.list.len; i++)
            if (!conforms(v->u.list.items[i], cx_get(schema, "items"), why)) return 0;
    }
    if (v->kind == CX_REC) {
        cx_value *required = cx_get(schema, "required");
        for (size_t i = 0; i < len_of(required); i++) {
            cx_value *f = cx_get(v, at(required, i)->u.str.s);
            if (!f || f->kind == CX_NULL) {
                *why = "a required field is missing from the answer";
                return 0;
            }
        }
        cx_value *props = cx_get(schema, "properties");
        for (size_t i = 0; props && i < props->u.rec.len; i++) {
            cx_value *f = cx_get(v, props->u.rec.keys[i]);
            if (f && f->kind != CX_NULL && !conforms(f, props->u.rec.vals[i], why)) return 0;
        }
    }
    return 1;
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
    if (v && !conforms(v, schema, why)) return NULL;
    if (v && cx_get_bool(prompt, "wrapped", 0)) {
        v = cx_get(v, "value");
        if (!v) *why = "missing `value`";
    }
    return v;
}

/* The fields of `v` that `schema` (a type of the program) has, at every
 * level: a server's answer, kept to what the program declared. */
static cx_value *project(cx_arena *a, cx_value *v, cx_value *schema) {
    if (!v || !schema || schema->kind != CX_REC) return v;
    cx_value *any = cx_get(schema, "anyOf");
    if (any) {
        for (size_t i = 0; i < len_of(any); i++) {
            const char *why = NULL;
            cx_value *p = project(a, v, at(any, i));
            if (conforms(p, at(any, i), &why)) return p;
        }
        return v;
    }
    if (v->kind == CX_LIST) {
        cx_value *items = cx_get(schema, "items");
        if (!items) return v;
        size_t n = v->u.list.len;
        cx_value **out = cx_alloc(a, (n ? n : 1) * sizeof *out);
        for (size_t i = 0; i < n; i++) out[i] = project(a, v->u.list.items[i], items);
        return cx_list(a, out, n);
    }
    if (v->kind != CX_REC) return v;
    cx_value *props = cx_get(schema, "properties");
    cx_value *each = cx_get(schema, "additionalProperties");
    size_t n = props && props->kind == CX_REC ? props->u.rec.len : v->u.rec.len;
    const char **keys = cx_alloc(a, (n ? n : 1) * sizeof *keys);
    cx_value **vals = cx_alloc(a, (n ? n : 1) * sizeof *vals);
    size_t k = 0;
    if (props && props->kind == CX_REC) {
        for (size_t i = 0; i < n; i++) {
            cx_value *f = cx_get(v, props->u.rec.keys[i]);
            if (!f) continue;
            keys[k] = props->u.rec.keys[i];
            vals[k++] = project(a, f, props->u.rec.vals[i]);
        }
    } else if (each) {
        for (size_t i = 0; i < n; i++) {
            keys[k] = v->u.rec.keys[i];
            vals[k++] = project(a, v->u.rec.vals[i], each);
        }
    } else {
        return v;
    }
    return cx_rec(a, keys, vals, k);
}

static size_t json_size(const cx_value *v) {
    cx_buf b = {0};
    cx_write(&b, v);
    size_t n = b.len;
    cx_buf_free(&b);
    return n;
}

/* The longest list (in JSON) with more than one item, and the longest text,
 * in `*slot`. */
static void largest(cx_value **slot, cx_value ***list, size_t *list_size, cx_value ***str) {
    cx_value *v = *slot;
    if (v->kind == CX_LIST) {
        size_t size = v->u.list.len > 1 ? json_size(v) : 0;
        if (size > *list_size) {
            *list_size = size;
            *list = slot;
        }
        for (size_t i = 0; i < v->u.list.len; i++) largest(&v->u.list.items[i], list, list_size, str);
    } else if (v->kind == CX_REC) {
        for (size_t i = 0; i < v->u.rec.len; i++) largest(&v->u.rec.vals[i], list, list_size, str);
    } else if (v->kind == CX_STR && (!*str || v->u.str.len > (**str)->u.str.len)) {
        *str = slot;
    }
}

/* Brings `v`, a value of its own (changed in place), within `limit` bytes of
 * JSON: the last items of the longest lists go first, then the second half
 * of the longest texts. 1 if anything was cut. */
static int shrink(cx_arena *a, cx_value **v, size_t limit) {
    int cut = 0;
    while (json_size(*v) > limit) {
        cx_value **list = NULL, **str = NULL;
        size_t list_size = 0;
        largest(v, &list, &list_size, &str);
        if (list) {
            (*list)->u.list.len--;
        } else if (str && (*str)->u.str.len > 64) {
            size_t n = (*str)->u.str.len / 2;
            const char *t = (*str)->u.str.s;
            while (n > 0 && ((unsigned char)t[n] & 0xC0) == 0x80) n--;
            char *s2 = cx_alloc(a, n + 4);
            memcpy(s2, t, n);
            memcpy(s2 + n, "\xE2\x80\xA6", 3); /* … */
            *str = cx_str(a, s2, n + 3);
        } else {
            break;
        }
        cut = 1;
    }
    return cut;
}

/*
 * A tool's answer as a value of its type. A tool that returns `Text` gets
 * the text (already cut at `max_output` by the I/O layer). Any other type
 * is read from the structured content, or from the text as JSON, whole;
 * only the fields the type declares are kept, and checked; then
 * `max_output` applies to what is left (4 bytes of JSON per token). With
 * `record`, what to write to the journal: that value, not the server's
 * whole answer.
 */
static cx_value *decode_tool(cx_arena *a, cx_value *tool, cx_value *ok, cx_value **record) {
    if (record) *record = ok;
    if (cx_get_bool(tool, "returns_text", 1)) return cx_get(ok, "text");
    /* `Unit`: what the server says back is not a value of the program. */
    if (cx_get_bool(tool, "returns_unit", 0)) return cx_null(a);
    cx_value *v = cx_get(ok, "json");
    if (!v || v->kind == CX_NULL) {
        cx_value *t = cx_get(ok, "text");
        v = t ? cx_parse(a, t->u.str.s, t->u.str.len, NULL) : NULL;
    }
    cx_value *schema = cx_get(tool, "returns");
    if (!v || !schema || schema->kind != CX_REC) return v;
    v = project(a, v, schema);
    const char *why = NULL;
    if (!conforms(v, schema, &why)) return NULL;
    int cut = cx_get_bool(ok, "truncated", 0);
    cx_value *max = cx_get(tool, "max_output");
    if (max && max->kind == CX_NUM && json_size(v) > (size_t)max->u.num * 4) {
        /* A copy of its own to cut. */
        cx_buf b = {0};
        cx_write(&b, v);
        v = cx_parse(a, b.data, b.len, NULL);
        cx_buf_free(&b);
        cut |= shrink(a, &v, (size_t)max->u.num * 4);
    }
    if (record) {
        const char *keys[4] = {"json", "text", "truncated", "ms"};
        cx_value *ms = cx_get(ok, "ms");
        cx_value *vals[4] = {v, cx_null(a), cx_bool(a, cut), ms ? ms : cx_num(a, 0)};
        *record = cx_rec(a, keys, vals, 4);
    }
    return v;
}

/* `max_output` for a call: the I/O layer cuts a tool's text, but the
 * answer of a tool with a type is cut once it is kept to its fields
 * (decode_tool). */
static void put_max_output(cx_buf *b, cx_value *tool) {
    cx_buf_puts(b, ",\"max_output\":");
    if (cx_get_bool(tool, "returns_text", 1))
        cx_write(b, cx_get(tool, "max_output"));
    else
        cx_buf_puts(b, "null");
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
         * journal stays locked, so no other call is recorded after it. In
         * PostgreSQL, "reached" is "committed". */
        cx_journal_sync(x->journal);
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

/* Errors after which a `write once` call may have happened anyway: the
 * request may have reached the tool before the answer was lost. */
static int maybe_happened(const char *kind) {
    return strcmp(kind, "Timeout") == 0 || strcmp(kind, "Unavailable") == 0 ||
           strcmp(kind, "Network") == 0;
}

/* Stops the run from an I/O thread: nothing may catch it. */
static void stop_run(exec *x, job *j, const char *msg) {
    pthread_mutex_lock(&x->mu);
    fail_locked(x, j->graph, j->node, msg);
    pthread_mutex_unlock(&x->mu);
}

/*
 * `on_uncertain verify(f(a, b))`: asks the `read` tool `f`, with the same
 * arguments, whether the call happened. 1: it did; 0: it did not; -1: the
 * question failed (`why` says why). A tool that returns a list answers with
 * what the call made, found again (empty if it did not happen); the list
 * goes in `*found`.
 */
static int verify_happened(exec *x, cx_arena *a, job *j, cx_value *policy, cx_value **found,
                           char *why, size_t why_len) {
    cx_value *vtool = at(x->tools, index_of(policy, "tool"));
    cx_value *req = cx_parse(a, j->req, strlen(j->req), NULL);
    cx_value *given = cx_get(req, "args");
    cx_value *wparams = cx_get(j->spec, "params");
    cx_value *vparams = cx_get(vtool, "params");
    cx_value *vargs = cx_get(policy, "args");
    const char *vname = cx_get_str(vtool, "name", "?");
    cx_buf b = {0};
    cx_buf_puts(&b, "{\"tool\":");
    cx_buf_json_str(&b, vname, strlen(vname));
    cx_buf_puts(&b, ",\"args\":{");
    for (size_t i = 0; i < len_of(vargs); i++) {
        cx_value *pname = at(wparams, (size_t)at(vargs, i)->u.num);
        const char *from = pname ? pname->u.str.s : "";
        if (i) cx_buf_putc(&b, ',');
        cx_write(&b, at(vparams, i));
        cx_buf_putc(&b, ':');
        cx_value *v = cx_get(given, from);
        if (v)
            cx_write(&b, v);
        else
            cx_buf_puts(&b, "null");
    }
    cx_buf_printf(&b, "},\"max_output\":null,\"timeout_ms\":%.0f}",
                  cx_get_num(vtool, "timeout_ms", 30000));
    int result = -1;
    for (int attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
        cx_value *ans = io_call(a, calyx_io_tool_call, b.data);
        cx_value *ok = cx_get(ans, "ok");
        if (ok) {
            cx_value *v = decode_tool(a, vtool, ok, NULL);
            if (v && v->kind == CX_BOOL) {
                result = v->u.b != 0;
                trace(x, j->label, "read  %s  -> %s  (verifying `%s`)", vname,
                      result ? "true" : "false", cx_get_str(j->spec, "name", "?"));
            } else if (v && v->kind == CX_LIST) {
                result = len_of(v) > 0;
                *found = v;
                trace(x, j->label, "read  %s  -> %s  (verifying `%s`)", vname,
                      result ? "found" : "not found", cx_get_str(j->spec, "name", "?"));
            } else {
                snprintf(why, why_len, "`%s` did not answer true or false, or a list", vname);
            }
            break;
        }
        cx_value *err = cx_get(ans, "error");
        const char *kind = cx_get_str(err, "kind", "Unavailable");
        snprintf(why, why_len, "`%s` failed: %s: %s", vname, kind,
                 cx_get_str(err, "message", "no answer"));
        if (!is_temporary(kind)) break;
        if (attempt < MAX_ATTEMPTS) sleep_s(backoff_s(attempt, kind));
    }
    cx_buf_free(&b);
    return result;
}

enum { R_DONE, R_REDO, R_FAIL, R_STOP };

/*
 * A batch write (`batch p`) that may have happened in part: `applied` are
 * the items `verify` found. Leaves in the request only the items of `p` not
 * among them (each found item matches one requested item) and returns how
 * many are left; 0 means all were applied.
 */
static size_t batch_remaining(cx_arena *a, job *j, cx_value *applied) {
    size_t idx = (size_t)cx_get_num(j->spec, "batch", 0);
    const char *pname = at(cx_get(j->spec, "params"), idx)->u.str.s;
    cx_value *req = cx_parse(a, j->req, strlen(j->req), NULL);
    cx_value *args = cx_get(req, "args");
    cx_value *items = cx_get(args, pname);
    size_t n = len_of(items), m = len_of(applied), left = 0;
    char *used = cx_alloc(a, m + 1);
    memset(used, 0, m + 1);
    cx_value **keep = cx_alloc(a, (n ? n : 1) * sizeof *keep);
    for (size_t i = 0; i < n; i++) {
        size_t k = 0;
        while (k < m && (used[k] || !cx_equal(at(items, i), at(applied, k)))) k++;
        if (k < m)
            used[k] = 1;
        else
            keep[left++] = at(items, i);
    }
    if (left == 0 || left == n) return left;
    /* The same request, with the items left. */
    const char **akeys = cx_alloc(a, args->u.rec.len * sizeof *akeys);
    cx_value **avals = cx_alloc(a, args->u.rec.len * sizeof *avals);
    for (size_t i = 0; i < args->u.rec.len; i++) {
        akeys[i] = args->u.rec.keys[i];
        avals[i] = strcmp(akeys[i], pname) == 0 ? cx_list(a, keep, left) : args->u.rec.vals[i];
    }
    const char **rkeys = cx_alloc(a, req->u.rec.len * sizeof *rkeys);
    cx_value **rvals = cx_alloc(a, req->u.rec.len * sizeof *rvals);
    for (size_t i = 0; i < req->u.rec.len; i++) {
        rkeys[i] = req->u.rec.keys[i];
        rvals[i] = strcmp(rkeys[i], "args") == 0 ? cx_rec(a, akeys, avals, args->u.rec.len)
                                                 : req->u.rec.vals[i];
    }
    cx_buf b = {0};
    cx_write(&b, cx_rec(a, rkeys, rvals, req->u.rec.len));
    free(j->req);
    j->req = cx_buf_take(&b);
    return left;
}

/*
 * A `write once` call that may or may not have happened (decision D2):
 * done (go on without its answer), redo (it did not happen), failed (a
 * failure `try` can catch) or stop (a person decides when resuming). Uses
 * the decision given when resuming, else the tool's `on_uncertain`. `how`
 * gets a description; `why` the message for failing or stopping.
 */
static int resolve_uncertain(exec *x, cx_arena *a, job *j, const char *cause, cx_value **found,
                             char *how, size_t how_len, char *why, size_t why_len) {
    const char *name = cx_get_str(j->spec, "name", "?");
    cx_value *policy = cx_get(j->spec, "on_uncertain");
    const char *pname = cx_get_str(policy, "policy", "pause");
    int unit = cx_get_bool(j->spec, "returns_unit", 0);
    if (j->uncertain && x->decision) {
        if (strcmp(x->decision, "retry") == 0) {
            snprintf(how, how_len, "repeated: decided when resuming");
            return R_REDO;
        }
        if (strcmp(x->decision, "failed") == 0) {
            snprintf(why, why_len,
                     "tool `%s` failed: Uncertain: its outcome is unknown and it was marked "
                     "failed when resuming",
                     name);
            return R_FAIL;
        }
        if (x->decision_value) {
            /* `--uncertain done=<value>`: what the person found it made. */
            *found = x->decision_value;
            snprintf(how, how_len, "taken as done with the answer given when resuming");
            return R_DONE;
        }
        if (unit) {
            snprintf(how, how_len, "taken as done: decided when resuming");
            return R_DONE;
        }
        snprintf(why, why_len,
                 "`%s` has an answer to use: resume with `--uncertain done=<answer>` (JSON, or "
                 "the text), `--uncertain retry` or `--uncertain failed`",
                 name);
        return R_STOP;
    }
    if (strcmp(pname, "accept_loss") == 0 && unit) {
        snprintf(how, how_len, "taken as done: on_uncertain accept_loss");
        return R_DONE;
    }
    if (strcmp(pname, "verify") == 0) {
        char verr[512] = "";
        int happened = verify_happened(x, a, j, policy, found, verr, sizeof verr);
        cx_value *bp = cx_get(j->spec, "batch");
        if (happened >= 0 && bp && bp->kind == CX_NUM) {
            size_t total = len_of(cx_get(cx_get(cx_parse(a, j->req, strlen(j->req), NULL), "args"),
                                         at(cx_get(j->spec, "params"), (size_t)bp->u.num)->u.str.s));
            size_t left = batch_remaining(a, j, *found);
            *found = NULL;
            if (left == 0) {
                snprintf(how, how_len, "done: verify found all %zu items applied", total);
                return R_DONE;
            }
            snprintf(how, how_len, "repeated with the %zu of %zu items verify did not find", left,
                     total);
            return R_REDO;
        }
        /* A list: what the call made; its first item is the answer. */
        if (happened == 1 && *found && (*found)->kind == CX_LIST) *found = at(*found, 0);
        if (happened == 0) *found = NULL;
        if (happened == 1 && (unit || *found)) {
            snprintf(how, how_len, *found ? "done: verify found what it made"
                                          : "done: verify says it happened");
            return R_DONE;
        }
        if (happened == 0) {
            snprintf(how, how_len, "repeated: verify says it did not happen");
            return R_REDO;
        }
        if (happened == -1) {
            snprintf(why, why_len,
                     "`%s` (write once) may or may not have happened (%s), and verifying "
                     "failed (%s); check it, then resume with `--uncertain done`, `--uncertain "
                     "retry` or `--uncertain failed`",
                     name, cause, verr);
            return R_STOP;
        }
    }
    snprintf(why, why_len,
             "`%s` (write once) may or may not have happened (%s); on_uncertain %s: check it, "
             "then resume with `--uncertain done`, `--uncertain retry` or `--uncertain failed`",
             name, cause, pname);
    return R_STOP;
}

/* A `write once` call taken as done: recorded, so it is never made again.
 * `found` is its answer, when `verify` found what it made. */
static cx_value *taken_as_done(exec *x, cx_arena *a, job *j, cx_value *found, const char *how,
                               char *why, size_t why_len) {
    cx_buf b = {0};
    if (found) {
        cx_buf t = {0};
        if (found->kind == CX_STR)
            cx_buf_json_str(&t, found->u.str.s, found->u.str.len);
        else
            cx_write(&t, found);
        cx_buf_puts(&b, "{\"text\":");
        if (found->kind == CX_STR)
            cx_buf_puts(&b, t.data);
        else
            cx_buf_json_str(&b, t.data, t.len);
        cx_buf_puts(&b, ",\"json\":");
        cx_write(&b, found);
        cx_buf_puts(&b, ",\"ms\":0}");
        cx_buf_free(&t);
    } else {
        cx_buf_puts(&b, "{\"text\":\"null\",\"json\":null,\"ms\":0}");
    }
    cx_value *ok = cx_parse(a, b.data, b.len, NULL);
    cx_buf_free(&b);
    trace(x, j->label, "write once %s  %s", cx_get_str(j->spec, "name", "?"), how);
    if (!journal_record(x, j, "write once", ok)) {
        snprintf(why, why_len, "cannot write the journal");
        return NULL;
    }
    return found ? found : cx_null(a);
}

/*
 * Runs one call, with its retries, in an I/O thread. Returns the decoded
 * value, or NULL with the reason in `why` ("stopped" if the run stopped).
 */
static cx_value *entity_job(exec *x, cx_arena *a, job *j, char *why, size_t why_len);

/*
 * Saga (D12): `compensate f(p, q)` on the tool of `j`. Records how to undo
 * this call, `{"tool": f, "args": [the values of p and q in this call]}`,
 * in memory and in the journal (`owe:<key>`, reaching the disk with the
 * sync that comes before every write).
 */
static void owe(exec *x, cx_arena *a, job *j) {
    cx_value *comp = cx_get(j->spec, "compensate");
    if (!comp || comp->kind == CX_NULL) return;
    cx_value *req = cx_parse(a, j->req, strlen(j->req), NULL);
    cx_value *args = req ? cx_get(req, "args") : NULL;
    cx_value *params = cx_get(j->spec, "params");
    cx_value *which = cx_get(comp, "args");
    cx_buf b = {0};
    cx_buf_printf(&b, "{\"tool\":%.0f,\"args\":[", cx_get_num(comp, "tool", 0));
    for (size_t i = 0; i < len_of(which); i++) {
        size_t k = (size_t)at(which, i)->u.num;
        cx_value *pn = at(params, k);
        cx_value *v = pn && args ? cx_get(args, pn->u.str.s) : NULL;
        if (i) cx_buf_putc(&b, ',');
        if (v)
            cx_write(&b, v);
        else
            cx_buf_puts(&b, "null");
    }
    cx_buf_puts(&b, "]}");
    pthread_mutex_lock(&x->mu);
    if (x->nowes == x->owes_cap) {
        x->owes_cap = x->owes_cap ? 2 * x->owes_cap : 8;
        x->owes = realloc(x->owes, x->owes_cap * sizeof *x->owes);
        if (!x->owes) abort();
    }
    x->owes[x->nowes].key = cx_strndup(&x->arena, j->key, strlen(j->key));
    x->owes[x->nowes].json = cx_strndup(&x->arena, b.data, b.len);
    x->nowes++;
    pthread_mutex_unlock(&x->mu);
    if (x->journal && cx_journal_get_mode(x->journal) != CX_JOURNAL_REPLAY) {
        char *key = fmt(a, "owe:%s", j->key);
        char hash[65];
        cx_sha256_hex(b.data, b.len, hash);
        pthread_mutex_lock(&x->jmu);
        cx_journal_record(x->journal, key, "read", hash, b.data, b.len);
        pthread_mutex_unlock(&x->jmu);
    }
    cx_buf_free(&b);
}

static cx_value *run_job(exec *x, cx_arena *a, job *j, char *why, size_t why_len) {
    if (j->entity) return entity_job(x, a, j, why, why_len);
    const char *name = cx_get_str(j->spec, j->is_model ? "id" : "name", "?");
    const char *effect = j->is_model ? "llm" : cx_get_str(j->spec, "effect", "read");
    const char *prompt_name = j->is_model ? cx_get_str(j->prompt, "name", "?") : "";
    int write_once = strcmp(effect, "write once") == 0;
    cx_value *retry_on = j->is_model ? NULL : cx_get(j->spec, "retry_on");
    /* A `write` with an idempotency key can be repeated safely (D2). */
    cx_value *key_param = j->is_model ? NULL : cx_get(j->spec, "idempotency_key");
    int keyed = key_param && key_param->kind == CX_NUM;
    char how[256] = "";

    if (j->uncertain) {
        cx_value *found = NULL;
        switch (resolve_uncertain(x, a, j, "the run stopped while it was in progress", &found, how,
                                  sizeof how, why, why_len)) {
        case R_DONE:
            return taken_as_done(x, a, j, found, how, why, why_len);
        case R_FAIL:
            return NULL;
        case R_STOP:
            stop_run(x, j, why);
            snprintf(why, why_len, "stopped");
            return NULL;
        default:
            trace(x, j->label, "write once %s  %s", name, how);
        }
    }
    /* A write that can be undone (D12): how, before it is sent, so that a
     * branch that loses a race is undone even if the run dies meanwhile. */
    if (!j->is_model && !j->uncertain) owe(x, a, j);
    /* Before an external write, the journal reaches the disk: a `write
     * once` with its "begin" (after a crash it is uncertain, never made
     * again blindly), any write with what its arguments came from (after a
     * machine crash, a model is not asked again for a different key). */
    if (!j->is_model && x->journal && !j->uncertain &&
        cx_journal_get_mode(x->journal) != CX_JOURNAL_REPLAY &&
        (write_once || strcmp(effect, "write") == 0)) {
        pthread_mutex_lock(&x->jmu);
        int synced = write_once ? cx_journal_begin(x->journal, j->key, j->req_hash)
                                : cx_journal_sync(x->journal);
        pthread_mutex_unlock(&x->jmu);
        if (!synced) {
            snprintf(why, why_len, "cannot write the journal before calling `%s`", name);
            return NULL;
        }
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
            cx_value *record = ok;
            cx_value *v = j->raw        ? ok
                          : j->is_model ? decode_model(a, j->prompt, ok, &perr)
                                        : decode_tool(a, j->spec, ok, &record);
            if (j->is_model) {
                pthread_mutex_lock(&x->mu);
                account(x, name, ok);
                pthread_mutex_unlock(&x->mu);
                size_t ncalls = len_of(cx_get(ok, "tool_calls"));
                trace(x, j->label, "llm   %s(%s)%s%s  %.2f s  %.0f -> %.0f tokens%s%s", name,
                      prompt_name, j->note ? "  " : "", j->note ? j->note : "",
                      cx_get_num(ok, "ms", 0) / 1000.0, cx_get_num(ok, "input_tokens", 0),
                      cx_get_num(ok, "output_tokens", 0),
                      ncalls ? (ncalls == 1 ? "  -> 1 tool call" : "  -> tool calls") : "",
                      attempt > 1 ? "  (retry)" : "");
            } else {
                trace(x, j->label, "%-5s %s  %.2f s%s%s", effect, name,
                      cx_get_num(ok, "ms", 0) / 1000.0,
                      cx_get_bool(record, "truncated", 0) ? "  (output cut at max_output)" : "",
                      attempt > 1 ? "  (retry)" : "");
            }
            if (v) {
                /* Only answers that decode are recorded. */
                if (!journal_record(x, j, effect, record)) {
                    snprintf(why, why_len, "cannot write the journal");
                    return NULL;
                }
                return v;
            }
            if (!j->is_model) {
                snprintf(why, why_len,
                         "tool `%s` failed: Decode: the output is not JSON of its declared type",
                         name);
                return NULL;
            }
            /* The answer does not match the prompt's type: ask again. */
            kind = "Decode";
            snprintf(why, why_len,
                     "model `%s` with prompt `%s` failed: Decode: the answer does not have the "
                     "prompt's type (%s)",
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
            /* A missing API key or tool server: no `try` or router may hide
             * it as a failed call; the run stops until the setup is fixed. */
            if (strcmp(kind, "Config") == 0) {
                stop_run(x, j, why);
                snprintf(why, why_len, "stopped");
                return NULL;
            }
            if (write_once && maybe_happened(kind)) {
                char cause[512];
                snprintf(cause, sizeof cause, "%s: %s", kind, message);
                cx_value *found = NULL;
                switch (resolve_uncertain(x, a, j, cause, &found, how, sizeof how, why, why_len)) {
                case R_DONE:
                    return taken_as_done(x, a, j, found, how, why, why_len);
                case R_REDO:
                    trace(x, j->label, "write once %s  %s", name, how);
                    if (attempt < MAX_ATTEMPTS) continue;
                    return NULL;
                case R_STOP:
                    stop_run(x, j, why);
                    snprintf(why, why_len, "stopped");
                    return NULL;
                default:
                    return NULL;
                }
            }
            /* Models repeat temporary errors; tools what `retry_on` lists, and
             * keyed writes temporary errors too; `write once` never repeats on
             * its own (D2). */
            int retry = j->is_model ? is_temporary(kind)
                        : write_once
                            ? 0
                            : (listed(retry_on, kind) ||
                               ((keyed || strcmp(effect, "sandbox") == 0) && is_temporary(kind)));
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
        if (x->ncancelled && is_cancelled(x, j->key)) {
            /* Its branch lost a race before the call started. */
            j->p->state = P_FAILED;
            j->p->error = "cancelled: another branch won the race";
            wake(x, NULL, j->p);
            check_stuck(x);
            pthread_mutex_unlock(&x->mu);
            free(j->req);
            free(j);
            continue;
        }
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
            /* The tasks that wait decide: `try` turns it into a value. */
            j->p->state = P_FAILED;
            j->p->error = cx_strndup(a, why, strlen(why));
        }
        wake(x, NULL, j->p);
        check_stuck(x);
        int lost = x->ncancelled && is_cancelled(x, j->key);
        pthread_mutex_unlock(&x->mu);
        /* In progress when its branch lost a race: it finished, unused. */
        if (lost) trace(x, j->label, "      lost the race: the answer is not used");
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
static cx_value *request(ctx *c, const char *key, int kind, cx_value *spec, cx_value *prompt,
                         cx_buf *req, const char *note) {
    exec *x = c->x;
    int is_model = kind == CALL_MODEL || kind == CALL_CHAT;
    char hash[65];
    cx_sha256_hex(req->data, req->len, hash);
    const char *name = cx_get_str(spec, is_model ? "id" : "name", "?");
    /* For an entity, `note` says `send` or `ask`. */
    int send = kind == CALL_ENTITY && note && strcmp(note, "send") == 0;
    const char *effect = is_model                ? "llm"
                         : kind == CALL_ENTITY ? (send ? "write" : "read")
                                               : cx_get_str(spec, "effect", "read");
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
        if (p->state == P_FAILED && !c->failure) c->failure = p->error;
        pthread_mutex_unlock(&x->mu);
        cx_buf_free(req);
        return result;
    }
    int uncertain = 0;
    if (x->journal) {
        int mismatch = 0;
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
        /* An uncertain `write once` goes to an I/O thread, which applies the
         * tool's `on_uncertain` (or the decision given when resuming). */
        if (msg[0]) {
            fail_locked(x, graph, c->node, msg);
            pthread_mutex_unlock(&x->mu);
            cx_buf_free(req);
            return NULL;
        }
        if (hit) {
            const char *why = NULL;
            cx_value *v = kind == CALL_CHAT     ? hit
                          : kind == CALL_ENTITY ? cx_get(hit, "value")
                          : is_model            ? decode_model(&c->w->arena, prompt, hit, &why)
                                                : decode_tool(&c->w->arena, spec, hit, NULL);
            if (!v) {
                fail_locked(x, graph, c->node, "the journal's answer does not decode");
            } else {
                x->from_journal++;
                if (is_model) account(x, name, hit);
                if (kind == CALL_ENTITY) is_model = 0;
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
    j->raw = kind == CALL_CHAT;
    j->note = note;
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
    j->uncertain = uncertain;
    j->entity = kind == CALL_ENTITY;
    j->send = send;
    job_heap_push(&x->jobs, j);
    pthread_cond_signal(&x->io_cv);
    pthread_mutex_unlock(&x->mu);
    return PENDING;
}

static cx_value *eval(ctx *c, cx_value *e);

/* The key of a call in this node instance: `scope#id`. */
static const char *call_key(ctx *c, cx_value *e) {
    return fmt(&c->w->arena, "%s#%zu", c->scope, index_of(e, "id"));
}

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

/* A prompt's text, with each {path} replaced by its argument. */
static cx_buf prompt_text(cx_value *prompt, cx_value **args, size_t n) {
    cx_value *params = cx_get(prompt, "params");
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
    return text;
}

/* The request for one model call with a prompt's text. */
static cx_buf model_request(cx_value *model, cx_value *prompt, const cx_buf *text) {
    cx_value *schema = cx_get(prompt, "schema");
    cx_buf req = {0};
    cx_buf_puts(&req, "{\"model\":");
    cx_write(&req, cx_get(model, "id"));
    cx_buf_puts(&req, ",\"prompt\":");
    cx_buf_json_str(&req, text->data ? text->data : "", text->len);
    cx_buf_puts(&req, ",\"schema\":");
    cx_write(&req, schema && schema->kind != CX_NULL ? schema : NULL);
    cx_buf_puts(&req, ",\"max_output\":");
    cx_write(&req, cx_get(model, "max_output"));
    cx_buf_puts(&req, ",\"timeout_ms\":300000}"); /* llm: 5 min per attempt (D22) */
    return req;
}

static cx_value *call_model(ctx *c, cx_value *e) {
    exec *x = c->x;
    cx_value *model = at(x->models, index_of(e, "model"));
    cx_value *prompt = at(x->prompts, index_of(e, "prompt"));
    if (!model || !prompt) return fatalf(c, "invalid IR: unknown model or prompt");
    cx_value *args_e = cx_get(e, "args");
    size_t n = len_of(args_e);
    cx_value **args = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *args);
    EVAL_ALL(c, args_e, args);
    cx_buf text = prompt_text(prompt, args, n);
    cx_buf req = model_request(model, prompt, &text);
    cx_buf_free(&text);
    return request(c, call_key(c, e), CALL_MODEL, model, prompt, &req, NULL);
}

/*
 * The sandboxes lent to a call (D26), as `"borrows":[{"param", "mode",
 * "path"}]`: the I/O layer holds their locks during the call and keeps
 * their snapshots. `vals[i]` is the value of parameter `i`.
 */
static void put_borrows(cx_buf *req, cx_value *tool, cx_value **vals, size_t n) {
    cx_value *borrows = cx_get(tool, "borrows");
    cx_value *params = cx_get(tool, "params");
    int first = 1;
    for (size_t i = 0; i < len_of(borrows) && i < n; i++) {
        cx_value *mode = at(borrows, i);
        if (!mode || mode->kind != CX_STR || !vals[i] || vals[i]->kind != CX_STR) continue;
        cx_buf_puts(req, first ? ",\"borrows\":[" : ",");
        first = 0;
        cx_buf_puts(req, "{\"param\":");
        cx_write(req, at(params, i));
        cx_buf_puts(req, ",\"mode\":");
        cx_write(req, mode);
        cx_buf_puts(req, ",\"path\":");
        cx_write(req, vals[i]);
        cx_buf_putc(req, '}');
    }
    if (!first) cx_buf_putc(req, ']');
}

/*
 * A precondition, as the tool receives it (D29): `{"state": field}` for
 * the tool's state, `{"op", "l", "r"}` or `{"op", "v"}` for operators, and
 * `{"value": v}` for any value of the graph, computed now. 0 on failure.
 */
static int guard_json(ctx *c, cx_value *e, cx_buf *b) {
    const char *k = cx_get_str(e, "k", "");
    if (strcmp(k, "state") == 0) {
        const char *f = cx_get_str(e, "field", "");
        cx_buf_puts(b, "{\"state\":");
        cx_buf_json_str(b, f, strlen(f));
        cx_buf_putc(b, '}');
        return 1;
    }
    if (strcmp(k, "bin") == 0 || strcmp(k, "un") == 0) {
        const char *op = cx_get_str(e, "op", "");
        cx_buf_puts(b, "{\"op\":");
        cx_buf_json_str(b, op, strlen(op));
        if (strcmp(k, "bin") == 0) {
            cx_buf_puts(b, ",\"l\":");
            if (!guard_json(c, cx_get(e, "l"), b)) return 0;
            cx_buf_puts(b, ",\"r\":");
            if (!guard_json(c, cx_get(e, "r"), b)) return 0;
        } else {
            cx_buf_puts(b, ",\"v\":");
            if (!guard_json(c, cx_get(e, "v"), b)) return 0;
        }
        cx_buf_putc(b, '}');
        return 1;
    }
    cx_value *v = eval(c, e);
    if (!v || v == PENDING) return 0; /* `requires` makes no calls */
    cx_buf_puts(b, "{\"value\":");
    cx_write(b, v);
    cx_buf_putc(b, '}');
    return 1;
}

static cx_value *tool_request(ctx *c, cx_value *tool, cx_value **args, size_t n,
                              cx_value *requires, const char *key);

static cx_value *call_tool(ctx *c, cx_value *e) {
    cx_value *tool = at(c->x->tools, index_of(e, "tool"));
    if (!tool) return fatalf(c, "invalid IR: unknown tool");
    cx_value *args_e = cx_get(e, "args");
    size_t n = len_of(args_e);
    cx_value **args = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *args);
    EVAL_ALL(c, args_e, args);
    return tool_request(c, tool, args, n, cx_get(e, "requires"), call_key(c, e));
}

/* A call of `tool` with these arguments, under `key` (`requires`: the
 * expressions of the preconditions, or NULL). */
static cx_value *tool_request(ctx *c, cx_value *tool, cx_value **args, size_t n,
                              cx_value *requires, const char *key) {
    const char *name = cx_get_str(tool, "name", "?");
    cx_value *params = cx_get(tool, "params");
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
    cx_buf_putc(&req, '}');
    put_max_output(&req, tool);
    cx_buf_printf(&req, ",\"timeout_ms\":%.0f", cx_get_num(tool, "timeout_ms", 30000));
    /* The idempotency key: the value of the parameter the tool names. */
    cx_value *kp = cx_get(tool, "idempotency_key");
    if (kp && kp->kind == CX_NUM && (size_t)kp->u.num < n) {
        cx_value *kv = args[(size_t)kp->u.num];
        cx_buf_puts(&req, ",\"idempotency_key\":");
        if (kv->kind == CX_STR) {
            cx_buf_json_str(&req, kv->u.str.s, kv->u.str.len);
        } else {
            cx_buf text = {0};
            cx_write(&text, kv);
            cx_buf_json_str(&req, text.data, text.len);
            cx_buf_free(&text);
        }
    }
    put_borrows(&req, tool, args, n);
    if (len_of(requires)) {
        cx_buf_puts(&req, ",\"requires\":[");
        for (size_t i = 0; i < len_of(requires); i++) {
            if (i) cx_buf_putc(&req, ',');
            if (!guard_json(c, at(requires, i), &req)) {
                cx_buf_free(&req);
                return c->failure ? NULL : failf(c, "a precondition of `%s` has no value", name);
            }
        }
        cx_buf_putc(&req, ']');
    }
    cx_buf_putc(&req, '}');
    return request(c, key, CALL_TOOL, tool, NULL, &req, NULL);
}

/* A subgraph runs as tasks of its own; this task waits for its result. */
static cx_value *call_graph(ctx *c, cx_value *e) {
    exec *x = c->x;
    cx_value *g = at(x->graphs, index_of(e, "graph"));
    if (!g) return fatalf(c, "invalid IR: unknown graph");
    cx_value *args_e = cx_get(e, "args");
    size_t n = len_of(args_e);
    if (n != len_of(cx_get(g, "params")))
        return fatalf(c, "graph `%s` expects %zu argument(s)", cx_get_str(g, "name", "?"),
                      len_of(cx_get(g, "params")));
    cx_value **args = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *args);
    EVAL_ALL(c, args_e, args);
    /* `decreases p` (D17): the compiler makes each call to itself pass a
     * smaller `p`; one that went below 0 missed its base case. */
    cx_value *dec = cx_get(g, "decreases");
    if (dec && dec->kind == CX_NUM && (size_t)dec->u.num < n) {
        cx_value *v = args[(size_t)dec->u.num];
        if (v && v->kind == CX_NUM && v->u.num < 0)
            return failf(c, "`%s` called with `%s` = %g: below 0, the recursion missed its base case",
                         cx_get_str(g, "name", "?"),
                         at(cx_get(g, "params"), (size_t)dec->u.num)->u.str.s, v->u.num);
    }
    const char *key = call_key(c, e);
    cx_value *result = PENDING;
    pthread_mutex_lock(&x->mu);
    pending *p = ptab_get(x, key);
    if (!p) {
        if (c->g->depth + 1 >= MAX_GRAPH_DEPTH) {
            pthread_mutex_unlock(&x->mu);
            return failf(c, "graphs nested too deeply");
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
    if (p->state == P_FAILED) {
        result = NULL;
        if (!c->failure) c->failure = p->error;
    }
    pthread_mutex_unlock(&x->mu);
    return result;
}

/* ----- operators ------------------------------------------------------------ */

static cx_value *concat(cx_arena *a, cx_value *l, cx_value *r) {
    if (l->kind == CX_STR) {
        cx_buf b = {0};
        cx_buf_put(&b, l->u.str.s, l->u.str.len);
        cx_buf_put(&b, r->u.str.s, r->u.str.len);
        cx_value *v = cx_str(a, b.data, b.len);
        cx_buf_free(&b);
        return v;
    }
    size_t n = l->u.list.len + r->u.list.len;
    cx_value **items = cx_alloc(a, (n ? n : 1) * sizeof *items);
    if (l->u.list.len) memcpy(items, l->u.list.items, l->u.list.len * sizeof *items);
    if (r->u.list.len)
        memcpy(items + l->u.list.len, r->u.list.items, r->u.list.len * sizeof *items);
    return cx_list(a, items, n);
}

static int compare(const cx_value *l, const cx_value *r) {
    if (l->kind == CX_NUM) return (l->u.num > r->u.num) - (l->u.num < r->u.num);
    return strcmp(l->u.str.s, r->u.str.s);
}

static cx_value *binary(ctx *c, cx_value *e) {
    const char *op = cx_get_str(e, "op", "");
    cx_arena *a = &c->w->arena;
    cx_value *l = eval(c, cx_get(e, "l"));
    if (!l || l == PENDING) return l;
    /* `and`/`or` stop early: the right side does not run (D32). */
    if (strcmp(op, "and") == 0 && l->kind == CX_BOOL && !l->u.b) return l;
    if (strcmp(op, "or") == 0 && l->kind == CX_BOOL && l->u.b) return l;
    cx_value *r = eval(c, cx_get(e, "r"));
    if (!r || r == PENDING) return r;
    if (strcmp(op, "and") == 0 || strcmp(op, "or") == 0) return r;
    if (strcmp(op, "in") == 0) {
        if (r->kind == CX_LIST) {
            for (size_t i = 0; i < r->u.list.len; i++)
                if (cx_equal(l, r->u.list.items[i])) return cx_bool(a, 1);
            return cx_bool(a, 0);
        }
        if (r->kind == CX_STR && l->kind == CX_STR) return cx_bool(a, strstr(r->u.str.s, l->u.str.s) != NULL);
        return failf(c, "`in` needs a list or a text on the right");
    }
    if (strcmp(op, "==") == 0) return cx_bool(a, cx_equal(l, r));
    if (strcmp(op, "!=") == 0) return cx_bool(a, !cx_equal(l, r));
    int same_kind = l->kind == r->kind && (l->kind == CX_NUM || l->kind == CX_STR);
    if (op[0] == '<' || op[0] == '>') {
        if (!same_kind) return failf(c, "cannot compare these values with `%s`", op);
        int k = compare(l, r);
        int res = strcmp(op, "<") == 0    ? k < 0
                  : strcmp(op, "<=") == 0 ? k <= 0
                  : strcmp(op, ">") == 0  ? k > 0
                                          : k >= 0;
        return cx_bool(a, res);
    }
    if (strcmp(op, "+") == 0 && l->kind == r->kind && (l->kind == CX_STR || l->kind == CX_LIST))
        return concat(a, l, r);
    if (l->kind != CX_NUM || r->kind != CX_NUM)
        return failf(c, "operator `%s` needs numbers", op);
    double x = l->u.num, y = r->u.num;
    switch (op[0]) {
    case '+': return cx_num(a, x + y);
    case '-': return cx_num(a, x - y);
    case '*': return cx_num(a, x * y);
    default:
        if (y == 0) return failf(c, "division by zero");
        return cx_num(a, x / y);
    }
}

/* ----- the pure layer (D27) -------------------------------------------------- */

/* A `def` applied to `n` values: its own local slots, parameters first. */
static cx_value *apply_def(ctx *c, cx_value *def, cx_value **args, size_t n) {
    size_t nl = (size_t)cx_get_num(def, "nlocals", 0);
    if (nl < n) nl = n;
    cx_value **locals = cx_alloc(&c->w->arena, (nl ? nl : 1) * sizeof *locals);
    for (size_t i = 0; i < nl; i++) locals[i] = i < n ? args[i] : NULL;
    cx_value **outer = c->locals;
    cx_value *outer_state = c->estate;
    c->locals = locals;
    c->estate = NULL;
    cx_value *v = eval(c, cx_get(def, "body"));
    c->locals = outer;
    c->estate = outer_state;
    return v;
}

/* A call to a `def`. */
static cx_value *call_def(ctx *c, cx_value *e) {
    cx_value *def = at(c->x->defs, index_of(e, "def"));
    if (!def) return fatalf(c, "invalid IR: unknown def");
    cx_value *args_e = cx_get(e, "args");
    size_t n = len_of(args_e);
    cx_value **args = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *args);
    for (size_t i = 0; i < n; i++) {
        args[i] = eval(c, at(args_e, i));
        if (!args[i] || args[i] == PENDING) return args[i];
    }
    return apply_def(c, def, args, n);
}

/* ----- routers (D30) -------------------------------------------------------- */

/*
 * `router(prompt(args))` with `policy cheapest_that_passes(check)`: the
 * router's models one at a time, cheapest first, each keyed `scope#id.i`,
 * until an answer passes `check` (a `def`). A model whose call fails (or
 * whose answer does not fit the prompt's type) is passed over like an
 * answer that fails the check. The choice goes to the journal (`scope#id`).
 * With no answer passing, the call fails (`try` catches it).
 */
static cx_value *call_route(ctx *c, cx_value *e) {
    exec *x = c->x;
    cx_value *router = at(x->routers, index_of(e, "router"));
    cx_value *prompt = at(x->prompts, index_of(e, "prompt"));
    if (!router || !prompt) return fatalf(c, "invalid IR: unknown router or prompt");
    cx_value *check = at(x->defs, index_of(router, "check"));
    cx_value *models = cx_get(router, "models");
    const char *rname = cx_get_str(router, "name", "?");
    const char *key = call_key(c, e);
    pthread_mutex_lock(&x->mu);
    pending *p = ptab_get(x, key);
    pthread_mutex_unlock(&x->mu);
    if (p && p->state == P_DONE) return p->value;
    if (x->journal) {
        int mismatch = 0;
        char hash[65];
        cx_sha256_hex(key, strlen(key), hash);
        pthread_mutex_lock(&x->jmu);
        cx_value *hit = cx_journal_lookup(x->journal, key, hash, &mismatch);
        pthread_mutex_unlock(&x->jmu);
        if (hit) {
            cx_value *v = cx_get(hit, "value");
            pthread_mutex_lock(&x->mu);
            x->from_journal++;
            new_pending(x, &c->w->arena, key, P_DONE, v);
            pthread_mutex_unlock(&x->mu);
            trace(x, c->label, "route %s  -> %s  from the journal", rname,
                  cx_get_str(hit, "model", "?"));
            return v;
        }
    }
    if (!check) return fatalf(c, "invalid IR: router `%s` has no check", rname);
    cx_value *args_e = cx_get(e, "args");
    size_t n = len_of(args_e);
    cx_value **args = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *args);
    EVAL_ALL(c, args_e, args);
    cx_buf text = prompt_text(prompt, args, n);
    const char *outer_failure = c->failure;
    for (size_t i = 0; i < len_of(models); i++) {
        cx_value *model = at(x->models, (size_t)at(models, i)->u.num);
        if (!model) {
            cx_buf_free(&text);
            return fatalf(c, "invalid IR: unknown model in router `%s`", rname);
        }
        cx_buf req = model_request(model, prompt, &text);
        c->failure = NULL;
        cx_value *v = request(c, fmt(&c->w->arena, "%s.%zu", key, i), CALL_MODEL, model, prompt,
                              &req, NULL);
        c->failure = outer_failure;
        if (v == PENDING) {
            cx_buf_free(&text);
            return PENDING;
        }
        if (!v) {
            pthread_mutex_lock(&x->mu);
            int stopping = x->stopping;
            pthread_mutex_unlock(&x->mu);
            if (stopping) {
                cx_buf_free(&text);
                return NULL;
            }
            continue; /* this model failed: try the next */
        }
        cx_value *ok = apply_def(c, check, &v, 1);
        if (!ok || ok == PENDING) {
            cx_buf_free(&text);
            return ok;
        }
        if (ok->kind != CX_BOOL || !ok->u.b) continue;
        cx_buf_free(&text);
        const char *mid = cx_get_str(model, "id", "?");
        const char *keys[2] = {"model", "value"};
        cx_value *vals[2] = {cx_cstr(&c->w->arena, mid), v};
        job j;
        memset(&j, 0, sizeof j);
        j.key = key;
        cx_sha256_hex(key, strlen(key), j.req_hash);
        if (!journal_record(x, &j, "read", cx_rec(&c->w->arena, keys, vals, 2)))
            return fatalf(c, "cannot write the journal");
        pthread_mutex_lock(&x->mu);
        new_pending(x, &c->w->arena, key, P_DONE, v);
        pthread_mutex_unlock(&x->mu);
        trace(x, c->label, "route %s  -> %s%s", rname, mid,
              i ? "  (the cheaper answers did not pass)" : "");
        return v;
    }
    cx_buf_free(&text);
    return failf(c, "router `%s`: no model's answer passed `%s`", rname,
                 cx_get_str(check, "name", "?"));
}

/* `[body for x in over if cond]` */
static cx_value *comprehension(ctx *c, cx_value *e) {
    cx_value *over = eval(c, cx_get(e, "over"));
    if (!over || over == PENDING) return over;
    if (over->kind != CX_LIST) return failf(c, "`for` over a value that is not a list");
    size_t slot = index_of(e, "slot");
    cx_value *cond_e = cx_get(e, "cond");
    cx_value **items = cx_alloc(&c->w->arena, (over->u.list.len ? over->u.list.len : 1) * sizeof *items);
    size_t n = 0;
    for (size_t i = 0; i < over->u.list.len; i++) {
        c->locals[slot] = over->u.list.items[i];
        if (cond_e && cond_e->kind != CX_NULL) {
            cx_value *ok = eval(c, cond_e);
            if (!ok || ok == PENDING) return ok;
            if (ok->kind != CX_BOOL || !ok->u.b) continue;
        }
        cx_value *v = eval(c, cx_get(e, "body"));
        if (!v || v == PENDING) return v;
        items[n++] = v;
    }
    return cx_list(&c->w->arena, items, n);
}

/* Lower or upper case: ASCII, and the accented Latin letters of UTF-8
 * (`é` / `É`: 0xC3 followed by 0xA0-0xBE / 0x80-0x9E, except × and ÷). */
static cx_value *text_case(cx_arena *a, const cx_value *t, int upper) {
    size_t n = t->u.str.len;
    const unsigned char *in = (const unsigned char *)t->u.str.s;
    char *s = cx_alloc(a, n + 1);
    for (size_t i = 0; i < n; i++) {
        unsigned char ch = in[i];
        if (i > 0 && in[i - 1] == 0xC3) {
            if (upper && ch >= 0xA0 && ch <= 0xBE && ch != 0xB7) ch -= 0x20;
            else if (!upper && ch >= 0x80 && ch <= 0x9E && ch != 0x97) ch += 0x20;
        } else if (ch < 0x80) {
            ch = (unsigned char)(upper ? toupper(ch) : tolower(ch));
        }
        s[i] = (char)ch;
    }
    s[n] = '\0';
    return cx_str(a, s, n);
}

static int is_space(int ch) {
    return ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r';
}

/* `len`, `take`, `sum`, `join`, `lower`, `upper`, `trim`. */
static cx_value *builtin(ctx *c, cx_value *e) {
    cx_arena *a = &c->w->arena;
    const char *name = cx_get_str(e, "name", "");
    cx_value *args_e = cx_get(e, "args");
    size_t n = len_of(args_e);
    cx_value **v = cx_alloc(a, (n ? n : 1) * sizeof *v);
    EVAL_ALL(c, args_e, v);
    cx_value *x = n > 0 ? v[0] : NULL;
    if (!x) return failf(c, "`%s` needs an argument", name);
    if (strcmp(name, "len") == 0) {
        if (x->kind == CX_LIST) return cx_num(a, (double)x->u.list.len);
        if (x->kind == CX_STR) {
            /* Characters, not bytes: count what does not continue UTF-8. */
            size_t chars = 0;
            for (size_t i = 0; i < x->u.str.len; i++)
                if (((unsigned char)x->u.str.s[i] & 0xC0) != 0x80) chars++;
            return cx_num(a, (double)chars);
        }
    }
    if (strcmp(name, "take") == 0 && x->kind == CX_LIST && n == 2 && v[1]->kind == CX_NUM) {
        double k = v[1]->u.num;
        size_t m = k <= 0 ? 0 : k >= (double)x->u.list.len ? x->u.list.len : (size_t)k;
        return cx_list(a, x->u.list.items, m);
    }
    if (strcmp(name, "sum") == 0 && x->kind == CX_LIST) {
        double s = 0;
        for (size_t i = 0; i < x->u.list.len; i++)
            if (x->u.list.items[i]->kind == CX_NUM) s += x->u.list.items[i]->u.num;
        return cx_num(a, s);
    }
    if (strcmp(name, "join") == 0 && x->kind == CX_LIST && n == 2 && v[1]->kind == CX_STR) {
        cx_buf b = {0};
        for (size_t i = 0; i < x->u.list.len; i++) {
            if (i) cx_buf_put(&b, v[1]->u.str.s, v[1]->u.str.len);
            cx_value *it = x->u.list.items[i];
            if (it->kind == CX_STR) cx_buf_put(&b, it->u.str.s, it->u.str.len);
        }
        cx_value *r = cx_str(a, b.data ? b.data : "", b.len);
        cx_buf_free(&b);
        return r;
    }
    if (x->kind == CX_STR) {
        if (strcmp(name, "lower") == 0) return text_case(a, x, 0);
        if (strcmp(name, "upper") == 0) return text_case(a, x, 1);
        if (strcmp(name, "trim") == 0) {
            size_t s = 0, t = x->u.str.len;
            while (s < t && is_space((unsigned char)x->u.str.s[s])) s++;
            while (t > s && is_space((unsigned char)x->u.str.s[t - 1])) t--;
            return cx_str(a, x->u.str.s + s, t - s);
        }
    }
    return failf(c, "`%s` cannot take these values", name);
}

/* ----- choices and loops ----------------------------------------------------- */

static const char *variant_name(const cx_value *v) {
    if (!v) return NULL;
    if (v->kind == CX_STR) return v->u.str.s;
    return cx_get_str(v, "kind", NULL);
}

/* The case that matches `v`, with its fields bound; NULL if none. */
static cx_value *pick_case(ctx *c, cx_value *e, cx_value *v) {
    const char *name = variant_name(v);
    cx_value *cases = cx_get(e, "cases");
    for (size_t i = 0; i < len_of(cases); i++) {
        cx_value *cs = at(cases, i);
        cx_value *variant = cx_get(cs, "variant");
        if (variant && variant->kind == CX_STR && (!name || strcmp(variant->u.str.s, name) != 0))
            continue;
        cx_value *binds = cx_get(cs, "binds");
        for (size_t k = 0; k < len_of(binds); k++) {
            cx_value *b = at(binds, k);
            size_t slot = (size_t)at(b, 1)->u.num;
            cx_value *field = cx_get(v, at(b, 0)->u.str.s);
            c->locals[slot] = field ? field : cx_null(&c->w->arena);
        }
        return cs;
    }
    failf(c, "no `case` for `%s`", name ? name : "?");
    return NULL;
}

enum { TAIL_NONE, TAIL_DONE, TAIL_NEXT };

/* A loop body: its value and whether it said `done` or `next`. */
static cx_value *eval_tail(ctx *c, cx_value *e, int *sig) {
    const char *k = cx_get_str(e, "k", "");
    if (strcmp(k, "done") == 0 || strcmp(k, "next") == 0) {
        *sig = k[0] == 'd' ? TAIL_DONE : TAIL_NEXT;
        return eval(c, cx_get(e, "v"));
    }
    if (strcmp(k, "match") == 0) {
        cx_value *v = eval(c, cx_get(e, "v"));
        if (!v || v == PENDING) return v;
        cx_value *cs = pick_case(c, e, v);
        return cs ? eval_tail(c, cx_get(cs, "body"), sig) : NULL;
    }
    if (strcmp(k, "if") == 0) {
        cx_value *cond = eval(c, cx_get(e, "c"));
        if (!cond || cond == PENDING) return cond;
        return eval_tail(c, cx_get(e, cond->kind == CX_BOOL && cond->u.b ? "t" : "e"), sig);
    }
    /* The steps of a turn, before what it gives. */
    if (strcmp(k, "let") == 0) {
        cx_value *v = eval(c, cx_get(e, "v"));
        if (!v || v == PENDING) return v;
        c->locals[index_of(e, "slot")] = v;
        return eval_tail(c, cx_get(e, "body"), sig);
    }
    return fatalf(c, "invalid IR: a loop body without `done` or `next`");
}

/* Each turn's calls are keyed by the turn: `scope#loop.turn`. */
static cx_value *eval_loop(ctx *c, cx_value *e) {
    cx_value *v = eval(c, cx_get(e, "init"));
    if (!v || v == PENDING) return v;
    size_t slot = index_of(e, "slot");
    size_t id = index_of(e, "id");
    double max = cx_get_num(e, "max", 1);
    const char *outer = c->scope;
    c->locals[slot] = v;
    for (long turn = 0; turn < (long)max; turn++) {
        c->scope = fmt(&c->w->arena, "%s#%zu.%ld", outer, id, turn);
        int sig = TAIL_NONE;
        cx_value *r = eval_tail(c, cx_get(e, "body"), &sig);
        c->scope = outer;
        if (!r || r == PENDING) return r;
        if (sig == TAIL_DONE) return r;
        c->locals[slot] = r;
    }
    cx_value *on_limit = cx_get(e, "on_limit");
    if (on_limit && on_limit->kind == CX_STR) return failf(c, "%s", on_limit->u.str.s);
    return c->locals[slot];
}

static cx_value *tagged(cx_arena *a, const char *kind, const char *field, cx_value *v) {
    const char *keys[2] = {"kind", field};
    cx_value *vals[2] = {cx_cstr(a, kind), v};
    return cx_rec(a, keys, vals, 2);
}

/* `try`: a failure inside becomes `Failed(error)`; a value, `Ok(value)`. */
static cx_value *eval_try(ctx *c, cx_value *e) {
    const char *outer = c->failure;
    c->failure = NULL;
    cx_value *v = eval(c, cx_get(e, "v"));
    const char *inner = c->failure;
    c->failure = outer;
    if (v == PENDING) return v;
    if (v) return tagged(&c->w->arena, "Ok", "value", v);
    /* A failure that stopped the run (a broken journal) is not caught. */
    pthread_mutex_lock(&c->x->mu);
    int stopping = c->x->stopping;
    pthread_mutex_unlock(&c->x->mu);
    if (stopping) return NULL;
    return tagged(&c->w->arena, "Failed", "error", cx_cstr(&c->w->arena, inner ? inner : "failed"));
}

/* ----- entities (D15) ----------------------------------------------------- */

/* `ask Entity(key).Handler(args)` / `send ...`: like other effects, the
 * answer comes from the journal if the call finished, else an I/O thread
 * does it. The request carries the computed key and arguments. */
static cx_value *call_message(ctx *c, cx_value *e) {
    exec *x = c->x;
    cx_value *entity = at(x->entities, index_of(e, "entity"));
    cx_value *handler = at(cx_get(entity, "handlers"), index_of(e, "handler"));
    if (!entity || !handler) return fatalf(c, "invalid IR: unknown entity or message");
    cx_value *args_e = cx_get(e, "args");
    size_t n = len_of(args_e);
    cx_value **args = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *args);
    EVAL_ALL(c, args_e, args);
    cx_buf req = {0};
    const char *ename = cx_get_str(entity, "name", "?");
    const char *hname = cx_get_str(handler, "name", "?");
    cx_buf_puts(&req, "{\"entity\":");
    cx_buf_json_str(&req, ename, strlen(ename));
    cx_buf_puts(&req, ",\"handler\":");
    cx_buf_json_str(&req, hname, strlen(hname));
    cx_buf_puts(&req, ",\"key\":");
    cx_write(&req, args[0]);
    cx_buf_puts(&req, ",\"args\":[");
    for (size_t i = 1; i < n; i++) {
        if (i > 1) cx_buf_putc(&req, ',');
        cx_write(&req, args[i]);
    }
    cx_buf_puts(&req, "]}");
    int send = strcmp(cx_get_str(e, "k", ""), "send") == 0;
    return request(c, call_key(c, e), CALL_ENTITY, entity, handler, &req, send ? "send" : "ask");
}

/* Reads a whole file into the arena, NUL-terminated; NULL if it is missing. */
static char *slurp(cx_arena *a, const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    cx_buf b = {0};
    char chunk[4096];
    size_t got;
    while ((got = fread(chunk, 1, sizeof chunk, f)) > 0) cx_buf_put(&b, chunk, got);
    fclose(f);
    char *s = cx_alloc(a, b.len + 1);
    if (b.len) memcpy(s, b.data, b.len);
    s[b.len] = '\0';
    *len = b.len;
    cx_buf_free(&b);
    return s;
}

/* Writes `data` to `path` atomically and durably. 0 on success. */
static int write_durable(const char *path, const char *data, size_t len) {
    char tmp[4200];
    snprintf(tmp, sizeof tmp, "%s.tmp", path);
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    int ok = write(fd, data, len) == (ssize_t)len && fsync(fd) == 0;
    ok = close(fd) == 0 && ok;
    return ok && rename(tmp, path) == 0 ? 0 : -1;
}

/* With the journal in PostgreSQL (pg.rs), entities live there too. */
int calyx_pg_enabled(void);
int calyx_pg_entity(const char *entity, const char *khash, const char *key, int exclusive,
                    char *(*step)(void *ud, const char *doc), void *ud);
int calyx_pg_file_append(const char *run, const char *name, const char *line, size_t len);
char *calyx_pg_file_read(const char *run, const char *name);
void calyx_string_free(char *s);

/* One message to one entity, wherever its document is kept. */
typedef struct {
    exec *x;
    cx_arena *a;
    job *j;
    char *why;
    size_t why_len;
    cx_value *entity, *handler, *key, *args;
    const char *ename, *hname, *kjson;
    cx_value *result; /* set when the message was answered or applied */
} emsg;

/*
 * Runs a message's handler on the entity's document (`{state, applied}`,
 * NULL for a new entity). Returns the new document, from malloc, when a
 * `send` changed it; NULL otherwise. Sets `m->result`, or `m->why`.
 */
static char *entity_step(emsg *m, const char *text, size_t len) {
    exec *x = m->x;
    job *j = m->j;
    const char *ename = m->ename, *hname = m->hname;
    char *out = NULL;
    /* The handler runs with its own context: the key and the message's
     * arguments in local slots, the state for `state` expressions. */
    worker w;
    memset(&w, 0, sizeof w);
    w.x = x;
    static gexec none;
    ctx hc;
    memset(&hc, 0, sizeof hc);
    hc.x = x;
    hc.w = &w;
    hc.g = &none;
    hc.node = j->node;
    size_t nl = (size_t)cx_get_num(m->handler, "nlocals", 1);
    hc.locals = cx_alloc(&w.arena, (nl ? nl : 1) * sizeof *hc.locals);
    hc.locals[0] = m->key;
    for (size_t i = 0; i < len_of(m->args) && i + 1 < nl; i++) hc.locals[i + 1] = at(m->args, i);

    cx_value *doc = text ? cx_parse(&w.arena, text, len, NULL) : NULL;
    cx_value *state = cx_get(doc, "state");
    cx_value *applied = cx_get(doc, "applied");
    cx_value *fields = cx_get(m->entity, "state");
    if (!state) { /* a new entity: the initial values */
        size_t nf = len_of(fields);
        const char **keys = cx_alloc(&w.arena, (nf ? nf : 1) * sizeof *keys);
        cx_value **vals = cx_alloc(&w.arena, (nf ? nf : 1) * sizeof *vals);
        for (size_t i = 0; i < nf; i++) {
            keys[i] = cx_get_str(at(fields, i), "name", "");
            vals[i] = eval(&hc, cx_get(at(fields, i), "init"));
            if (!vals[i]) vals[i] = cx_null(&w.arena);
        }
        state = cx_rec(&w.arena, keys, vals, nf);
    }
    hc.estate = state;
    if (!j->send) {
        cx_value *v = eval(&hc, cx_get(m->handler, "answer"));
        if (!v) {
            snprintf(m->why, m->why_len, "entity `%s`, message `%s`: %s", ename, hname,
                     hc.failure ? hc.failure : "failed");
        } else {
            cx_buf vb = {0};
            cx_write(&vb, v);
            m->result = cx_parse(m->a, vb.data, vb.len, NULL);
            cx_buf_free(&vb);
        }
        trace(x, j->label, "ask   %s(%s).%s", ename, m->kjson, hname);
    } else {
        /* The message's id: this run and the call's place in it. */
        char id[1024];
        snprintf(id, sizeof id, "%s %s", x->run_id, j->key);
        int seen = 0;
        for (size_t i = 0; i < len_of(applied) && !seen; i++)
            seen = at(applied, i)->kind == CX_STR && strcmp(at(applied, i)->u.str.s, id) == 0;
        if (seen) {
            trace(x, j->label, "send  %s(%s).%s  already applied", ename, m->kjson, hname);
            m->result = cx_null(m->a);
        } else {
            /* Every update sees the state before the message. */
            size_t nf = state->kind == CX_REC ? state->u.rec.len : 0;
            const char **keys = cx_alloc(&w.arena, (nf ? nf : 1) * sizeof *keys);
            cx_value **vals = cx_alloc(&w.arena, (nf ? nf : 1) * sizeof *vals);
            for (size_t i = 0; i < nf; i++) {
                keys[i] = state->u.rec.keys[i];
                vals[i] = state->u.rec.vals[i];
            }
            cx_value *updates = cx_get(m->handler, "updates");
            int failed = 0;
            for (size_t u = 0; u < len_of(updates) && !failed; u++) {
                const char *f = cx_get_str(at(updates, u), "field", "");
                cx_value *v = eval(&hc, cx_get(at(updates, u), "v"));
                if (!v) {
                    failed = 1;
                    break;
                }
                for (size_t i = 0; i < nf; i++)
                    if (strcmp(keys[i], f) == 0) vals[i] = v;
            }
            if (failed) {
                snprintf(m->why, m->why_len, "entity `%s`, message `%s`: %s", ename, hname,
                         hc.failure ? hc.failure : "failed");
            } else {
                cx_buf db = {0};
                cx_buf_puts(&db, "{\"state\":");
                cx_write(&db, cx_rec(&w.arena, keys, vals, nf));
                cx_buf_puts(&db, ",\"applied\":[");
                for (size_t i = 0; i < len_of(applied); i++) {
                    cx_write(&db, at(applied, i));
                    cx_buf_putc(&db, ',');
                }
                cx_buf_json_str(&db, id, strlen(id));
                cx_buf_puts(&db, "]}");
                out = malloc(db.len + 1);
                if (out) {
                    memcpy(out, db.data, db.len);
                    out[db.len] = '\0';
                    m->result = cx_null(m->a);
                } else {
                    snprintf(m->why, m->why_len, "entity `%s`: out of memory", ename);
                }
                cx_buf_free(&db);
            }
            trace(x, j->label, "send  %s(%s).%s", ename, m->kjson, hname);
        }
    }
    cx_arena_free(&w.arena);
    return out;
}

static char *pg_entity_step(void *ud, const char *doc) {
    return entity_step(ud, doc, doc ? strlen(doc) : 0);
}

/*
 * One message to an entity, in an I/O thread. The entity's document holds
 * its state and the ids of the messages applied to it, replaced together,
 * so a message is applied exactly once even if a run that sent it is
 * resumed and sends it again. One owner per key: shared for `ask`,
 * exclusive for `send`.
 *
 * On one machine the entity lives in
 * `.calyx/entities/<Entity>/<first 16 hex digits of SHA-256(key)>/`
 * (`entity.json`, replaced atomically; `flock` on `lock`). With the journal
 * in PostgreSQL it is a row of `calyx_entities`, locked by the message's
 * transaction (`FOR UPDATE` / `FOR SHARE`), for every machine.
 */
static cx_value *entity_job(exec *x, cx_arena *a, job *j, char *why, size_t why_len) {
    cx_value *req = cx_parse(a, j->req, strlen(j->req), NULL);
    emsg m = {.x = x, .a = a, .j = j, .why = why, .why_len = why_len};
    m.entity = j->spec;
    m.handler = j->prompt;
    m.ename = cx_get_str(m.entity, "name", "?");
    m.hname = cx_get_str(m.handler, "name", "?");
    m.key = cx_get(req, "key");
    m.args = cx_get(req, "args");
    const char *ename = m.ename;
    cx_buf kb = {0};
    cx_write(&kb, m.key);
    m.kjson = kb.data;
    char khash[65];
    cx_sha256_hex(kb.data, kb.len, khash);
    khash[16] = '\0';
    int lockfd = -1;
    if (calyx_pg_enabled()) {
        if (!calyx_pg_entity(ename, khash, kb.data, j->send, pg_entity_step, &m)) {
            /* Rolled back: nothing was applied. */
            m.result = NULL;
            if (!*why) snprintf(why, why_len, "entity `%s`: the database failed", ename);
        }
    } else {
        char dir[2048], path[2200];
        snprintf(dir, sizeof dir, ".calyx/entities/%s/%s", ename, khash);
        if (cx_mkdirs(dir) != 0) {
            snprintf(why, why_len, "entity `%s`: cannot create %.300s", ename, dir);
            goto out;
        }
        snprintf(path, sizeof path, "%s/key.json", dir);
        if (access(path, F_OK) != 0) write_durable(path, kb.data, kb.len);
        snprintf(path, sizeof path, "%s/lock", dir);
        lockfd = open(path, O_RDWR | O_CREAT, 0644);
        if (lockfd < 0 || flock(lockfd, j->send ? LOCK_EX : LOCK_SH) != 0) {
            snprintf(why, why_len, "entity `%s`: cannot lock %.300s", ename, path);
            goto out;
        }
        size_t len = 0;
        snprintf(path, sizeof path, "%s/entity.json", dir);
        char *text = slurp(a, path, &len);
        char *doc = entity_step(&m, text, len);
        if (doc && write_durable(path, doc, strlen(doc)) != 0) {
            snprintf(why, why_len, "entity `%s`: cannot write %.300s", ename, path);
            m.result = NULL;
        }
        free(doc);
    }
    /* For tests: `kill -9` after the entity applied the message and before
     * the journal records it (the window exactly-once by message id is
     * for). */
    if (m.result && j->send && getenv("CALYX_CRASH_IN_SEND")) {
        fprintf(stderr, "calyx: CALYX_CRASH_IN_SEND, exiting abruptly\n");
        _exit(137);
    }
out:
    if (lockfd >= 0) {
        flock(lockfd, LOCK_UN);
        close(lockfd);
    }
    cx_value *result = m.result;
    cx_buf_free(&kb);
    if (result) {
        const char *keys[1] = {"value"};
        cx_value *vals[1] = {result};
        if (!journal_record(x, j, j->send ? "write" : "read", cx_rec(a, keys, vals, 1))) {
            snprintf(why, why_len, "cannot write the journal");
            return NULL;
        }
    }
    return result;
}

/* ----- receive (D21) -------------------------------------------------------- */

/* One of the run's files (`waits.jsonl`, `inbox.jsonl`): in its directory,
 * or with the journal in PostgreSQL, in `calyx_files`, for every machine. */
static char *run_file(ctx *c, const char *file, size_t *len) {
    if (calyx_pg_enabled()) {
        char *t = calyx_pg_file_read(c->x->run_id, file);
        if (!t) return NULL;
        *len = strlen(t);
        char *s = cx_alloc(&c->w->arena, *len + 1);
        memcpy(s, t, *len + 1);
        calyx_string_free(t);
        return s;
    }
    char path[2200];
    snprintf(path, sizeof path, "%s/%s", c->x->run_dir, file);
    return slurp(&c->w->arena, path, len);
}

/* The `value` of the line of `file` (one of the run's) whose `key` is
 * `key`, or NULL. */
static cx_value *keyed_line(ctx *c, const char *file, const char *key, const char *field) {
    size_t len = 0;
    char *text = run_file(c, file, &len);
    cx_value *found = NULL;
    for (char *line = text; line && *line;) {
        char *end = strchr(line, '\n');
        size_t l = end ? (size_t)(end - line) : strlen(line);
        cx_value *v = cx_parse(&c->w->arena, line, l, NULL);
        if (v && strcmp(cx_get_str(v, "key", ""), key) == 0) found = cx_get(v, field);
        line = end ? end + 1 : line + l;
    }
    return found;
}

/* Appends a line to one of the run's files. One write per line: receives
 * running in parallel append to the same file, and two writes (line, then
 * newline) could interleave with theirs. */
static int append_line(exec *x, const char *file, const char *data, size_t len) {
    if (calyx_pg_enabled()) return calyx_pg_file_append(x->run_id, file, data, len) ? 0 : -1;
    char path[2200];
    snprintf(path, sizeof path, "%s/%s", x->run_dir, file);
    char *line = malloc(len + 1);
    if (!line) return -1;
    memcpy(line, data, len);
    line[len] = '\n';
    int fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0644);
    int ok = fd >= 0 && write(fd, line, len + 1) == (ssize_t)(len + 1) && fsync(fd) == 0;
    free(line);
    if (fd < 0) return -1;
    return close(fd) == 0 && ok ? 0 : -1;
}

/* Records the value a `receive` got (a message, or its timeout value). */
static cx_value *received(ctx *c, const char *key, cx_value *v, int timed_out) {
    exec *x = c->x;
    const char *keys[2] = {"value", "timed_out"};
    cx_value *vals[2] = {v, cx_bool(&c->w->arena, timed_out)};
    job j;
    memset(&j, 0, sizeof j);
    j.key = key;
    cx_sha256_hex(key, strlen(key), j.req_hash);
    if (!journal_record(x, &j, "read", cx_rec(&c->w->arena, keys, vals, 2)))
        return fatalf(c, "cannot write the journal");
    pthread_mutex_lock(&x->mu);
    new_pending(x, &c->w->arena, fmt(&c->w->arena, "%s", key), P_DONE, v);
    pthread_mutex_unlock(&x->mu);
    return v;
}

/*
 * `receive M, timeout T:` + `on timeout: v`. A message delivered to this
 * receive (`calyx deliver`, into `inbox.jsonl`) is its value. Otherwise,
 * the first time the run gets here the deadline is written down
 * (`waits.jsonl`: now + T), so it holds across stops and resumes; once it
 * passes, the value is `v`. Until then the receive waits: when nothing
 * else can run, the run stops, to be resumed later (`calyx resume`,
 * `calyx tick`). What it got goes to the journal like any call.
 */
static cx_value *call_receive(ctx *c, cx_value *e) {
    exec *x = c->x;
    const char *key = call_key(c, e);
    const char *msg = cx_get_str(e, "message", "?");
    pthread_mutex_lock(&x->mu);
    pending *p = ptab_get(x, key);
    pthread_mutex_unlock(&x->mu);
    if (p && p->state == P_DONE) return p->value;
    if (x->journal) {
        int mismatch = 0;
        char hash[65];
        cx_sha256_hex(key, strlen(key), hash);
        pthread_mutex_lock(&x->jmu);
        cx_value *hit = cx_journal_lookup(x->journal, key, hash, &mismatch);
        int replay = cx_journal_get_mode(x->journal) == CX_JOURNAL_REPLAY;
        pthread_mutex_unlock(&x->jmu);
        if (hit) {
            cx_value *v = cx_get(hit, "value");
            pthread_mutex_lock(&x->mu);
            x->from_journal++;
            new_pending(x, &c->w->arena, key, P_DONE, v);
            pthread_mutex_unlock(&x->mu);
            trace(x, c->label, "recv  %s  from the journal", msg);
            return v;
        }
        if (replay) return fatalf(c, "replay: `receive %s` is not in the journal", msg);
    }
    if (!x->run_dir) return failf(c, "`receive %s` needs a journal (run without --no-journal)", msg);
    /* `about`: what the message is about. The wait starts once it exists,
     * so the deadline does not run while, say, the proposal is written. */
    cx_value *about = NULL;
    if (cx_get(e, "about")) {
        about = eval(c, cx_get(e, "about"));
        if (!about || about == PENDING) return about;
    }

    cx_value *u = keyed_line(c, "waits.jsonl", key, "until");
    double until = u && u->kind == CX_NUM ? u->u.num : -1;
    cx_value *delivered = keyed_line(c, "inbox.jsonl", key, "value");
    if (delivered) {
        /* What counts is when the message arrived, not when the run is
         * resumed: one that came after the deadline is not taken (`calyx
         * deliver` refuses it already; this covers an inbox written by hand
         * or by an older version, which have no time and are taken). */
        cx_value *at = keyed_line(c, "inbox.jsonl", key, "at");
        if (until >= 0 && at && at->kind == CX_NUM && at->u.num >= until) {
            trace(x, c->label, "recv  %s  delivered after the deadline: not taken", msg);
        } else {
            trace(x, c->label, "recv  %s  delivered", msg);
            return received(c, key, delivered, 0);
        }
    }
    double t = (double)time(NULL);
    if (until < 0) {
        until = t + cx_get_num(e, "timeout_s", 0);
        cx_buf line = {0};
        cx_buf_puts(&line, "{\"key\":");
        cx_buf_json_str(&line, key, strlen(key));
        cx_buf_puts(&line, ",\"message\":");
        cx_buf_json_str(&line, msg, strlen(msg));
        cx_buf_printf(&line, ",\"until\":%.0f", until);
        if (about) {
            cx_buf_puts(&line, ",\"about\":");
            cx_write(&line, about);
        }
        cx_buf_putc(&line, '}');
        int bad = append_line(x, "waits.jsonl", line.data, line.len) != 0;
        cx_buf_free(&line);
        if (bad) return fatalf(c, "cannot write the deadline of `receive %s`", msg);
    }
    if (t >= until) {
        cx_value *v = eval(c, cx_get(e, "on_timeout"));
        if (!v || v == PENDING) return v;
        trace(x, c->label, "recv  %s  timed out", msg);
        return received(c, key, v, 1);
    }
    time_t ut = (time_t)until;
    struct tm tmv;
    gmtime_r(&ut, &tmv);
    char when[64];
    strftime(when, sizeof when, "%Y-%m-%d %H:%M:%S UTC", &tmv);
    pthread_mutex_lock(&x->mu);
    x->waiting++;
    if (!x->wait_desc)
        x->wait_desc = fmt(&x->arena, "waiting: `%s` at `%s`, until %s", msg, key, when);
    pthread_mutex_unlock(&x->mu);
    trace(x, c->label, "recv  %s  waiting until %s", msg, when);
    return PENDING;
}

/* ----- for each, inside an expression ------------------------------------- */

/* `for each x in over: body`: every item at once, each keyed by its
 * position (`scope#id[j]`), so their calls start together. */
static cx_value *eval_each(ctx *c, cx_value *e) {
    cx_value *over = eval(c, cx_get(e, "over"));
    if (!over || over == PENDING) return over;
    if (over->kind != CX_LIST) return failf(c, "`for each` over a value that is not a list");
    size_t slot = index_of(e, "slot");
    size_t id = index_of(e, "id");
    size_t m = over->u.list.len;
    cx_value **items = cx_alloc(&c->w->arena, (m ? m : 1) * sizeof *items);
    const char *outer = c->scope;
    int waiting = 0;
    for (size_t j = 0; j < m; j++) {
        c->locals[slot] = over->u.list.items[j];
        c->scope = fmt(&c->w->arena, "%s#%zu[%zu]", outer, id, j);
        cx_value *v = eval(c, cx_get(e, "body"));
        c->scope = outer;
        if (!v) return NULL;
        if (v == PENDING)
            waiting = 1;
        else
            items[j] = v;
    }
    return waiting ? PENDING : cx_list(&c->w->arena, items, m);
}

/* ----- races (D12) ---------------------------------------------------------- */

static void cancel_branch(exec *x, const char *prefix) {
    if (x->ncancelled == x->cancelled_cap) {
        x->cancelled_cap = x->cancelled_cap ? 2 * x->cancelled_cap : 8;
        x->cancelled = realloc(x->cancelled, x->cancelled_cap * sizeof *x->cancelled);
        if (!x->cancelled) abort();
    }
    x->cancelled[x->ncancelled++] = cx_strndup(&x->arena, prefix, strlen(prefix));
}

/* The obligations (`owe`) of the calls under `prefix` (a losing branch),
 * from this run and, when resuming, from the journal. */
typedef struct {
    const char **keys;
    const char **jsons;
    size_t n, cap;
    const char *prefix;
    cx_arena *a;
} owed;

static void owed_add(owed *o, const char *key, const char *json) {
    size_t n = strlen(o->prefix);
    if (strncmp(key, o->prefix, n) != 0 || key[n] != '#') return;
    for (size_t i = 0; i < o->n; i++)
        if (strcmp(o->keys[i], key) == 0) return;
    if (o->n == o->cap) {
        o->cap = o->cap ? 2 * o->cap : 8;
        const char **k = cx_alloc(o->a, o->cap * sizeof *k);
        const char **v = cx_alloc(o->a, o->cap * sizeof *v);
        if (o->n) {
            memcpy(k, o->keys, o->n * sizeof *k);
            memcpy(v, o->jsons, o->n * sizeof *v);
        }
        o->keys = k;
        o->jsons = v;
    }
    o->keys[o->n] = key;
    o->jsons[o->n] = json;
    o->n++;
}

static void owed_from_journal(void *ud, const char *key, cx_value *ok) {
    owed *o = ud;
    cx_buf b = {0};
    cx_write(&b, ok);
    owed_add(o, key + strlen("owe:"), cx_strndup(o->a, b.data, b.len));
    cx_buf_free(&b);
}

/*
 * Saga (D12): undoes what the losing branches of the race `key` wrote with
 * tools that declare `compensate`. Each undo is a call keyed `undo:<the
 * write's key>`: made once, also across resumes. A write still in progress
 * is waited for first, so its undo does not overtake it. A write cancelled
 * before it started owes nothing. Returns PENDING until every undo is done,
 * NULL if one failed.
 */
/* `name` is the winner, or one of the winners (`race first N`). */
static int won_by(const cx_value *winners, const char *name) {
    if (winners && winners->kind == CX_STR) return strcmp(winners->u.str.s, name) == 0;
    for (size_t i = 0; i < len_of(winners); i++)
        if (at(winners, i)->kind == CX_STR && strcmp(at(winners, i)->u.str.s, name) == 0) return 1;
    return 0;
}

static cx_value *compensate_losers(ctx *c, const char *key, cx_value *names, cx_value *winners) {
    exec *x = c->x;
    int waiting = 0;
    for (size_t i = 0; i < len_of(names); i++) {
        const char *name = at(names, i)->u.str.s;
        if (won_by(winners, name)) continue;
        owed o = {.prefix = fmt(&c->w->arena, "%s.%s", key, name), .a = &c->w->arena};
        pthread_mutex_lock(&x->mu);
        for (size_t k = 0; k < x->nowes; k++) owed_add(&o, x->owes[k].key, x->owes[k].json);
        pthread_mutex_unlock(&x->mu);
        if (x->journal) {
            pthread_mutex_lock(&x->jmu);
            cx_journal_each(x->journal, fmt(&c->w->arena, "owe:%s", o.prefix), owed_from_journal,
                            &o);
            pthread_mutex_unlock(&x->jmu);
        }
        for (size_t k = 0; k < o.n; k++) {
            pthread_mutex_lock(&x->mu);
            pending *w = ptab_get(x, o.keys[k]);
            int inflight = w && w->state == P_INFLIGHT;
            if (inflight) add_waiter(&c->w->arena, w, c->t);
            pthread_mutex_unlock(&x->mu);
            if (inflight) {
                waiting = 1;
                continue;
            }
            cx_value *ob = cx_parse(&c->w->arena, o.jsons[k], strlen(o.jsons[k]), NULL);
            cx_value *tool = ob ? at(x->tools, (size_t)cx_get_num(ob, "tool", -1)) : NULL;
            if (!tool) return fatalf(c, "invalid compensation for `%s`", o.keys[k]);
            cx_value *list = cx_get(ob, "args");
            size_t n = len_of(list);
            cx_value **args = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *args);
            for (size_t m = 0; m < n; m++) args[m] = at(list, m);
            const char *ukey = fmt(&c->w->arena, "undo:%s", o.keys[k]);
            pthread_mutex_lock(&x->mu);
            int first = ptab_get(x, ukey) == NULL;
            pthread_mutex_unlock(&x->mu);
            if (first)
                trace(x, c->label, "undo  %s  (branch `%s` lost)", cx_get_str(tool, "name", "?"),
                      name);
            cx_value *r = tool_request(c, tool, args, n, NULL, ukey);
            if (!r) return NULL;
            if (r == PENDING) waiting = 1;
        }
    }
    return waiting ? PENDING : cx_null(&c->w->arena);
}

char *calyx_sandbox_fork(const char *src, const char *key, const char *journal_dir);

/*
 * `fork repo` (D13): a copy of the sandbox for whoever receives it (a
 * subgraph that edits it, a branch of a race). Made once per key (the
 * place of the `fork` in the run, so each branch and each item has its
 * own), next to the original; a resumed run finds it and puts it back to
 * its last journaled snapshot.
 */
static cx_value *eval_fork(ctx *c, cx_value *e) {
    exec *x = c->x;
    const char *key = call_key(c, e);
    pthread_mutex_lock(&x->mu);
    pending *p = ptab_get(x, key);
    pthread_mutex_unlock(&x->mu);
    if (p && p->state == P_DONE) return p->value;
    cx_value *src = eval(c, cx_get(e, "v"));
    if (!src || src == PENDING) return src;
    if (src->kind != CX_STR) return fatalf(c, "invalid IR: `fork` of something not a sandbox");
    char hash[65];
    cx_sha256_hex(key, strlen(key), hash);
    hash[16] = '\0';
    cx_value *path;
    if (x->journal && cx_journal_get_mode(x->journal) == CX_JOURNAL_REPLAY) {
        /* A replay calls no tool: the path is enough. */
        path = cx_cstr(&c->w->arena, fmt(&c->w->arena, "%s.forks/%s", src->u.str.s, hash));
    } else {
        int resume = x->journal && cx_journal_get_mode(x->journal) == CX_JOURNAL_RESUME;
        char *made = calyx_sandbox_fork(src->u.str.s, hash, resume ? x->run_dir : NULL);
        if (!made) return fatalf(c, "cannot fork the sandbox %.300s", src->u.str.s);
        path = cx_cstr(&c->w->arena, made);
        calyx_string_free(made);
        trace(x, c->label, "fork  %s", path->u.str.s);
    }
    pthread_mutex_lock(&x->mu);
    new_pending(x, &c->w->arena, key, P_DONE, path);
    pthread_mutex_unlock(&x->mu);
    return path;
}

/* "`a`" or "`a`, `b`", for the trace. */
static const char *race_names(ctx *c, cx_value *winners) {
    if (winners && winners->kind == CX_STR) return fmt(&c->w->arena, "`%s`", winners->u.str.s);
    cx_buf b = {0};
    for (size_t i = 0; i < len_of(winners); i++)
        cx_buf_printf(&b, "%s`%s`", i ? ", " : "", at(winners, i)->u.str.s);
    const char *s = fmt(&c->w->arena, "%s", b.data ? b.data : "");
    cx_buf_free(&b);
    return s;
}

/*
 * `race first where cond:` + `name: value` branches + `on none`. With
 * `race first N`, the first N branches that pass win, and the race gives
 * their values as a list, in the order of the branches (a quorum). Every
 * branch runs at once, its calls keyed by it (`scope#id.name`). The first
 * branch seen with a value that passes `cond` wins: the winner goes to the
 * journal (the race is decided once, also across resumes) and the other
 * branches are cancelled between steps: their subgraphs stop and their
 * calls not started yet are dropped; calls in progress finish, unused. A
 * branch that fails loses. With every branch finished and no winner,
 * `on none`.
 */
static cx_value *eval_race(ctx *c, cx_value *e) {
    exec *x = c->x;
    const char *key = call_key(c, e);
    cx_value *names = cx_get(e, "names");
    const char *wkey = fmt(&c->w->arena, "%s~winner", key);
    pthread_mutex_lock(&x->mu);
    pending *p = ptab_get(x, key);
    pending *pw = ptab_get(x, wkey);
    pthread_mutex_unlock(&x->mu);
    if (p && p->state == P_DONE) {
        /* Decided: done when the losers are undone (D12). */
        if (pw && pw->value) {
            cx_value *u = compensate_losers(c, key, names, pw->value);
            if (!u || u == PENDING) return u;
        }
        return p->value;
    }
    if (x->journal) {
        int mismatch = 0;
        char hash[65];
        cx_sha256_hex(key, strlen(key), hash);
        pthread_mutex_lock(&x->jmu);
        cx_value *hit = cx_journal_lookup(x->journal, key, hash, &mismatch);
        pthread_mutex_unlock(&x->jmu);
        if (hit) {
            cx_value *v = cx_get(hit, "value");
            cx_value *winners = cx_get(hit, "winners");
            if (!winners) winners = cx_cstr(&c->w->arena, cx_get_str(hit, "winner", "?"));
            pthread_mutex_lock(&x->mu);
            x->from_journal++;
            new_pending(x, &c->w->arena, key, P_DONE, v);
            new_pending(x, &c->w->arena, wkey, P_DONE, winners);
            pthread_mutex_unlock(&x->mu);
            trace(x, c->label, "race  won by %s  from the journal", race_names(c, winners));
            cx_value *u = compensate_losers(c, key, names, winners);
            if (!u || u == PENDING) return u;
            return v;
        }
    }
    cx_value *branches = cx_get(e, "branches");
    cx_value *cond = cx_get(e, "cond");
    size_t n = len_of(branches);
    size_t slot = index_of(e, "slot");
    const char *outer_scope = c->scope;
    const char *outer_failure = c->failure;
    const char *outer_label = c->label;
    /* `race first N`: N winners, their values as a list (a quorum). */
    cx_value *count = cx_get(e, "count");
    int many = count && count->kind == CX_NUM;
    size_t want = many ? (size_t)count->u.num : 1;
    int open = 0;
    size_t nwon = 0;
    size_t *wins = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *wins);
    cx_value **won = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *won);
    for (size_t i = 0; i < n && nwon < want; i++) {
        const char *name = at(names, i)->u.str.s;
        c->scope = fmt(&c->w->arena, "%s.%s", key, name);
        c->label = fmt(&c->w->arena, "%s.%s", outer_label, name);
        c->failure = NULL;
        cx_value *v = eval(c, at(branches, i));
        c->scope = outer_scope;
        c->label = outer_label;
        c->failure = outer_failure;
        if (v == PENDING) {
            open = 1;
            continue;
        }
        if (!v) {
            /* A failed branch loses; a failure that stopped the run does not. */
            pthread_mutex_lock(&x->mu);
            int stopping = x->stopping;
            pthread_mutex_unlock(&x->mu);
            if (stopping) return NULL;
            continue;
        }
        if (cond && cond->kind != CX_NULL) {
            c->locals[slot] = v;
            cx_value *ok = eval(c, cond);
            if (!ok) return NULL;
            if (ok == PENDING || ok->kind != CX_BOOL || !ok->u.b) continue;
        }
        wins[nwon] = i;
        won[nwon++] = v;
    }
    if (nwon == want && want > 0) {
        cx_value **wnames = cx_alloc(&c->w->arena, want * sizeof *wnames);
        for (size_t k = 0; k < want; k++) wnames[k] = at(names, wins[k]);
        cx_value *winners = many ? cx_list(&c->w->arena, wnames, want) : wnames[0];
        cx_value *value = many ? cx_list(&c->w->arena, won, want) : won[0];
        const char *keys[2] = {many ? "winners" : "winner", "value"};
        cx_value *vals[2] = {winners, value};
        job j;
        memset(&j, 0, sizeof j);
        j.key = key;
        cx_sha256_hex(key, strlen(key), j.req_hash);
        if (!journal_record(x, &j, "read", cx_rec(&c->w->arena, keys, vals, 2)))
            return fatalf(c, "cannot write the journal");
        pthread_mutex_lock(&x->mu);
        new_pending(x, &c->w->arena, key, P_DONE, value);
        new_pending(x, &c->w->arena, wkey, P_DONE, winners);
        for (size_t i = 0; i < n; i++)
            if (!won_by(winners, at(names, i)->u.str.s))
                cancel_branch(x, fmt(&c->w->arena, "%s.%s", key, at(names, i)->u.str.s));
        pthread_mutex_unlock(&x->mu);
        trace(x, c->label, "race  won by %s", race_names(c, winners));
        cx_value *u = compensate_losers(c, key, names, winners);
        if (!u || u == PENDING) return u;
        return value;
    }
    if (open) return PENDING;
    if (many)
        trace(x, c->label, "race  fewer than %zu branches won", want);
    else
        trace(x, c->label, "race  no branch won");
    cx_value *fail = cx_get(e, "on_none_fail");
    if (fail && fail->kind == CX_STR) return failf(c, "%s", fail->u.str.s);
    return eval(c, cx_get(e, "on_none"));
}

/* ----- agents ------------------------------------------------------------- */

/* The tool definitions the model sees (OpenAI "function" format). */
static void tool_defs(exec *x, cx_value *tools, cx_buf *b) {
    cx_buf_putc(b, '[');
    for (size_t i = 0; i < len_of(tools); i++) {
        cx_value *t = at(x->tools, (size_t)at(tools, i)->u.num);
        const char *name = cx_get_str(t, "name", "?");
        if (i) cx_buf_putc(b, ',');
        cx_buf_puts(b, "{\"type\":\"function\",\"function\":{\"name\":");
        cx_buf_json_str(b, name, strlen(name));
        cx_buf_puts(b, ",\"description\":");
        const char *d = cx_get_str(t, "description", name);
        cx_buf_json_str(b, d, strlen(d));
        cx_buf_puts(b, ",\"parameters\":");
        cx_write(b, cx_get(t, "schema"));
        cx_buf_puts(b, "}}");
    }
    cx_buf_putc(b, ']');
}

static void user_message(cx_buf *msgs, const char *text) {
    cx_buf_puts(msgs, ",{\"role\":\"user\",\"content\":");
    cx_buf_json_str(msgs, text, strlen(text));
    cx_buf_putc(msgs, '}');
}

/* One model call of the agent: the conversation so far, maybe with tools. */
static cx_value *agent_turn(ctx *c, cx_value *model, cx_value *prompt, const char *key,
                            cx_buf *msgs, const char *tools, const char *note) {
    cx_buf req = {0};
    cx_buf_puts(&req, "{\"model\":");
    cx_write(&req, cx_get(model, "id"));
    cx_buf_puts(&req, ",\"messages\":");
    cx_buf_put(&req, msgs->data, msgs->len);
    cx_buf_puts(&req, "],\"tools\":");
    cx_buf_puts(&req, tools ? tools : "null");
    cx_buf_puts(&req, ",\"max_output\":");
    cx_write(&req, cx_get(model, "max_output"));
    cx_buf_puts(&req, ",\"timeout_ms\":300000}");
    return request(c, key, CALL_CHAT, model, prompt, &req, note);
}

/* The agent's answer, as a value of the task's type. */
static cx_value *agent_answer(ctx *c, cx_value *model, cx_value *prompt, const char *base,
                              const char *text) {
    cx_value *schema = cx_get(prompt, "schema");
    if (!schema || schema->kind == CX_NULL) return cx_cstr(&c->w->arena, text);
    /* One more call turns the answer into the declared type. */
    cx_buf p = {0};
    cx_buf_puts(&p, "Rewrite the answer below in the requested JSON format, without adding "
                    "anything.\n\n");
    cx_buf_puts(&p, text);
    cx_buf req = {0};
    cx_buf_puts(&req, "{\"model\":");
    cx_write(&req, cx_get(model, "id"));
    cx_buf_puts(&req, ",\"prompt\":");
    cx_buf_json_str(&req, p.data, p.len);
    cx_buf_free(&p);
    cx_buf_puts(&req, ",\"schema\":");
    cx_write(&req, schema);
    cx_buf_puts(&req, ",\"max_output\":");
    cx_write(&req, cx_get(model, "max_output"));
    cx_buf_puts(&req, ",\"timeout_ms\":300000}");
    return request(c, fmt(&c->w->arena, "%s.format", base), CALL_MODEL, model, prompt, &req,
                   "format");
}

/* `on turn_limit` / `on stuck`: fail, or ask for a final answer. */
static cx_value *agent_limit(ctx *c, cx_value *e, const char *which, cx_value *model,
                             cx_value *prompt, const char *base, cx_buf *msgs,
                             const char *instruction) {
    cx_value *action = cx_get(e, which);
    if (action && action->kind == CX_STR) return failf(c, "%s", action->u.str.s);
    user_message(msgs, instruction);
    cx_value *ok = agent_turn(c, model, prompt, fmt(&c->w->arena, "%s.final", base), msgs, NULL,
                              "final answer");
    if (!ok || ok == PENDING) return ok;
    return agent_answer(c, model, prompt, base, cx_get_str(ok, "text", ""));
}

static agent_progress *agent_find(exec *x, const char *key) {
    for (agent_progress *p = x->agents[hash_str(key) & 255]; p; p = p->next)
        if (strcmp(p->key, key) == 0) return p;
    return NULL;
}

/* Writes down that the agent finished turn `turn - 1` (with `mu` held). */
static void agent_save(exec *x, const char *key, long turn, const cx_buf *msgs,
                       const char *previous, int repeats) {
    agent_progress *p = agent_find(x, key);
    if (!p) {
        p = calloc(1, sizeof *p);
        if (!p) abort();
        p->key = strdup(key);
        size_t b = hash_str(key) & 255;
        p->next = x->agents[b];
        x->agents[b] = p;
    }
    char *m = malloc(msgs->len + 1);
    char *prev = previous ? strdup(previous) : NULL;
    if (!m || !p->key || (previous && !prev)) abort();
    memcpy(m, msgs->data, msgs->len);
    m[msgs->len] = '\0';
    free(p->msgs);
    free(p->previous);
    p->msgs = m;
    p->len = msgs->len;
    p->previous = prev;
    p->turn = turn;
    p->repeats = repeats;
}

/*
 * The ReAct cycle (D5). Each turn sends the whole conversation; the
 * assistant's message goes back exactly as it came (providers attach data
 * to it). Tool calls of one turn run in parallel; a failed tool call is an
 * observation for the model, not a failure of the run.
 */
static cx_value *call_agent(ctx *c, cx_value *e) {
    exec *x = c->x;
    cx_arena *a = &c->w->arena;
    cx_value *model = at(x->models, index_of(e, "model"));
    cx_value *prompt = at(x->prompts, index_of(e, "prompt"));
    if (!model || !prompt) return fatalf(c, "invalid IR: unknown model or prompt");
    cx_value *args_e = cx_get(e, "args");
    size_t n = len_of(args_e);
    cx_value **args = cx_alloc(a, (n ? n : 1) * sizeof *args);
    EVAL_ALL(c, args_e, args);
    cx_value *tools = cx_get(e, "tools");
    /* Sandboxes lent to the tools: computed once, added to every call. */
    cx_value *bound_e = cx_get(e, "bound");
    size_t nt = len_of(tools);
    cx_value ***bound = cx_alloc(a, (nt ? nt : 1) * sizeof *bound);
    for (size_t k = 0; k < nt; k++) {
        cx_value *t = at(x->tools, (size_t)at(tools, k)->u.num);
        size_t np = len_of(cx_get(t, "params"));
        bound[k] = cx_alloc(a, (np ? np : 1) * sizeof **bound);
        for (size_t p = 0; p < np; p++) bound[k][p] = NULL;
        cx_value *list = at(bound_e, k);
        for (size_t b = 0; b < len_of(list); b++) {
            cx_value *entry = at(list, b);
            size_t p = index_of(entry, "param");
            cx_value *v = eval(c, cx_get(entry, "v"));
            if (!v || v == PENDING) return v;
            if (p < np) bound[k][p] = v;
        }
    }
    cx_buf defs = {0};
    tool_defs(x, tools, &defs);
    const char *defs_s = fmt(a, "%s", defs.data);
    cx_buf_free(&defs);
    const char *base = call_key(c, e);

    cx_buf task = prompt_text(prompt, args, n);
    cx_buf msgs = {0};
    cx_buf_puts(&msgs, "[{\"role\":\"user\",\"content\":");
    cx_buf_json_str(&msgs, task.data ? task.data : "", task.len);
    cx_buf_putc(&msgs, '}');
    cx_buf_free(&task);

    double max_turns = cx_get_num(e, "max_turns", 1);
    const char *previous = NULL;
    int repeats = 0;
    cx_value *result = NULL;
    long first = 0;
    pthread_mutex_lock(&x->mu);
    agent_progress *saved = agent_find(x, base);
    if (saved) {
        first = saved->turn;
        repeats = saved->repeats;
        previous = saved->previous ? fmt(a, "%s", saved->previous) : NULL;
        msgs.len = 0;
        cx_buf_put(&msgs, saved->msgs, saved->len);
    }
    pthread_mutex_unlock(&x->mu);
    for (long turn = first;; turn++) {
        if (turn >= (long)max_turns) {
            result = agent_limit(c, e, "on_turn_limit", model, prompt, base, &msgs,
                                 "You reached the limit of turns. Do not call tools anymore: "
                                 "give your final answer now, with what you know.");
            break;
        }
        cx_value *ok = agent_turn(c, model, prompt, fmt(a, "%s.t%ld", base, turn), &msgs, defs_s,
                                  fmt(a, "turn %ld", turn + 1));
        if (!ok || ok == PENDING) {
            result = ok;
            break;
        }
        cx_buf_putc(&msgs, ',');
        cx_write(&msgs, cx_get(ok, "message"));
        cx_value *calls = cx_get(ok, "tool_calls");
        if (len_of(calls) == 0) {
            result = agent_answer(c, model, prompt, base, cx_get_str(ok, "text", ""));
            break;
        }
        /* Stuck: the same calls (tools not marked `repeatable`) three turns in a row. */
        cx_buf sig = {0};
        for (size_t i = 0; i < len_of(calls); i++) {
            cx_value *call = at(calls, i);
            const char *name = cx_get_str(call, "name", "?");
            int repeatable = 0;
            for (size_t k = 0; k < len_of(tools); k++) {
                cx_value *t = at(x->tools, (size_t)at(tools, k)->u.num);
                if (strcmp(cx_get_str(t, "name", ""), name) == 0)
                    repeatable = cx_get_bool(t, "repeatable", 0);
            }
            if (repeatable) continue;
            cx_buf_puts(&sig, name);
            cx_write(&sig, cx_get(call, "arguments"));
        }
        const char *now_sig = fmt(a, "%s", sig.data ? sig.data : "");
        cx_buf_free(&sig);
        repeats = previous && now_sig[0] && strcmp(previous, now_sig) == 0 ? repeats + 1 : 0;
        previous = now_sig;
        if (repeats >= 2) {
            result = agent_limit(c, e, "on_stuck", model, prompt, base, &msgs,
                                 "You are repeating the same action with the same arguments. "
                                 "Stop calling tools and give your final answer now.");
            break;
        }
        int waiting = 0;
        for (size_t i = 0; i < len_of(calls); i++) {
            cx_value *call = at(calls, i);
            const char *name = cx_get_str(call, "name", "?");
            cx_value *tool = NULL;
            size_t tk = 0;
            for (size_t k = 0; k < len_of(tools); k++) {
                cx_value *t = at(x->tools, (size_t)at(tools, k)->u.num);
                if (strcmp(cx_get_str(t, "name", ""), name) == 0) {
                    tool = t;
                    tk = k;
                }
            }
            cx_buf obs = {0};
            if (!tool) {
                cx_buf_printf(&obs, "error: there is no tool `%s`", name);
            } else {
                cx_buf req = {0};
                cx_buf_puts(&req, "{\"tool\":");
                cx_buf_json_str(&req, name, strlen(name));
                cx_buf_puts(&req, ",\"args\":{");
                /* The model's arguments, then the lent sandboxes (the model
                 * never chooses those). */
                cx_value *arguments = cx_get(call, "arguments");
                cx_value *tparams = cx_get(tool, "params");
                size_t np = len_of(tparams);
                int any = 0;
                if (arguments && arguments->kind == CX_REC) {
                    for (size_t f = 0; f < arguments->u.rec.len; f++) {
                        const char *fname = arguments->u.rec.keys[f];
                        int lent = 0;
                        for (size_t p = 0; p < np; p++)
                            if (bound[tk][p] && strcmp(at(tparams, p)->u.str.s, fname) == 0) lent = 1;
                        if (lent) continue;
                        if (any) cx_buf_putc(&req, ',');
                        any = 1;
                        cx_buf_json_str(&req, fname, strlen(fname));
                        cx_buf_putc(&req, ':');
                        cx_write(&req, arguments->u.rec.vals[f]);
                    }
                }
                for (size_t p = 0; p < np; p++) {
                    if (!bound[tk][p]) continue;
                    if (any) cx_buf_putc(&req, ',');
                    any = 1;
                    cx_write(&req, at(tparams, p));
                    cx_buf_putc(&req, ':');
                    cx_write(&req, bound[tk][p]);
                }
                cx_buf_putc(&req, '}');
                put_max_output(&req, tool);
                cx_buf_printf(&req, ",\"timeout_ms\":%.0f", cx_get_num(tool, "timeout_ms", 30000));
                put_borrows(&req, tool, bound[tk], np);
                cx_buf_putc(&req, '}');
                const char *outer = c->failure;
                c->failure = NULL;
                cx_value *r = request(c, fmt(a, "%s.t%ld.c%zu", base, turn, i), CALL_TOOL, tool,
                                      NULL, &req, NULL);
                const char *why = c->failure;
                c->failure = outer;
                if (r == PENDING) {
                    waiting = 1;
                } else if (r) {
                    render(&obs, r);
                } else {
                    pthread_mutex_lock(&x->mu);
                    int stopping = x->stopping;
                    pthread_mutex_unlock(&x->mu);
                    if (stopping) {
                        cx_buf_free(&msgs);
                        return NULL;
                    }
                    cx_buf_printf(&obs, "error: %s", why ? why : "the tool failed");
                }
            }
            cx_buf_puts(&msgs, ",{\"role\":\"tool\",\"tool_call_id\":");
            cx_write(&msgs, cx_get(call, "id"));
            cx_buf_puts(&msgs, ",\"content\":");
            cx_buf_json_str(&msgs, obs.data ? obs.data : "", obs.len);
            cx_buf_putc(&msgs, '}');
            cx_buf_free(&obs);
        }
        if (waiting) {
            result = PENDING;
            break;
        }
        pthread_mutex_lock(&x->mu);
        agent_save(x, base, turn + 1, &msgs, previous, repeats);
        pthread_mutex_unlock(&x->mu);
    }
    cx_buf_free(&msgs);
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
        return v ? v : fatalf(c, "invalid IR: node used before it was computed");
    }
    if (strcmp(k, "item") == 0) return c->item;
    if (strcmp(k, "local") == 0) {
        cx_value *v = c->locals[index_of(e, "i")];
        return v ? v : fatalf(c, "invalid IR: local used before it was bound");
    }
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
    if (strcmp(k, "record") == 0) {
        cx_value *names = cx_get(e, "names");
        cx_value *values = cx_get(e, "values");
        size_t n = len_of(values);
        cx_value **vs = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *vs);
        const char **ks = cx_alloc(&c->w->arena, (n ? n : 1) * sizeof *ks);
        EVAL_ALL(c, values, vs);
        for (size_t i = 0; i < n; i++) ks[i] = at(names, i)->u.str.s;
        return cx_rec(&c->w->arena, ks, vs, n);
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
    if (strcmp(k, "bin") == 0) return binary(c, e);
    if (strcmp(k, "un") == 0) {
        cx_value *v = eval(c, cx_get(e, "v"));
        if (!v || v == PENDING) return v;
        if (strcmp(cx_get_str(e, "op", ""), "not") == 0)
            return cx_bool(&c->w->arena, !(v->kind == CX_BOOL && v->u.b));
        if (v->kind != CX_NUM) return failf(c, "`-` needs a number");
        return cx_num(&c->w->arena, -v->u.num);
    }
    if (strcmp(k, "if") == 0) {
        cx_value *cond = eval(c, cx_get(e, "c"));
        if (!cond || cond == PENDING) return cond;
        /* Only the branch taken runs (D32). */
        return eval(c, cx_get(e, cond->kind == CX_BOOL && cond->u.b ? "t" : "e"));
    }
    if (strcmp(k, "match") == 0) {
        cx_value *v = eval(c, cx_get(e, "v"));
        if (!v || v == PENDING) return v;
        cx_value *cs = pick_case(c, e, v);
        return cs ? eval(c, cx_get(cs, "body")) : NULL;
    }
    if (strcmp(k, "loop") == 0) return eval_loop(c, e);
    if (strcmp(k, "try") == 0) return eval_try(c, e);
    if (strcmp(k, "agent") == 0) return call_agent(c, e);
    if (strcmp(k, "ask") == 0 || strcmp(k, "send") == 0) return call_message(c, e);
    if (strcmp(k, "bool") == 0) return cx_get(e, "v");
    if (strcmp(k, "receive") == 0) return call_receive(c, e);
    if (strcmp(k, "each") == 0) return eval_each(c, e);
    if (strcmp(k, "race") == 0) return eval_race(c, e);
    if (strcmp(k, "fork") == 0) return eval_fork(c, e);
    if (strcmp(k, "let") == 0) {
        cx_value *v = eval(c, cx_get(e, "v"));
        if (!v || v == PENDING) return v;
        c->locals[index_of(e, "slot")] = v;
        return eval(c, cx_get(e, "body"));
    }
    if (strcmp(k, "def") == 0) return call_def(c, e);
    if (strcmp(k, "comp") == 0) return comprehension(c, e);
    if (strcmp(k, "builtin") == 0) return builtin(c, e);
    if (strcmp(k, "state") == 0 && c->estate) {
        cx_value *v = cx_get(c->estate, cx_get_str(e, "field", ""));
        return v ? v : cx_null(&c->w->arena);
    }
    if (strcmp(k, "model") == 0) return call_model(c, e);
    if (strcmp(k, "route") == 0) return call_route(c, e);
    if (strcmp(k, "tool") == 0) return call_tool(c, e);
    if (strcmp(k, "graph") == 0) return call_graph(c, e);
    return fatalf(c, "invalid IR: unknown expression `%s`", k);
}

/* ----- workers -------------------------------------------------------------- */

/* A node of a subgraph failed: the call that started the subgraph fails. */
static void subgraph_failed(exec *x, worker *w, gexec *g, const char *node, const char *why) {
    if (g->failed) return;
    g->failed = 1;
    g->parent->state = P_FAILED;
    g->parent->error = fmt(&w->arena, "graph `%s` failed at node `%s`: %s",
                           cx_get_str(g->graph, "name", "?"), node, why);
    wake(x, w, g->parent);
}

static void run_task(worker *w, task *t) {
    exec *x = w->x;
    gexec *g = t->g;
    cx_value *node = at(g->nodes_ir, t->node);
    const char *name = cx_get_str(node, "name", "?");
    ctx c = {x, w, t, g, NULL, NULL, NULL, name, name, NULL, NULL, NULL};
    if (t->item < 0) {
        c.instance = fmt(&w->arena, "%s/%s", g->path, name);
    } else {
        c.instance = fmt(&w->arena, "%s/%s[%ld]", g->path, name, t->item);
        c.label = fmt(&w->arena, "%s[%ld]", name, t->item);
        c.item = at(g->lists[t->node], (size_t)t->item);
    }
    c.scope = c.instance;
    size_t nlocals = (size_t)cx_get_num(node, "nlocals", 0);
    c.locals = cx_alloc(&w->arena, (nlocals ? nlocals : 1) * sizeof *c.locals);
    memset(c.locals, 0, (nlocals ? nlocals : 1) * sizeof *c.locals);

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
    } else if (!v) {
        t->state = T_DONE;
        /* Nothing caught the failure: it fails the subgraph, or the run. */
        if (!x->stopping) {
            const char *why = c.failure ? c.failure : "unknown failure";
            if (g->parent)
                subgraph_failed(x, w, g, name, why);
            else
                fail_locked(x, cx_get_str(g->graph, "name", "?"), name, why);
        }
    } else {
        t->state = T_DONE;
        if (!x->stopping && !g->failed) {
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
        if (t && x->ncancelled && is_cancelled(x, t->g->path)) {
            /* Its branch lost a race: it stops here. */
            t->state = T_DONE;
            pthread_mutex_unlock(&x->mu);
            continue;
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
    x->entities = cx_get(ir, "entities");
    x->defs = cx_get(ir, "defs");
    x->routers = cx_get(ir, "routers");

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
    x->decision = cx_get_str(options, "uncertain", NULL);
    x->decision_value = cx_get(options, "uncertain_value");
    if (x->decision_value && x->decision_value->kind == CX_NULL) x->decision_value = NULL;
    x->nworkers = x->deterministic ? 1 : cpu_count();
    x->nio = x->deterministic ? 1 : threads < 1 ? 1 : threads > 256 ? 256 : (int)threads;

    /* The journal: the program is identified by the hash of its IR (D23). */
    const char *dir = cx_get_str(options, "journal", NULL);
    /* The run's id for entities: its directory's name (the same when
     * resumed); without a journal, unique to this process. */
    if (dir) {
        const char *slash = strrchr(dir, '/');
        x->run_id = slash ? slash + 1 : dir;
        x->run_dir = dir;
    } else {
        x->run_id = fmt(&x->arena, "pid%ld-%ld", (long)getpid(), (long)time(NULL));
    }
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
                now() - x->t0,
                result ? "finished" : x->waiting ? "stopped to wait" : "failed", x->model_calls,
                x->input_tokens,
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
    for (size_t i = 0; i < 256; i++)
        for (agent_progress *p = x->agents[i], *next; p; p = next) {
            next = p->next;
            free(p->key);
            free(p->msgs);
            free(p->previous);
            free(p);
        }
    free(x->cancelled);
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
