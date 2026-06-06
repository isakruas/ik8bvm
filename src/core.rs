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

use crate::devices::AvrCoreClass;
use crate::hw;

const IO_TCCR0A: u32 = 0x24;
const IO_TCCR0B: u32 = 0x25;
const IO_TCNT0: u32 = 0x26;
const IO_OCR0A: u32 = 0x27;
const IO_TIFR0: u32 = 0x15;
const IO_TIMSK0: u32 = 0x4E;
const BIT_OCF0A: u8 = 1;

pub struct AvrVm {
    pub r: [u8; 32],
    pub pc: u32,
    pub sp: u16,
    pub sreg: u8,
    pub rampx: u8,
    pub rampy: u8,
    pub rampz: u8,
    pub rampd: u8,
    pub eind: u8,

    pub flash: Vec<u8>,
    pub sram: Vec<u8>,
    pub eeprom: Vec<u8>,
    pub io: Vec<u8>,

    pub device: String,
    pub core: AvrCoreClass,
    pub flash_bytes: u32,
    pub sram_bytes: u32,
    pub eeprom_bytes: u32,
    pub io_bytes: u32,
    pub sram_start: u32,

    pub cycles: u64,
    pub running: bool,
    pub trace: bool,
    /// Captured instruction trace (filled only while `trace` is on). Kept in a
    /// buffer instead of printed to stdout so a host (e.g. the IDE) can show it.
    pub trace_buf: Vec<String>,
    /// Cap on `trace_buf` length (0 = unlimited) to protect the host UI.
    pub trace_limit: usize,
    /// Set when the trace was cut short by `trace_limit`.
    pub trace_truncated: bool,
    pub unknown_opcode: bool,
    pub eempe_timer: u32,
    pub eeprom_write_cycles_left: u32,
    pub eeprom_write_addr: u32,
    pub eeprom_write_val: u8,
    pub timer0_acc: u32,
    pub timer0_compa_vec: u8,
    pub irq_pending: [bool; 256],
}

impl AvrVm {
    pub fn new(
        device: String,
        core: AvrCoreClass,
        flash_bytes: u32,
        sram_bytes: u32,
        eeprom_bytes: u32,
        sram_start: u32,
    ) -> Self {
        let io_bytes = sram_start.saturating_sub(0x20);
        let timer0_compa_vec = timer0_compa_vec_for(&device);
        Self {
            r: [0; 32],
            pc: 0,
            sp: (sram_start + sram_bytes - 1) as u16,
            sreg: 0,
            rampx: 0,
            rampy: 0,
            rampz: 0,
            rampd: 0,
            eind: 0,
            flash: vec![0; flash_bytes as usize],
            sram: vec![0; sram_bytes as usize],
            eeprom: vec![0; eeprom_bytes.max(1) as usize],
            io: vec![0; io_bytes as usize],
            device,
            core,
            flash_bytes,
            sram_bytes,
            eeprom_bytes,
            io_bytes,
            sram_start,
            cycles: 0,
            running: true,
            trace: false,
            trace_buf: Vec::new(),
            trace_limit: 0,
            trace_truncated: false,
            unknown_opcode: false,
            eempe_timer: 0,
            eeprom_write_cycles_left: 0,
            eeprom_write_addr: 0,
            eeprom_write_val: 0,
            timer0_acc: 0,
            timer0_compa_vec,
            irq_pending: [false; 256],
        }
    }

    pub fn set_flag(&mut self, bit: u8, val: bool) {
        if val {
            self.sreg |= 1 << bit;
        } else {
            self.sreg &= !(1 << bit);
        }
    }

    pub fn get_flag(&self, bit: u8) -> bool {
        (self.sreg & (1 << bit)) != 0
    }

    /// Record a trace line when tracing is enabled, honoring `trace_limit`.
    pub fn trace_line(&mut self, line: String) {
        if !self.trace {
            return;
        }
        if self.trace_limit != 0 && self.trace_buf.len() >= self.trace_limit {
            self.trace_truncated = true;
            return;
        }
        self.trace_buf.push(line);
    }

