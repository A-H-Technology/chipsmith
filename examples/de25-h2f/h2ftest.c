// Board test of examples/de25-h2f through /dev/mem (the HPS-to-FPGA
// bridge at 0x40000000): the memory written and read back with 32-, 64-
// and 128-bit accesses and memcpy, checked word for word, and the core's
// own count of the multi-beat bursts it saw. The bridge is 128 bits wide,
// so a narrower write is one beat with some strobes, and memcpy's 32-byte
// accesses are bursts.
//
//   zig cc -target aarch64-linux-musl -static -O2 h2ftest.c -o h2ftest
//   ./h2ftest
#include <arm_neon.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define WORDS 128  /* 32 words of 128 bits */

static uint32_t rng = 0x12345678;
static uint32_t next(void) { rng ^= rng << 13; rng ^= rng >> 17; rng ^= rng << 5; return rng; }

static int check(volatile uint32_t *m, const uint32_t *want, const char *how) {
    int bad = 0;
    for (int i = 0; i < WORDS; i++)
        if (m[i] != want[i]) {
            if (bad < 4) printf("  %s: word %d %08x, want %08x\n", how, i, m[i], want[i]);
            bad++;
        }
    printf("%-28s %d wrong of %d words\n", how, bad, WORDS);
    return bad;
}

int main(int argc, char **argv) {
    (void)argc; (void)argv;
    setvbuf(stdout, NULL, _IONBF, 0);
    int fd = open("/dev/mem", O_RDWR | O_SYNC);
    if (fd < 0) { perror("/dev/mem"); return 1; }
    volatile uint32_t *r = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0x40000000);
    if (r == MAP_FAILED) { perror("mmap"); return 1; }
    // Each register is a 128-bit word: 0x1000, 0x1010, ... (u32 index 4k).
    volatile uint32_t *m = r, *reg = r + 0x400;
#define REG(k) reg[4 * (k)]
    printf("id %08x\n", REG(0));
    if (REG(0) != 0x48324600) { printf("not examples/de25-h2f\n"); return 1; }
    REG(4) = 1;  // clear the burst counts
    uint32_t want[WORDS];
    int bad = 0;

    for (int i = 0; i < WORDS; i++) m[i] = want[i] = next();
    bad += check(m, want, "32-bit writes");

    volatile uint64_t *m64 = (volatile uint64_t *)m;
    for (int i = 0; i < WORDS / 2; i++) {
        uint64_t v = ((uint64_t)next() << 32) | next();
        m64[i] = v;
        want[2 * i] = (uint32_t)v;
        want[2 * i + 1] = (uint32_t)(v >> 32);
    }
    bad += check(m, want, "64-bit writes");

    for (int i = 0; i < WORDS; i += 4) {
        uint32_t w[4] = {next(), next(), next(), next()};
        vst1q_u32((uint32_t *)&m[i], vld1q_u32(w));
        memcpy(&want[i], w, 16);
    }
    bad += check(m, want, "128-bit writes");

    // Reads the same widths, of what's there.
    int rbad = 0;
    for (int i = 0; i < WORDS / 2; i++) {
        uint64_t v = m64[i];
        if ((uint32_t)v != want[2 * i] || (uint32_t)(v >> 32) != want[2 * i + 1]) rbad++;
    }
    printf("%-28s %d wrong of %d pairs\n", "64-bit reads", rbad, WORDS / 2);
    bad += rbad;
    rbad = 0;
    for (int i = 0; i < WORDS; i += 4) {
        uint32_t w[4];
        vst1q_u32(w, vld1q_u32((const uint32_t *)&m[i]));
        if (memcmp(w, &want[i], 16)) rbad++;
    }
    printf("%-28s %d wrong of %d quads\n", "128-bit reads", rbad, WORDS / 4);
    bad += rbad;

    // 64-byte copies both ways (four NEON registers a load or store), the
    // bursts a driver moving a buffer makes. musl's memcpy faults on a
    // /dev/mem device mapping.
    for (int i = 0; i < WORDS; i++) want[i] = next();
    for (int i = 0; i < WORDS; i += 16)
        vst1q_u32_x4((uint32_t *)&m[i], vld1q_u32_x4(&want[i]));
    uint32_t back[WORDS];
    for (int i = 0; i < WORDS; i += 16)
        vst1q_u32_x4(&back[i], vld1q_u32_x4((const uint32_t *)&m[i]));
    rbad = memcmp(back, want, sizeof want) != 0;
    bad += check(m, want, "64-byte writes") + rbad;
    printf("%-28s %s\n", "64-byte reads", rbad ? "differ" : "same");

    // Byte and halfword strobes inside the words.
    volatile uint8_t *b = (volatile uint8_t *)m;
    for (int i = 0; i < WORDS; i++) { uint8_t v = next(); b[4 * i + (i & 3)] = v; ((uint8_t *)want)[4 * i + (i & 3)] = v; }
    bad += check(m, want, "byte writes");

    uint32_t wb = REG(1), rb = REG(2), mx = REG(3);
    printf("bursts seen: %u writes, %u reads of more than one beat; longest %u and %u beats\n",
           wb, rb, ((mx >> 8) & 0xff) + 1, (mx & 0xff) + 1);
    if (wb == 0 || rb == 0) { printf("no multi-beat bursts: the test didn't test bursts\n"); return 1; }
    printf(bad ? "FAIL: %d errors\n" : "every burst wrote and read its words\n", bad);
    return bad != 0;
}
