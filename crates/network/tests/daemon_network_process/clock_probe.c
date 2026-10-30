#ifdef __APPLE__
#define _DARWIN_C_SOURCE
#else
#define _GNU_SOURCE
#endif
#include <stdio.h>
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <unistd.h>
#include <sys/time.h>
#include <time.h>

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "expect_locked") == 0) {
        const char *path = getenv("FOUNDRY_FIXTURE_CLOCK_DELTA");
        if (path == NULL) return 123;
        int fd = open(path, O_RDONLY | O_NOFOLLOW);
        if (fd < 0) return 123;
        int result = flock(fd, LOCK_SH | LOCK_NB);
        int blocked = result != 0 && (errno == EWOULDBLOCK || errno == EAGAIN);
        if (close(fd) != 0) return 123;
        return blocked ? 0 : 123;
    }
    struct timespec wall, monotonic;
    struct timeval legacy;
    if (clock_gettime(CLOCK_REALTIME, &wall) != 0 ||
        clock_gettime(CLOCK_MONOTONIC, &monotonic) != 0 ||
        gettimeofday(&legacy, NULL) != 0) return 122;
    printf("%lld %lld %lld\n", (long long)wall.tv_sec,
           (long long)legacy.tv_sec,
           (long long)monotonic.tv_sec * 1000000000LL + monotonic.tv_nsec);
    return 0;
}