    pub fn read_data(&self, addr: u32) -> u8 {
        if addr < 32 {
            self.r[addr as usize]
        } else if self.core != AvrCoreClass::RC && addr == 0x5F {
            self.sreg
        } else if self.core != AvrCoreClass::RC && addr == 0x5D {
            (self.sp & 0xFF) as u8
        } else if self.core != AvrCoreClass::RC && addr == 0x5E {
            (self.sp >> 8) as u8
        } else if addr < self.sram_start {
            let io_addr = addr - 0x20;

            if let Some(ee) = hw::eeprom_hw(&self.device) {
                if ee.is_modern && io_addr == ee.data {
                    let eear = self.eeprom_address(ee);
                    if eear < self.eeprom_bytes {
                        return self.eeprom[eear as usize];
                    }
                }
            }

            if let Some(spi) = hw::spi_hw(&self.device) {
                if io_addr == spi.status {
                    return self.io_read_raw(io_addr) | 0x80;
                }
            }

            if let Some(twi) = hw::twi_hw(&self.device) {
                if io_addr == twi.ctrl {
                    return self.io_read_raw(io_addr) | 0x80;
                }
            }

            if let Some(uart) = hw::uart_hw(&self.device) {
                if io_addr == uart.status {
                    return self.io_read_raw(io_addr) | 0x60;
                }
            }

            self.io_read_raw(io_addr)
        } else if addr < self.sram_start + self.sram_bytes {
            self.sram[(addr - self.sram_start) as usize]
        } else {
            0
        }
    }

    pub fn write_data(&mut self, addr: u32, v: u8) {
        if addr < 32 {
            self.r[addr as usize] = v;
        } else if self.core != AvrCoreClass::RC && addr == 0x5F {
            self.sreg = v;
        } else if self.core != AvrCoreClass::RC && addr == 0x5D {
            self.sp = (self.sp & 0xFF00) | (v as u16);
        } else if self.core != AvrCoreClass::RC && addr == 0x5E {
            self.sp = (self.sp & 0x00FF) | ((v as u16) << 8);
        } else if addr < self.sram_start {
            let io_addr = addr - 0x20;
            let ee = hw::eeprom_hw(&self.device);

            if let Some(ee) = ee {
                if self.eeprom_write_cycles_left > 0
                    && (io_addr == ee.ctrl
                        || io_addr == ee.addr_l
                        || io_addr == ee.addr_h
                        || io_addr == ee.data)
                {
                    return;
                }
            }

            let old = self.io_read_raw(io_addr);
            self.io_write_raw(io_addr, v);

            if let Some(ee) = ee {
                if !ee.is_modern && io_addr == ee.ctrl {
                    self.eeprom_handle_eecr_write(old, v, ee);
                } else if ee.is_modern && io_addr == ee.ctrl {
                    self.eeprom_handle_modern_write(v, ee);
                }
            }
        } else if addr < self.sram_start + self.sram_bytes {
            self.sram[(addr - self.sram_start) as usize] = v;
        }
    }

    pub fn flash_word(&self, byte_addr: u32) -> u16 {
        if byte_addr + 1 < self.flash_bytes {
            let low = self.flash[byte_addr as usize] as u16;
            let high = self.flash[(byte_addr + 1) as usize] as u16;
            low | (high << 8)
        } else {
            0
        }
    }

    pub fn flash_byte(&self, byte_addr: u32) -> u8 {
        if byte_addr < self.flash_bytes {
            self.flash[byte_addr as usize]
        } else {
            0
        }
    }

    pub fn flags_add(&mut self, a: u8, b: u8, res: u8, cin: u8) {
        let full = (a as u16).wrapping_add(b as u16).wrapping_add(cin as u16);
        self.set_flag(5, (((a & 0x0F) + (b & 0x0F) + cin) & 0x10) != 0); // H
        self.set_flag(0, (full & 0x100) != 0); // C
        self.set_flag(1, res == 0); // Z
        self.set_flag(2, (res & 0x80) != 0); // N
        self.set_flag(3, ((!(a ^ b) & (a ^ res)) & 0x80) != 0); // V
        self.set_flag(4, self.get_flag(2) ^ self.get_flag(3)); // S
    }

    pub fn flags_sub(&mut self, a: u8, b: u8, cin: u8, carry_is_borrow: bool) {
        let full = (a as i32) - (b as i32) - (cin as i32);
        let res = full as u8;
        let hb = (!a & b) | (b & res) | (res & !a);

        self.set_flag(5, (hb & 0x08) != 0); // H
        self.set_flag(0, (full & 0x100) != 0); // C

        if carry_is_borrow && res == 0 {
            // Preserve Z
        } else {
            self.set_flag(1, res == 0); // Z
        }

        self.set_flag(2, (res & 0x80) != 0); // N
        self.set_flag(3, (((a & !b & !res) | (!a & b & res)) & 0x80) != 0); // V
        self.set_flag(4, self.get_flag(2) ^ self.get_flag(3)); // S
    }

