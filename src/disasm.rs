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

// AVR instruction disassembler.
//
// `disasm_static` is the single decoder: given an opcode (and the following word
// for 2-word forms) and the instruction address, it returns the mnemonic text,
// the instruction width in words, and an [`Annot`] describing what runtime detail
// the execution trace should append. Both the live trace (`disasm`) and the
// static `.hex` listing (`disassemble`) share this one decoder, so the opcode
// table lives in exactly one place.

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

/// Runtime detail the live trace appends after the static mnemonic. The static
/// listing ignores it.
pub enum Annot {
    /// Nothing to add.
    None,
    /// Result written to register `d`.
    Rd(usize),
    /// Result written to the R1:R0 pair (multiplies).
    R1R0,
    /// A conditional skip; append "(skip)" when it diverged.
    Skip,
    /// A conditional branch; append "(taken)" when it diverged.
    BranchTaken,
    /// Target is only known at runtime (RET/IJMP); append the post-exec PC.
    PcTarget,
}

/// 12-bit signed relative target (RJMP/RCALL) as an absolute byte address.
fn rel12(op: u16, pc: u32) -> (i32, u32) {
    let mut k = (op & 0x0FFF) as i32;
    if k & 0x0800 != 0 {
        k -= 0x1000;
    }
    (k, (pc as i32 + 2 + k * 2) as u32)
}

/// 7-bit signed branch target as an absolute byte address.
fn rel7(op: u16, pc: u32) -> (i32, u32) {
    let mut k = ((op >> 3) & 0x7F) as i32;
    if k & 0x40 != 0 {
        k -= 0x80;
    }
    (k, (pc as i32 + 2 + k * 2) as u32)
}

/// Decode one instruction statically. `op2` is the following flash word (used by
/// 2-word forms; ignored otherwise) and `pc` is this instruction's byte address.
pub fn disasm_static(op: u16, op2: u16, pc: u32) -> (String, u8, Annot) {
    let d = d5(op);
    let r = r5(op);
    let t = |s: String| (s, 1u8, Annot::None);

    match op & 0xF000 {
        0x0000 => {
            if op == 0x0000 {
                t("NOP".into())
            } else if (op & 0xFF00) == 0x0100 {
                let dd = ((op >> 3) & 0x1E) as usize;
                let rr = ((op & 0x0F) << 1) as usize;
                t(format!("MOVW R{}:R{} <- R{}:R{}", dd + 1, dd, rr + 1, rr))
            } else if (op & 0xFF00) == 0x0200 {
                (format!("MULS R{},R{}", d4(op), 16 + (op & 0x0F) as usize), 1, Annot::R1R0)
            } else if (op & 0xFF88) == 0x0300 {
                t(format!("MULSU R{},R{}", d3(op), r3(op)))
            } else if (op & 0xFF88) == 0x0308 {
                t(format!("FMUL R{},R{}", d3(op), r3(op)))
            } else if (op & 0xFF88) == 0x0380 {
                t(format!("FMULS R{},R{}", d3(op), r3(op)))
            } else if (op & 0xFF88) == 0x0388 {
                t(format!("FMULSU R{},R{}", d3(op), r3(op)))
            } else if (op & 0xFC00) == 0x0400 {
                t(format!("CPC R{},R{}", d, r))
            } else if (op & 0xFC00) == 0x0800 {
                t(format!("SBC R{},R{}", d, r))
            } else if (op & 0xFC00) == 0x0C00 {
                (format!("ADD R{},R{}", d, r), 1, Annot::Rd(d))
            } else {
                t(format!(".dw 0x{:04X}", op))
            }
        }
        0x1000 => {
            if (op & 0xFC00) == 0x1000 {
                (format!("CPSE R{},R{}", d, r), 1, Annot::Skip)
            } else if (op & 0xFC00) == 0x1400 {
                t(format!("CP R{},R{}", d, r))
            } else if (op & 0xFC00) == 0x1800 {
                (format!("SUB R{},R{}", d, r), 1, Annot::Rd(d))
            } else {
                (format!("ADC R{},R{}", d, r), 1, Annot::Rd(d))
            }
        }
        0x2000 => {
            if (op & 0xFC00) == 0x2000 {
                t(format!("AND R{},R{}", d, r))
            } else if (op & 0xFC00) == 0x2400 {
                t(format!("EOR R{},R{}", d, r))
            } else if (op & 0xFC00) == 0x2800 {
                t(format!("OR R{},R{}", d, r))
            } else {
                t(format!("MOV R{},R{}", d, r))
            }
        }
        0x3000 => t(format!("CPI R{},0x{:02X}", d4(op), k8(op))),
        0x4000 => t(format!("SBCI R{},0x{:02X}", d4(op), k8(op))),
        0x5000 => t(format!("SUBI R{},0x{:02X}", d4(op), k8(op))),
        0x6000 => t(format!("ORI R{},0x{:02X}", d4(op), k8(op))),
        0x7000 => t(format!("ANDI R{},0x{:02X}", d4(op), k8(op))),
        0x8000 | 0xA000 => {
            let ptr = if (op & 0x0008) != 0 { "Y" } else { "Z" };
            let q = imm_q(op);
            if (op & 0x0200) != 0 {
                t(format!("STD {}+{}, R{}", ptr, q, d))
            } else {
                t(format!("LDD R{}, {}+{}", d, ptr, q))
            }
        }
        0x9000 => decode_9000(op, op2, d),
        0xB000 => {
            let a = 0x20 + (((op >> 5) & 0x30) | (op & 0x0F));
            if (op & 0xF800) == 0xB000 {
                t(format!("IN R{},0x{:02X}", d, a))
            } else {
                t(format!("OUT 0x{:02X},R{}", a, d))
            }
        }
        0xC000 => {
            let (k, target) = rel12(op, pc);
            t(format!("RJMP {:+} -> 0x{:06X}", k, target))
        }
        0xD000 => {
            let (k, target) = rel12(op, pc);
            t(format!("RCALL {:+} -> 0x{:06X}", k, target))
        }
        0xE000 => t(format!("LDI R{},0x{:02X}", d4(op), k8(op))),
        0xF000 => {
            if (op & 0xFC00) == 0xF000 || (op & 0xFC00) == 0xF400 {
                let bit = (op & 0x07) as usize;
                let (_, target) = rel7(op, pc);
                let name = if (op & 0x0400) == 0 {
                    ["BRCS", "BREQ", "BRMI", "BRVS", "BRLT", "BRHS", "BRTS", "BRIE"][bit]
                } else {
                    ["BRCC", "BRNE", "BRPL", "BRVC", "BRGE", "BRHC", "BRTC", "BRID"][bit]
                };
                (format!("{} 0x{:06X}", name, target), 1, Annot::BranchTaken)
            } else if (op & 0xFE08) == 0xF800 {
                t(format!("BLD R{},{}", d, op & 0x07))
            } else if (op & 0xFE08) == 0xFA00 {
                t(format!("BST R{},{}", d, op & 0x07))
            } else if (op & 0xFE08) == 0xFC00 {
                (format!("SBRC R{},{}", d, op & 0x07), 1, Annot::Skip)
            } else if (op & 0xFE08) == 0xFE00 {
                (format!("SBRS R{},{}", d, op & 0x07), 1, Annot::Skip)
            } else {
                t(format!(".dw 0x{:04X}", op))
            }
        }
        _ => t(format!(".dw 0x{:04X}", op)),
    }
}

