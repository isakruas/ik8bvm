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

// Instruction disassembler for the trace.
//
// Reproduces the textual form the original C avr-vm printed with `-t`:
//   "<ophex>[ <op2hex>]  MNEMONIC operands [-> result]"
// It is called from `step()` *after* the instruction executed, so jump targets
// (`c.pc`) and result bytes (`c.r[..]`) shown match the executed effect, just
// like the C TRACE macro did inside each handler.

use crate::core::AvrVm;

#[inline]
fn d5(op: u16) -> usize {
    ((op >> 4) & 0x1F) as usize
}
#[inline]
fn r5(op: u16) -> usize {
    (((op & 0x0200) >> 5) | (op & 0x0F)) as usize
}
#[inline]
fn d3(op: u16) -> usize {
    16 + ((op >> 4) & 0x07) as usize
}
#[inline]
fn r3(op: u16) -> usize {
    16 + (op & 0x07) as usize
}
#[inline]
fn d4(op: u16) -> usize {
    16 + ((op >> 4) & 0x0F) as usize
}
#[inline]
fn k8(op: u16) -> u16 {
    ((op >> 4) & 0xF0) | (op & 0x0F)
}
#[inline]
fn imm_q(op: u16) -> u16 {
    ((op >> 8) & 0x20) | ((op >> 7) & 0x18) | (op & 0x07)
}

fn bset_name(s: u8) -> &'static str {
    ["SEC", "SEZ", "SEN", "SEV", "SES", "SEH", "SET", "SEI"][(s & 7) as usize]
}
fn bclr_name(s: u8) -> &'static str {
    ["CLC", "CLZ", "CLN", "CLV", "CLS", "CLH", "CLT", "CLI"][(s & 7) as usize]
}

/// R1:R0 as a 16-bit value (multiply results).
fn r1r0(c: &AvrVm) -> u16 {
    (c.r[1] as u16) << 8 | c.r[0] as u16
}