    pub fn flags_logic(&mut self, res: u8) {
        self.set_flag(1, res == 0); // Z
        self.set_flag(2, (res & 0x80) != 0); // N
        self.set_flag(3, false); // V (cleared)
        self.set_flag(4, self.get_flag(2) ^ self.get_flag(3)); // S
    }

    pub fn step(&mut self) {
        let start_cycles = self.cycles;
        crate::decode::step(self);
        let consumed = self.cycles.saturating_sub(start_cycles);
        self.advance_eeprom_timers(consumed);
        self.timer0_tick(consumed as u32);
        self.service_interrupts();
    }

    pub fn raise_interrupt(&mut self, vector_index: u8) {
        if vector_index != 0 {
            self.irq_pending[vector_index as usize] = true;
        }
    }

    pub fn reset(&mut self) {
        self.r = [0; 32];
        self.io.fill(0);
        self.sram.fill(0);
        self.pc = 0;
        self.sp = (self.sram_start + self.sram_bytes - 1) as u16;
        self.sreg = 0;
        self.rampx = 0;
        self.rampy = 0;
        self.rampz = 0;
        self.rampd = 0;
        self.eind = 0;
        self.cycles = 0;
        self.running = true;
        self.trace_buf.clear();
        self.trace_truncated = false;
        self.unknown_opcode = false;
        self.eempe_timer = 0;
        self.eeprom_write_cycles_left = 0;
        self.eeprom_write_addr = 0;
        self.eeprom_write_val = 0;
        self.timer0_acc = 0;
        self.irq_pending = [false; 256];
    }

    fn io_read_raw(&self, io_addr: u32) -> u8 {
        self.io.get(io_addr as usize).copied().unwrap_or(0)
    }

    fn io_write_raw(&mut self, io_addr: u32, v: u8) {
        if let Some(slot) = self.io.get_mut(io_addr as usize) {
            *slot = v;
        }
    }

    fn io_set_bits(&mut self, io_addr: u32, mask: u8) {
        let v = self.io_read_raw(io_addr) | mask;
        self.io_write_raw(io_addr, v);
    }

    fn io_clear_bits(&mut self, io_addr: u32, mask: u8) {
        let v = self.io_read_raw(io_addr) & !mask;
        self.io_write_raw(io_addr, v);
    }

    fn eeprom_address(&self, ee: hw::EepromHw) -> u32 {
        self.io_read_raw(ee.addr_l) as u32 | (((self.io_read_raw(ee.addr_h) & 0x0F) as u32) << 8)
    }

    fn eeprom_handle_eecr_write(&mut self, old_eecr: u8, new_eecr: u8, ee: hw::EepromHw) {
        let eear = self.eeprom_address(ee);

        if (new_eecr & (1 << 2)) != 0 {
            self.eempe_timer = 4;
        } else {
            self.eempe_timer = 0;
        }

        if (new_eecr & (1 << 0)) != 0 && (old_eecr & (1 << 0)) == 0 {
            if eear < self.eeprom_bytes {
                self.io_write_raw(ee.data, self.eeprom[eear as usize]);
            }
            self.io_clear_bits(ee.ctrl, 1 << 0);
        }

        if (new_eecr & (1 << 1)) != 0 && (old_eecr & (1 << 1)) == 0 {
            if self.eempe_timer > 0 {
                self.eeprom_write_addr = eear;
                self.eeprom_write_val = self.io_read_raw(ee.data);
                self.eeprom_write_cycles_left = 64;
                self.eempe_timer = 0;
                self.io_clear_bits(ee.ctrl, 1 << 2);
            } else {
                self.io_clear_bits(ee.ctrl, 1 << 1);
            }
        }
    }

    fn eeprom_handle_modern_write(&mut self, new_ctrl: u8, ee: hw::EepromHw) {
        if new_ctrl == 0x04 {
            self.eeprom_write_addr = self.eeprom_address(ee);
            self.eeprom_write_val = self.io_read_raw(ee.data);
            self.eeprom_write_cycles_left = 64;
            self.io_set_bits(ee.status, 0x01);
        }
    }