fn decode_9000(op: u16, op2: u16, d: usize) -> (String, u8, Annot) {
    let t = |s: String| (s, 1u8, Annot::None);

    if (op & 0xFE0F) == 0x9000 {
        return (format!("LDS R{},[0x{:04X}]", d, op2), 2, Annot::None);
    }
    if (op & 0xFE0F) == 0x9200 {
        return (format!("STS [0x{:04X}],R{}", op2, d), 2, Annot::None);
    }
    if (op & 0xFC00) == 0x9000
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
        return if is_st {
            t(format!("ST {},R{}", ptr, d))
        } else {
            t(format!("LD R{},{}", d, ptr))
        };
    }
    if (op & 0xFE0F) == 0x9004 {
        return t(format!("LPM R{},Z", d));
    }
    if (op & 0xFE0F) == 0x9005 {
        return t(format!("LPM R{},Z+", d));
    }
    if (op & 0xFE0F) == 0x9006 {
        return t(format!("ELPM R{},Z", d));
    }
    if (op & 0xFE0F) == 0x9007 {
        return t(format!("ELPM R{},Z+", d));
    }
    if op == 0x95C8 {
        return t("LPM".into());
    }
    if op == 0x95D8 {
        return t("ELPM".into());
    }
    if op == 0x95E8 || op == 0x95F8 {
        return t("SPM".into());
    }
    if (op & 0xFE0F) == 0x900F {
        return t(format!("POP R{}", d));
    }
    if (op & 0xFE0F) == 0x9204 {
        return t(format!("XCH Z,R{}", d));
    }
    if (op & 0xFE0F) == 0x9205 {
        return t(format!("LAS Z,R{}", d));
    }
    if (op & 0xFE0F) == 0x9206 {
        return t(format!("LAC Z,R{}", d));
    }
    if (op & 0xFE0F) == 0x9207 {
        return t(format!("LAT Z,R{}", d));
    }
    if (op & 0xFE0F) == 0x920F {
        return t(format!("PUSH R{}", d));
    }
    if (op & 0xFF8F) == 0x9408 {
        return t(bset_name(((op >> 4) & 0x07) as u8).to_string());
    }
    if (op & 0xFF8F) == 0x9488 {
        return t(bclr_name(((op >> 4) & 0x07) as u8).to_string());
    }
    if (op & 0xFE0F) == 0x9400 {
        return t(format!("COM R{}", d));
    }
    if (op & 0xFE0F) == 0x9401 {
        return t(format!("NEG R{}", d));
    }
    if (op & 0xFE0F) == 0x9403 {
        return (format!("INC R{}", d), 1, Annot::Rd(d));
    }
    if (op & 0xFE0F) == 0x940A {
        return (format!("DEC R{}", d), 1, Annot::Rd(d));
    }
    if (op & 0xFF0F) == 0x940B {
        return t(format!("DES round {}", (op >> 4) & 0x0F));
    }
    if (op & 0xFE0F) == 0x9405 {
        return t(format!("ASR R{}", d));
    }
    if (op & 0xFE0F) == 0x9406 {
        return t(format!("LSR R{}", d));
    }
    if (op & 0xFE0F) == 0x9407 {
        return t(format!("ROR R{}", d));
    }
    if (op & 0xFE0F) == 0x9402 {
        return t(format!("SWAP R{}", d));
    }
    if (op & 0xFE0E) == 0x940C {
        let word = ((((op >> 4) & 0x1F) as u32) << 17) | (((op & 1) as u32) << 16) | op2 as u32;
        return (format!("JMP 0x{:06X}", word * 2), 2, Annot::None);
    }
    if (op & 0xFE0E) == 0x940E {
        let word = ((((op >> 4) & 0x1F) as u32) << 17) | (((op & 1) as u32) << 16) | op2 as u32;
        return (format!("CALL 0x{:06X}", word * 2), 2, Annot::None);
    }
    if op == 0x9409 {
        return ("IJMP".into(), 1, Annot::PcTarget);
    }
    if op == 0x9509 {
        return t("ICALL".into());
    }
    if op == 0x9419 {
        return t("EIJMP".into());
    }
    if op == 0x9519 {
        return t("EICALL".into());
    }
    if op == 0x9508 {
        return ("RET".into(), 1, Annot::PcTarget);
    }
    if op == 0x9518 {
        return t("RETI".into());
    }
    if op == 0x95A8 {
        return t("WDR".into());
    }
    if op == 0x9588 {
        return t("SLEEP".into());
    }
    if (op & 0xFF00) == 0x9B00 {
        let a = 0x20 + ((op >> 3) & 0x1F);
        return (format!("SBIS 0x{:02X},{}", a, op & 0x07), 1, Annot::Skip);
    }
    if (op & 0xFF00) == 0x9900 {
        let a = 0x20 + ((op >> 3) & 0x1F);
        return (format!("SBIC 0x{:02X},{}", a, op & 0x07), 1, Annot::Skip);
    }
    if (op & 0xFF00) == 0x9A00 {
        let a = 0x20 + ((op >> 3) & 0x1F);
        return t(format!("SBI 0x{:02X},{}", a, op & 0x07));
    }
    if (op & 0xFF00) == 0x9800 {
        let a = 0x20 + ((op >> 3) & 0x1F);
        return t(format!("CBI 0x{:02X},{}", a, op & 0x07));
    }
    if op == 0x9598 {
        return t("BREAK".into());
    }
    if (op & 0xFC00) == 0x9C00 {
        return (format!("MUL R{},R{}", d5(op), r5(op)), 1, Annot::R1R0);
    }
    if (op & 0xFF00) == 0x9600 {
        let di = 24 + ((op >> 4) & 0x03) as usize * 2;
        let k6 = ((op >> 2) & 0x30) | (op & 0x0F);
        return t(format!("ADIW R{}:R{},{}", di + 1, di, k6));
    }
    if (op & 0xFF00) == 0x9700 {
        let di = 24 + ((op >> 4) & 0x03) as usize * 2;
        let k6 = ((op >> 2) & 0x30) | (op & 0x0F);
        return t(format!("SBIW R{}:R{},{}", di + 1, di, k6));
    }
    t(format!(".dw 0x{:04X}", op))
}

