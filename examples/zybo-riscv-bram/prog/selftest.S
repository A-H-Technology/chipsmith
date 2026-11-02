# The program zybo-riscv-bram runs out of its block RAM (RV32I). Each pass
# fills 64 words of the same RAM with a pattern of the pass number, a
# byte of each overwritten with sb, reads them back with lw and lbu and
# counts what differs; then it stores the passes and the errors to the
# I/O register at 0x10000000.
    .text
    .globl _start
_start:
    lui   x1, 0x10000          # the I/O register
    li    x5, 0                # passes
    li    x11, 0               # errors
    li    x12, 0xffff00ff
loop:
    li    x2, 0x400            # the data, words 256 to 319
    li    x4, 64
    li    x3, 0
fill:
    slli  x6, x3, 3
    xor   x6, x6, x3
    add   x6, x6, x5
    slli  x7, x3, 2
    add   x7, x7, x2
    sw    x6, 0(x7)
    sb    x3, 1(x7)            # byte 1 := i
    addi  x3, x3, 1
    bne   x3, x4, fill
    li    x3, 0
check:
    slli  x6, x3, 3
    xor   x6, x6, x3
    add   x6, x6, x5
    and   x6, x6, x12
    andi  x13, x3, 0xff
    slli  x14, x13, 8
    or    x6, x6, x14
    slli  x7, x3, 2
    add   x7, x7, x2
    lw    x8, 0(x7)
    beq   x8, x6, 1f
    addi  x11, x11, 1
1:  lbu   x9, 1(x7)
    beq   x9, x13, 2f
    addi  x11, x11, 1
2:  addi  x3, x3, 1
    bne   x3, x4, check
    addi  x5, x5, 1
    sw    x5, 0(x1)
    sw    x11, 4(x1)
    j     loop
