// Board test of examples/de25-f2h: the fabric writing and reading this
// process's own memory through the FPGA-to-HPS bridge, which goes through
// the CCU and so is coherent with the CPUs' caches. Nothing here flushes
// or invalidates anything: if the bridge weren't coherent, the checks would
// see stale lines.
//
// Per 4 KiB page of a locked buffer (its physical address from
// /proc/self/pagemap, so run as root): the core writes its pattern (seed
// + 8 i + k in lane k of beat i) and the CPU checks it through its cached
// mapping; then the CPU writes another seed's pattern and the core reads
// it back and counts what's wrong. The core's registers are on the
// lightweight bridge at 0x20000000 (src/f2h.v).
//
//   zig cc -target aarch64-linux-musl -static -O2 f2htest.c -o f2htest
//   ./f2htest [ATTR [SID]]
//
// ATTR is the core's 0x20 register: AxPROT bits 2:0, AxCACHE 7:4,
// AxDOMAIN 9:8 (default 0x1F2: non-secure, write-back allocate, inner
// shareable). SID is its 0x2C register, the SMMU stream (default 11, the
// one nixos-fpga's hardware.terasic.de25-nano.fpga2hpsStream puts in an
// identity domain; any other stream faults or is aborted, SLVERR).
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#define PAGES 16
#define PAGE 4096
#define BEATS (PAGE / 32)

static volatile uint32_t *r;

static uint64_t phys(void *v) {
    int fd = open("/proc/self/pagemap", O_RDONLY);
    if (fd < 0) { perror("pagemap"); exit(1); }
    uint64_t e = 0;
    if (pread(fd, &e, 8, (uintptr_t)v / PAGE * 8) != 8) { perror("pagemap read"); exit(1); }
    close(fd);
    if (!(e >> 63) || !(e & ((1ull << 55) - 1))) {
        fprintf(stderr, "no physical address for %p (run as root)\n", v);
        exit(1);
    }
    return (e & ((1ull << 55) - 1)) * PAGE;
}

// Start a command (1 write, 2 read) and wait for the core to go idle.
static int run(uint32_t cmd) {
    r[4] = cmd;
    struct timespec t0, t;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    while (r[5] >> 31) {
        clock_gettime(CLOCK_MONOTONIC, &t);
        if (t.tv_sec - t0.tv_sec > 1) return -1;
    }
    return 0;
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    uint32_t attr = argc > 1 ? strtoul(argv[1], 0, 0) : 0x1F2;
    uint32_t sid = argc > 2 ? strtoul(argv[2], 0, 0) : 11;
    int fd = open("/dev/mem", O_RDWR | O_SYNC);
    if (fd < 0) { perror("/dev/mem"); return 1; }
    r = mmap(0, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0x20000000);
    if (r == MAP_FAILED) { perror("mmap"); return 1; }
    if (r[0] != 0xF2480000) { printf("id %08x, not de25-f2h\n", r[0]); return 1; }

    uint32_t *buf = aligned_alloc(PAGE, PAGES * PAGE);
    memset(buf, 0, PAGES * PAGE);
    if (mlock(buf, PAGES * PAGE)) { perror("mlock"); return 1; }
    r[8] = attr;
    r[11] = sid;
    printf("attr %03x sid %05x\n", r[8], r[11]);

    int fails = 0;
    for (int p = 0; p < PAGES; p++) {
        uint32_t *pg = buf + p * PAGE / 4;
        uint64_t pa = phys(pg);
        if (pa >> 32) { printf("page %d at %llx: above 4 GiB, skipped\n", p, (unsigned long long)pa); continue; }
        // The fabric writes, the CPU checks.
        uint32_t seed = 0x10000000u * (p + 1) + 0x1234;
        r[1] = pa; r[2] = BEATS; r[3] = seed;
        if (run(1)) { printf("page %d: write timed out (written %u)\n", p, r[5] & 0xffff); return 1; }
        int bad = 0;
        for (int i = 0; i < PAGE / 4; i++)
            if (pg[i] != seed + i) {
                if (bad < 3) printf("  page %d word %d %08x, want %08x\n", p, i, pg[i], seed + i);
                bad++;
            }
        uint32_t resp = r[7], last = r[10];
        // The CPU writes, the fabric checks.
        seed ^= 0x0F0F0000;
        for (int i = 0; i < PAGE / 4; i++) pg[i] = seed + i;
        r[3] = seed;
        if (run(2)) { printf("page %d: read timed out (read %u)\n", p, r[9]); return 1; }
        uint32_t wrong = r[6], readn = r[9], resp2 = r[7], last2 = r[10];
        printf("page %2d at %08llx: fabric->cpu %d wrong words, B errs %u; cpu->fabric %u wrong of %u beats, R errs %u; last bad B/R %04x/%04x\n",
               p, (unsigned long long)pa, bad, resp >> 16, wrong, readn, resp2 & 0xffff, last2 >> 16, last2 & 0xffff);
        (void)last;
        fails += bad || wrong || readn != BEATS || (resp >> 16) || (resp2 & 0xffff);
    }
    printf("%s: %d of %d pages wrong\n", fails ? "FAIL" : "ok", fails, PAGES);
    return fails != 0;
}