    fn advance_eeprom_timers(&mut self, consumed: u64) {
        let consumed = consumed.min(u32::MAX as u64) as u32;
        let ee = hw::eeprom_hw(&self.device);
        let ctrl_reg = ee.map(|ee| ee.ctrl).unwrap_or(0x1F);

        if self.eempe_timer > 0 {
            if consumed >= self.eempe_timer {
                self.eempe_timer = 0;
                if !ee.map(|ee| ee.is_modern).unwrap_or(false) {
                    self.io_clear_bits(ctrl_reg, 1 << 2);
                }
            } else {
                self.eempe_timer -= consumed;
            }
        }

        if self.eeprom_write_cycles_left > 0 {
            if consumed >= self.eeprom_write_cycles_left {
                self.eeprom_write_cycles_left = 0;
                if self.eeprom_write_addr < self.eeprom_bytes {
                    self.eeprom[self.eeprom_write_addr as usize] = self.eeprom_write_val;
                }

                if let Some(ee) = ee {
                    if ee.is_modern {
                        self.io_clear_bits(ee.status, 0x01);
                        self.io_clear_bits(ee.ctrl, 0x07);
                    } else {
                        self.io_clear_bits(ctrl_reg, 1 << 1);
                    }
                } else {
                    self.io_clear_bits(ctrl_reg, 1 << 1);
                }
            } else {
                self.eeprom_write_cycles_left -= consumed;
            }
        }
    }

    fn timer0_tick(&mut self, cycles: u32) {
        if self.timer0_compa_vec == 0 {
            return;
        }

        let prescaler = match self.io_read_raw(IO_TCCR0B) & 0x07 {
            1 => 1,
            2 => 8,
            3 => 64,
            4 => 256,
            5 => 1024,
            _ => return,
        };

        let ctc = (self.io_read_raw(IO_TCCR0A) & 0x03) == 0x02;
        let ocr = self.io_read_raw(IO_OCR0A);
        self.timer0_acc = self.timer0_acc.saturating_add(cycles);

        while self.timer0_acc >= prescaler {
            self.timer0_acc -= prescaler;
            let t = self.io_read_raw(IO_TCNT0);
            if ctc && t == ocr {
                self.io_write_raw(IO_TCNT0, 0);
                self.io_set_bits(IO_TIFR0, 1 << BIT_OCF0A);
            } else {
                let next = t.wrapping_add(1);
                self.io_write_raw(IO_TCNT0, next);
                if !ctc && next == ocr {
                    self.io_set_bits(IO_TIFR0, 1 << BIT_OCF0A);
                }
            }
        }
    }

    fn service_interrupts(&mut self) {
        if self.timer0_compa_vec != 0
            && (self.io_read_raw(IO_TIMSK0) & (1 << BIT_OCF0A)) != 0
            && (self.io_read_raw(IO_TIFR0) & (1 << BIT_OCF0A)) != 0
        {
            self.raise_interrupt(self.timer0_compa_vec);
        }

        if !self.get_flag(7) {
            return;
        }

        let Some(vec) = (1..self.irq_pending.len()).find(|&i| self.irq_pending[i]) else {
            return;
        };

        self.irq_pending[vec] = false;
        self.set_flag(7, false);

        if self.timer0_compa_vec != 0 && vec as u8 == self.timer0_compa_vec {
            self.io_clear_bits(IO_TIFR0, 1 << BIT_OCF0A);
        }

        self.push16(self.pc as u16);
        self.pc = vec as u32 * self.vector_slot_bytes();
        self.cycles += 5;

        if self.trace {
            self.trace_line(format!("IRQ vector {} -> PC=0x{:06X}", vec, self.pc));
        }
    }

    fn vector_slot_bytes(&self) -> u32 {
        if self.core == AvrCoreClass::RC {
            2
        } else {
            4
        }
    }

    fn push8(&mut self, v: u8) {
        self.write_data(self.sp as u32, v);
        self.sp = self.sp.wrapping_sub(1);
    }

    fn push16(&mut self, v: u16) {
        self.push8((v >> 8) as u8);
        self.push8((v & 0xFF) as u8);
    }
}

