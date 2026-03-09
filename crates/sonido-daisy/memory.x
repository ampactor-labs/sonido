/* STM32H750IBK6 Memory Layout — Daisy Seed (Electrosmith bootloader, BOOT_SRAM)
 *
 * The Electrosmith bootloader stores the user binary in QSPI flash
 * (0x90040000) and copies it to AXI SRAM (0x24000000) on each boot.
 * Code executes from zero-wait-state SRAM, avoiding the QSPI XIP
 * clock conflict that prevents Embassy clock reconfiguration.
 *
 * Flash via DFU (same command for both BOOT_SRAM and BOOT_QSPI):
 *   dfu-util -a 0 -s 0x90040000:leave -D firmware.bin
 *
 * Max program size: 480 KB (bootloader reserves 32 KB at end of AXI SRAM).
 */
MEMORY
{
    /* AXI SRAM — code executes here after bootloader copy from QSPI.
       480 KB usable (bootloader reserves 0x24078000–0x24080000). */
    FLASH  (rx)  : ORIGIN = 0x24000000, LENGTH = 480K

    /* DTCM RAM — 128 KB, 0-wait state.
       cortex-m-rt places .bss, .data, and stack here (via RAM).
       Stack grows down from top of DTCM (0x20020000). */
    RAM    (rwx) : ORIGIN = 0x20000000, LENGTH = 128K

    /* D2 SRAM1 — 32 KB DMA-accessible (SAI audio DMA buffers) */
    RAM_D2_DMA (rwx) : ORIGIN = 0x30000000, LENGTH = 32K

    /* D2 SRAM2+3 — 256 KB DMA-accessible (general purpose) */
    RAM_D2 (rwx) : ORIGIN = 0x30008000, LENGTH = 256K

    /* D3 SRAM4 — 64 KB, low-power domain */
    SRAM4  (rwx) : ORIGIN = 0x38000000, LENGTH = 64K

    /* QSPI Flash — storage only, not executed (bootloader copies to SRAM) */
    QSPIFLASH (rx) : ORIGIN = 0x90040000, LENGTH = 7936K

    /* External SDRAM — 64 MB, 4-8 wait state */
    SDRAM  (rwx) : ORIGIN = 0xC0000000, LENGTH = 64M
}

/* Stack at top of DTCM — zero-wait-state access */
_stack_start = ORIGIN(RAM) + LENGTH(RAM);

/* SAI DMA buffers must be in DMA-accessible D2 SRAM1 (0x30000000).
 * Without this section, .sram1_bss becomes an orphan and the linker
 * places it at an unpredictable address → DMA bus fault → HardFault.
 * The -R .sram1_bss objcopy flag strips this section from the flashed
 * binary; GroundedArrayCell handles zero-init at runtime. */
SECTIONS
{
    .sram1_bss (NOLOAD) :
    {
        . = ALIGN(4);
        *(.sram1_bss)
        *(.sram1_bss*)
        . = ALIGN(4);
    } > RAM_D2_DMA

    .sdram_bss (NOLOAD) :
    {
        . = ALIGN(4);
        *(.sdram_bss)
        *(.sdram_bss*)
        . = ALIGN(4);
    } > SDRAM
}
