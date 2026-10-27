/*
 * compat.h - the few POSIX calls the runtime makes, on Windows.
 *
 * The runtime is written against POSIX. On Windows (MinGW-w64, target
 * x86_64-pc-windows-gnu) pthreads, clock_gettime and nanosleep come with
 * the toolchain; what does not is here, with the same guarantees:
 *
 * - flock: LockFileEx on the whole file (one entity's message at a time,
 *   across processes);
 * - fsync: FlushFileBuffers (through _commit): written means on disk;
 * - rename over an existing file: MoveFileEx with REPLACE_EXISTING and
 *   WRITE_THROUGH (POSIX rename replaces atomically; Windows' rename
 *   refuses);
 * - creating a file that must not exist yet: O_EXCL (MSVCRT's fopen does
 *   not know "x");
 * - O_BINARY: without it, open() writes "\r\n" for every "\n".
 *
 * Include it after the system headers.
 */
#ifndef CALYX_COMPAT_H
#define CALYX_COMPAT_H

#include <fcntl.h>
#include <stdio.h>
#include <string.h>

#ifdef _WIN32

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#define NOGDI
#include <direct.h>
#include <io.h>
#include <time.h>
#include <windows.h>

#define LOCK_SH 1
#define LOCK_EX 2
#define LOCK_UN 8

static inline int cx_flock(int fd, int op) {
    HANDLE h = (HANDLE)_get_osfhandle(fd);
    if (h == INVALID_HANDLE_VALUE) return -1;
    OVERLAPPED o;
    memset(&o, 0, sizeof o);
    if (op & LOCK_UN) return UnlockFileEx(h, 0, MAXDWORD, MAXDWORD, &o) ? 0 : -1;
    DWORD flags = (op & LOCK_EX) ? LOCKFILE_EXCLUSIVE_LOCK : 0;
    return LockFileEx(h, flags, 0, MAXDWORD, MAXDWORD, &o) ? 0 : -1;
}
#define flock cx_flock

#define fsync _commit
#define mkdir(path, mode) _mkdir(path)

static inline int cx_rename(const char *from, const char *to) {
    return MoveFileExA(from, to, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) ? 0 : -1;
}
#define rename cx_rename

static inline int cx_truncate(const char *path, long long len) {
    int fd = _open(path, _O_RDWR | _O_BINARY);
    if (fd < 0) return -1;
    int r = _chsize_s(fd, len) == 0 ? 0 : -1;
    _close(fd);
    return r;
}
#define truncate cx_truncate

#define gmtime_r(t, tm) (gmtime_s((tm), (t)) == 0 ? (tm) : NULL)

static inline long cx_cpus(void) {
    SYSTEM_INFO s;
    GetSystemInfo(&s);
    return (long)s.dwNumberOfProcessors;
}

#define CX_O_BINARY O_BINARY

#else /* POSIX */

#include <unistd.h>

static inline long cx_cpus(void) { return sysconf(_SC_NPROCESSORS_ONLN); }

#define CX_O_BINARY 0

#endif

/* The last part of a path (`.calyx/runs/<id>`, or `.calyx\\runs\\<id>` on
 * Windows): the run's id. */
static inline const char *cx_basename(const char *path) {
    const char *base = path;
    for (const char *p = path; *p; p++)
        if (*p == '/' || *p == '\\') base = p + 1;
    return base;
}

/* A new file opened for appending that must not exist yet; NULL if it does. */
static inline FILE *cx_create_new(const char *path) {
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | CX_O_BINARY, 0644);
    if (fd < 0) return NULL;
    FILE *f = fdopen(fd, "wb");
    if (!f) close(fd);
    return f;
}

#endif
