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

//! Exhaustive SREG conformance against the AVR Instruction Set Manual
//! (DS40002198). For every flag-affecting ALU instruction the expected status
//! bits are recomputed *directly from the manual's boolean formulas* — an
//! implementation independent from the VM's arithmetic-based flag code — and
//! checked across the full operand space. Any divergence between the silicon
//! spec and the emulator surfaces here.
//!
//! SREG bit layout (manual §A): C=0, Z=1, N=2, V=3, S=4, H=5, T=6, I=7.

use ik8bvm::core::AvrVm;
use ik8bvm::decode::step;
use ik8bvm::devices::AvrCoreClass;

const C: u8 = 1 << 0;
const Z: u8 = 1 << 1;
const N: u8 = 1 << 2;
const V: u8 = 1 << 3;
const S: u8 = 1 << 4;
const H: u8 = 1 << 5;

fn vm() -> AvrVm {
    // Unknown core class disables per-core opcode gating, so every encoding
    // executes; flag semantics are core-independent on classic 8-bit AVR.
    AvrVm::new("generic".to_string(), AvrCoreClass::Unknown, 256, 512, 64, 0x100)
}

fn put(c: &mut AvrVm, op: u16) {
    c.flash[0] = (op & 0xFF) as u8;
    c.flash[1] = (op >> 8) as u8;
    c.pc = 0;
}

#[inline]
fn b(x: u8, n: u8) -> bool {
    (x >> n) & 1 != 0
}
#[inline]
fn b16(x: u16, n: u8) -> bool {
    (x >> n) & 1 != 0
}

/// Assemble an SREG byte from the individual flag booleans.
fn sreg(c_: bool, z: bool, n: bool, v: bool, s: bool, h: bool) -> u8 {
    (c_ as u8) | (z as u8) << 1 | (n as u8) << 2 | (v as u8) << 3 | (s as u8) << 4 | (h as u8) << 5
}

// ---- reference flag models, transcribed from the manual ----

/// ADD/ADC: H,S,V,N,Z,C affected.  (manual §6.1 / §6.2)
fn ref_add(rd: u8, rr: u8, cin: u8) -> (u8, u8) {
    let res = rd.wrapping_add(rr).wrapping_add(cin);
    let h = (b(rd, 3) && b(rr, 3)) || (b(rr, 3) && !b(res, 3)) || (!b(res, 3) && b(rd, 3));
    let cf = (b(rd, 7) && b(rr, 7)) || (b(rr, 7) && !b(res, 7)) || (!b(res, 7) && b(rd, 7));
    let vf = (b(rd, 7) && b(rr, 7) && !b(res, 7)) || (!b(rd, 7) && !b(rr, 7) && b(res, 7));
    let nf = b(res, 7);
    let zf = res == 0;
    (sreg(cf, zf, nf, vf, nf ^ vf, h), H | S | V | N | Z | C)
}

/// SUB/SUBI/CP/CPI (cin=0) and SBC/SBCI/CPC (cin=carry, is_carry=true): the
/// carry variants keep Z unchanged on a zero result.  (manual §6.93 etc.)
fn ref_sub(rd: u8, rr: u8, cin: u8, is_carry: bool, prev_z: bool) -> (u8, u8) {
    let res = rd.wrapping_sub(rr).wrapping_sub(cin);
    let h = (!b(rd, 3) && b(rr, 3)) || (b(rr, 3) && b(res, 3)) || (b(res, 3) && !b(rd, 3));
    let cf = (!b(rd, 7) && b(rr, 7)) || (b(rr, 7) && b(res, 7)) || (b(res, 7) && !b(rd, 7));
    let vf = (b(rd, 7) && !b(rr, 7) && !b(res, 7)) || (!b(rd, 7) && b(rr, 7) && b(res, 7));
    let nf = b(res, 7);
    let zf = if is_carry { res == 0 && prev_z } else { res == 0 };
    (sreg(cf, zf, nf, vf, nf ^ vf, h), H | S | V | N | Z | C)
}

/// AND/ANDI/OR/ORI/EOR: S,V(=0),N,Z affected.
fn ref_logic(res: u8) -> (u8, u8) {
    let nf = b(res, 7);
    (sreg(false, res == 0, nf, false, nf, false), S | V | N | Z)
}