/// Disassemble `op` (executed from `cur_pc`) for the live trace, appending the
/// post-execution result/target the way the C TRACE macro did.
pub fn disasm(c: &AvrVm, op: u16, cur_pc: u32) -> String {
    let op2 = c.flash_word(cur_pc + 2);
    let (text, words, annot) = disasm_static(op, op2, cur_pc);
    let prefix = if words == 2 {
        format!("{:04X} {:04X}  ", op, op2)
    } else {
        format!("{:04X}  ", op)
    };
    let diverged = c.pc != cur_pc.wrapping_add(2);
    let suffix = match annot {
        Annot::None => String::new(),
        Annot::Rd(d) => format!(" -> 0x{:02X}", c.r[d]),
        Annot::R1R0 => format!(" -> R1:R0=0x{:04X}", r1r0(c)),
        Annot::Skip => if diverged { " (skip)".to_string() } else { String::new() },
        Annot::BranchTaken => if diverged { " (taken)".to_string() } else { String::new() },
        Annot::PcTarget => format!(" -> 0x{:06X}", c.pc),
    };
    format!("{}{}{}", prefix, text, suffix)
}

/// Width of the instruction at `op` in words (1 or 2).
fn instr_words(op: u16) -> usize {
    if (op & 0xFE0F) == 0x9000
        || (op & 0xFE0F) == 0x9200
        || (op & 0xFE0E) == 0x940C
        || (op & 0xFE0E) == 0x940E
    {
        2
    } else {
        1
    }
}

