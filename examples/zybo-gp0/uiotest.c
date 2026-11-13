// Board test of the register block in examples/zybo-gp0 (and de25-lw, the
// same map) through /dev/uioN: the ID word, the counter's rate, a walking-bit
// test of the scratch register and the interrupt it raises, woken on in read().
//
//   zig cc -target arm-linux-musleabihf -static -O2 uiotest.c -o uiotest   # Zybo
//   zig cc -target aarch64-linux-musl -static -O2 uiotest.c -o uiotest      # DE25-Nano
//   ./uiotest /dev/uio0
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *dev = argc > 1 ? argv[1] : "/dev/uio0";
    int fd = open(dev, O_RDWR);
    if (fd < 0) { perror(dev); return 1; }
    volatile uint32_t *r = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (r == MAP_FAILED) { perror("mmap"); return 1; }
    printf("id      %08x\n", r[0]);
    uint32_t c0 = r[2]; usleep(100000); uint32_t c1 = r[2];
    printf("counter %08x -> %08x: %.3f MHz\n", c0, c1, (c1 - c0) / 100000.0);
    int bad = 0;
    for (uint32_t v = 1; v; v <<= 1) { r[1] = v; if (r[1] != v) bad++; r[1] = ~v; if (r[1] != ~v) bad++; }
    r[1] = 0xC0FFEE42;
    printf("scratch %08x, %d walking-bit errors\n", r[1], bad);
    // The interrupt: unmask (write 1), raise it, block in read().
    for (int k = 0; k < 3; k++) {
        uint32_t one = 1, count;
        r[3] = 0;
        write(fd, &one, 4);
        struct timespec t0, t1;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        r[3] = 1;
        struct pollfd p = { fd, POLLIN, 0 };
        int n = poll(&p, 1, 1000);
        if (n == 1 && read(fd, &count, 4) == 4) {
            clock_gettime(CLOCK_MONOTONIC, &t1);
            printf("irq     #%u after %.1f us\n", count,
                   (t1.tv_sec - t0.tv_sec) * 1e6 + (t1.tv_nsec - t0.tv_nsec) / 1e3);
        } else {
            printf("irq     none within 1 s\n");
        }
        r[3] = 0;
    }
    return 0;
}