fn run2(op: u16, rd: u8, rr: u8, init: u8) -> u8 {
    let mut c = vm();
    put(&mut c, op);
    c.r[16] = rd;
    c.r[17] = rr;
    c.sreg = init;
    step(&mut c);
    c.sreg
}

fn enc_2op(base: u16, d: u8, r: u8) -> u16 {
    base | (((r as u16) & 0x10) << 5) | (((d as u16) & 0x1F) << 4) | ((r as u16) & 0x0F)
}

fn enc_imm(base: u16, d: u8, k: u8) -> u16 {
    base | (((k as u16) & 0xF0) << 4) | ((((d - 16) as u16) & 0x0F) << 4) | ((k as u16) & 0x0F)
}

fn enc_unary(base: u16, d: u8) -> u16 {
    base | (((d as u16) & 0x1F) << 4)
}

fn assert_two_op(name: &str, base: u16, reference: impl Fn(u8, u8, u8, bool) -> (u8, u8), carry: bool, is_carry_z: bool) {
    let op = enc_2op(base, 16, 17);
    for init in [0x00u8, 0x01, 0x02, 0x03, 0xFF] {
        let cin = if carry { (init & 1) } else { 0 };
        let prev_z = init & Z != 0;
        for rd in 0..=255u8 {
            for rr in 0..=255u8 {
                let got = run2(op, rd, rr, init);
                let (bits, mask) = reference(rd, rr, cin, prev_z);
                let _ = is_carry_z;
                let want = (init & !mask) | (bits & mask);
                assert_eq!(
                    got, want,
                    "{name}: rd={rd:#04x} rr={rr:#04x} cin={cin} init={init:#04x} -> got {got:#04x} want {want:#04x}"
                );
            }
        }
    }
}

#[test]
fn add_matches_manual() {
    assert_two_op("ADD", 0x0C00, |rd, rr, _c, _z| ref_add(rd, rr, 0), false, false);
}

#[test]
fn adc_matches_manual() {
    assert_two_op("ADC", 0x1C00, |rd, rr, c, _z| ref_add(rd, rr, c), true, false);
}

#[test]
fn sub_matches_manual() {
    assert_two_op("SUB", 0x1800, |rd, rr, _c, _z| ref_sub(rd, rr, 0, false, false), false, false);
}

#[test]
fn sbc_matches_manual() {
    // Covers the Rd=0,Rr=0xFF,cin=1 borrow corner (true result -256).
    assert_two_op("SBC", 0x0800, |rd, rr, c, z| ref_sub(rd, rr, c, true, z), true, true);
}

#[test]
fn cp_matches_manual() {
    let op = enc_2op(0x1400, 16, 17);
    for init in [0x00u8, 0xFF, 0x2A] {
        for rd in 0..=255u8 {
            for rr in 0..=255u8 {
                let mut c = vm();
                put(&mut c, op);
                c.r[16] = rd;
                c.r[17] = rr;
                c.sreg = init;
                step(&mut c);
                let (bits, mask) = ref_sub(rd, rr, 0, false, false);
                assert_eq!(c.sreg, (init & !mask) | (bits & mask), "CP rd={rd:#x} rr={rr:#x}");
                // CP must not modify the operands.
                assert_eq!((c.r[16], c.r[17]), (rd, rr));
            }
        }
    }
}

#[test]
fn cpc_matches_manual() {
    let op = enc_2op(0x0400, 16, 17);
    for init in [0x00u8, 0x01, 0x02, 0x03, 0xFF] {
        let cin = init & 1;
        let prev_z = init & Z != 0;
        for rd in 0..=255u8 {
            for rr in 0..=255u8 {
                let got = run2(op, rd, rr, init);
                let (bits, mask) = ref_sub(rd, rr, cin, true, prev_z);
                assert_eq!(got, (init & !mask) | (bits & mask), "CPC rd={rd:#x} rr={rr:#x} init={init:#x}");
            }
        }
    }
}

