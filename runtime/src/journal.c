#define _POSIX_C_SOURCE 200809L
/* macOS hides sysconf(_SC_NPROCESSORS_ONLN) and friends under strict POSIX. */
#define _DARWIN_C_SOURCE

#include "journal.h"

#include "sha256.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

/* Answers above this size go to blobs/ (decision D20). */
#define BLOB_THRESHOLD 4096
#define SYNC_INTERVAL 1.0

typedef struct {
    const char *key;
    const char *req;
    cx_value *entry; /* the "call" line */
    int finished;    /* 0 for a "begin" without its "call" */
} slot;

struct cx_journal {
    cx_arena *arena;
    char *dir;
    cx_journal_mode mode;
    FILE *f; /* NULL in replay mode: nothing is written */
    double last_sync;
    slot *slots; /* open addressing; capacity is a power of two */
    size_t cap, used;
};

static double now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static size_t hash_key(const char *s) {
    size_t h = 1469598103934665603ULL & (size_t)-1; /* FNV-1a */
    for (; *s; s++) h = (h ^ (unsigned char)*s) * (size_t)1099511628211ULL;
    return h;
}

static slot *find(const cx_journal *j, const char *key) {
    if (!j->cap) return NULL;
    for (size_t i = hash_key(key) & (j->cap - 1);; i = (i + 1) & (j->cap - 1)) {
        slot *s = &j->slots[i];
        if (!s->key) return NULL;
        if (strcmp(s->key, key) == 0) return s;
    }
}

static void put(cx_journal *j, const char *key, const char *req, cx_value *entry, int finished) {
    if ((j->used + 1) * 2 > j->cap) {
        size_t cap = j->cap ? j->cap * 2 : 64;
        slot *old = j->slots;
        size_t old_cap = j->cap;
        j->slots = calloc(cap, sizeof(slot));
        if (!j->slots) abort();
        j->cap = cap;
        j->used = 0;
        for (size_t i = 0; i < old_cap; i++)
            if (old[i].key) put(j, old[i].key, old[i].req, old[i].entry, old[i].finished);
        free(old);
    }
    slot *s = find(j, key);
    if (!s) {
        size_t i = hash_key(key) & (j->cap - 1);
        while (j->slots[i].key) i = (i + 1) & (j->cap - 1);
        s = &j->slots[i];
        s->key = key;
        j->used++;
    }
    s->req = req;
    s->entry = entry;
    s->finished = finished;
}

static char *path_join(cx_arena *a, const char *dir, const char *name) {
    size_t n = strlen(dir) + strlen(name) + 2;
    char *p = cx_alloc(a, n);
    snprintf(p, n, "%s/%s", dir, name);
    return p;
}

/* Creates `dir` and its parents. */
static int mkdirs(const char *dir) {
    char tmp[4096];
    size_t n = strlen(dir);
    if (n == 0 || n >= sizeof tmp) return -1;
    memcpy(tmp, dir, n + 1);
    for (char *p = tmp + 1; *p; p++) {
        if (*p != '/') continue;
        *p = '\0';
        if (mkdir(tmp, 0755) != 0 && errno != EEXIST) return -1;
        *p = '/';
    }
    return mkdir(tmp, 0755) != 0 && errno != EEXIST ? -1 : 0;
}

/* Reads a whole file into the arena. */
static char *read_file(cx_arena *a, const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    char *data = NULL;
    if (fseek(f, 0, SEEK_END) == 0) {
        long size = ftell(f);
        if (size >= 0 && fseek(f, 0, SEEK_SET) == 0) {
            data = cx_alloc(a, (size_t)size + 1);
            *len = fread(data, 1, (size_t)size, f);
            data[*len] = '\0';
        }
    }
    fclose(f);
    return data;
}

static void sync_file(cx_journal *j) {
    if (!j->f) return;
    fflush(j->f);
    fsync(fileno(j->f));
    j->last_sync = now();
}

/* Appends one line; it reaches the OS before this returns. */
static int append(cx_journal *j, const char *line, size_t len, int must_sync) {
    if (!j->f) return 1;
    if (fwrite(line, 1, len, j->f) != len || fputc('\n', j->f) == EOF || fflush(j->f) != 0)
        return 0;
    if (must_sync || now() - j->last_sync >= SYNC_INTERVAL) sync_file(j);
    return 1;
}

