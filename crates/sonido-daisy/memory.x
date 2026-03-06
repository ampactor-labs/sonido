/* STM32H750IBK6 Memory Layout — Daisy Seed (Electrosmith bootloader, QSPI XIP) */
MEMORY
{
    /* QSPI Flash — XIP via Electrosmith bootloader.
       Bootloader occupies internal flash (0x08000000) and first 256 KB
       of QSPI (0x90000000–0x9003FFFF). User program at 0x90040000.
       Bootloader inits QSPI memory-mapped mode before jumping here. */
    FLASH  (rx)  : ORIGIN = 0x90040000, LENGTH = 7936K

    /* DTCM RAM — 128 KB, 0-wait state, data only (no execute).
       cortex-m-rt places .bss, .data, and stack here (via RAM).
       Stack grows down from top of DTCM (0x20020000). */
    RAM    (rwx) : ORIGIN = 0x20000000, LENGTH = 128K

    /* AXI SRAM — 512 KB, 0-1 wait state.
       Used for: heap, delay lines, reverb buffers. */
    AXISRAM (rwx) : ORIGIN = 0x24000000, LENGTH = 512K

    /* D2 SRAM1+2+3 — 288 KB total, DMA-accessible (SAI audio buffers) */
    SRAM1  (rwx) : ORIGIN = 0x30000000, LENGTH = 128K
    SRAM2  (rwx) : ORIGIN = 0x30020000, LENGTH = 128K
    SRAM3  (rwx) : ORIGIN = 0x30040000, LENGTH = 32K

    /* D3 SRAM4 — 64 KB, low-power domain */
    SRAM4  (rwx) : ORIGIN = 0x38000000, LENGTH = 64K

    /* External SDRAM — 64 MB, 4-8 wait state */
    SDRAM  (rwx) : ORIGIN = 0xC0000000, LENGTH = 64M
}

/* Stack at top of DTCM (= top of RAM region) — zero-wait-state access */
_stack_start = ORIGIN(RAM) + LENGTH(RAM);
