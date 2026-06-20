# ik8bvm

**An 8-bit AVR CPU core emulator written in Rust.**
**Author:** Isak Ruas
**License:** Apache License 2.0

---

## 1. Overview

ik8bvm is a software emulator of the 8-bit AVR CPU core (AVRe / AVRe+ /
AVRxm / AVRxt / AVRrc class). It decodes and executes AVR-8 machine code
loaded from standard Intel `.hex` files and models the register file, status
flags, and instruction cycle counts.


The decoder implements AVR instruction groups including pointer-based
loads/stores, the multiply family, program-memory access
(`LPM`/`ELPM`/`SPM`), indirect jumps/calls, the XMEGA atomic
read-modify-write instructions, and the XMEGA `DES` round. It also models a
number of peripherals: classic and modern EEPROM access; Timer0/Timer1 with
overflow and output-compare A/B interrupts; the USART-RX, ADC-complete, SPI-STC
and TWI completion interrupts; external INT0/INT1 and pin-change (PCINT)
interrupts; and the ready bits of the SPI/TWI/UART status registers (see §3 and §7).
Timer/interrupt register addresses, vectors and trigger pins are taken per device
from the toolchain's own data (std register maps + the Microchip ATDF pinouts).
The SREG (status-flag) semantics of every ALU instruction are checked against
the AVR Instruction Set Manual by an exhaustive conformance test (see §8).

### Project layout

| Path | Purpose |
| :--- | :--- |
| `src/lib.rs` | Library crate root; re-exports `AvrVm`. |
| `src/core.rs` | `AvrVm` struct, unified memory map, EEPROM, timer and interrupt logic. |
| `src/decode.rs` | Instruction decoder and execution engine. |
| `src/disasm.rs` | Trace disassembler (renders each instruction as text). |
| `src/devices.rs` | `AvrCoreClass` and the `AVR_DEVICE_TABLE` of device presets. |
| `src/hw.rs` | Intel `.hex` loader and per-device peripheral register maps. |
| `src/main.rs` | Command-line driver: loads a `.hex` file and runs it. |
| `Makefile` | Docker-based `cargo build` / `cargo clean` wrappers. |

---

## 2. Building and running

ik8bvm builds with a standard Rust toolchain. The `Makefile` wraps Cargo in
Docker so no local toolchain is required:

```bash
make build    # cargo build --release (via Docker)
make clean    # cargo clean (via Docker)
```

Or, with Cargo directly:

```bash
cargo build --release   # build the library and the CLI
cargo test              # run the unit tests
```

The CLI is organised as subcommands:

```bash
./target/release/ik8bvm run <file.hex> --mcu <device> [options]
./target/release/ik8bvm devices     # list supported device presets
./target/release/ik8bvm version
./target/release/ik8bvm help
```

A bare `./target/release/ik8bvm <file.hex> [options]` is accepted as a shorthand
for `run`.

| Option | Effect |
| :--- | :--- |
| `--mcu <device>` | Select a device preset (core class + memory layout). |
| `--trace`, `-t` | Trace: print each instruction (with disassembly) as it executes. |
| `--limit <N>`, `-n <N>` | Stop after `N` instructions (default: run until halt). |
| `--dump`, `-d` | Dump the register file and SREG at exit. |
| `--peek <ADDR>` | Read and print one data-space byte at `ADDR` at exit. |
| `--peek-len <N>` | Read `N` consecutive bytes starting at `--peek ADDR`. |
| `--irq <VEC>` | Queue interrupt vector index `VEC` (repeatable; `1..255`). |
| `--irq-at <VEC:STEP>` | Queue vector `VEC` when executed instruction count reaches `STEP`. |
| `--irq-every <VEC:N>` | Queue vector `VEC` every `N` executed instructions. |

Example:

```bash
./target/release/ik8bvm run prog.hex --mcu atmega32 --dump
./target/release/ik8bvm devices
./target/release/ik8bvm run prog.hex --mcu atmega328p --irq 1 --trace
./target/release/ik8bvm run prog.hex --mcu atmega328p --irq-at 14:5000 --irq-every 14:10000
```

The CLI exit code is `0` on normal completion and `2` if the core halted on
an unknown/illegal opcode for the selected core.

### Using it as a library

