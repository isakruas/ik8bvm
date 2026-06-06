// Copyright 2026 The IKIDE Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use ik8bvm::core::AvrVm;
use ik8bvm::devices::{AvrCoreClass, AVR_DEVICE_TABLE};
use std::env;
use std::process;

const AVR_FLASH_BYTES: u32 = 256 * 1024;
const AVR_SRAM_BYTES: u32 = 16 * 1024;
const AVR_EEPROM_BYTES: u32 = 4 * 1024;
const AVR_SRAM_START: u32 = 0x100;

struct IrqAtEvent {
    vec: u8,
    step: u64,
    fired: bool,
}

struct IrqEveryEvent {
    vec: u8,
    period: u64,
    next_at: u64,
}

fn usage(prog: &str) {
    println!("ik8bvm {} - AVR-8 instruction-set simulator", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Usage: {} <command> [arguments]", prog);
    println!();
    println!("Commands:");
    println!("  run <file.hex>         Simulate a HEX image (a bare <file.hex> also works)");
    println!("  devices                List known devices and exit");
    println!("  version                Print version");
    println!("  help                   Show this help text");
    println!();
    println!("Simulation options (run) — shared with `ik8b sim`/`ik8b run`:");
    println!("  --mcu <device>         Preload memory/core config for <device>");
    println!("  --trace, -t            Enable instruction trace");
    println!("  --dump,  -d            Dump registers at exit");
    println!("  --limit, -n <N>        Stop after N instructions (default: unlimited)");
    println!("  --irq <V>              Queue interrupt vector V at startup (repeatable)");
    println!("  --irq-at <V:STEP>      Queue vector V once at instruction STEP");
    println!("  --irq-every <V:N>      Queue vector V every N instructions");
    println!("  --peek <addr>          Print data memory at <addr> on exit");
    println!("  --peek-len <N>         Number of bytes for --peek (default: 1)");
}

/// Consumes the value following an option flag, erroring out if it is missing.
fn take_value(args: &[String], i: &mut usize, label: &str, prog: &str) -> String {
    *i += 1;
    args.get(*i).cloned().unwrap_or_else(|| {
        eprintln!("Missing value for {}", label);
        usage(prog);
        process::exit(1);
    })
}

/// Parses a `VEC:NUMBER` pair for `--irq-at` / `--irq-every`.
fn parse_irq_pair(s: &str) -> Option<(u8, u64)> {
    let (v, n) = s.split_once(':')?;
    Some((parse_irq_vec(v)?, parse_u64(n)?))
}

fn parse_u64(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok();
    }
    if s.len() > 1 && s.starts_with('0') {
        return u64::from_str_radix(&s[1..], 8).ok();
    }
    s.parse().ok()
}

fn parse_irq_vec(s: &str) -> Option<u8> {
    let vec = parse_u64(s)?;
    if (1..=255).contains(&vec) {
        Some(vec as u8)
    } else {
        None
    }
}

fn core_name(core: AvrCoreClass) -> &'static str {
    match core {
        AvrCoreClass::RC => "AVRrc",
        AvrCoreClass::E => "AVRe",
        AvrCoreClass::EP => "AVRe+",
        AvrCoreClass::XT => "AVRxt",
        AvrCoreClass::XM => "AVRxm",
        AvrCoreClass::Unknown => "generic",
    }
}

fn list_mcus() {
    println!("{:<22} {:<7} {:>9} {:>8} {:>8}", "DEVICE", "CORE", "FLASH", "SRAM", "EEPROM");
    for d in AVR_DEVICE_TABLE {
        println!(
            "{:<22} {:<7} {:>8}B {:>7}B {:>7}B",
            d.name,
            core_name(d.core),
            d.flash_bytes,
            d.sram_bytes,
            d.eeprom_bytes
        );
    }
}