/*
 * Loads the entries of an existing journal. Returns the size of the valid
 * prefix: a crash can leave a torn last line, which is dropped.
 */
static long load(cx_journal *j, const char *ir_hash, cx_buf *err) {
    size_t len = 0;
    char *path = path_join(j->arena, j->dir, CX_JOURNAL_FILE);
    char *data = read_file(j->arena, path, &len);
    if (!data) {
        cx_buf_printf(err, "cannot read the journal `%s`", path);
        return -1;
    }
    size_t pos = 0, valid = 0;
    int line_no = 0;
    while (pos < len) {
        char *nl = memchr(data + pos, '\n', len - pos);
        size_t end = nl ? (size_t)(nl - data) : len;
        line_no++;
        const char *perr = NULL;
        cx_value *e = cx_parse(j->arena, data + pos, end - pos, &perr);
        if (!e || !nl) {
            if (end >= len) break; /* torn last line */
            cx_buf_printf(err, "the journal is damaged at line %d (%s)", line_no,
                          perr ? perr : "line not terminated");
            return -1;
        }
        const char *type = cx_get_str(e, "type", "");
        if (line_no == 1) {
            if (strcmp(type, "run") != 0 ||
                cx_get_num(e, "version", -1) != CX_JOURNAL_VERSION) {
                cx_buf_puts(err, "not a Calyx journal, or from another version");
                return -1;
            }
            if (strcmp(cx_get_str(e, "ir_hash", ""), ir_hash) != 0) {
                cx_buf_puts(err,
                            "the program changed since this run started: a run finishes on the "
                            "version it started with (D23)");
                return -1;
            }
        } else if (strcmp(type, "call") == 0) {
            put(j, cx_get_str(e, "key", ""), cx_get_str(e, "req", ""), e, 1);
        } else if (strcmp(type, "begin") == 0) {
            const char *key = cx_get_str(e, "key", "");
            slot *s = find(j, key);
            if (!s || !s->finished) put(j, key, cx_get_str(e, "req", ""), NULL, 0);
        }
        pos = end + 1;
        valid = pos;
    }
    if (line_no == 0) {
        cx_buf_puts(err, "the journal is empty");
        return -1;
    }
    return (long)valid;
}

cx_journal *cx_journal_open(cx_arena *a, const char *dir, cx_journal_mode mode,
                            const char *ir_hash, const char *header_json, cx_buf *err) {
    cx_journal *j = cx_alloc(a, sizeof *j);
    memset(j, 0, sizeof *j);
    j->arena = a;
    j->dir = cx_strndup(a, dir, strlen(dir));
    j->mode = mode;
    char *path = path_join(a, dir, CX_JOURNAL_FILE);

    if (mode == CX_JOURNAL_NEW) {
        if (mkdirs(dir) != 0 || mkdirs(path_join(a, dir, "blobs")) != 0) {
            cx_buf_printf(err, "cannot create the run directory `%s`", dir);
            return NULL;
        }
        j->f = fopen(path, "wbx"); /* never overwrite an existing run */
        if (!j->f) {
            cx_buf_printf(err, "cannot create the journal `%s`", path);
            return NULL;
        }
        if (!append(j, header_json, strlen(header_json), 1)) {
            cx_buf_printf(err, "cannot write the journal `%s`", path);
            fclose(j->f);
            return NULL;
        }
        return j;
    }

    long valid = load(j, ir_hash, err);
    if (valid < 0) {
        free(j->slots);
        return NULL;
    }
    if (mode == CX_JOURNAL_RESUME) {
        /* Drop a torn last line before appending after it. */
        if (truncate(path, valid) != 0 || !(j->f = fopen(path, "ab"))) {
            cx_buf_printf(err, "cannot append to the journal `%s`", path);
            free(j->slots);
            return NULL;
        }
        char line[128];
        snprintf(line, sizeof line, "{\"type\":\"resume\",\"at\":%lld}", (long long)time(NULL));
        append(j, line, strlen(line), 1);
    }
    return j;
}

cx_journal_mode cx_journal_get_mode(const cx_journal *j) { return j->mode; }