```rust
use ik8bvm::core::AvrVm;
use ik8bvm::devices::{AvrCoreClass, AVR_DEVICE_TABLE};

let dev = AVR_DEVICE_TABLE.iter().find(|d| d.name == "atmega328p").unwrap();
let mut vm = AvrVm::new(
    dev.name.to_string(), dev.core,
    dev.flash_bytes, dev.sram_bytes, dev.eeprom_bytes, dev.sram_start,
);
vm.sp = dev.ram_end as u16;
ik8bvm::hw::load_hex(&mut vm, "prog.hex").unwrap();

vm.trace = true;            // capture an instruction trace into vm.trace_buf
vm.trace_limit = 50_000;    // optional cap to bound memory (0 = unlimited)
while vm.running {
    vm.step();
}
// Inspect vm.r, vm.pc, vm.sp, vm.sreg, vm.cycles, vm.trace_buf, ...
```

When tracing is enabled the disassembled lines are collected in
`vm.trace_buf` (instead of being printed), so a host such as the IDE can
render them; the CLI drains that buffer to stdout for `-t`.

---

## 3. CPU architecture and memory

  * **General-purpose registers:** 32 8-bit registers, R0–R31.
  * **Program counter (PC):** byte address into flash, held as `u32`
    (instructions are 16-bit words, so the PC is always even).
  * **Stack pointer (SP):** 16-bit, initialized to the top of SRAM. Push
    post-decrements, pop pre-increments.
  * **Status register (SREG):** 8 flag bits.
  * **Program memory (flash):** device-dependent (from selected `-mmcu` preset).
  * **Data memory (SRAM):** device-dependent (from selected `-mmcu` preset).
  * **EEPROM:** device-dependent; reachable from emulated code through the
    classic `EECR`/`EEDR`/`EEAR` registers and the modern (XMEGA / megaAVR-0)
    NVM controller, with programming-cycle timing modeled.
  * **I/O space:** device-dependent window `[0x20, RAMSTART)`, reachable through
    `IN`/`OUT` and the unified data map. SPI/TWI/UART status reads return their
    "ready" bits set so simple polling loops make progress.
  * **Timers & interrupts:** Timer0/Timer1 count at their prescaler and raise
    overflow / output-compare-A/B interrupts; USART-RX, ADC-complete, SPI-STC and
    TWI completion interrupts fire from the matching peripheral event; external
    INT0/INT1 and pin-change (PCINT) interrupts fire from the watched pins. Every
    source is gated by its enable bit and the global I flag, exactly as on the
    part, so an interrupt-driven program runs without any host intervention. Per
    device, the registers/vectors/bits come from the toolchain's own data and the
    Microchip ATDF files; a self-loop (`RJMP .-2`) with interrupts enabled is
    treated as an idle wait, not a halt.
  * **Extended addressing:** `RAMPX`, `RAMPY`, `RAMPZ` extend the X/Y/Z
    pointers; `RAMPZ` extends `ELPM`; `EIND` extends `EICALL`/`EIJMP`.

---

## 4. Data memory map

All data access (everything except program flash) goes through
`read_data` / `write_data` over a single unified address space. The exact
boundaries are preset-dependent (`-mmcu`):

| Address range | Size | Description |
| :--- | :--- | :--- |
| `0x0000 – 0x001F` | 32 B | Register file (R0–R31) |
| `0x0020 – RAMSTART-1` | device-dependent | I/O register space |
| `RAMSTART – RAMEND` | device-dependent | Internal SRAM |

Without `-mmcu`, a generic preset is used.

---

## 5. Status register (SREG)

| Bit | 7 | 6 | 5 | 4 | 3 | 2 | 1 | 0 |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Flag** | I | T | H | S | V | N | Z | C |

  * **I** – global interrupt enable
  * **T** – bit copy storage (`BST`/`BLD`)
  * **H** – half-carry
  * **S** – sign (N ⊕ V)
  * **V** – two's complement overflow
  * **N** – negative
  * **Z** – zero
  * **C** – carry

---

## 6. Implemented instruction set

### 6.1 Arithmetic and logic

| Mnemonic | Description |
| :--- | :--- |
| `ADD` / `ADC` | Add without / with carry |
| `ADIW` | Add immediate to word (R24/26/28/30) |
| `SUB` / `SBC` | Subtract without / with carry |
| `SUBI` / `SBCI` | Subtract immediate without / with carry |
| `SBIW` | Subtract immediate from word |
| `AND` / `ANDI` | Logical AND, register / immediate |
| `OR` / `ORI` | Logical OR, register / immediate |
| `EOR` | Exclusive OR |
| `COM` | One's complement |
| `NEG` | Two's complement negate |
| `INC` / `DEC` | Increment / decrement |
| `MUL` / `MULS` / `MULSU` | 8×8 multiply: unsigned / signed / signed·unsigned |
| `FMUL` / `FMULS` / `FMULSU` | Fractional multiply variants |

