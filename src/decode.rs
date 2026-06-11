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

use crate::core::AvrVm;
use crate::devices::AvrCoreClass;

const F_H: u8 = 5;
const F_T: u8 = 6;
const F_I: u8 = 7;

fn cyc(c: &AvrVm, e: u64, xm: u64, xt: u64, rc: u64) -> u64 {
    match c.core {
        AvrCoreClass::XM => xm,
        AvrCoreClass::XT => xt,
        AvrCoreClass::RC => rc,
        _ => e,
    }
}

fn ld_st_cyc(c: &AvrVm, is_store: bool, mode: u8) -> u64 {
    if is_store {
        if mode == 2 {
            cyc(c, 2, 2, 1, 2)
        } else {
            cyc(c, 2, 1, 1, 1)
        }
    } else if mode == 0 {
        cyc(c, 2, 2, 2, 1)
    } else if mode == 1 {
        cyc(c, 2, 2, 2, 2)
    } else {
        cyc(c, 2, 3, 2, 2)
    }
}

fn reg_d5(op: u16) -> usize {
    ((op >> 4) & 0x1F) as usize
}

fn reg_r5(op: u16) -> usize {
    (((op & 0x0200) >> 5) | (op & 0x0F)) as usize
}

fn reg_d3(op: u16) -> usize {
    16 + ((op >> 4) & 0x07) as usize
}

fn reg_r3(op: u16) -> usize {
    16 + (op & 0x07) as usize
}

fn imm_q(op: u16) -> u16 {
    ((op >> 8) & 0x20) | ((op >> 7) & 0x18) | (op & 0x07)
}

fn pair(c: &AvrVm, lo: usize) -> u16 {
    (c.r[lo] as u16) | ((c.r[lo + 1] as u16) << 8)
}

fn set_pair(c: &mut AvrVm, lo: usize, val: u16) {
    c.r[lo] = (val & 0xFF) as u8;
    c.r[lo + 1] = (val >> 8) as u8;
}

fn addr_with_ramp(ramp: u8, ptr: u16) -> u32 {
    ((ramp as u32) << 16) | ptr as u32
}

fn push8(c: &mut AvrVm, val: u8) {
    c.write_data(c.sp as u32, val);
    c.sp = c.sp.wrapping_sub(1);
}

fn pop8(c: &mut AvrVm) -> u8 {
    c.sp = c.sp.wrapping_add(1);
    c.read_data(c.sp as u32)
}

fn push16(c: &mut AvrVm, val: u16) {
    push8(c, (val >> 8) as u8);
    push8(c, (val & 0xFF) as u8);
}

fn pop16(c: &mut AvrVm) -> u16 {
    let lo = pop8(c) as u16;
    let hi = pop8(c) as u16;
    (hi << 8) | lo
}

fn is_2_word_instruction(op: u16) -> bool {
    (op & 0xFE0F) == 0x9000
        || (op & 0xFE0F) == 0x9200
        || (op & 0xFE0E) == 0x940C
        || (op & 0xFE0E) == 0x940E
}

fn do_skip(c: &mut AvrVm) {
    let next = c.flash_word(c.pc);
    if is_2_word_instruction(next) {
        c.pc += 4;
        c.cycles += 2;
    } else {
        c.pc += 2;
        c.cycles += 1;
    }
}

fn add_word_offset(byte_pc_after_fetch: u32, word_offset: i16) -> u32 {
    (byte_pc_after_fetch as i64 + word_offset as i64 * 2) as u32
}

fn unknown_opcode(c: &mut AvrVm, op: u16, pc: u32) {
    c.unknown_opcode = true;
    c.running = false;
    eprintln!("Unknown opcode: {:04X} at PC={:04X}", op, pc);
}

fn gate_opcode(c: &mut AvrVm, op: u16, pc: u32) -> bool {
    if c.core == AvrCoreClass::Unknown {
        return false;
    }

    let xm = c.core == AvrCoreClass::XM;
    let e = c.core == AvrCoreClass::E;
    let rc = c.core == AvrCoreClass::RC;
    let mut bad = false;

    if (op & 0xFE0F) == 0x9204
        || (op & 0xFE0F) == 0x9205
        || (op & 0xFE0F) == 0x9206
        || (op & 0xFE0F) == 0x9207
        || (op & 0xFF0F) == 0x940B
    {
        bad = !xm;
    } else if (op & 0xFC00) == 0x9C00
        || (op & 0xFF00) == 0x0200
        || (op & 0xFF88) == 0x0300
        || (op & 0xFF88) == 0x0308
        || (op & 0xFF88) == 0x0380
        || (op & 0xFF88) == 0x0388
    {
        bad = e || rc;
    } else if op == 0x9419
        || op == 0x9519
        || (op & 0xFE0F) == 0x9006
        || (op & 0xFE0F) == 0x9007
        || op == 0x95D8
    {
        bad = e || rc;
    } else if (op & 0xFF00) == 0x9600
        || (op & 0xFF00) == 0x9700
        || (op & 0xFF00) == 0x0100
        || (op & 0xFE0E) == 0x940C
        || (op & 0xFE0E) == 0x940E
        || (op & 0xFE0F) == 0x9004
        || (op & 0xFE0F) == 0x9005
        || op == 0x95C8
        || op == 0x95E8
        || op == 0x95F8
    {
        bad = rc;
    } else if ((op & 0xD208) == 0x8000
        || (op & 0xD208) == 0x8008
        || (op & 0xD208) == 0x8200
        || (op & 0xD208) == 0x8208)
        && imm_q(op) != 0
    {
        bad = rc;
    } else if rc {
        // Reduced-core register file is r16..r31: any register operand below
        // r16 is reserved on AVRrc, as are the 2-word LDS/STS encodings.
        let d = ((op >> 4) & 0x1F) as u8;
        let r5 = (((op >> 5) & 0x10) | (op & 0x0F)) as u8;
        bad = match op & 0xF000 {
            // CPC/SBC/ADD (0x0000 family; MOVW/MULS/FMUL* are gated above).
            0x0000 => matches!(op & 0xFC00, 0x0400 | 0x0800 | 0x0C00) && (d < 16 || r5 < 16),
            // CPSE/CP/SUB/ADC and AND/EOR/OR/MOV.
            0x1000 | 0x2000 => d < 16 || r5 < 16,
            // LD/ST through X/Y/Z (q == 0 forms; q != 0 is gated above).
            0x8000 | 0xA000 => d < 16,
            0x9000 => {
                if (op & 0xFE0F) == 0x9000 || (op & 0xFE0F) == 0x9200 {
                    true // 2-word LDS/STS
                } else if (op & 0xFC00) == 0x9000 {
                    d < 16 // LD/ST variants, PUSH/POP
                } else if (op & 0xFE00) == 0x9400
                    && matches!(op & 0x000F, 0x0..=0x3 | 0x5..=0x7 | 0xA)
                {
                    d < 16 // COM/NEG/SWAP/INC/ASR/LSR/ROR/DEC
                } else {
                    false
                }
            }
            // IN/OUT.
            0xB000 => d < 16,
            // BLD/BST/SBRC/SBRS.
            0xF000 => (op & 0xF800) == 0xF800 && d < 16,
            _ => false,
        };
    }

    if bad {
        c.unknown_opcode = true;
        c.running = false;
        eprintln!(
            "Illegal instruction 0x{:04X} for {:?} at PC=0x{:06X}",
            op, c.core, pc
        );
    }
    bad
}

