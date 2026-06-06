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
    println!("Usage: {} <file.hex> [-mmcu=DEVICE] [-t] [-n MAX_INSTR] [-d]", prog);
    println!("  -mmcu=DEVICE preload memory/core config for DEVICE");
    println!("  -t           enable instruction trace");
    println!("  -n MAX       stop after MAX instructions (default: unlimited)");
    println!("  -d           dump registers at exit");
    println!("  --irq=VEC            queue one interrupt vector (repeatable)");
    println!("  --irq-at=VEC:STEP    queue vector when executed-instr count reaches STEP");
    println!("  --irq-every=VEC:N    queue vector every N executed instructions");
    println!("  --list-mcus  list known devices and exit");
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

    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--list-mcus" {
            list_mcus();
            return;
        } else if arg == "-t" {
            trace = true;
        } else if arg == "-d" {
            dump = true;
        } else if arg == "-n" {
            i += 1;
            if i >= args.len() {
                eprintln!("Missing value for -n");
                usage(prog);
                process::exit(1);
            }
            max_instr = parse_u64(&args[i]).unwrap_or(0);
        } else if let Some(device) = arg.strip_prefix("-mmcu=") {
            mmcu = Some(device.to_string());
        } else if arg == "-m" {
            i += 1;
            if i >= args.len() {
                eprintln!("Missing value for -m");
                usage(prog);
                process::exit(1);
            }
            peek_addr = parse_u64(&args[i]).map(|v| v as u32);
        } else if arg == "-mlen" {
            i += 1;
            if i >= args.len() {
                eprintln!("Missing value for -mlen");
                usage(prog);
                process::exit(1);
            }
            peek_len = parse_u64(&args[i]).unwrap_or(1) as u32;
        } else if let Some(vec) = arg.strip_prefix("--irq=") {
            let Some(vec) = parse_irq_vec(vec) else {
                eprintln!("Invalid --irq vector '{}' (expected 1..255)", vec);
                process::exit(1);
            };
            startup_irqs.push(vec);
        } else if let Some(rest) = arg.strip_prefix("--irq-at=") {
            let Some((vec_s, step_s)) = rest.split_once(':') else {
                eprintln!("Invalid --irq-at format '{}' (expected VEC:STEP)", rest);
                process::exit(1);
            };
            let Some(vec) = parse_irq_vec(vec_s) else {
                eprintln!("Invalid --irq-at vector field '{}'", rest);
                process::exit(1);
            };
            let Some(step) = parse_u64(step_s) else {
                eprintln!("Invalid --irq-at '{}' (expected VEC:STEP)", rest);
                process::exit(1);
            };
            irq_at_events.push(IrqAtEvent { vec, step, fired: false });
        } else if let Some(rest) = arg.strip_prefix("--irq-every=") {
            let Some((vec_s, period_s)) = rest.split_once(':') else {
                eprintln!("Invalid --irq-every format '{}' (expected VEC:N)", rest);
                process::exit(1);
            };
            let Some(vec) = parse_irq_vec(vec_s) else {
                eprintln!("Invalid --irq-every vector field '{}'", rest);
                process::exit(1);
            };
            let Some(period) = parse_u64(period_s) else {
                eprintln!("Invalid --irq-every '{}' (expected VEC:N)", rest);
                process::exit(1);
            };
            if period == 0 {
                eprintln!("Invalid --irq-every '{}' (N must be > 0)", rest);
                process::exit(1);
            }
            irq_every_events.push(IrqEveryEvent {
                vec,
                period,
                next_at: period,
            });
        } else if !arg.starts_with('-') {
            if hexfile.is_none() {
                hexfile = Some(arg.clone());
            } else {
                eprintln!("Multiple hex files specified");
                usage(prog);
                process::exit(1);
            }
        } else {
            eprintln!("Unknown option: {}", arg);
            usage(prog);
            process::exit(1);
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