cx_value *cx_journal_lookup(cx_journal *j, const char *key, const char *req_hash, int *mismatch) {
    *mismatch = 0;
    slot *s = find(j, key);
    if (!s || !s->finished) return NULL;
    if (strcmp(s->req, req_hash) != 0) {
        *mismatch = 1;
        return NULL;
    }
    cx_value *ok = cx_get(s->entry, "ok");
    if (ok) return ok;
    const char *blob = cx_get_str(s->entry, "blob", NULL);
    if (!blob) return NULL;
    size_t len = 0;
    char *name = path_join(j->arena, "blobs", blob);
    char *data = read_file(j->arena, path_join(j->arena, j->dir, name), &len);
    return data ? cx_parse(j->arena, data, len, NULL) : NULL;
}

int cx_journal_uncertain(const cx_journal *j, const char *key) {
    slot *s = find(j, key);
    return s && !s->finished;
}

int cx_journal_begin(cx_journal *j, const char *key, const char *req_hash) {
    cx_buf line = {0};
    cx_buf_puts(&line, "{\"type\":\"begin\",\"key\":");
    cx_buf_json_str(&line, key, strlen(key));
    cx_buf_printf(&line, ",\"req\":\"%s\"}", req_hash);
    int ok = append(j, line.data, line.len, 1);
    cx_buf_free(&line);
    return ok;
}

/* Writes a blob once, atomically: a temporary file renamed into place. */
static int write_blob(cx_journal *j, const char *hash, const char *data, size_t len) {
    char *name = path_join(j->arena, "blobs", hash);
    char *path = path_join(j->arena, j->dir, name);
    struct stat st;
    if (stat(path, &st) == 0) return 1; /* same content, already there */
    size_t n = strlen(path) + 5;
    char *tmp = cx_alloc(j->arena, n);
    snprintf(tmp, n, "%s.tmp", path);
    FILE *f = fopen(tmp, "wb");
    if (!f) return 0;
    int ok = fwrite(data, 1, len, f) == len && fflush(f) == 0 && fsync(fileno(f)) == 0;
    ok = fclose(f) == 0 && ok;
    return ok && rename(tmp, path) == 0;
}

int cx_journal_record(cx_journal *j, const char *key, const char *effect, const char *req_hash,
                      const char *ok_json, size_t ok_len) {
    if (!j->f) return 1;
    cx_buf line = {0};
    cx_buf_puts(&line, "{\"type\":\"call\",\"key\":");
    cx_buf_json_str(&line, key, strlen(key));
    cx_buf_puts(&line, ",\"effect\":");
    cx_buf_json_str(&line, effect, strlen(effect));
    cx_buf_printf(&line, ",\"req\":\"%s\",", req_hash);
    int ok = 1;
    if (ok_len > BLOB_THRESHOLD) {
        char hash[65];
        cx_sha256_hex(ok_json, ok_len, hash);
        ok = write_blob(j, hash, ok_json, ok_len);
        cx_buf_printf(&line, "\"blob\":\"%s\"}", hash);
    } else {
        cx_buf_puts(&line, "\"ok\":");
        cx_buf_put(&line, ok_json, ok_len);
        cx_buf_putc(&line, '}');
    }
    /* External writes are synced at once; the rest at most once a second. */
    int is_write = strncmp(effect, "write", 5) == 0;
    ok = ok && append(j, line.data, line.len, is_write);
    cx_buf_free(&line);
    return ok;
}

void cx_journal_end(cx_journal *j, const char *ok_json, const char *error) {
    if (!j->f) return;
    cx_buf line = {0};
    cx_buf_puts(&line, "{\"type\":\"end\",");
    if (ok_json) {
        cx_buf_puts(&line, "\"ok\":");
        cx_buf_puts(&line, ok_json);
    } else {
        cx_buf_puts(&line, "\"error\":");
        cx_buf_json_str(&line, error ? error : "", error ? strlen(error) : 0);
    }
    cx_buf_putc(&line, '}');
    append(j, line.data, line.len, 1);
    cx_buf_free(&line);
}

void cx_journal_close(cx_journal *j) {
    if (!j) return;
    if (j->f) {
        sync_file(j);
        fclose(j->f);
        j->f = NULL;
    }
    free(j->slots);
    j->slots = NULL;
}