#[test]
fn logic_ops_match_manual() {
    let cases: [(&str, u16, fn(u8, u8) -> u8); 3] = [
        ("AND", 0x2000, |a, b| a & b),
        ("OR", 0x2800, |a, b| a | b),
        ("EOR", 0x2400, |a, b| a ^ b),
    ];
    for (name, base, f) in cases {
        let op = enc_2op(base, 16, 17);
        for init in [0x00u8, 0xFF] {
            for rd in 0..=255u8 {
                for rr in 0..=255u8 {
                    let got = run2(op, rd, rr, init);
                    let (bits, mask) = ref_logic(f(rd, rr));
                    assert_eq!(got, (init & !mask) | (bits & mask), "{name} rd={rd:#x} rr={rr:#x}");
                }
            }
        }
    }
}

#[test]
fn imm_ops_match_manual() {
    // (name, base, is_sub_family, carry, logic_fn)
    for k in 0..=255u8 {
        for init in [0x00u8, 0x01, 0x03, 0xFF] {
            let cin = init & 1;
            let prev_z = init & Z != 0;
            for rd in 0..=255u8 {
                // SUBI
                let mut c = vm();
                put(&mut c, enc_imm(0x5000, 16, k));
                c.r[16] = rd;
                c.sreg = init;
                step(&mut c);
                let (bits, mask) = ref_sub(rd, k, 0, false, false);
                assert_eq!(c.sreg, (init & !mask) | (bits & mask), "SUBI rd={rd:#x} k={k:#x}");
                assert_eq!(c.r[16], rd.wrapping_sub(k));

                // SBCI
                let mut c = vm();
                put(&mut c, enc_imm(0x4000, 16, k));
                c.r[16] = rd;
                c.sreg = init;
                step(&mut c);
                let (bits, mask) = ref_sub(rd, k, cin, true, prev_z);
                assert_eq!(c.sreg, (init & !mask) | (bits & mask), "SBCI rd={rd:#x} k={k:#x} init={init:#x}");
                assert_eq!(c.r[16], rd.wrapping_sub(k).wrapping_sub(cin));

                // CPI
                let got = {
                    let mut c = vm();
                    put(&mut c, enc_imm(0x3000, 16, k));
                    c.r[16] = rd;
                    c.sreg = init;
                    step(&mut c);
                    c.sreg
                };
                let (bits, mask) = ref_sub(rd, k, 0, false, false);
                assert_eq!(got, (init & !mask) | (bits & mask), "CPI rd={rd:#x} k={k:#x}");

                // ANDI / ORI
                for (base, f) in [(0x7000u16, (|a: u8, b: u8| a & b) as fn(u8, u8) -> u8), (0x6000u16, |a, b| a | b)] {
                    let mut c = vm();
                    put(&mut c, enc_imm(base, 16, k));
                    c.r[16] = rd;
                    c.sreg = init;
                    step(&mut c);
                    let (bits, mask) = ref_logic(f(rd, k));
                    assert_eq!(c.sreg, (init & !mask) | (bits & mask), "imm-logic rd={rd:#x} k={k:#x}");
                }
            }
        }
    }
}

