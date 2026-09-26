/* Memory layout for LPC55S69 Core 0 */
MEMORY
{
  /* The LPC55S69 has 640KB Flash starting at 0x0 */
  FLASH (rx) : ORIGIN = 0x00000000, LENGTH = 640K

  /* SRAMX is executable SRAM at 0x04000000 for hot code copied at reset. */
  SRAM (rx) : ORIGIN = 0x04000000, LENGTH = 32K

  /* RAM is split into multiple blocks; 0x20000000 is the main SRAM */
  RAM (xrw)  : ORIGIN = 0x20000000, LENGTH = 256K
}

SECTIONS
{
  .sram_code : ALIGN(4)
  {
    . = ALIGN(4);
    __ssram_code = .;
    KEEP(*(.data.sram_code .data.sram_code.*));
    . = ALIGN(4);
    __esram_code = .;
  } > SRAM AT> FLASH

  __sisram_code = LOADADDR(.sram_code);
} INSERT AFTER .rodata;

ASSERT(__ssram_code % 4 == 0 && __esram_code % 4 == 0, "
BUG(lpc55s69): .sram_code is not 4-byte aligned");

ASSERT(__sisram_code % 4 == 0, "
BUG(lpc55s69): the LMA of .sram_code is not 4-byte aligned");