/// Disassemble `op` (executed from `cur_pc`) into the C trace's text form,
/// reading `c`'s post-execution state for targets and results.
pub fn disasm(c: &AvrVm, op: u16, cur_pc: u32) -> String {
    let d = d5(op);
    let r = r5(op);
    let one = |m: String| format!("{:04X}  {}", op, m);
    let two = |w: u16, m: String| format!("{:04X} {:04X}  {}", op, w, m);
    // Skip / branch happened iff PC didn't simply advance past this 1-word op.
    let diverged = c.pc != cur_pc.wrapping_add(2);

    match op & 0xF000 {
        0x0000 => {
            if op == 0x0000 {
                one("NOP".into())
            } else if (op & 0xFF00) == 0x0100 {
                let dd = ((op >> 3) & 0x1E) as usize;
                let rr = ((op & 0x0F) << 1) as usize;
                one(format!("MOVW R{}:R{} <- R{}:R{}", dd + 1, dd, rr + 1, rr))
            } else if (op & 0xFF00) == 0x0200 {
                one(format!("MULS R{},R{} -> R1:R0=0x{:04X}", d4(op), 16 + (op & 0x0F) as usize, r1r0(c)))
            } else if (op & 0xFF88) == 0x0300 {
                one(format!("MULSU R{},R{}", d3(op), r3(op)))
            } else if (op & 0xFF88) == 0x0308 {
                one(format!("FMUL R{},R{}", d3(op), r3(op)))
            } else if (op & 0xFF88) == 0x0380 {
                one(format!("FMULS R{},R{}", d3(op), r3(op)))
            } else if (op & 0xFF88) == 0x0388 {
                one(format!("FMULSU R{},R{}", d3(op), r3(op)))
            } else if (op & 0xFC00) == 0x0400 {
                one(format!("CPC R{},R{}", d, r))
            } else if (op & 0xFC00) == 0x0800 {
                one(format!("SBC R{},R{}", d, r))
            } else if (op & 0xFC00) == 0x0C00 {
                one(format!("ADD R{},R{} -> 0x{:02X}", d, r, c.r[d]))
            } else {
                one(format!(".dw 0x{:04X}", op))
            }
        }
        0x1000 => {
            if (op & 0xFC00) == 0x1000 {
                one(format!("CPSE R{},R{} {}", d, r, if diverged { "(skip)" } else { "" }))
            } else if (op & 0xFC00) == 0x1400 {
                one(format!("CP R{},R{}", d, r))
            } else if (op & 0xFC00) == 0x1800 {
                one(format!("SUB R{},R{} -> 0x{:02X}", d, r, c.r[d]))
            } else {
                one(format!("ADC R{},R{} -> 0x{:02X}", d, r, c.r[d]))
            }
        }
        0x2000 => {
            if (op & 0xFC00) == 0x2000 {
                one(format!("AND R{},R{}", d, r))
            } else if (op & 0xFC00) == 0x2400 {
                one(format!("EOR R{},R{}", d, r))
            } else if (op & 0xFC00) == 0x2800 {
                one(format!("OR R{},R{}", d, r))
            } else {
                one(format!("MOV R{},R{}", d, r))
            }
        }
        0x3000 => one(format!("CPI R{},0x{:02X}", d4(op), k8(op))),
        0x4000 => one(format!("SBCI R{},0x{:02X}", d4(op), k8(op))),
        0x5000 => one(format!("SUBI R{},0x{:02X}", d4(op), k8(op))),
        0x6000 => one(format!("ORI R{},0x{:02X}", d4(op), k8(op))),
        0x7000 => one(format!("ANDI R{},0x{:02X}", d4(op), k8(op))),
        0x8000 | 0xA000 => {
            let ptr = if (op & 0x0008) != 0 { "Y" } else { "Z" };
            let q = imm_q(op);
            if (op & 0x0200) != 0 {
                one(format!("STD {}+{}, R{}", ptr, q, d))
            } else {
                one(format!("LDD R{}, {}+{}", d, ptr, q))
            }
        }
        0x9000 => disasm_9000(c, op, cur_pc, d, &one, &two, diverged),
        0xB000 => {
            let a = 0x20 + (((op >> 5) & 0x30) | (op & 0x0F));
            if (op & 0xF800) == 0xB000 {
                one(format!("IN R{},0x{:02X}", d, a))
            } else {
                one(format!("OUT 0x{:02X},R{}", a, d))
            }
        }
        0xC000 => {
            let mut k = (op & 0x0FFF) as i16;
            if k & 0x0800 != 0 {
                k |= -4096i16;
            }
            if k == -1 {
                one("RJMP -1 (HALT)".into())
            } else {
                one(format!("RJMP {:+} -> 0x{:06X}", k, c.pc))
            }
        }
        0xD000 => {
            let mut k = (op & 0x0FFF) as i16;
            if k & 0x0800 != 0 {
                k |= -4096i16;
            }
            one(format!("RCALL {:+}", k))
        }
        0xE000 => one(format!("LDI R{},0x{:02X}", d4(op), k8(op))),
        0xF000 => {
            if (op & 0xFC00) == 0xF000 || (op & 0xFC00) == 0xF400 {
                let s = (op & 0x07) as u8;
                let mut k = ((op >> 3) & 0x7F) as i8;
                if k & 0x40 != 0 {
                    k |= -128i8;
                }
                let name = if (op & 0x0400) == 0 { "BRBS" } else { "BRBC" };
                one(format!("{} {},{:+} {}", name, s, k, if diverged { "(taken)" } else { "" }))
            } else if (op & 0xFE08) == 0xF800 {
                one(format!("BLD R{},{}", d, op & 0x07))
            } else if (op & 0xFE08) == 0xFA00 {
                one(format!("BST R{},{}", d, op & 0x07))
            } else if (op & 0xFE08) == 0xFC00 {
                one(format!("SBRC R{},{} {}", d, op & 0x07, if diverged { "(skip)" } else { "" }))
            } else if (op & 0xFE08) == 0xFE00 {
                one(format!("SBRS R{},{} {}", d, op & 0x07, if diverged { "(skip)" } else { "" }))
            } else {
                one(format!(".dw 0x{:04X}", op))
            }
        }
        _ => one(format!(".dw 0x{:04X}", op)),
    }
}