/// Parse Intel HEX text into a flat, byte-addressed flash image (little-endian
/// words). Unsupported record types are skipped; malformed lines are ignored.
pub fn parse_ihex(text: &str) -> Vec<u8> {
    let mut flash: Vec<u8> = Vec::new();
    let mut base: usize = 0;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix(':') else { continue };
        let bytes: Vec<u8> = (0..rest.len() / 2)
            .filter_map(|i| u8::from_str_radix(&rest[i * 2..i * 2 + 2], 16).ok())
            .collect();
        if bytes.len() < 5 {
            continue;
        }
        let len = bytes[0] as usize;
        let addr = ((bytes[1] as usize) << 8) | bytes[2] as usize;
        let rtype = bytes[3];
        if bytes.len() < 4 + len + 1 {
            continue;
        }
        let data = &bytes[4..4 + len];
        match rtype {
            0x00 => {
                let start = base + addr;
                let end = start + len;
                if flash.len() < end {
                    flash.resize(end, 0);
                }
                flash[start..end].copy_from_slice(data);
            }
            0x02 if data.len() == 2 => {
                base = (((data[0] as usize) << 8) | data[1] as usize) << 4;
            }
            0x04 if data.len() == 2 => {
                base = (((data[0] as usize) << 8) | data[1] as usize) << 16;
            }
            0x01 => break,
            _ => {}
        }
    }
    flash
}

/// Disassemble a flash image into a listing, one instruction per line:
/// `address:  words  MNEMONIC operands`.
pub fn disassemble(flash: &[u8]) -> String {
    let used = flash
        .iter()
        .rposition(|&b| b != 0)
        .map(|i| (i / 2) * 2 + 2)
        .unwrap_or(0);
    if used == 0 {
        return "(empty program)\n".to_string();
    }
    let mut out = String::new();
    let mut a = 0usize;
    while a + 1 < used {
        let op = u16::from_le_bytes([flash[a], flash[a + 1]]);
        let op2 = if a + 3 < flash.len() {
            u16::from_le_bytes([flash[a + 2], flash[a + 3]])
        } else {
            0
        };
        let words = instr_words(op);
        let (text, _, _) = disasm_static(op, op2, a as u32);
        let cols = if words == 2 {
            format!("{:04X} {:04X}", op, op2)
        } else {
            format!("{:04X}     ", op)
        };
        out.push_str(&format!("{:06X}:  {}  {}\n", a, cols, text));
        a += words * 2;
    }
    out
}

/// Disassemble Intel HEX text directly.
pub fn disassemble_ihex(text: &str) -> String {
    disassemble(&parse_ihex(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_common_instructions() {
        assert_eq!(disasm_static(0x0000, 0, 0).0, "NOP");
        assert_eq!(disasm_static(0xEF0F, 0, 0).0, "LDI R16,0xFF");
        assert_eq!(disasm_static(0x9508, 0, 0).0, "RET");
        assert_eq!(disasm_static(0x0C12, 0, 0).0, "ADD R1,R2");
    }

    #[test]
    fn two_word_jmp() {
        let (text, words, _) = disasm_static(0x940C, 0x0034, 0);
        assert_eq!(words, 2);
        assert_eq!(text, "JMP 0x000068");
    }

    #[test]
    fn relative_branch_target() {
        // 0xF411 = BRNE with k=+2 from 0x10: 0x10 + 2 + 2*2 = 0x16.
        assert_eq!(disasm_static(0xF411, 0, 0x10).0, "BRNE 0x000016");
    }

    #[test]
    fn parse_and_disassemble() {
        let flash = parse_ihex(":020000000C9462\n:00000001FF\n");
        assert_eq!(&flash[0..2], &[0x0C, 0x94]);
        assert!(disassemble(&flash).contains("JMP"));
    }
}