fn exec_ld_st_ptr(c: &mut AvrVm, op: u16, ptr_lo: usize, ramp: u8, is_store: bool, mode: u8) {
    let d = reg_d5(op);
    let mut ptr = pair(c, ptr_lo);
    if mode == 2 {
        ptr = ptr.wrapping_sub(1);
        set_pair(c, ptr_lo, ptr);
    }

    let addr = addr_with_ramp(ramp, ptr);
    if is_store {
        c.write_data(addr, c.r[d]);
    } else {
        c.r[d] = c.read_data(addr);
    }

    if mode == 1 {
        set_pair(c, ptr_lo, ptr.wrapping_add(1));
    }
    c.cycles += ld_st_cyc(c, is_store, mode);
}

fn exec_ldd_std(c: &mut AvrVm, op: u16, use_y: bool, is_store: bool) {
    let d = reg_d5(op);
    let q = imm_q(op);
    let ptr_lo = if use_y { 28 } else { 30 };
    let ramp = if use_y { c.rampy } else { c.rampz };
    let addr = addr_with_ramp(ramp, pair(c, ptr_lo).wrapping_add(q));
    if is_store {
        c.write_data(addr, c.r[d]);
        c.cycles += cyc(c, 2, 2, 1, 0);
    } else {
        c.r[d] = c.read_data(addr);
        c.cycles += cyc(c, 2, 3, 2, 0);
    }
}

fn flash_byte_wrapped(c: &AvrVm, addr: u32) -> u8 {
    if c.flash_bytes == 0 {
        0
    } else {
        c.flash[(addr % c.flash_bytes) as usize]
    }
}

const DES_IP: [u8; 64] = [
    58, 50, 42, 34, 26, 18, 10, 2, 60, 52, 44, 36, 28, 20, 12, 4, 62, 54, 46, 38, 30, 22, 14, 6,
    64, 56, 48, 40, 32, 24, 16, 8, 57, 49, 41, 33, 25, 17, 9, 1, 59, 51, 43, 35, 27, 19, 11, 3, 61,
    53, 45, 37, 29, 21, 13, 5, 63, 55, 47, 39, 31, 23, 15, 7,
];
const DES_FP: [u8; 64] = [
    40, 8, 48, 16, 56, 24, 64, 32, 39, 7, 47, 15, 55, 23, 63, 31, 38, 6, 46, 14, 54, 22, 62, 30,
    37, 5, 45, 13, 53, 21, 61, 29, 36, 4, 44, 12, 52, 20, 60, 28, 35, 3, 43, 11, 51, 19, 59, 27,
    34, 2, 42, 10, 50, 18, 58, 26, 33, 1, 41, 9, 49, 17, 57, 25,
];
const DES_E: [u8; 48] = [
    32, 1, 2, 3, 4, 5, 4, 5, 6, 7, 8, 9, 8, 9, 10, 11, 12, 13, 12, 13, 14, 15, 16, 17, 16, 17, 18,
    19, 20, 21, 20, 21, 22, 23, 24, 25, 24, 25, 26, 27, 28, 29, 28, 29, 30, 31, 32, 1,
];
const DES_P: [u8; 32] = [
    16, 7, 20, 21, 29, 12, 28, 17, 1, 15, 23, 26, 5, 18, 31, 10, 2, 8, 24, 14, 32, 27, 3, 9, 19,
    13, 30, 6, 22, 11, 4, 25,
];
const DES_PC1: [u8; 56] = [
    57, 49, 41, 33, 25, 17, 9, 1, 58, 50, 42, 34, 26, 18, 10, 2, 59, 51, 43, 35, 27, 19, 11, 3, 60,
    52, 44, 36, 63, 55, 47, 39, 31, 23, 15, 7, 62, 54, 46, 38, 30, 22, 14, 6, 61, 53, 45, 37, 29,
    21, 13, 5, 28, 20, 12, 4,
];
const DES_PC2: [u8; 48] = [
    14, 17, 11, 24, 1, 5, 3, 28, 15, 6, 21, 10, 23, 19, 12, 4, 26, 8, 16, 7, 27, 20, 13, 2, 41, 52,
    31, 37, 47, 55, 30, 40, 51, 45, 33, 48, 44, 49, 39, 56, 34, 53, 46, 42, 50, 36, 29, 32,
];
const DES_SHIFT: [u8; 16] = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];
const DES_SBOX: [[u8; 64]; 8] = [
    [
        14, 4, 13, 1, 2, 15, 11, 8, 3, 10, 6, 12, 5, 9, 0, 7, 0, 15, 7, 4, 14, 2, 13, 1, 10, 6, 12,
        11, 9, 5, 3, 8, 4, 1, 14, 8, 13, 6, 2, 11, 15, 12, 9, 7, 3, 10, 5, 0, 15, 12, 8, 2, 4, 9,
        1, 7, 5, 11, 3, 14, 10, 0, 6, 13,
    ],
    [
        15, 1, 8, 14, 6, 11, 3, 4, 9, 7, 2, 13, 12, 0, 5, 10, 3, 13, 4, 7, 15, 2, 8, 14, 12, 0, 1,
        10, 6, 9, 11, 5, 0, 14, 7, 11, 10, 4, 13, 1, 5, 8, 12, 6, 9, 3, 2, 15, 13, 8, 10, 1, 3, 15,
        4, 2, 11, 6, 7, 12, 0, 5, 14, 9,
    ],
    [
        10, 0, 9, 14, 6, 3, 15, 5, 1, 13, 12, 7, 11, 4, 2, 8, 13, 7, 0, 9, 3, 4, 6, 10, 2, 8, 5,
        14, 12, 11, 15, 1, 13, 6, 4, 9, 8, 15, 3, 0, 11, 1, 2, 12, 5, 10, 14, 7, 1, 10, 13, 0, 6,
        9, 8, 7, 4, 15, 14, 3, 11, 5, 2, 12,
    ],
    [
        7, 13, 14, 3, 0, 6, 9, 10, 1, 2, 8, 5, 11, 12, 4, 15, 13, 8, 11, 5, 6, 15, 0, 3, 4, 7, 2,
        12, 1, 10, 14, 9, 10, 6, 9, 0, 12, 11, 7, 13, 15, 1, 3, 14, 5, 2, 8, 4, 3, 15, 0, 6, 10, 1,
        13, 8, 9, 4, 5, 11, 12, 7, 2, 14,
    ],
    [
        2, 12, 4, 1, 7, 10, 11, 6, 8, 5, 3, 15, 13, 0, 14, 9, 14, 11, 2, 12, 4, 7, 13, 1, 5, 0, 15,
        10, 3, 9, 8, 6, 4, 2, 1, 11, 10, 13, 7, 8, 15, 9, 12, 5, 6, 3, 0, 14, 11, 8, 12, 7, 1, 14,
        2, 13, 6, 15, 0, 9, 10, 4, 5, 3,
    ],
    [
        12, 1, 10, 15, 9, 2, 6, 8, 0, 13, 3, 4, 14, 7, 5, 11, 10, 15, 4, 2, 7, 12, 9, 5, 6, 1, 13,
        14, 0, 11, 3, 8, 9, 14, 15, 5, 2, 8, 12, 3, 7, 0, 4, 10, 1, 13, 11, 6, 4, 3, 2, 12, 9, 5,
        15, 10, 11, 14, 1, 7, 6, 0, 8, 13,
    ],
    [
        4, 11, 2, 14, 15, 0, 8, 13, 3, 12, 9, 7, 5, 10, 6, 1, 13, 0, 11, 7, 4, 9, 1, 10, 14, 3, 5,
        12, 2, 15, 8, 6, 1, 4, 11, 13, 12, 3, 7, 14, 10, 15, 6, 8, 0, 5, 9, 2, 6, 11, 13, 8, 1, 4,
        10, 7, 9, 5, 0, 15, 14, 2, 3, 12,
    ],
    [
        13, 2, 8, 4, 6, 15, 11, 1, 10, 9, 3, 14, 5, 0, 12, 7, 1, 15, 13, 8, 10, 3, 7, 4, 12, 5, 6,
        11, 0, 14, 9, 2, 7, 11, 4, 1, 9, 12, 14, 2, 0, 6, 10, 13, 15, 3, 5, 8, 2, 1, 14, 7, 4, 10,
        8, 13, 15, 12, 9, 0, 3, 5, 6, 11,
    ],
];

