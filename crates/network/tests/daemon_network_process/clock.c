#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <sys/file.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static _Thread_local int inside;
static void invalid_clock(void) {
    static const char message[] = "invalid private fixture clock input\n";
    (void)write(STDERR_FILENO, message, sizeof(message) - 1);
    _exit(121);
}
static int64_t delta(void) {
    const char *path = getenv("FOUNDRY_FIXTURE_CLOCK_DELTA");
    if (path == NULL) invalid_clock();
    int fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0 || flock(fd, LOCK_SH) != 0) invalid_clock();
    struct stat metadata;
    if (fstat(fd, &metadata) != 0 || !S_ISREG(metadata.st_mode) ||
        metadata.st_uid != geteuid() || (metadata.st_mode & 0777) != 0600 ||
        metadata.st_nlink != 1 || metadata.st_size < 2 || metadata.st_size > 24)
        invalid_clock();
    char bytes[25];
    ssize_t count = read(fd, bytes, sizeof(bytes) - 1);
    char extra;
    if (count != metadata.st_size || read(fd, &extra, 1) != 0) invalid_clock();
    if (close(fd) != 0) invalid_clock();
    bytes[count] = '\0';
    if (bytes[count - 1] != '\n') invalid_clock();
    int64_t value = 0;
    for (ssize_t i = 0; i < count - 1; ++i) {
        if (bytes[i] < '0' || bytes[i] > '9') invalid_clock();
        value = value * 10 + bytes[i] - '0';
        if (value > 31622400) invalid_clock();
    }
    return value;
}

#ifdef __APPLE__
static int fixture_clock_gettime(clockid_t id, struct timespec *value) {
    if (inside) return clock_gettime(id, value);
    inside = 1;
    int result = clock_gettime(id, value);
    if (result == 0 && id == CLOCK_REALTIME) value->tv_sec += delta();
    inside = 0;
    return result;
}
static int fixture_gettimeofday(struct timeval *value, void *zone) {
    if (inside) return gettimeofday(value, zone);
    inside = 1;
    int result = gettimeofday(value, zone);
    if (result == 0) value->tv_sec += delta();
    inside = 0;
    return result;
}
#define FIXTURE_INTERPOSE(replacement, original) \
    __attribute__((used, section("__DATA,__interpose"))) \
    static const struct { const void *next; const void *previous; } \
    interpose_##original = { (const void *)&replacement, (const void *)&original }
FIXTURE_INTERPOSE(fixture_clock_gettime, clock_gettime);
FIXTURE_INTERPOSE(fixture_gettimeofday, gettimeofday);
#else
int clock_gettime(clockid_t id, struct timespec *value) {
    int (*original)(clockid_t, struct timespec *) = dlsym(RTLD_NEXT, "clock_gettime");
    if (original == NULL) invalid_clock();
    if (inside) return original(id, value);
    inside = 1;
    int result = original(id, value);
    if (result == 0 && id == CLOCK_REALTIME) value->tv_sec += delta();
    inside = 0;
    return result;
}
int gettimeofday(struct timeval *value, void *zone) {
    int (*original)(struct timeval *, void *) = dlsym(RTLD_NEXT, "gettimeofday");
    if (original == NULL) invalid_clock();
    if (inside) return original(value, zone);
    inside = 1;
    int result = original(value, zone);
    if (result == 0) value->tv_sec += delta();
    inside = 0;
    return result;
}
#endif