#[test]
fn unary_ops_match_manual() {
    for init in [0x00u8, 0x01, 0xFF, 0x2A] {
        let prev_c = init & 1 != 0;
        for rd in 0..=255u8 {
            // COM: res=!rd; C=1, V=0, N, Z, S. (H unchanged)
            {
                let res = !rd;
                let nf = b(res, 7);
                let bits = sreg(true, res == 0, nf, false, nf, false);
                let mask = C | Z | N | V | S;
                let got = run_unary(0x9400, rd, init);
                assert_eq!(got.1, (init & !mask) | (bits & mask), "COM rd={rd:#x}");
                assert_eq!(got.0, res);
            }
            // NEG: res=0-rd; H=R3|Rd3; V=(res==0x80); N; Z; C=(res!=0); S.
            {
                let res = 0u8.wrapping_sub(rd);
                let nf = b(res, 7);
                let vf = res == 0x80;
                let h = b(res, 3) || b(rd, 3);
                let bits = sreg(res != 0, res == 0, nf, vf, nf ^ vf, h);
                let mask = H | S | V | N | Z | C;
                let got = run_unary(0x9401, rd, init);
                assert_eq!(got.1, (init & !mask) | (bits & mask), "NEG rd={rd:#x}");
                assert_eq!(got.0, res);
            }
            // INC: V=(res==0x80); N; Z; S. (C,H unchanged)
            {
                let res = rd.wrapping_add(1);
                let nf = b(res, 7);
                let vf = res == 0x80;
                let bits = sreg(false, res == 0, nf, vf, nf ^ vf, false);
                let mask = S | V | N | Z;
                let got = run_unary(0x9403, rd, init);
                assert_eq!(got.1, (init & !mask) | (bits & mask), "INC rd={rd:#x}");
                assert_eq!(got.0, res);
            }
            // DEC: V=(res==0x7F); N; Z; S. (C,H unchanged)
            {
                let res = rd.wrapping_sub(1);
                let nf = b(res, 7);
                let vf = res == 0x7F;
                let bits = sreg(false, res == 0, nf, vf, nf ^ vf, false);
                let mask = S | V | N | Z;
                let got = run_unary(0x940A, rd, init);
                assert_eq!(got.1, (init & !mask) | (bits & mask), "DEC rd={rd:#x}");
                assert_eq!(got.0, res);
            }
            // ASR: res=(rd>>1)|(rd&0x80); C=rd0; N; Z; V=N^C; S. (H unchanged)
            {
                let res = (rd >> 1) | (rd & 0x80);
                let cf = b(rd, 0);
                let nf = b(res, 7);
                let vf = nf ^ cf;
                let bits = sreg(cf, res == 0, nf, vf, nf ^ vf, false);
                let mask = C | Z | N | V | S;
                let got = run_unary(0x9405, rd, init);
                assert_eq!(got.1, (init & !mask) | (bits & mask), "ASR rd={rd:#x}");
                assert_eq!(got.0, res);
            }
            // LSR: res=rd>>1; C=rd0; N=0; Z; V=N^C; S. (H unchanged)
            {
                let res = rd >> 1;
                let cf = b(rd, 0);
                let nf = false;
                let vf = nf ^ cf;
                let bits = sreg(cf, res == 0, nf, vf, nf ^ vf, false);
                let mask = C | Z | N | V | S;
                let got = run_unary(0x9406, rd, init);
                assert_eq!(got.1, (init & !mask) | (bits & mask), "LSR rd={rd:#x}");
                assert_eq!(got.0, res);
            }
            // ROR: res=(rd>>1)|(prevC<<7); C=rd0; N; Z; V=N^C; S. (H unchanged)
            {
                let res = (rd >> 1) | ((prev_c as u8) << 7);
                let cf = b(rd, 0);
                let nf = b(res, 7);
                let vf = nf ^ cf;
                let bits = sreg(cf, res == 0, nf, vf, nf ^ vf, false);
                let mask = C | Z | N | V | S;
                let got = run_unary(0x9407, rd, init);
                assert_eq!(got.1, (init & !mask) | (bits & mask), "ROR rd={rd:#x} init={init:#x}");
                assert_eq!(got.0, res);
            }
            // SWAP: no flags, just nibble swap.
            {
                let res = (rd << 4) | (rd >> 4);
                let got = run_unary(0x9402, rd, init);
                assert_eq!(got.1, init, "SWAP must not touch SREG");
                assert_eq!(got.0, res);
            }
        }
    }
}

/// Returns (result register, resulting SREG) for a `1001 010d dddd ....` op on r16.
fn run_unary(base: u16, rd: u8, init: u8) -> (u8, u8) {
    let mut c = vm();
    put(&mut c, enc_unary(base, 16));
    c.r[16] = rd;
    c.sreg = init;
    step(&mut c);
    (c.r[16], c.sreg)
}