fn dump_regs(vm: &AvrVm) {
    println!(
        "Device: {} ({})  flash={} SRAM={} EEPROM={}",
        vm.device,
        core_name(vm.core),
        vm.flash_bytes,
        vm.sram_bytes,
        vm.eeprom_bytes
    );
    println!("Registers:");
    for i in 0..32 {
        print!("R{:<2} = 0x{:02X}", i, vm.r[i]);
        if i % 4 == 3 {
            println!();
        } else {
            print!(" | ");
        }
    }
    println!();
    println!("PC   = 0x{:06X}", vm.pc);
    println!("SP   = 0x{:04X}", vm.sp);
    println!(
        "SREG = 0x{:02X}  [{}{}{}{}{}{}{}{}]",
        vm.sreg,
        if vm.sreg & 0x80 != 0 { 'I' } else { '-' },
        if vm.sreg & 0x40 != 0 { 'T' } else { '-' },
        if vm.sreg & 0x20 != 0 { 'H' } else { '-' },
        if vm.sreg & 0x10 != 0 { 'S' } else { '-' },
        if vm.sreg & 0x08 != 0 { 'V' } else { '-' },
        if vm.sreg & 0x04 != 0 { 'N' } else { '-' },
        if vm.sreg & 0x02 != 0 { 'Z' } else { '-' },
        if vm.sreg & 0x01 != 0 { 'C' } else { '-' }
    );
    println!("Cycles = {}", vm.cycles);
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let prog = &args[0];

    let mut trace = false;
    let mut dump = false;
    let mut max_instr: u64 = 0;
    let mut mmcu: Option<String> = None;
    let mut hexfile: Option<String> = None;
    let mut peek_addr: Option<u32> = None;
    let mut peek_len: u32 = 1;
    let mut startup_irqs = Vec::new();
    let mut irq_at_events: Vec<IrqAtEvent> = Vec::new();
    let mut irq_every_events: Vec<IrqEveryEvent> = Vec::new();

    // Hierarchical dispatch, coherent with the ik8b CLI vocabulary.
    let mut start = 1;
    match args.get(1).map(String::as_str) {
        Some("devices") | Some("--list-devices") | Some("--list-mcus") => {
            list_mcus();
            return;
        }
        Some("version") | Some("-V") | Some("--version") => {
            println!("ik8bvm {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("help") | Some("-h") | Some("--help") => {
            usage(prog);
            return;
        }
        // `run <hex>` simulates an image; a bare `<hex>` is an implicit `run`.
        Some("run") => start = 2,
        _ => {}
    }

    let mut i = start;
    while i < args.len() {
        let arg = args[i].clone();
        match arg.as_str() {
            "--trace" | "-t" => trace = true,
            "--dump" | "-d" => dump = true,
            "--limit" | "-n" => max_instr = parse_u64(&take_value(&args, &mut i, "--limit", prog)).unwrap_or(0),
            "--mcu" => mmcu = Some(take_value(&args, &mut i, "--mcu", prog)),
            "--peek" => peek_addr = parse_u64(&take_value(&args, &mut i, "--peek", prog)).map(|v| v as u32),
            "--peek-len" => peek_len = parse_u64(&take_value(&args, &mut i, "--peek-len", prog)).unwrap_or(1) as u32,
            "--irq" => {
                let v = take_value(&args, &mut i, "--irq", prog);
                match parse_irq_vec(&v) {
                    Some(vec) => startup_irqs.push(vec),
                    None => { eprintln!("Invalid --irq vector '{}' (expected 1..255)", v); process::exit(1); }
                }
            }
            "--irq-at" => {
                let v = take_value(&args, &mut i, "--irq-at", prog);
                match parse_irq_pair(&v) {
                    Some((vec, step)) => irq_at_events.push(IrqAtEvent { vec, step, fired: false }),
                    None => { eprintln!("Invalid --irq-at '{}' (expected VEC:STEP)", v); process::exit(1); }
                }
            }
            "--irq-every" => {
                let v = take_value(&args, &mut i, "--irq-every", prog);
                match parse_irq_pair(&v) {
                    Some((_, 0)) => { eprintln!("Invalid --irq-every '{}' (PERIOD must be > 0)", v); process::exit(1); }
                    Some((vec, period)) => irq_every_events.push(IrqEveryEvent { vec, period, next_at: period }),
                    None => { eprintln!("Invalid --irq-every '{}' (expected VEC:PERIOD)", v); process::exit(1); }
                }
            }
            other if !other.starts_with('-') && hexfile.is_none() => hexfile = Some(other.to_string()),
            other if !other.starts_with('-') => {
                eprintln!("Multiple hex files specified");
                usage(prog);
                process::exit(1);
            }
            other => {
                eprintln!("Unknown option: {}", other);
                usage(prog);
                process::exit(1);
            }
        }
        i += 1;
    }

    let Some(hex) = hexfile else {
        usage(prog);
        process::exit(1);
    };

    let mut vm = if let Some(mmcu) = mmcu {
        let dev = AVR_DEVICE_TABLE
            .iter()
            .find(|d| d.name == mmcu)
            .unwrap_or_else(|| {
                eprintln!("Unknown device: {} (try --list-mcus)", mmcu);
                process::exit(1);
            });
        let mut vm = AvrVm::new(
            mmcu,
            dev.core,
            dev.flash_bytes,
            dev.sram_bytes,
            dev.eeprom_bytes,
            dev.sram_start,
        );
        vm.sp = dev.ram_end as u16;
        vm
    } else {
        AvrVm::new(
            "generic".to_string(),
            AvrCoreClass::Unknown,
            AVR_FLASH_BYTES,
            AVR_SRAM_BYTES,
            AVR_EEPROM_BYTES,
            AVR_SRAM_START,
        )
    };
    vm.trace = trace;

    if let Err(e) = ik8bvm::hw::load_hex(&mut vm, &hex) {
        eprintln!("Failed to load hex file: {}", e);
        process::exit(1);
    }

    for vec in startup_irqs {
        vm.raise_interrupt(vec);
    }

    let mut executed: u64 = 0;
    while vm.running {
        for event in &mut irq_at_events {
            if !event.fired && executed >= event.step {
                vm.raise_interrupt(event.vec);
                event.fired = true;
            }
        }
        for event in &mut irq_every_events {
            if executed >= event.next_at {
                vm.raise_interrupt(event.vec);
                event.next_at += event.period;
            }
        }

        vm.step();
        executed += 1;
        if max_instr > 0 && executed >= max_instr {
            break;
        }
    }

    if vm.trace {
        for line in &vm.trace_buf {
            println!("{}", line);
        }
        if vm.trace_truncated {
            println!("... trace truncated at {} lines", vm.trace_buf.len());
        }
    }

    if dump || vm.unknown_opcode {
        dump_regs(&vm);
    }

    if let Some(addr) = peek_addr {
        for k in 0..peek_len {
            let a = addr + k;
            println!("MEM[0x{:04X}] = 0x{:02X}", a, vm.read_data(a));
        }
    }

    if vm.unknown_opcode {
        process::exit(2);
    }
}