fn timer0_compa_vec_for(device: &str) -> u8 {
    match device {
        "atmega328" | "atmega328p" => 14,
        "atmega1284" | "atmega1284p" => 16,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atmega328p() -> AvrVm {
        let mut vm = AvrVm::new(
            "atmega328p".to_string(),
            AvrCoreClass::EP,
            32768,
            2048,
            1024,
            0x100,
        );
        vm.sp = 0x08FF;
        vm
    }

    fn atmega4809() -> AvrVm {
        AvrVm::new(
            "atmega4809".to_string(),
            AvrCoreClass::XT,
            49152,
            6144,
            5376,
            0x2800,
        )
    }

    #[test]
    fn classic_eeprom_read_sets_data_register_and_clears_eere() {
        let mut c = atmega328p();
        c.eeprom[0x123] = 0xA5;
        c.write_data(0x41, 0x23); // EEARL
        c.write_data(0x42, 0x01); // EEARH

        c.write_data(0x3F, 0x01); // EECR.EERE

        assert_eq!(c.read_data(0x40), 0xA5); // EEDR
        assert_eq!(c.read_data(0x3F) & 0x01, 0);
    }

    #[test]
    fn classic_eeprom_write_completes_after_programming_cycles() {
        let mut c = atmega328p();
        c.write_data(0x41, 0x05); // EEARL
        c.write_data(0x42, 0x00); // EEARH
        c.write_data(0x40, 0xA7); // EEDR

        c.write_data(0x3F, 0x04); // EEMPE
        c.write_data(0x3F, 0x06); // EEMPE | EEPE

        assert_eq!(c.eeprom[5], 0x00);
        assert_eq!(c.eeprom_write_cycles_left, 64);
        assert_eq!(c.read_data(0x3F) & 0x04, 0);

        for _ in 0..64 {
            c.step();
        }

        assert_eq!(c.eeprom[5], 0xA7);
        assert_eq!(c.eeprom_write_cycles_left, 0);
        assert_eq!(c.read_data(0x3F) & 0x02, 0);
    }

    #[test]
    fn modern_eeprom_command_sets_busy_and_commits_after_cycles() {
        let mut c = atmega4809();
        let addr_l = 0x20 + 0xFE8;
        let addr_h = 0x20 + 0xFE9;
        let data = 0x20 + 0xFE6;
        let ctrl = 0x20 + 0xFE0;
        let status = 0x20 + 0xFE2;

        c.write_data(addr_l, 0x12);
        c.write_data(addr_h, 0x00);
        c.write_data(data, 0x5A);
        c.write_data(ctrl, 0x04);

        assert_eq!(c.read_data(status) & 0x01, 0x01);
        assert_eq!(c.eeprom_write_cycles_left, 64);

        for _ in 0..64 {
            c.step();
        }

        assert_eq!(c.eeprom[0x12], 0x5A);
        assert_eq!(c.read_data(status) & 0x01, 0);
        assert_eq!(c.read_data(ctrl) & 0x07, 0);
    }

    #[test]
    fn spi_and_twi_status_reads_report_ready_bits() {
        let mut c = atmega328p();
        c.write_data(0x20 + 0x2D, 0x02); // SPSR
        c.write_data(0x20 + 0x9C, 0x04); // TWCR

        assert_eq!(c.read_data(0x20 + 0x2D), 0x82);
        assert_eq!(c.read_data(0x20 + 0x9C), 0x84);
    }

    #[test]
    fn queued_interrupt_is_serviced_between_instructions() {
        let mut c = atmega328p();
        let initial_sp = c.sp;
        c.set_flag(7, true);
        c.raise_interrupt(3);

        c.step();

        assert_eq!(c.pc, 12);
        assert_eq!(c.cycles, 6);
        assert!(!c.get_flag(7));
        assert_eq!(c.sp, initial_sp - 2);
        assert_eq!(c.read_data((initial_sp - 1) as u32), 0x02);
        assert_eq!(c.read_data(initial_sp as u32), 0x00);
    }

    #[test]
    fn timer0_ctc_compare_match_queues_and_services_interrupt() {
        let mut c = atmega328p();
        c.set_flag(7, true);
        c.write_data(0x20 + IO_TCCR0A, 0x02); // CTC mode
        c.write_data(0x20 + IO_TCCR0B, 0x01); // no prescaling
        c.write_data(0x20 + IO_OCR0A, 0x00);
        c.write_data(0x20 + IO_TIMSK0, 1 << BIT_OCF0A);

        c.step();

        assert_eq!(c.pc, 14 * 4);
        assert_eq!(c.cycles, 6);
        assert_eq!(c.read_data(0x20 + IO_TIFR0) & (1 << BIT_OCF0A), 0);
    }
}