#[allow(clippy::too_many_arguments)]
fn disasm_9000(
    c: &AvrVm,
    op: u16,
    cur_pc: u32,
    d: usize,
    one: &dyn Fn(String) -> String,
    two: &dyn Fn(u16, String) -> String,
    diverged: bool,
) -> String {
    let second = c.flash_word(cur_pc + 2);
    if (op & 0xFE0F) == 0x9000 {
        two(second, format!("LDS R{},[0x{:04X}]", d, second))
    } else if (op & 0xFE0F) == 0x9200 {
        two(second, format!("STS [0x{:04X}],R{}", second, d))
    } else if (op & 0xFC00) == 0x9000
        && matches!(op & 0x000F, 0x0C | 0x0D | 0x0E | 0x09 | 0x0A | 0x01 | 0x02)
    {
        let is_st = (op & 0x0200) != 0;
        let ptr = match op & 0x000F {
            0x0C => "X",
            0x0D => "X+",
            0x0E => "-X",
            0x09 => "Y+",
            0x0A => "-Y",
            0x01 => "Z+",
            0x02 => "-Z",
            _ => unreachable!(),
        };
        if is_st {
            one(format!("ST {},R{}", ptr, d))
        } else {
            one(format!("LD R{},{}", d, ptr))
        }
    } else if (op & 0xFE0F) == 0x9004 {
        one(format!("LPM R{},Z", d))
    } else if (op & 0xFE0F) == 0x9005 {
        one(format!("LPM R{},Z+", d))
    } else if (op & 0xFE0F) == 0x9006 {
        one(format!("ELPM R{},Z", d))
    } else if (op & 0xFE0F) == 0x9007 {
        one(format!("ELPM R{},Z+", d))
    } else if op == 0x95C8 {
        one("LPM".into())
    } else if op == 0x95D8 {
        one("ELPM".into())
    } else if op == 0x95E8 || op == 0x95F8 {
        one("SPM".into())
    } else if (op & 0xFE0F) == 0x900F {
        one(format!("POP R{}", d))
    } else if (op & 0xFE0F) == 0x9204 {
        one(format!("XCH Z,R{}", d))
    } else if (op & 0xFE0F) == 0x9205 {
        one(format!("LAS Z,R{}", d))
    } else if (op & 0xFE0F) == 0x9206 {
        one(format!("LAC Z,R{}", d))
    } else if (op & 0xFE0F) == 0x9207 {
        one(format!("LAT Z,R{}", d))
    } else if (op & 0xFE0F) == 0x920F {
        one(format!("PUSH R{}", d))
    } else if (op & 0xFF8F) == 0x9408 {
        one(bset_name(((op >> 4) & 0x07) as u8).to_string())
    } else if (op & 0xFF8F) == 0x9488 {
        one(bclr_name(((op >> 4) & 0x07) as u8).to_string())
    } else if (op & 0xFE0F) == 0x9400 {
        one(format!("COM R{}", d))
    } else if (op & 0xFE0F) == 0x9401 {
        one(format!("NEG R{}", d))
    } else if (op & 0xFE0F) == 0x9403 {
        one(format!("INC R{} -> 0x{:02X}", d, c.r[d]))
    } else if (op & 0xFE0F) == 0x940A {
        one(format!("DEC R{} -> 0x{:02X}", d, c.r[d]))
    } else if (op & 0xFF0F) == 0x940B {
        one(format!("DES round {}", (op >> 4) & 0x0F))
    } else if (op & 0xFE0F) == 0x9405 {
        one(format!("ASR R{}", d))
    } else if (op & 0xFE0F) == 0x9406 {
        one(format!("LSR R{}", d))
    } else if (op & 0xFE0F) == 0x9407 {
        one(format!("ROR R{}", d))
    } else if (op & 0xFE0F) == 0x9402 {
        one(format!("SWAP R{}", d))
    } else if (op & 0xFE0E) == 0x940C {
        two(second, format!("JMP 0x{:06X}", c.pc))
    } else if (op & 0xFE0E) == 0x940E {
        two(second, format!("CALL 0x{:06X}", c.pc))
    } else if op == 0x9409 {
        one(format!("IJMP -> 0x{:06X}", c.pc))
    } else if op == 0x9509 {
        one("ICALL".into())
    } else if op == 0x9419 {
        one("EIJMP".into())
    } else if op == 0x9519 {
        one("EICALL".into())
    } else if op == 0x9508 {
        one(format!("RET -> 0x{:06X}", c.pc))
    } else if op == 0x9518 {
        one("RETI".into())
    } else if op == 0x95A8 {
        one("WDR".into())
    } else if op == 0x9588 {
        one("SLEEP".into())
    } else if (op & 0xFF00) == 0x9B00 {
        let a = 0x20 + ((op >> 3) & 0x1F);
        one(format!("SBIS 0x{:02X},{} {}", a, op & 0x07, if diverged { "(skip)" } else { "" }))
    } else if (op & 0xFF00) == 0x9900 {
        let a = 0x20 + ((op >> 3) & 0x1F);
        one(format!("SBIC 0x{:02X},{} {}", a, op & 0x07, if diverged { "(skip)" } else { "" }))
    } else if (op & 0xFF00) == 0x9A00 {
        let a = 0x20 + ((op >> 3) & 0x1F);
        one(format!("SBI 0x{:02X},{}", a, op & 0x07))
    } else if (op & 0xFF00) == 0x9800 {
        let a = 0x20 + ((op >> 3) & 0x1F);
        one(format!("CBI 0x{:02X},{}", a, op & 0x07))
    } else if op == 0x9598 {
        one("BREAK".into())
    } else if (op & 0xFC00) == 0x9C00 {
        one(format!("MUL R{},R{} -> 0x{:04X}", d5(op), r5(op), r1r0(c)))
    } else if (op & 0xFF00) == 0x9600 {
        let di = 24 + ((op >> 4) & 0x03) as usize * 2;
        let k6 = ((op >> 2) & 0x30) | (op & 0x0F);
        one(format!("ADIW R{}:R{},{}", di + 1, di, k6))
    } else if (op & 0xFF00) == 0x9700 {
        let di = 24 + ((op >> 4) & 0x03) as usize * 2;
        let k6 = ((op >> 2) & 0x30) | (op & 0x0F);
        one(format!("SBIW R{}:R{},{}", di + 1, di, k6))
    } else {
        one(format!(".dw 0x{:04X}", op))
    }
}