#[test]
fn adiw_sbiw_match_manual() {
    for k in 0..=63u8 {
        for init in [0x00u8, 0xFF, 0x2A] {
            for hi in [0x00u8, 0x7F, 0x80, 0xFF, 0x01] {
                for lo in [0x00u8, 0x01, 0xFF, 0x80] {
                    let orig = (lo as u16) | ((hi as u16) << 8);

                    // ADIW r24, k
                    {
                        let res = orig.wrapping_add(k as u16);
                        let cf = (orig as u32 + k as u32) > 0xFFFF;
                        let vf = !b16(orig, 15) && b16(res, 15);
                        let nf = b16(res, 15);
                        let bits = sreg(cf, res == 0, nf, vf, nf ^ vf, false);
                        let mask = C | Z | N | V | S;
                        let (rl, rh, got) = run_word(0x9600, k, lo, hi, init);
                        assert_eq!(got, (init & !mask) | (bits & mask), "ADIW orig={orig:#06x} k={k}");
                        assert_eq!((rl as u16) | ((rh as u16) << 8), res);
                    }
                    // SBIW r24, k
                    {
                        let res = orig.wrapping_sub(k as u16);
                        let cf = (orig as i32 - k as i32) < 0;
                        let vf = b16(orig, 15) && !b16(res, 15);
                        let nf = b16(res, 15);
                        let bits = sreg(cf, res == 0, nf, vf, nf ^ vf, false);
                        let mask = C | Z | N | V | S;
                        let (rl, rh, got) = run_word(0x9700, k, lo, hi, init);
                        assert_eq!(got, (init & !mask) | (bits & mask), "SBIW orig={orig:#06x} k={k}");
                        assert_eq!((rl as u16) | ((rh as u16) << 8), res);
                    }
                }
            }
        }
    }
}

fn run_word(base: u16, k: u8, lo: u8, hi: u8, init: u8) -> (u8, u8, u8) {
    let op = base | (((k as u16) & 0x30) << 2) | ((k as u16) & 0x0F); // dd=0 -> r24
    let mut c = vm();
    put(&mut c, op);
    c.r[24] = lo;
    c.r[25] = hi;
    c.sreg = init;
    step(&mut c);
    (c.r[24], c.r[25], c.sreg)
}

#[test]
fn mul_family_matches_manual() {
    // MUL: C=R15, Z=(result==0); result in R1:R0.
    let op = 0x9C00 | (((17u16) & 0x10) << 5) | ((16u16) << 4) | (17u16 & 0x0F);
    for rd in 0..=255u8 {
        for rr in 0..=255u8 {
            let mut c = vm();
            put(&mut c, op);
            c.r[16] = rd;
            c.r[17] = rr;
            c.sreg = 0x2A;
            step(&mut c);
            let res = (rd as u16) * (rr as u16);
            assert_eq!((c.r[0] as u16) | ((c.r[1] as u16) << 8), res, "MUL {rd}*{rr}");
            assert_eq!(c.sreg & C != 0, b16(res, 15), "MUL C {rd}*{rr}");
            assert_eq!(c.sreg & Z != 0, res == 0, "MUL Z {rd}*{rr}");
        }
    }

    // MULS r16,r17 (signed): C=R15, Z.
    let op = 0x0200 | ((0u16) << 4) | 1u16; // MULS R16,R17
    for rd in 0..=255u8 {
        for rr in 0..=255u8 {
            let mut c = vm();
            put(&mut c, op);
            c.r[16] = rd;
            c.r[17] = rr;
            c.sreg = 0x2A;
            step(&mut c);
            let res = ((rd as i8 as i16) * (rr as i8 as i16)) as u16;
            assert_eq!((c.r[0] as u16) | ((c.r[1] as u16) << 8), res, "MULS {rd}*{rr}");
            assert_eq!(c.sreg & C != 0, b16(res, 15), "MULS C {rd}*{rr}");
            assert_eq!(c.sreg & Z != 0, res == 0, "MULS Z {rd}*{rr}");
        }
    }
}

/// BSET/BCLR (and their SE_/CL_ aliases) set/clear exactly one SREG bit.
#[test]
fn bset_bclr_cover_all_bits() {
    for s in 0..8u16 {
        let mut c = vm();
        put(&mut c, 0x9408 | (s << 4)); // BSET s
        c.sreg = 0x00;
        step(&mut c);
        assert_eq!(c.sreg, 1 << s, "BSET {s}");

        let mut c = vm();
        put(&mut c, 0x9488 | (s << 4)); // BCLR s
        c.sreg = 0xFF;
        step(&mut c);
        assert_eq!(c.sreg, !(1u8 << s), "BCLR {s}");
    }
}