fn des_permute(input: u64, table: &[u8], nout: usize, nin: u8) -> u64 {
    let mut out = 0;
    for &bit in table.iter().take(nout) {
        out = (out << 1) | ((input >> (nin - bit)) & 1);
    }
    out
}

fn des_feistel(r: u32, subkey: u64) -> u32 {
    let x = des_permute(r as u64, &DES_E, 48, 32) ^ subkey;
    let mut out = 0u32;
    for (i, sbox) in DES_SBOX.iter().enumerate() {
        let six = ((x >> (42 - 6 * i)) & 0x3F) as usize;
        let row = (((six >> 5) & 1) << 1) | (six & 1);
        let col = (six >> 1) & 0x0F;
        out = (out << 4) | sbox[row * 16 + col] as u32;
    }
    des_permute(out as u64, &DES_P, 32, 32) as u32
}

fn des_subkeys(key: u64) -> [u64; 16] {
    let k = des_permute(key, &DES_PC1, 56, 64);
    let mut c = ((k >> 28) & 0x0FFF_FFFF) as u32;
    let mut d = (k & 0x0FFF_FFFF) as u32;
    let mut keys = [0u64; 16];
    for (i, key) in keys.iter_mut().enumerate() {
        let shift = DES_SHIFT[i] as u32;
        c = ((c << shift) | (c >> (28 - shift))) & 0x0FFF_FFFF;
        d = ((d << shift) | (d >> (28 - shift))) & 0x0FFF_FFFF;
        *key = des_permute(((c as u64) << 28) | d as u64, &DES_PC2, 48, 56);
    }
    keys
}

fn exec_des(c: &mut AvrVm, round: usize, decrypt: bool) {
    let mut block = 0u64;
    let mut key = 0u64;
    for i in (0..8).rev() {
        block = (block << 8) | c.r[i] as u64;
        key = (key << 8) | c.r[8 + i] as u64;
    }

    let subkeys = des_subkeys(key);
    let x = des_permute(block, &DES_IP, 64, 64);
    let left = (x >> 32) as u32;
    let right = x as u32;
    let mut new_left = right;
    let mut new_right =
        left ^ des_feistel(right, subkeys[if decrypt { 15 - round } else { round }]);
    if round == 15 {
        std::mem::swap(&mut new_left, &mut new_right);
    }

    let result = des_permute(
        ((new_left as u64) << 32) | new_right as u64,
        &DES_FP,
        64,
        64,
    );
    for i in 0..8 {
        c.r[i] = (result >> (8 * i)) as u8;
    }
}

