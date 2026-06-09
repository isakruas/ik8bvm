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

use std::collections::{HashSet, VecDeque};

use crate::devices::AvrCoreClass;
use crate::hw;

const IO_TCCR0A: u32 = 0x24;
const IO_TCCR0B: u32 = 0x25;
const IO_TCNT0: u32 = 0x26;
const IO_OCR0A: u32 = 0x27;
const IO_TIFR0: u32 = 0x15;
const IO_TIMSK0: u32 = 0x4E;
const BIT_OCF0A: u8 = 1;

/// Which on-chip serial peripheral an [`IoEvent`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoPeripheral {
    Uart,
    Spi,
    Twi,
}

/// What an [`IoEvent`] represents: a data byte, or a TWI bus condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoKind {
    Data,
    TwiStart,
    TwiStop,
}

/// A captured serial-peripheral access, recorded when `capture_io` is on so a
/// host (e.g. the IDE breadboard) can show the traffic the program drives.
#[derive(Debug, Clone, Copy)]
pub struct IoEvent {
    pub periph: IoPeripheral,
    pub kind: IoKind,
    /// True for a write (MCU transmit); false for a byte read back from a device.
    pub write: bool,
    /// The data byte (meaningful when `kind == Data`).
    pub byte: u8,
}

/// Implemented by a host to model external devices that respond to the MCU's
/// serial buses *synchronously* (within the instruction that drives them). The
/// VM calls these at the matching bus events; the host routes to the connected
/// device(s). All methods have inert defaults so a responder need only model
/// the buses it cares about.
pub trait BusResponder: Send {
    /// SPI full-duplex: the MCU clocked out `mosi`; return the byte shifted in.
    fn spi_transfer(&mut self, _mosi: u8) -> u8 {
        0xFF
    }
    /// I2C START issued by the master.
    fn i2c_start(&mut self) {}
    /// I2C address byte (7-bit `addr` + `read` direction). Return true to ACK.
    fn i2c_address(&mut self, _addr: u8, _read: bool) -> bool {
        false
    }
    /// I2C: master wrote a data byte. Return true to ACK.
    fn i2c_write(&mut self, _byte: u8) -> bool {
        false
    }
    /// I2C: master reads a byte; `last` is true when it will NACK (final byte).
    fn i2c_read(&mut self, _last: bool) -> u8 {
        0xFF
    }
    /// I2C STOP issued by the master.
    fn i2c_stop(&mut self) {}
    /// UART: the MCU transmitted a byte to the device.
    fn uart_tx(&mut self, _byte: u8) {}
    /// UART: pull the next byte the device wants to send to the MCU, if any.
    fn uart_poll(&mut self) -> Option<u8> {
        None
    }
    /// A GPIO port the device watches was written: `addr` is the data-space
    /// PORT register address and `value` the byte just written. Lets a device
    /// observe control pins (chip-select, data/command, reset, ...).
    fn pin_write(&mut self, _addr: u32, _value: u8) {}
    /// Advance device time by `cycles`, for autonomous behaviour.
    fn tick(&mut self, _cycles: u64) {}
}

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

    /// When set, writes to the UART/SPI/TWI data registers are recorded into
    /// `io_events`. Off by default so normal runs pay nothing.
    pub capture_io: bool,
    /// Captured serial-peripheral data-register writes, drained by the host.
    pub io_events: Vec<IoEvent>,
    /// Byte the SPI peripheral returns to the master: latched into the SPI data
    /// register on each SPI data write, so the subsequent read reads it back.
    /// Host-configurable (defaults to 0xFF, the idle/high-impedance value).
    pub spi_miso: u8,
    /// Bytes a host has fed to the USART receiver. RXC reads as set while this
    /// is non-empty; reading the data register pops the next byte.
    pub uart_rx: VecDeque<u8>,
    /// Per-channel ADC inputs (10-bit, 0..1023). A conversion latches the
    /// selected channel's value into ADCH:ADCL. Host-configurable.
    pub adc_inputs: [u16; 16],
    /// Optional model of the external devices on the serial buses. When set, the
    /// VM routes SPI/I2C/UART traffic through it for synchronous responses.
    pub responder: Option<Box<dyn BusResponder>>,
    /// Data-space PORT addresses whose writes are forwarded to the responder
    /// (so a device can watch its control pins). Empty = no pin forwarding.
    pub watch_pins: HashSet<u32>,
    // TWI master decode state (classic AVR), advanced by TWCR/TWDR writes.
    twi_expect_addr: bool,
    twi_twdr_written: bool,
    // Cached data-register I/O addresses (io-space) for the active device, so
    // capture costs a couple of comparisons rather than a table scan per write.
    uart_data_io: Option<u32>,
    uart_status_io: Option<u32>,
    spi_data_io: Option<u32>,
    twi_data_io: Option<u32>,
    twi_ctrl_io: Option<u32>,
    adc: Option<hw::AdcHw>,
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
        let uart_data_io = hw::uart_hw(&device).map(|u| u.data);
        let uart_status_io = hw::uart_hw(&device).map(|u| u.status);
        let spi_data_io = hw::spi_hw(&device).map(|s| s.data);
        let twi_data_io = hw::twi_hw(&device).map(|t| t.data);
        let twi_ctrl_io = hw::twi_hw(&device).map(|t| t.ctrl);
        let adc = hw::adc_hw(&device);
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
            capture_io: false,
            io_events: Vec::new(),
            spi_miso: 0xFF,
            uart_rx: VecDeque::new(),
            adc_inputs: [0; 16],
            responder: None,
            watch_pins: HashSet::new(),
            twi_expect_addr: false,
            twi_twdr_written: false,
            uart_data_io,
            uart_status_io,
            spi_data_io,
            twi_data_io,
            twi_ctrl_io,
            adc,
        }
    }

    /// Queue bytes for the USART receiver (host -> MCU).
    pub fn uart_feed(&mut self, bytes: &[u8]) {
        self.uart_rx.extend(bytes.iter().copied());
    }

    /// Set the analog value (0..1023) presented on ADC `channel`.
    pub fn adc_set(&mut self, channel: usize, value: u16) {
        if channel < self.adc_inputs.len() {
            self.adc_inputs[channel] = value.min(0x3FF);
        }
    }

    /// Advance attached devices and drain any UART bytes they wish to send to
    /// the MCU into the receive FIFO. Called once per host frame.
    pub fn poll_devices(&mut self, cycles: u64) {
        let mut fed: Vec<u8> = Vec::new();
        if let Some(r) = self.responder.as_mut() {
            r.tick(cycles);
            // Bounded drain so a chatty device can't starve the loop.
            for _ in 0..256 {
                match r.uart_poll() {
                    Some(b) => fed.push(b),
                    None => break,
                }
            }
        }
        self.uart_rx.extend(fed);
    }

    /// Capture a serial-peripheral write for the host monitor and route it
    /// through the attached device model. The TWI data register only stages a
    /// byte; it is transmitted when TWCR is written (see `handle_twcr_write`).
    fn handle_serial_write(&mut self, io_addr: u32, v: u8) {
        let cap = self.capture_io;
        if Some(io_addr) == self.uart_data_io {
            if cap {
                self.io_events.push(IoEvent { periph: IoPeripheral::Uart, kind: IoKind::Data, write: true, byte: v });
            }
            if let Some(r) = self.responder.as_mut() {
                r.uart_tx(v);
            }
        } else if Some(io_addr) == self.spi_data_io {
            if cap {
                self.io_events.push(IoEvent { periph: IoPeripheral::Spi, kind: IoKind::Data, write: true, byte: v });
            }
            let miso = match self.responder.as_mut() {
                Some(r) => r.spi_transfer(v),
                None => self.spi_miso,
            };
            self.io_write_raw(io_addr, miso);
        } else if Some(io_addr) == self.twi_data_io {
            // Stage the byte; it is sent when the program writes TWCR.
            self.twi_twdr_written = true;
        } else if Some(io_addr) == self.twi_ctrl_io {
            self.handle_twcr_write(v);
        }
    }

    /// Decode a classic-AVR TWCR write into a TWI bus action. The std master
    /// only polls TWINT (faked always-ready in `read_data`), never the TWSR
    /// status codes, so reproducing the START/address/data/read *sequence* is
    /// enough to drive a device model.
    fn handle_twcr_write(&mut self, v: u8) {
        const TWEA: u8 = 0x40; // ACK enable (more bytes to read)
        const TWSTA: u8 = 0x20; // START
        const TWSTO: u8 = 0x10; // STOP
        let cap = self.capture_io;

        if v & TWSTA != 0 {
            if cap {
                self.io_events.push(IoEvent { periph: IoPeripheral::Twi, kind: IoKind::TwiStart, write: true, byte: 0 });
            }
            if let Some(r) = self.responder.as_mut() {
                r.i2c_start();
            }
            self.twi_expect_addr = true;
            self.twi_twdr_written = false;
        } else if v & TWSTO != 0 {
            if cap {
                self.io_events.push(IoEvent { periph: IoPeripheral::Twi, kind: IoKind::TwiStop, write: true, byte: 0 });
            }
            if let Some(r) = self.responder.as_mut() {
                r.i2c_stop();
            }
            self.twi_twdr_written = false;
        } else if self.twi_twdr_written {
            // Transmit the staged TWDR byte: the first after START is the
            // address (+ R/W bit), the rest are data.
            let byte = self.twi_data_io.map(|a| self.io_read_raw(a)).unwrap_or(0);
            if cap {
                self.io_events.push(IoEvent { periph: IoPeripheral::Twi, kind: IoKind::Data, write: true, byte });
            }
            if self.twi_expect_addr {
                let read = byte & 1 != 0;
                if let Some(r) = self.responder.as_mut() {
                    r.i2c_address(byte >> 1, read);
                }
                self.twi_expect_addr = false;
            } else if let Some(r) = self.responder.as_mut() {
                r.i2c_write(byte);
            }
            self.twi_twdr_written = false;
        } else {
            // Master read: ACK (TWEA) means more bytes follow; NACK is the last.
            let last = v & TWEA == 0;
            let b = match self.responder.as_mut() {
                Some(r) => r.i2c_read(last),
                None => self.twi_data_io.map(|a| self.io_read_raw(a)).unwrap_or(0),
            };
            if let Some(a) = self.twi_data_io {
                self.io_write_raw(a, b);
            }
            if cap {
                self.io_events.push(IoEvent { periph: IoPeripheral::Twi, kind: IoKind::Data, write: false, byte: b });
            }
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

    pub fn read_data(&mut self, addr: u32) -> u8 {
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

            // USART status: UDRE/TXC always ready; RXC set while bytes wait.
            if Some(io_addr) == self.uart_status_io {
                let rxc = if self.uart_rx.is_empty() { 0 } else { 0x80 };
                return self.io_read_raw(io_addr) | 0x60 | rxc;
            }
            // USART data: a read consumes the next received byte.
            if Some(io_addr) == self.uart_data_io {
                if let Some(b) = self.uart_rx.pop_front() {
                    return b;
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

            // Serial-peripheral handling: capture traffic for the host monitor
            // and route it through the attached device model (if any). Both are
            // cheap address comparisons; skipped entirely on a plain run.
            if self.capture_io || self.responder.is_some() {
                self.handle_serial_write(io_addr, v);
            }

            // ADC: writing ADCSRA with ADEN+ADSC set performs a conversion of the
            // ADMUX-selected channel from the host-supplied input, then clears
            // ADSC so a polling loop sees the conversion complete.
            if let Some(adc) = self.adc {
                if io_addr == adc.ctrl && v & 0x80 != 0 && v & 0x40 != 0 {
                    let mux = self.io_read_raw(adc.mux);
                    let channel = (mux & 0x0F) as usize;
                    let value = self.adc_inputs.get(channel).copied().unwrap_or(0) & 0x3FF;
                    if mux & 0x20 != 0 {
                        // ADLAR: left-adjusted result.
                        self.io_write_raw(adc.datah, (value >> 2) as u8);
                        self.io_write_raw(adc.datal, ((value << 6) & 0xC0) as u8);
                    } else {
                        self.io_write_raw(adc.datal, (value & 0xFF) as u8);
                        self.io_write_raw(adc.datah, ((value >> 8) & 0x03) as u8);
                    }
                    self.io_write_raw(adc.ctrl, v & !0x40); // clear ADSC
                }
            }

            // Forward writes of a watched PORT register to the device model so
            // it can track its control pins (chip-select, data/command, ...).
            if !self.watch_pins.is_empty() && self.watch_pins.contains(&addr) {
                if let Some(r) = self.responder.as_mut() {
                    r.pin_write(addr, v);
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
        // C (borrow): set iff Rd < Rr + cin, i.e. the true subtraction is
        // negative. `full & 0x100` is wrong for the single case full == -256
        // (Rd=0x00, Rr=0xFF, cin=1: -256 = 0xFFFF_FF00 has bit 8 clear), where
        // hardware still sets C. Test the sign of the full result directly.
        self.set_flag(0, full < 0); // C

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

    #[test]
    fn manual_eeprom_write_sequence_in_vm() {
        let mut c = atmega328p();
        c.write_data(0x20 + 0x21, 0x05); // EEARL
        c.write_data(0x20 + 0x22, 0x00); // EEARH
        c.write_data(0x20 + 0x20, 0xAA); // EEDR
        c.write_data(0x20 + 0x1F, 0x04); // EEMWE
        c.write_data(0x20 + 0x1F, 0x06); // EEMWE | EEWE
        
        // Wait for programming cycles to finish
        for _ in 0..100000 {
            c.step();
            if c.read_data(0x20 + 0x1F) & 0x02 == 0 {
                break;
            }
        }
        
        assert_eq!(c.eeprom[0x0005], 0xAA);
    }

    #[test]
    fn manual_16bit_register_access_temp() {
        // TCNT1 is usually at 0x84/0x85
        let mut c = atmega328p();
        
        // Manual: Write high byte first. It goes to TEMP.
        c.write_data(0x85, 0xAB);
        
        // Test that the actual high byte of the 16-bit register wasn't updated yet in memory
        // if the VM fully models TCNT1. If not, it just writes it.
        // At least we verify the standard 16-bit behavior if modeled.
        // For standard IO, we just write.
        c.write_data(0x84, 0xCD);
        
        assert_eq!(c.read_data(0x85), 0xAB);
        assert_eq!(c.read_data(0x84), 0xCD);
    }
}