### 6.2 Branch, jump, and call

| Mnemonic | Description |
| :--- | :--- |
| `RJMP` / `IJMP` / `EIJMP` | Relative / indirect (Z) / extended indirect jump |
| `JMP` | Absolute 22-bit jump |
| `RCALL` / `ICALL` / `EICALL` | Relative / indirect / extended indirect call |
| `CALL` | Absolute 22-bit call |
| `RET` / `RETI` | Return from subroutine / interrupt |
| `CP` / `CPC` / `CPI` | Compare, with carry, with immediate |
| `CPSE` | Compare and skip if equal |
| `SBRC` / `SBRS` | Skip if register bit clear / set |
| `SBIC` / `SBIS` | Skip if I/O bit clear / set |
| `BRBS` / `BRBC` | Branch if SREG bit set / clear (covers `BREQ`, `BRNE`, …) |

### 6.3 Data transfer

| Mnemonic | Description |
| :--- | :--- |
| `MOV` / `MOVW` | Copy register / register pair |
| `LDI` | Load immediate (R16–R31) |
| `LD` / `ST` | Indirect load/store via X, Y, Z (with post-inc / pre-dec) |
| `LDD` / `STD` | Load/store with displacement, Y+q / Z+q |
| `LDS` / `STS` | Direct load/store, data space (16-bit address) |
| `LPM` / `ELPM` | Load from program memory (Z, extended via RAMPZ) |
| `SPM` | Store to program memory |
| `IN` / `OUT` | Read / write I/O register |
| `PUSH` / `POP` | Stack push / pop |
| `XCH` / `LAS` / `LAC` / `LAT` | XMEGA atomic read-modify-write on (Z) |

### 6.4 Bit and bit-test

| Mnemonic | Description |
| :--- | :--- |
| `LSR` / `ROR` / `ASR` | Logical / rotate-through-carry / arithmetic shift right |
| `SWAP` | Swap nibbles |
| `SBI` / `CBI` | Set / clear bit in I/O register |
| `BST` / `BLD` | Store register bit to T / load T into register bit |
| `BSET` / `BCLR` | Set / clear a SREG bit (covers `SEC`/`CLC`, `SEI`/`CLI`, …) |

### 6.5 MCU control

| Mnemonic | Description |
| :--- | :--- |
| `NOP` | No operation |
| `SLEEP` | Accepted; no-op in the VM (no sleep modes) |
| `WDR` | Watchdog reset; no-op in the VM |
| `BREAK` | Accepted; no-op in the VM (does not halt) |
| `DES` | One DES round on R0–R7 with key R8–R15; H selects encrypt/decrypt |

---

## 7. Limitations

The emulator models the **CPU core** plus a small set of peripherals, not a
complete microcontroller:

1.  **Partial peripherals.** EEPROM, Timer0/Timer1 (overflow + compare A/B),
    ADC conversion, and the SPI/TWI/UART data paths are modeled, but not every
    peripheral feature: there is no Timer input-capture / COMPC, no analog
    comparator, no watchdog timer, and the I/O space outside the modeled
    registers is a plain byte array.
2.  **Interrupts: peripheral-driven and manual.** `SEI`, `CLI`, and `RETI`
    manage the I flag; delivery is priority-dispatched (lowest vector first).
    The VM auto-raises Timer0/1 overflow & compare-A/B, USART-RX, ADC-complete,
    SPI-STC, TWI, external INT0/INT1 and pin-change (PCINT) interrupts from their
    events; the pending-vector queue (`--irq`, `--irq-at`, `--irq-every`) injects
    any other vector manually. Not yet auto-raised: INT2–7, UART UDRE/TX-complete,
    timer input-capture, analog-comparator, EE-/SPM-ready, watchdog.
3.  **`LDS`/`STS` use a 16-bit address.** `RAMPD` is not applied, so extended
    direct addressing only matters on devices with more than 64 KB of data
    space.
4.  **`SLEEP`, `WDR`, and `BREAK`** advance the PC but have no architectural
    side effects. `DES` is fully implemented.

---

## 8. Tests

Unit tests live alongside the sources (`#[cfg(test)]` modules in `core.rs`
and `decode.rs`). They load opcodes directly into flash, step the core, and
assert on register, flag, EEPROM, and peripheral state. In addition,
`tests/conformance.rs` is an **exhaustive SREG conformance suite**: for every
flag-affecting ALU instruction it recomputes the expected status bits directly
from the AVR Instruction Set Manual's boolean formulas and checks them across
the full operand space. Run them with:

```bash
cargo test
```
