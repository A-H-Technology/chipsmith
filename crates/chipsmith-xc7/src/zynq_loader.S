@ chipsmith's Zynq-7000 first-stage loader: the BootROM copies it into OCM
@ at 0 and jumps there in ARM state. It configures the PL from the
@ bitstream that follows it in QSPI flash, through the PCAP, and parks.
@ No DDR, PLL or MIO setup: a PL-only design needs none, and the BootROM
@ has already set up the QSPI's clock and pins to load this.
@
@ The same sequence as crates/chipsmith-jtag/examples/zynq_pcap.rs, which
@ checked it on the Zybo from the host. The last two words are patched by
@ chipsmith_xc7::zynq::qspi_boot_image: the bitstream's linear-window
@ address (bit 0 set: the DMA's last transfer) and its length in words.
@
@ Rebuild (in the devshell):
@   arm-none-eabi-as -o zynq_loader.o zynq_loader.S
@   arm-none-eabi-objcopy -O binary zynq_loader.o zynq_loader.bin

    .arm
    .syntax unified
    .text
    .global _start
_start:
    @ The QSPI controller in linear mode with plain READ (0x03): the
    @ controller drives CS and starts transfers itself (Config_reg bits 16,
    @ 15, 14 and 10 clear); the BootROM's clock divider and the rest stay.
    ldr     r0, =0xe000d000
    mov     r1, #0
    str     r1, [r0, #0x14]         @ disable
    ldr     r1, [r0]
    bic     r1, r1, #0x1c400
    str     r1, [r0]
    ldr     r1, =0x80000003
    str     r1, [r0, #0xa0]         @ LQSPI_CFG
    mov     r1, #1
    str     r1, [r0, #0x14]         @ enable

    @ devcfg (UG585 6.4.2): unlock, PCAP selected, no loopback.
    ldr     r0, =0xf8007000
    ldr     r1, =0x757bdf0d
    str     r1, [r0, #0x34]         @ UNLOCK
    mvn     r2, #0
    str     r2, [r0, #0x0c]         @ clear INT_STS
    ldr     r1, [r0]
    orr     r1, r1, #0x0c000000     @ PCAP_PR | PCAP_MODE
    str     r1, [r0]
    ldr     r2, [r0, #0x80]
    bic     r2, r2, #0x10           @ PCAP_LPBK off
    str     r2, [r0, #0x80]

    @ Pulse PROG_B and wait for INIT to fall and rise again.
    orr     r1, r1, #0x40000000
    str     r1, [r0]
    bic     r1, r1, #0x40000000
    str     r1, [r0]
1:  ldr     r2, [r0, #0x14]
    tst     r2, #0x10
    bne     1b
    orr     r1, r1, #0x40000000
    str     r1, [r0]
2:  ldr     r2, [r0, #0x14]
    tst     r2, #0x10
    beq     2b

    @ One DMA from the linear window to the PCAP.
    mvn     r2, #0
    str     r2, [r0, #0x0c]         @ clear INT_STS
    ldr     r2, bitstream_src
    str     r2, [r0, #0x18]         @ DMA_SRC_ADDR
    mvn     r2, #0
    str     r2, [r0, #0x1c]         @ DMA_DST_ADDR: the PCAP, last transfer
    ldr     r2, bitstream_words
    str     r2, [r0, #0x20]         @ DMA_SRC_LEN
    str     r2, [r0, #0x24]         @ DMA_DST_LEN
3:  ldr     r2, [r0, #0x0c]
    tst     r2, #0x2000             @ DMA_DONE_INT
    beq     3b
4:  ldr     r2, [r0, #0x0c]
    tst     r2, #0x4                @ PCFG_DONE_INT
    beq     4b

    @ PS-PL level shifters on, as Xilinx's FSBL leaves them, so a design
    @ that later talks to the PS can.
    ldr     r0, =0xf8000008
    ldr     r1, =0xdf0d
    str     r1, [r0]                @ SLCR_UNLOCK
    ldr     r0, =0xf8000900
    mov     r1, #0xf
    str     r1, [r0]                @ LVL_SHFTR_EN

5:  wfe
    b       5b

    .ltorg
bitstream_src:
    .word   0
bitstream_words:
    .word   0