pub fn step(c: &mut AvrVm) {
    // The program counter is physically only as wide as the flash, so
    // execution wraps modulo the flash size — this is how RJMP/RCALL reach
    // the whole flash on parts without JMP/CALL (8 KB and below).
    if c.flash_bytes > 0 && c.pc >= c.flash_bytes {
        c.pc %= c.flash_bytes;
    }
    let op = c.flash_word(c.pc);
    let cur_pc = c.pc;
    c.pc += 2;
    if gate_opcode(c, op, cur_pc) {
        if c.trace {
            c.trace_line(format!("PC={:06X}  {:04X}  <illegal>", cur_pc, op));
        }
        return;
    }

    let d = reg_d5(op);
    let r = reg_r5(op);
    let k8 = ((op >> 4) & 0xF0) | (op & 0x0F);

    match op & 0xF000 {
        0x0000 => {
            if op == 0x0000 {
                c.cycles += 1;
            } else if (op & 0xFF00) == 0x0100 {
                // MOVW
                let d2 = ((op >> 3) & 0x1E) as usize;
                let r2 = ((op & 0x0F) << 1) as usize;
                c.r[d2] = c.r[r2];
                c.r[d2 + 1] = c.r[r2 + 1];
                c.cycles += 1;
            } else if (op & 0xFF00) == 0x0200 {
                // MULS
                let d_val = c.r[16 + ((op >> 4) & 0x0F) as usize] as i8;
                let r_val = c.r[16 + (op & 0x0F) as usize] as i8;
                let res = (d_val as i16) * (r_val as i16);
                c.r[0] = (res & 0xFF) as u8;
                c.r[1] = (res >> 8) as u8;
                c.set_flag(0, res < 0);
                c.set_flag(1, res == 0);
                c.cycles += 2;
            } else if (op & 0xFF88) == 0x0300 {
                // MULSU
                let d = reg_d3(op);
                let r = reg_r3(op);
                let res = (c.r[d] as i8 as i16).wrapping_mul(c.r[r] as i16);
                c.r[0] = (res & 0xFF) as u8;
                c.r[1] = (res >> 8) as u8;
                c.set_flag(0, res < 0);
                c.set_flag(1, res == 0);
                c.cycles += 2;
            } else if (op & 0xFF88) == 0x0308 {
                // FMUL
                let d = reg_d3(op);
                let r = reg_r3(op);
                let res = (c.r[d] as u16) * (c.r[r] as u16);
                let shifted = res << 1;
                c.r[0] = (shifted & 0xFF) as u8;
                c.r[1] = (shifted >> 8) as u8;
                c.set_flag(0, (res & 0x8000) != 0);
                c.set_flag(1, shifted == 0);
                c.cycles += 2;
            } else if (op & 0xFF88) == 0x0380 {
                // FMULS
                let d = reg_d3(op);
                let r = reg_r3(op);
                let res = (c.r[d] as i8 as i16).wrapping_mul(c.r[r] as i8 as i16);
                let shifted = (res as u16) << 1;
                c.r[0] = (shifted & 0xFF) as u8;
                c.r[1] = (shifted >> 8) as u8;
                c.set_flag(0, ((res as u16) & 0x8000) != 0);
                c.set_flag(1, shifted == 0);
                c.cycles += 2;
            } else if (op & 0xFF88) == 0x0388 {
                // FMULSU
                let d = reg_d3(op);
                let r = reg_r3(op);
                let res = (c.r[d] as i8 as i16).wrapping_mul(c.r[r] as i16);
                let shifted = (res as u16) << 1;
                c.r[0] = (shifted & 0xFF) as u8;
                c.r[1] = (shifted >> 8) as u8;
                c.set_flag(0, ((res as u16) & 0x8000) != 0);
                c.set_flag(1, shifted == 0);
                c.cycles += 2;
            } else if (op & 0xFC00) == 0x0400 {
                // CPC
                let rd = c.r[d];
                let rr = c.r[r];
                let c_in = if c.get_flag(0) { 1 } else { 0 };
                c.flags_sub(rd, rr, c_in, true);
                c.cycles += 1;
            } else if (op & 0xFC00) == 0x0800 {
                // SBC
                let rd = c.r[d];
                let rr = c.r[r];
                let c_in = if c.get_flag(0) { 1 } else { 0 };
                let res = rd.wrapping_sub(rr).wrapping_sub(c_in);
                c.r[d] = res;
                c.flags_sub(rd, rr, c_in, true);
                c.cycles += 1;
            } else if (op & 0xFC00) == 0x0C00 {
                // ADD
                let rd = c.r[d];
                let rr = c.r[r];
                let res = rd.wrapping_add(rr);
                c.r[d] = res;
                c.flags_add(rd, rr, res, 0);
                c.cycles += 1;
            } else {
                unknown_opcode(c, op, cur_pc);
            }
        }
        0x1000 => {
            if (op & 0xFC00) == 0x1000 {
                // CPSE
                c.cycles += 1;
                if c.r[d] == c.r[r] {
                    do_skip(c);
                }
            } else if (op & 0xFC00) == 0x1400 {
                // CP
                let rd = c.r[d];
                let rr = c.r[r];
                c.flags_sub(rd, rr, 0, false);
                c.cycles += 1;
            } else if (op & 0xFC00) == 0x1800 {
                // SUB
                let rd = c.r[d];
                let rr = c.r[r];
                let res = rd.wrapping_sub(rr);
                c.r[d] = res;
                c.flags_sub(rd, rr, 0, false);
                c.cycles += 1;
            } else if (op & 0xFC00) == 0x1C00 {
                // ADC
                let rd = c.r[d];
                let rr = c.r[r];
                let c_in = if c.get_flag(0) { 1 } else { 0 };
                let res = rd.wrapping_add(rr).wrapping_add(c_in);
                c.r[d] = res;
                c.flags_add(rd, rr, res, c_in);
                c.cycles += 1;
            }
        }
        0x2000 => {
            if (op & 0xFC00) == 0x2000 {
                // AND
                c.r[d] &= c.r[r];
                let res = c.r[d];
                c.flags_logic(res);
                c.cycles += 1;
            } else if (op & 0xFC00) == 0x2400 {
                // EOR
                c.r[d] ^= c.r[r];
                let res = c.r[d];
                c.flags_logic(res);
                c.cycles += 1;
            } else if (op & 0xFC00) == 0x2800 {
                // OR
                c.r[d] |= c.r[r];
                let res = c.r[d];
                c.flags_logic(res);
                c.cycles += 1;
            } else if (op & 0xFC00) == 0x2C00 {
                // MOV
                c.r[d] = c.r[r];
                c.cycles += 1;
            }
        }
        0x3000 => {
            // CPI
            let d4 = 16 + ((op >> 4) & 0x0F) as usize;
            let rd = c.r[d4];
            c.flags_sub(rd, k8 as u8, 0, false);
            c.cycles += 1;
        }
        0x4000 => {
            // SBCI
            let d4 = 16 + ((op >> 4) & 0x0F) as usize;
            let rd = c.r[d4];
            let c_in = if c.get_flag(0) { 1 } else { 0 };
            let res = rd.wrapping_sub(k8 as u8).wrapping_sub(c_in);
            c.r[d4] = res;
            c.flags_sub(rd, k8 as u8, c_in, true);
            c.cycles += 1;
        }
        0x5000 => {
            // SUBI
            let d4 = 16 + ((op >> 4) & 0x0F) as usize;
            let rd = c.r[d4];
            let res = rd.wrapping_sub(k8 as u8);
            c.r[d4] = res;
            c.flags_sub(rd, k8 as u8, 0, false);
            c.cycles += 1;
        }
        0x6000 => {
            // ORI
            let d4 = 16 + ((op >> 4) & 0x0F) as usize;
            c.r[d4] |= k8 as u8;
            let res = c.r[d4];
            c.flags_logic(res);
            c.cycles += 1;
        }
        0x7000 => {
            // ANDI
            let d4 = 16 + ((op >> 4) & 0x0F) as usize;
            c.r[d4] &= k8 as u8;
            let res = c.r[d4];
            c.flags_logic(res);
            c.cycles += 1;
        }
        0x8000 | 0xA000 => {
            // LDD / STD
            exec_ldd_std(c, op, (op & 0x0008) != 0, (op & 0x0200) != 0);
        }
        0x9000 => {
            if (op & 0xFE0F) == 0x9000 {
                // LDS
                let addr = c.flash_word(c.pc) as u32;
                c.pc += 2;
                c.r[d] = c.read_data(addr);
                c.cycles += cyc(c, 2, 3, 3, 2);
            } else if (op & 0xFE0F) == 0x9200 {
                // STS
                let addr = c.flash_word(c.pc) as u32;
                c.pc += 2;
                c.write_data(addr, c.r[d]);
                c.cycles += cyc(c, 2, 2, 2, 1);
            } else if (op & 0xFC00) == 0x9000
                && ((op & 0x000F) == 0x0C
                    || (op & 0x000F) == 0x0D
                    || (op & 0x000F) == 0x0E
                    || (op & 0x000F) == 0x09
                    || (op & 0x000F) == 0x0A
                    || (op & 0x000F) == 0x01
                    || (op & 0x000F) == 0x02)
            {
                let is_st = (op & 0x0200) != 0;
                let (ptr_lo, ramp, mode) = match op & 0x000F {
                    0x0C => (26, c.rampx, 0), // X
                    0x0D => (26, c.rampx, 1), // X+
                    0x0E => (26, c.rampx, 2), // -X
                    0x09 => (28, c.rampy, 1), // Y+
                    0x0A => (28, c.rampy, 2), // -Y
                    0x01 => (30, c.rampz, 1), // Z+
                    0x02 => (30, c.rampz, 2), // -Z
                    _ => unreachable!(),
                };
                exec_ld_st_ptr(c, op, ptr_lo, ramp, is_st, mode);
            } else if (op & 0xFE0F) == 0x9004 || (op & 0xFE0F) == 0x9005 {
                // LPM Rd, Z / Z+
                let mut addr = (c.r[30] as u16) | ((c.r[31] as u16) << 8);
                c.r[d] = c.flash_byte(addr as u32);
                if (op & 0x000F) == 0x0005 {
                    addr = addr.wrapping_add(1);
                    c.r[30] = (addr & 0xFF) as u8;
                    c.r[31] = (addr >> 8) as u8;
                }
                c.cycles += 3;
            } else if (op & 0xFE0F) == 0x9006 || (op & 0xFE0F) == 0x9007 {
                // ELPM Rd, Z / Z+
                let mut addr = addr_with_ramp(c.rampz, pair(c, 30));
                c.r[d] = flash_byte_wrapped(c, addr);
                if (op & 0x000F) == 0x0007 {
                    addr = addr.wrapping_add(1) & 0x00FF_FFFF;
                    c.rampz = (addr >> 16) as u8;
                    set_pair(c, 30, addr as u16);
                }
                c.cycles += 3;
            } else if op == 0x95C8 {
                // LPM
                let addr = (c.r[30] as u16) | ((c.r[31] as u16) << 8);
                c.r[0] = c.flash_byte(addr as u32);
                c.cycles += 3;
            } else if op == 0x95D8 {
                // ELPM
                let addr = addr_with_ramp(c.rampz, pair(c, 30));
                c.r[0] = flash_byte_wrapped(c, addr);
                c.cycles += 3;
            } else if op == 0x95E8 || op == 0x95F8 {
                // SPM / SPM Z+
                let mut addr = addr_with_ramp(c.rampz, pair(c, 30));
                if addr + 1 < c.flash_bytes {
                    let idx = addr as usize;
                    c.flash[idx] = c.r[0];
                    c.flash[idx + 1] = c.r[1];
                }
                if op == 0x95F8 {
                    addr = addr.wrapping_add(2) & 0x00FF_FFFF;
                    c.rampz = (addr >> 16) as u8;
                    set_pair(c, 30, addr as u16);
                }
                c.cycles += 4;
            } else if (op & 0xFE0F) == 0x900F {
                // POP
                c.r[d] = pop8(c);
                c.cycles += cyc(c, 2, 2, 2, 3);
            } else if (op & 0xFE0F) == 0x9204 {
                // XCH
                let addr = addr_with_ramp(c.rampz, pair(c, 30));
                let mem = c.read_data(addr);
                let rd = c.r[d];
                c.write_data(addr, rd);
                c.r[d] = mem;
                c.cycles += 2;
            } else if (op & 0xFE0F) == 0x9205 {
                // LAS
                let addr = addr_with_ramp(c.rampz, pair(c, 30));
                let mem = c.read_data(addr);
                let rd = c.r[d];
                c.write_data(addr, mem | rd);
                c.r[d] = mem;
                c.cycles += 2;
            } else if (op & 0xFE0F) == 0x9206 {
                // LAC
                let addr = addr_with_ramp(c.rampz, pair(c, 30));
                let mem = c.read_data(addr);
                let rd = c.r[d];
                c.write_data(addr, mem & !rd);
                c.r[d] = mem;
                c.cycles += 2;
            } else if (op & 0xFE0F) == 0x9207 {
                // LAT
                let addr = addr_with_ramp(c.rampz, pair(c, 30));
                let mem = c.read_data(addr);
                let rd = c.r[d];
                c.write_data(addr, mem ^ rd);
                c.r[d] = mem;
                c.cycles += 2;
            } else if (op & 0xFE0F) == 0x920F {
                // PUSH
                push8(c, c.r[d]);
                c.cycles += cyc(c, 2, 1, 1, 1);
            } else if (op & 0xFF8F) == 0x9408 {
                // BSET
                let s = ((op >> 4) & 0x07) as u8;
                c.set_flag(s, true);
                c.cycles += 1;
            } else if (op & 0xFF8F) == 0x9488 {
                // BCLR
                let s = ((op >> 4) & 0x07) as u8;
                c.set_flag(s, false);
                c.cycles += 1;
            } else if (op & 0xFE0F) == 0x9400 {
                // COM
                let res = !c.r[d];
                c.r[d] = res;
                c.set_flag(0, true);
                c.flags_logic(res);
                c.cycles += 1;
            } else if (op & 0xFE0F) == 0x9401 {
                // NEG
                let a = c.r[d];
                let res = (0u8).wrapping_sub(a);
                c.r[d] = res;
                c.set_flag(3, res == 0x80);
                c.set_flag(0, res != 0x00);
                c.set_flag(5, ((res | a) & 0x08) != 0);
                c.set_flag(1, res == 0);
                c.set_flag(2, (res & 0x80) != 0);
                c.set_flag(4, c.get_flag(2) ^ c.get_flag(3));
                c.cycles += 1;
            } else if (op & 0xFE0F) == 0x9403 {
                // INC
                let res = c.r[d].wrapping_add(1);
                c.r[d] = res;
                c.set_flag(1, res == 0);
                c.set_flag(2, (res & 0x80) != 0);
                c.set_flag(3, res == 0x80);
                c.set_flag(4, c.get_flag(2) ^ c.get_flag(3));
                c.cycles += 1;
            } else if (op & 0xFE0F) == 0x940A {
                // DEC
                let res = c.r[d].wrapping_sub(1);
                c.r[d] = res;
                c.set_flag(1, res == 0);
                c.set_flag(2, (res & 0x80) != 0);
                c.set_flag(3, res == 0x7F);
                c.set_flag(4, c.get_flag(2) ^ c.get_flag(3));
                c.cycles += 1;
            } else if (op & 0xFF0F) == 0x940B {
                // DES
                let round = ((op >> 4) & 0x0F) as usize;
                exec_des(c, round, c.get_flag(F_H));
                c.cycles += 1;
            } else if (op & 0xFE0F) == 0x9405 {
                // ASR
                let rd = c.r[d];
                let res = (rd >> 1) | (rd & 0x80);
                c.r[d] = res;
                c.set_flag(0, rd & 1 != 0);
                c.set_flag(1, res == 0);
                c.set_flag(2, res & 0x80 != 0);
                c.set_flag(3, c.get_flag(2) ^ c.get_flag(0));
                c.set_flag(4, c.get_flag(2) ^ c.get_flag(3));
                c.cycles += 1;
            } else if (op & 0xFE0F) == 0x9406 {
                // LSR
                let rd = c.r[d];
                let res = rd >> 1;
                c.r[d] = res;
                c.set_flag(0, rd & 1 != 0);
                c.set_flag(1, res == 0);
                c.set_flag(2, false);
                c.set_flag(3, c.get_flag(0));
                c.set_flag(4, c.get_flag(3));
                c.cycles += 1;
            } else if (op & 0xFE0F) == 0x9407 {
                // ROR
                let rd = c.r[d];
                let c_in = if c.get_flag(0) { 0x80 } else { 0 };
                let res = (rd >> 1) | c_in;
                c.r[d] = res;
                c.set_flag(0, rd & 1 != 0);
                c.set_flag(1, res == 0);
                c.set_flag(2, res & 0x80 != 0);
                c.set_flag(3, c.get_flag(2) ^ c.get_flag(0));
                c.set_flag(4, c.get_flag(2) ^ c.get_flag(3));
                c.cycles += 1;
            } else if (op & 0xFE0F) == 0x9402 {
                // SWAP
                c.r[d] = (c.r[d] << 4) | (c.r[d] >> 4);
                c.cycles += 1;
            } else if (op & 0xFE0E) == 0x940C {
                // JMP
                let nxt = c.flash_word(c.pc) as u32;
                c.pc += 2;
                let addr = nxt | (((op & 0x01F0) as u32) << 13) | (((op & 1) as u32) << 16);
                c.pc = addr * 2;
                c.cycles += 3;
            } else if (op & 0xFE0E) == 0x940E {
                // CALL
                let nxt = c.flash_word(c.pc) as u32;
                c.pc += 2;
                let addr = nxt | (((op & 0x01F0) as u32) << 13) | (((op & 1) as u32) << 16);
                push16(c, c.pc as u16);
                c.pc = addr * 2;
                c.cycles += cyc(c, 4, 3, 3, 0);
            } else if op == 0x9409 {
                // IJMP
                let z = (c.r[30] as u32) | ((c.r[31] as u32) << 8);
                c.pc = z * 2;
                c.cycles += 2;
            } else if op == 0x9509 {
                // ICALL
                let z = (c.r[30] as u32) | ((c.r[31] as u32) << 8);
                push16(c, c.pc as u16);
                c.pc = z * 2;
                c.cycles += cyc(c, 3, 2, 2, 3);
            } else if op == 0x9419 {
                // EIJMP
                let z = (c.r[30] as u32) | ((c.r[31] as u32) << 8) | ((c.eind as u32) << 16);
                c.pc = z * 2;
                c.cycles += 2;
            } else if op == 0x9519 {
                // EICALL
                let z = (c.r[30] as u32) | ((c.r[31] as u32) << 8) | ((c.eind as u32) << 16);
                push16(c, c.pc as u16);
                c.pc = z * 2;
                c.cycles += cyc(c, 4, 3, 3, 0);
            } else if op == 0x9508 {
                // RET
                c.pc = pop16(c) as u32;
                c.cycles += cyc(c, 4, 4, 4, 6);
            } else if op == 0x9518 {
                // RETI
                c.pc = pop16(c) as u32;
                c.set_flag(F_I, true);
                c.cycles += cyc(c, 4, 4, 4, 6);
            } else if op == 0x95A8 {
                // WDR
                c.cycles += 1;
            } else if op == 0x9588 {
                // SLEEP
                c.cycles += 1;
            } else if (op & 0xFF00) == 0x9B00 {
                // SBIS
                let a = ((op >> 3) & 0x1F) as u32;
                let b = (op & 0x07) as u8;
                let val = c.read_data(c.io_data_addr(a));
                c.cycles += 1;
                if (val & (1 << b)) != 0 {
                    do_skip(c);
                }
            } else if (op & 0xFF00) == 0x9900 {
                // SBIC
                let a = ((op >> 3) & 0x1F) as u32;
                let b = (op & 0x07) as u8;
                let val = c.read_data(c.io_data_addr(a));
                c.cycles += 1;
                if (val & (1 << b)) == 0 {
                    do_skip(c);
                }
            } else if (op & 0xFF00) == 0x9A00 {
                // SBI
                let a = ((op >> 3) & 0x1F) as u32;
                let b = (op & 0x07) as u8;
                let mut val = c.read_data(c.io_data_addr(a));
                val |= 1 << b;
                c.write_data(c.io_data_addr(a), val);
                c.cycles += 2;
            } else if (op & 0xFF00) == 0x9800 {
                // CBI
                let a = ((op >> 3) & 0x1F) as u32;
                let b = (op & 0x07) as u8;
                let mut val = c.read_data(c.io_data_addr(a));
                val &= !(1 << b);
                c.write_data(c.io_data_addr(a), val);
                c.cycles += 2;
            } else if op == 0x9598 {
                // BREAK
                c.cycles += 1;
            } else if (op & 0xFC00) == 0x9C00 {
                // MUL
                let d = ((op >> 4) & 0x1F) as usize;
                let r = (((op & 0x0200) >> 5) | (op & 0x000F)) as usize;
                let res = (c.r[d] as u16) * (c.r[r] as u16);
                c.r[0] = (res & 0xFF) as u8;
                c.r[1] = (res >> 8) as u8;
                c.set_flag(0, (res & 0x8000) != 0);
                c.set_flag(1, res == 0);
                c.cycles += 2;
            } else if (op & 0xFF00) == 0x9600 {
                // ADIW
                let dd = ((op >> 4) & 0x03) as usize;
                let di = 24 + dd * 2;
                let k6 = (((op >> 2) & 0x30) | (op & 0x0F)) as u16;
                let orig = (c.r[di] as u16) | ((c.r[di + 1] as u16) << 8);
                let r32 = (orig as u32).wrapping_add(k6 as u32);
                let res = r32 as u16;
                c.r[di] = (res & 0xFF) as u8;
                c.r[di + 1] = ((res >> 8) & 0xFF) as u8;
                c.set_flag(0, (r32 & 0x10000) != 0);
                c.set_flag(1, res == 0);
                c.set_flag(2, (res & 0x8000) != 0);
                c.set_flag(3, (!orig & res & 0x8000) != 0);
                c.set_flag(4, c.get_flag(2) ^ c.get_flag(3));
                c.cycles += 2;
            } else if (op & 0xFF00) == 0x9700 {
                // SBIW
                let dd = ((op >> 4) & 0x03) as usize;
                let di = 24 + dd * 2;
                let k6 = (((op >> 2) & 0x30) | (op & 0x0F)) as u16;
                let orig = (c.r[di] as u16) | ((c.r[di + 1] as u16) << 8);
                let r32 = (orig as u32).wrapping_sub(k6 as u32);
                let res = r32 as u16;
                c.r[di] = (res & 0xFF) as u8;
                c.r[di + 1] = ((res >> 8) & 0xFF) as u8;
                c.set_flag(0, (r32 & 0x10000) != 0);
                c.set_flag(1, res == 0);
                c.set_flag(2, (res & 0x8000) != 0);
                c.set_flag(3, (orig & !res & 0x8000) != 0);
                c.set_flag(4, c.get_flag(2) ^ c.get_flag(3));
                c.cycles += 2;
            } else {
                unknown_opcode(c, op, cur_pc);
            }
        }
        0xB000 => {
            if (op & 0xF800) == 0xB000 {
                // IN
                let a = ((op >> 5) & 0x30) | (op & 0x0F);
                c.r[d] = c.read_data(c.io_data_addr(a as u32));
                c.cycles += 1;
            } else if (op & 0xF800) == 0xB800 {
                // OUT
                let a = ((op >> 5) & 0x30) | (op & 0x0F);
                c.write_data(c.io_data_addr(a as u32), c.r[d]);
                c.cycles += 1;
            }
        }
        0xC000 => {
            // RJMP
            let mut k = (op & 0x0FFF) as i16;
            if k & 0x0800 != 0 {
                k |= -4096;
            }
            if k == -1 {
                c.cycles += 1;
                c.running = false;
            } else {
                c.pc = add_word_offset(c.pc, k);
                c.cycles += 2;
            }
        }
        0xD000 => {
            // RCALL
            let mut k = (op & 0x0FFF) as i16;
            if k & 0x0800 != 0 {
                k |= -4096;
            }
            push16(c, c.pc as u16);
            c.pc = add_word_offset(c.pc, k);
            c.cycles += cyc(c, 3, 2, 2, 3);
        }
        0xE000 => {
            // LDI
            let d4 = 16 + ((op >> 4) & 0x0F) as usize;
            c.r[d4] = k8 as u8;
            c.cycles += 1;
        }
        0xF000 => {
            if (op & 0xFC00) == 0xF000 || (op & 0xFC00) == 0xF400 {
                // BRBS / BRBC
                let s = (op & 0x07) as u8;
                let mut k = ((op >> 3) & 0x7F) as i8;
                if k & 0x40 != 0 {
                    k |= -128;
                }
                let is_brbs = (op & 0x0400) == 0;
                let flag = c.get_flag(s);
                if (is_brbs && flag) || (!is_brbs && !flag) {
                    c.pc = add_word_offset(c.pc, k as i16);
                    c.cycles += 2;
                } else {
                    c.cycles += 1;
                }
            } else if (op & 0xFE08) == 0xF800 {
                // BLD
                let b = (op & 0x07) as u8;
                let t = c.get_flag(F_T);
                if t {
                    c.r[d] |= 1 << b;
                } else {
                    c.r[d] &= !(1 << b);
                }
                c.cycles += 1;
            } else if (op & 0xFE08) == 0xFA00 {
                // BST
                let b = (op & 0x07) as u8;
                let val = (c.r[d] & (1 << b)) != 0;
                c.set_flag(F_T, val);
                c.cycles += 1;
            } else if (op & 0xFE08) == 0xFC00 {
                // SBRC
                let b = (op & 0x07) as u8;
                c.cycles += 1;
                if (c.r[d] & (1 << b)) == 0 {
                    do_skip(c);
                }
            } else if (op & 0xFE08) == 0xFE00 {
                // SBRS
                let b = (op & 0x07) as u8;
                c.cycles += 1;
                if (c.r[d] & (1 << b)) != 0 {
                    do_skip(c);
                }
            } else {
                unknown_opcode(c, op, cur_pc);
            }
        }
        _ => {
            unknown_opcode(c, op, cur_pc);
        }
    }

    // Emit the disassembled trace line, reading the just-updated state so jump
    // targets and result bytes match the executed effect (like the C `-t`).
    if c.trace {
        let text = crate::disasm::disasm(c, op, cur_pc);
        c.trace_line(format!("PC={:06X}  {}", cur_pc, text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm(flash_bytes: u32) -> AvrVm {
        AvrVm::new(
            "generic".to_string(),
            AvrCoreClass::Unknown,
            flash_bytes,
            512,
            4096,
            0x100,
        )
    }

    fn put_op(c: &mut AvrVm, byte_addr: u32, op: u16) {
        c.flash[byte_addr as usize] = (op & 0xFF) as u8;
        c.flash[byte_addr as usize + 1] = (op >> 8) as u8;
    }

    fn block_from_r0_r7(c: &AvrVm) -> u64 {
        let mut out = 0u64;
        for i in (0..8).rev() {
            out = (out << 8) | c.r[i] as u64;
        }
        out
    }

    #[test]
    fn mulsu_sign_extends_rd() {
        let mut c = vm(64);
        put_op(&mut c, 0, 0x0301); // MULSU R16,R17
        c.r[16] = 0xFF;
        c.r[17] = 0x02;

        step(&mut c);

        assert_eq!(c.r[0], 0xFE);
        assert_eq!(c.r[1], 0xFF);
        assert!(c.get_flag(0));
        assert!(!c.get_flag(1));
        assert_eq!(c.cycles, 2);
    }

    #[test]
    fn implements_fmuls_and_fmulsu() {
        let mut c = vm(64);
        put_op(&mut c, 0, 0x0380); // FMULS R16,R16
        c.r[16] = 0x80;

        step(&mut c);

        assert_eq!((c.r[1], c.r[0]), (0x80, 0x00));
        assert!(!c.get_flag(0));
        assert!(!c.get_flag(1));

        c.pc = 2;
        put_op(&mut c, 2, 0x0389); // FMULSU R16,R17
        c.r[16] = 0xFF;
        c.r[17] = 0x02;

        step(&mut c);

        assert_eq!((c.r[1], c.r[0]), (0xFF, 0xFC));
        assert!(c.get_flag(0));
        assert!(!c.get_flag(1));
    }

    #[test]
    fn elpm_rd_z_plus_uses_rampz_and_increments_24_bit_z() {
        let mut c = vm(0x20_000);
        put_op(&mut c, 0, 0x9007); // ELPM R0,Z+
        c.rampz = 1;
        set_pair(&mut c, 30, 2);
        c.flash[0x1_0002] = 0xAB;

        step(&mut c);

        assert_eq!(c.r[0], 0xAB);
        assert_eq!(c.rampz, 1);
        assert_eq!(pair(&c, 30), 3);
        assert_eq!(c.cycles, 3);
    }

    #[test]
    fn spm_z_plus_writes_r1_r0_to_flash_and_increments_z() {
        let mut c = vm(64);
        put_op(&mut c, 0, 0x95F8); // SPM Z+
        c.r[0] = 0x34;
        c.r[1] = 0x12;
        set_pair(&mut c, 30, 4);

        step(&mut c);

        assert_eq!(&c.flash[4..6], &[0x34, 0x12]);
        assert_eq!(pair(&c, 30), 6);
        assert_eq!(c.cycles, 4);
    }

    #[test]
    fn z_read_modify_write_opcodes_return_old_memory() {
        let mut c = vm(64);
        set_pair(&mut c, 30, 0x100);

        put_op(&mut c, 0, 0x9224); // XCH Z,R2
        c.write_data(0x100, 0x55);
        c.r[2] = 0xAA;
        step(&mut c);
        assert_eq!(c.r[2], 0x55);
        assert_eq!(c.read_data(0x100), 0xAA);

        c.pc = 2;
        put_op(&mut c, 2, 0x9235); // LAS Z,R3
        c.write_data(0x100, 0x50);
        c.r[3] = 0x0F;
        step(&mut c);
        assert_eq!(c.r[3], 0x50);
        assert_eq!(c.read_data(0x100), 0x5F);

        c.pc = 4;
        put_op(&mut c, 4, 0x9246); // LAC Z,R4
        c.write_data(0x100, 0xFF);
        c.r[4] = 0x0F;
        step(&mut c);
        assert_eq!(c.r[4], 0xFF);
        assert_eq!(c.read_data(0x100), 0xF0);

        c.pc = 6;
        put_op(&mut c, 6, 0x9257); // LAT Z,R5
        c.write_data(0x100, 0x55);
        c.r[5] = 0x0F;
        step(&mut c);
        assert_eq!(c.r[5], 0x55);
        assert_eq!(c.read_data(0x100), 0x5A);
    }

    #[test]
    fn sbrs_skips_when_register_bit_is_set() {
        let mut c = vm(64);
        put_op(&mut c, 0, 0xFE31); // SBRS R3,1
        put_op(&mut c, 2, 0x0000);
        c.r[3] = 0x02;

        step(&mut c);

        assert_eq!(c.pc, 4);
        assert_eq!(c.cycles, 2);
    }

    #[test]
    fn relative_jumps_and_branches_use_signed_word_offsets() {
        let mut c = vm(64);
        c.pc = 4;
        put_op(&mut c, 4, 0xCFFE); // RJMP -2

        step(&mut c);

        assert_eq!(c.pc, 2);
        assert_eq!(c.cycles, 2);

        c.pc = 4;
        c.cycles = 0;
        c.set_flag(0, true);
        put_op(&mut c, 4, 0xF3F8); // BRBS C,-1

        step(&mut c);

        assert_eq!(c.pc, 4);
        assert_eq!(c.cycles, 2);
    }

    #[test]
    fn out_uses_the_register_field_in_bits_8_to_4() {
        let mut c = vm(64);
        put_op(&mut c, 0, 0xB945); // OUT 0x25,R20
        c.r[20] = 0xAA;
        c.r[5] = 0x11;

        step(&mut c);

        assert_eq!(c.read_data(0x25), 0xAA);
    }

    #[test]
    fn des_rounds_match_standard_test_vector() {
        let mut c = vm(128);
        for round in 0..16 {
            put_op(&mut c, round * 2, 0x940B | ((round as u16) << 4));
        }

        let plaintext = 0x0123_4567_89AB_CDEFu64;
        let key = 0x1334_5779_9BBC_DFF1u64;
        for i in 0..8 {
            c.r[i] = (plaintext >> (8 * i)) as u8;
            c.r[8 + i] = (key >> (8 * i)) as u8;
        }

        for _ in 0..16 {
            step(&mut c);
        }

        assert_eq!(block_from_r0_r7(&c), 0x85E8_1354_0F0A_B405);
        assert_eq!(c.cycles, 16);
    }

    #[test]
    fn calls_push_and_pop_byte_return_addresses() {
        let mut c = vm(64);
        put_op(&mut c, 0, 0xD001); // RCALL +1 -> byte PC 4
        put_op(&mut c, 4, 0x9508); // RET
        let initial_sp = c.sp;

        step(&mut c);

        assert_eq!(c.pc, 4);
        assert_eq!(c.sp, initial_sp - 2);
        assert_eq!(c.read_data((initial_sp - 1) as u32), 0x02);
        assert_eq!(c.read_data(initial_sp as u32), 0x00);

        step(&mut c);

        assert_eq!(c.pc, 2);
        assert_eq!(c.sp, initial_sp);
    }

    #[test]
    fn rcall_negative_offset_pushes_byte_return_address() {
        let mut c = vm(64);
        c.pc = 4;
        put_op(&mut c, 4, 0xDFFE); // RCALL -2
        let initial_sp = c.sp;

        step(&mut c);

        assert_eq!(c.pc, 2);
        assert_eq!(c.sp, initial_sp - 2);
        assert_eq!(c.read_data((initial_sp - 1) as u32), 0x06);
        assert_eq!(c.read_data(initial_sp as u32), 0x00);
    }
}
