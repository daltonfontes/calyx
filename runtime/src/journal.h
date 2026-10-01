/*
 * journal.h - the journal of an execution (decisions D6, D14, D20).
 *
 * One entry per completed model or tool call, appended to a file and never
 * rewritten. Resuming, recovering from a crash and replaying are the same
 * operation: run the graph again, and take every call already in the
 * journal from it instead of paying for it again.
 *
 * Layout of a run directory:
 *
 *   journal.jsonl   one JSON object per line:
 *     {"type":"run", "version":1, "ir_hash", "graph", "args", "program", "started"}
 *     {"type":"call", "key", "effect", "req", "ok": answer}   (or "blob": hash)
 *     {"type":"begin", "key", "req"}         before a `write once` call
 *     {"type":"resume", "at"}
 *     {"type":"end", "ok": value}  or  {"type":"end", "error": message}
 *   blobs/<sha256>  answers larger than 4 KB, stored by content (D20)
 *
 * A call's key is its place in the realized graph: graph, node, fan-out
 * item and position inside the node (e.g. `research/answers[2]#1`). It
 * does not depend on the order calls ran in, so it survives parallelism.
 * `req` is the hash of the request: on resume, a different request under
 * the same key means the run is not deterministic, and it stops.
 *
 * Durability: every entry reaches the operating system before the next
 * effect starts (a crash of the process loses nothing). fsync, which also
 * survives a power failure, runs at most once a second, and always before
 * external writes and at the end.
 */
#ifndef CALYX_JOURNAL_H
#define CALYX_JOURNAL_H

#include "json.h"

#define CX_JOURNAL_VERSION 1
#define CX_JOURNAL_FILE "journal.jsonl"

typedef enum { CX_JOURNAL_NEW, CX_JOURNAL_RESUME, CX_JOURNAL_REPLAY } cx_journal_mode;

typedef struct cx_journal cx_journal;

/*
 * Opens the journal in `dir`. NEW creates the directory and writes the
 * header (`header_json`, a complete JSON object, written as the first
 * line). RESUME and REPLAY load the entries and
 * check that the program is the same (`ir_hash`). On failure returns NULL
 * and writes the reason to `err`.
 */
cx_journal *cx_journal_open(cx_arena *a, const char *dir, cx_journal_mode mode,
                            const char *ir_hash, const char *header_json, cx_buf *err);

cx_journal_mode cx_journal_get_mode(const cx_journal *j);

/*
 * The answer recorded for `key`, or NULL. When the key exists with another
 * request, sets *mismatch and returns NULL.
 */
cx_value *cx_journal_lookup(cx_journal *j, const char *key, const char *req_hash, int *mismatch);

/* A `write once` call started and never finished: its outcome is unknown. */
int cx_journal_uncertain(const cx_journal *j, const char *key);

/* Creates `dir` and its parents; 0 on success. */
int cx_mkdirs(const char *dir);

/* Records that a `write once` call is about to start (synced to disk). */
int cx_journal_begin(cx_journal *j, const char *key, const char *req_hash);

/* Records a completed call. `ok_json` is the answer. Returns 0 on I/O error. */
int cx_journal_record(cx_journal *j, const char *key, const char *effect, const char *req_hash,
                      const char *ok_json, size_t ok_len);

/* Records the result or the error of the run. */
void cx_journal_end(cx_journal *j, const char *ok_json, const char *error);

/* Syncs and closes. */
void cx_journal_close(cx_journal *j);

#endif /* CALYX_JOURNAL_H */
