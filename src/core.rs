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

// Timer0 register I/O addresses (atmega328p), used by the unit tests below; the
// runtime timer model takes addresses from `hw::timers` per device.
#[cfg(test)]
const IO_TCCR0A: u32 = 0x24;
#[cfg(test)]
const IO_TCCR0B: u32 = 0x25;
#[cfg(test)]
const IO_OCR0A: u32 = 0x27;
#[cfg(test)]
const IO_TIFR0: u32 = 0x15;
#[cfg(test)]
const IO_TIMSK0: u32 = 0x4E;
#[cfg(test)]
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
    /// Low data space (0x00-0x3F) backing store for AVRxt/AVRxm cores, where
    /// VPORTs and GPIO registers live below the CPU SP/SREG registers.
    pub lowio: [u8; 64],

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
    /// Per-timer prescaler remainder (slot index = `TimerHw.acc`).
    pub timer_accs: [u32; 4],
    /// Timer/counter descriptors for the active device (empty = none modelled).
    pub timers: Vec<hw::TimerHw>,
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
    uart_ctrl_io: Option<u32>,
    spi_ctrl_io: Option<u32>,
    spi_status_io: Option<u32>,
    adc: Option<hw::AdcHw>,
    /// Non-timer interrupt vectors (UART RX, ADC, SPI, TWI, INT0/1) for the part.
    periph: Option<hw::PeriphIrqs>,
    /// External interrupts (INT0/INT1) and the previous level of each watched pin.
    ext_irqs: Vec<hw::ExtIrq>,
    ext_prev: Vec<u8>,
    /// Pin-change interrupt groups and the previous masked snapshot of each.
    pc_ints: Vec<hw::PcIntGroup>,
    pcint_prev: Vec<u8>,
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
        // The io vec is indexed by datasheet I/O address. On AVRrc the whole
        // 0x00..sram_start data range below SRAM is I/O; elsewhere the first
        // 0x20 data addresses are the register file.
        let io_bytes = if core == AvrCoreClass::RC {
            sram_start
        } else {
            sram_start.saturating_sub(0x20)
        };
        let timers = hw::timers(&device);
        // Address 0 in the hardware tables means "no such peripheral register";
        // it must never match a real I/O access (I/O 0x00 is a live register on
        // many parts, e.g. PINA on the reduced-core tinys).
        let uart_data_io = hw::uart_hw(&device).map(|u| u.data).filter(|&a| a != 0);
        let uart_status_io = hw::uart_hw(&device).map(|u| u.status).filter(|&a| a != 0);
        let spi_data_io = hw::spi_hw(&device).map(|s| s.data).filter(|&a| a != 0);
        let twi_data_io = hw::twi_hw(&device).map(|t| t.data).filter(|&a| a != 0);
        let twi_ctrl_io = hw::twi_hw(&device).map(|t| t.ctrl).filter(|&a| a != 0);
        let uart_ctrl_io = hw::uart_hw(&device).map(|u| u.ctrl).filter(|&a| a != 0);
        let spi_ctrl_io = hw::spi_hw(&device).map(|s| s.ctrl).filter(|&a| a != 0);
        let spi_status_io = hw::spi_hw(&device).map(|s| s.status).filter(|&a| a != 0);
        let adc = hw::adc_hw(&device);
        let periph = hw::periph_irqs(&device);
        let ext_irqs = hw::ext_irqs(&device);
        let ext_prev = vec![1u8; ext_irqs.len()]; // assume inputs idle-high (pull-ups)
        let pc_ints = hw::pc_ints(&device);
        let pcint_prev = vec![0u8; pc_ints.len()];
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
            lowio: [0; 64],
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
            timer_accs: [0; 4],
            timers,
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
            uart_ctrl_io,
            spi_ctrl_io,
            spi_status_io,
            adc,
            periph,
            ext_irqs,
            ext_prev,
            pc_ints,
            pcint_prev,
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
        } else if self.core == AvrCoreClass::XT && self.twi_ctrl_io.is_some() && {
            let ctrl = self.twi_ctrl_io.unwrap();
            io_addr == ctrl || io_addr == ctrl + 3 || io_addr == ctrl + 4
        } {
            self.handle_modern_twi_write(io_addr, v);
        } else if Some(io_addr) == self.spi_data_io {
            let miso = match self.responder.as_mut() {
                Some(r) => r.spi_transfer(v),
                None => self.spi_miso,
            };
            if cap {
                // SPI is full duplex: record both directions of the exchange.
                self.io_events.push(IoEvent { periph: IoPeripheral::Spi, kind: IoKind::Data, write: true, byte: v });
                self.io_events.push(IoEvent { periph: IoPeripheral::Spi, kind: IoKind::Data, write: false, byte: miso });
            }
            self.io_write_raw(io_addr, miso);
            if let Some(st) = self.spi_status_io {
                self.io_set_bits(st, 0x80); // SPIF: transfer complete
            }
        } else if Some(io_addr) == self.twi_data_io {
            // Stage the byte; it is sent when the program writes TWCR.
            self.twi_twdr_written = true;
        } else if Some(io_addr) == self.twi_ctrl_io {
            self.handle_twcr_write(v);
            if let Some(ctrl) = self.twi_ctrl_io {
                self.io_set_bits(ctrl, 0x80); // TWINT: operation complete
            }
        }
    }

    /// Decode a modern (AVRxt) TWI master register write into bus actions:
    /// MADDR (ctrl+3) carries START + slave address (with auto-receive of the
    /// first byte on a read transaction), MDATA (ctrl+4) carries data bytes,
    /// and MCTRLB (ctrl) the ACK/NACK + receive-next (MCMD = 2) or STOP
    /// (MCMD = 3) commands.
    fn handle_modern_twi_write(&mut self, io_addr: u32, v: u8) {
        let cap = self.capture_io;
        let ctrl = match self.twi_ctrl_io {
            Some(c) => c,
            None => return,
        };
        let mdata = ctrl + 4;
        if io_addr == ctrl + 3 {
            if cap {
                self.io_events.push(IoEvent { periph: IoPeripheral::Twi, kind: IoKind::TwiStart, write: true, byte: 0 });
                self.io_events.push(IoEvent { periph: IoPeripheral::Twi, kind: IoKind::Data, write: true, byte: v });
            }
            let read = v & 1 != 0;
            if let Some(r) = self.responder.as_mut() {
                r.i2c_start();
                r.i2c_address(v >> 1, read);
            }
            if read {
                // The master auto-receives the first byte after SLA+R.
                let b = match self.responder.as_mut() {
                    Some(r) => r.i2c_read(false),
                    None => self.io_read_raw(mdata),
                };
                self.io_write_raw(mdata, b);
                if cap {
                    self.io_events.push(IoEvent { periph: IoPeripheral::Twi, kind: IoKind::Data, write: false, byte: b });
                }
            }
        } else if io_addr == mdata {
            if cap {
                self.io_events.push(IoEvent { periph: IoPeripheral::Twi, kind: IoKind::Data, write: true, byte: v });
            }
            if let Some(r) = self.responder.as_mut() {
                r.i2c_write(v);
            }
        } else {
            // MCTRLB command write: ACKACT in bit 2, MCMD in bits 1:0.
            let nack = v & 0x04 != 0;
            match v & 0x03 {
                0x03 => {
                    if cap {
                        self.io_events.push(IoEvent { periph: IoPeripheral::Twi, kind: IoKind::TwiStop, write: true, byte: 0 });
                    }
                    if let Some(r) = self.responder.as_mut() {
                        r.i2c_stop();
                    }
                }
                0x02 => {
                    let b = match self.responder.as_mut() {
                        Some(r) => r.i2c_read(nack),
                        None => self.io_read_raw(mdata),
                    };
                    self.io_write_raw(mdata, b);
                    if cap {
                        self.io_events.push(IoEvent { periph: IoPeripheral::Twi, kind: IoKind::Data, write: false, byte: b });
                    }
                }
                _ => {}
            }
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

    /// Data-space address of I/O location `a` (the operand of IN/OUT/SBI/CBI/
    /// SBIC/SBIS). Only the classic cores alias the I/O file at 0x20 in data
    /// space behind the register file; AVRrc, AVRxt, and AVRxm map I/O (and
    /// the CPU SP/SREG registers) directly at data 0x00-0x3F.
    pub fn io_data_addr(&self, a: u32) -> u32 {
        match self.core {
            AvrCoreClass::RC | AvrCoreClass::XT | AvrCoreClass::XM => a,
            _ => a + 0x20,
        }
    }

    pub fn read_data(&mut self, addr: u32) -> u8 {
        if self.core == AvrCoreClass::RC {
            // AVRrc data space (ATtiny4/5/9/10/20/40): the register file is
            // not memory mapped. I/O occupies 0x00-0x3F with SPL/SPH/SREG at
            // 0x3D/0x3E/0x3F, and SRAM starts immediately after at 0x40.
            return if addr == 0x3D {
                (self.sp & 0xFF) as u8
            } else if addr == 0x3E {
                (self.sp >> 8) as u8
            } else if addr == 0x3F {
                self.sreg
            } else if addr < self.sram_start {
                self.read_io(addr)
            } else if addr < self.sram_start + self.sram_bytes {
                self.sram[(addr - self.sram_start) as usize]
            } else {
                0
            };
        }
        if matches!(self.core, AvrCoreClass::XT | AvrCoreClass::XM) && addr < 0x40 {
            // Modern/XMEGA low data space: VPORTs and GPIO registers from
            // 0x00, CPU SPL/SPH/SREG at 0x3D-0x3F. The register file is not
            // memory mapped; peripheral space continues at 0x40 (modelled by
            // the shifted `io` file below).
            return match addr {
                0x3D => (self.sp & 0xFF) as u8,
                0x3E => (self.sp >> 8) as u8,
                0x3F => self.sreg,
                _ => self.lowio[addr as usize],
            };
        }
        let classic = !matches!(self.core, AvrCoreClass::XT | AvrCoreClass::XM);
        if classic && addr < 32 {
            self.r[addr as usize]
        } else if classic && addr == 0x5F {
            self.sreg
        } else if classic && addr == 0x5D {
            (self.sp & 0xFF) as u8
        } else if classic && addr == 0x5E {
            (self.sp >> 8) as u8
        } else if addr < self.sram_start {
            self.read_io(addr - 0x20)
        } else if addr < self.sram_start + self.sram_bytes {
            self.sram[(addr - self.sram_start) as usize]
        } else {
            0
        }
    }

    /// Read one byte from the I/O file at `io_addr` (datasheet I/O address),
    /// applying the peripheral models layered over the raw register bytes.
    fn read_io(&mut self, io_addr: u32) -> u8 {
        if let Some(ee) = hw::eeprom_hw(&self.device) {
            if ee.is_modern && ee.data != 0 && io_addr == ee.data {
                let eear = self.eeprom_address(ee);
                if eear < self.eeprom_bytes {
                    return self.eeprom[eear as usize];
                }
            }
        }

        if let Some(spi) = hw::spi_hw(&self.device) {
            if spi.status != 0 && io_addr == spi.status {
                return self.io_read_raw(io_addr) | 0x80;
            }
        }

        if let Some(twi) = hw::twi_hw(&self.device) {
            if twi.ctrl != 0 && io_addr == twi.ctrl {
                return self.io_read_raw(io_addr) | 0x80;
            }
            // Modern (AVRxt) TWI: the table's ctrl is MCTRLB (offset 0x04);
            // MSTATUS sits right after it (offset 0x05). Fake WIF|RIF so the
            // host-side master poll loop sees every transfer complete.
            if self.core == AvrCoreClass::XT && twi.ctrl != 0 && io_addr == twi.ctrl + 1 {
                return self.io_read_raw(io_addr) | 0xC0;
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
    }

    pub fn write_data(&mut self, addr: u32, v: u8) {
        if self.core == AvrCoreClass::RC {
            // AVRrc data space: see `read_data` for the layout.
            if addr == 0x3D {
                self.sp = (self.sp & 0xFF00) | (v as u16);
            } else if addr == 0x3E {
                self.sp = (self.sp & 0x00FF) | ((v as u16) << 8);
            } else if addr == 0x3F {
                self.sreg = v;
            } else if addr < self.sram_start {
                self.write_io(addr, v, addr);
            } else if addr < self.sram_start + self.sram_bytes {
                self.sram[(addr - self.sram_start) as usize] = v;
            }
            return;
        }
        if matches!(self.core, AvrCoreClass::XT | AvrCoreClass::XM) && addr < 0x40 {
            // Modern/XMEGA low data space: see `read_data` for the layout.
            match addr {
                0x3D => self.sp = (self.sp & 0xFF00) | (v as u16),
                0x3E => self.sp = (self.sp & 0x00FF) | ((v as u16) << 8),
                0x3F => self.sreg = v,
                _ => self.lowio[addr as usize] = v,
            }
            return;
        }
        let classic = !matches!(self.core, AvrCoreClass::XT | AvrCoreClass::XM);
        if classic && addr < 32 {
            self.r[addr as usize] = v;
        } else if classic && addr == 0x5F {
            self.sreg = v;
        } else if classic && addr == 0x5D {
            self.sp = (self.sp & 0xFF00) | (v as u16);
        } else if classic && addr == 0x5E {
            self.sp = (self.sp & 0x00FF) | ((v as u16) << 8);
        } else if addr < self.sram_start {
            self.write_io(addr - 0x20, v, addr);
        } else if addr < self.sram_start + self.sram_bytes {
            self.sram[(addr - self.sram_start) as usize] = v;
        }
    }

    /// Write one byte to the I/O file at `io_addr` (datasheet I/O address).
    /// `data_addr` is the originating data-space address, used for the
    /// watched-pin forwarding which is keyed on data-space PORT addresses.
    fn write_io(&mut self, io_addr: u32, v: u8, data_addr: u32) {
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

        // Modern (AVRxt) TWI master: writing MADDR (ctrl+3) makes us bus
        // owner, an MCTRLB (ctrl) STOP command (MCMD = 3) returns the bus to
        // idle. The BUSSTATE bits in MSTATUS (ctrl+1) are kept current even
        // on a plain run, because the std driver routes the address write
        // through them.
        if self.core == AvrCoreClass::XT {
            if let Some(twi) = hw::twi_hw(&self.device) {
                if twi.ctrl != 0 {
                    let mstatus = twi.ctrl + 1;
                    if io_addr == twi.ctrl + 3 {
                        let st = self.io_read_raw(mstatus);
                        self.io_write_raw(mstatus, (st & !0x03) | 0x02); // owner
                    } else if io_addr == twi.ctrl && (v & 0x03) == 0x03 {
                        let st = self.io_read_raw(mstatus);
                        self.io_write_raw(mstatus, (st & !0x03) | 0x01); // idle
                    }
                }
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
                let cleared = v & !0x40; // clear ADSC
                self.io_write_raw(adc.ctrl, cleared | 0x10); // set ADIF (conversion complete)
            }
        }

        // Forward writes of a watched PORT register to the device model so
        // it can track its control pins (chip-select, data/command, ...).
        if !self.watch_pins.is_empty() && self.watch_pins.contains(&data_addr) {
            if let Some(r) = self.responder.as_mut() {
                r.pin_write(data_addr, v);
            }
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
        self.tick_timers(consumed as u32);
        self.tick_ext_irqs();
        self.tick_pcint();
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
        self.lowio.fill(0);
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
        self.timer_accs = [0; 4];
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

    /// Advance every modelled timer by `cycles`, setting overflow / compare
    /// flags. Each timer counts at its own prescaler; in CTC mode it clears at
    /// OCRA (setting the compare flag), otherwise it wraps (setting overflow).
    fn tick_timers(&mut self, cycles: u32) {
        for i in 0..self.timers.len() {
            let t = self.timers[i]; // TimerHw is Copy: no borrow conflict / alloc.
            let prescaler = cs_prescaler(self.io_read_raw(t.clock_reg));
            if prescaler == 0 {
                continue; // clock stopped (CS = 0) or external source.
            }
            self.timer_accs[t.acc] = self.timer_accs[t.acc].saturating_add(cycles);
            let ctc = (self.io_read_raw(t.ctc_reg) & t.ctc_mask) == t.ctc_val;
            let mut guard = 0u32;
            while self.timer_accs[t.acc] >= prescaler && guard < 200_000 {
                self.timer_accs[t.acc] -= prescaler;
                guard += 1;
                self.timer_count(&t, ctc);
            }
        }
    }

    /// Advance one timer by a single tick.
    fn timer_count(&mut self, t: &hw::TimerHw, ctc: bool) {
        if t.bits16 {
            let cnt = ((self.io_read_raw(t.tcnt_h) as u16) << 8) | self.io_read_raw(t.tcnt) as u16;
            let ocra = ((self.io_read_raw(t.ocra_h) as u16) << 8) | self.io_read_raw(t.ocra) as u16;
            if ctc && cnt == ocra {
                self.io_write_raw(t.tcnt, 0);
                self.io_write_raw(t.tcnt_h, 0);
                self.set_timer_flag(t.compa);
            } else {
                let next = cnt.wrapping_add(1);
                self.io_write_raw(t.tcnt, (next & 0xFF) as u8);
                self.io_write_raw(t.tcnt_h, (next >> 8) as u8);
                if next == 0 {
                    self.set_timer_flag(t.ovf);
                }
                if next == ocra {
                    self.set_timer_flag(t.compa);
                }
                if t.compb.vector != 0 {
                    let ocrb = ((self.io_read_raw(t.ocrb_h) as u16) << 8) | self.io_read_raw(t.ocrb) as u16;
                    if next == ocrb {
                        self.set_timer_flag(t.compb);
                    }
                }
            }
        } else {
            let cnt = self.io_read_raw(t.tcnt);
            let ocra = self.io_read_raw(t.ocra);
            if ctc && cnt == ocra {
                self.io_write_raw(t.tcnt, 0);
                self.set_timer_flag(t.compa);
            } else {
                let next = cnt.wrapping_add(1);
                self.io_write_raw(t.tcnt, next);
                if next == 0 {
                    self.set_timer_flag(t.ovf);
                }
                if next == ocra {
                    self.set_timer_flag(t.compa);
                }
                if t.compb.vector != 0 {
                    let ocrb = self.io_read_raw(t.ocrb);
                    if next == ocrb {
                        self.set_timer_flag(t.compb);
                    }
                }
            }
        }
    }

    /// Watch each external-interrupt pin and set its EIFR flag on the configured
    /// edge/level (EICRA sense bits): 0=low level, 1=any edge, 2=falling, 3=rising.
    fn tick_ext_irqs(&mut self) {
        for i in 0..self.ext_irqs.len() {
            let e = self.ext_irqs[i];
            let cur = (self.io_read_raw(e.pin_reg) >> e.pin_bit) & 1;
            let prev = self.ext_prev[i];
            let sense = (self.io_read_raw(e.sense_reg) >> e.sense_shift) & 0x03;
            let trigger = match sense {
                0 => cur == 0,            // low level (held while low)
                1 => cur != prev,         // any logical change
                2 => prev == 1 && cur == 0, // falling edge
                _ => prev == 0 && cur == 1, // rising edge
            };
            if trigger {
                self.io_set_bits(e.flag_reg, 1 << e.flag_bit);
            }
            self.ext_prev[i] = cur;
        }
    }

    /// Watch each enabled pin-change pin (PCMSK) and set the group's PCIFR flag
    /// when the masked snapshot changes.
    fn tick_pcint(&mut self) {
        for i in 0..self.pc_ints.len() {
            let g = self.pc_ints[i];
            let msk = self.io_read_raw(g.pcmsk);
            let mut snap = 0u8;
            for b in 0..8 {
                if msk & (1 << b) != 0 {
                    let (preg, pbit) = g.pins[b];
                    if preg != 0 && (self.io_read_raw(preg) >> pbit) & 1 != 0 {
                        snap |= 1 << b;
                    }
                }
            }
            if snap != self.pcint_prev[i] {
                self.io_set_bits(g.pcifr, 1 << g.pcif_bit);
            }
            self.pcint_prev[i] = snap;
        }
    }

    fn set_timer_flag(&mut self, irq: hw::TimerIrq) {
        if irq.vector != 0 {
            self.io_set_bits(irq.flag_reg, 1 << irq.bit);
        }
    }

    /// Queue peripheral interrupts whose enable bit and completion flag are set.
    /// Uses the standard AVR bit positions; events are driven by the VM's own
    /// UART receive FIFO, ADC conversion, SPI transfer and TWI operation.
    fn service_periph_interrupts(&mut self) {
        let Some(p) = self.periph else { return };

        // USART RX complete: RXCIE (UCSRB bit7) + a byte waiting (RXC).
        if p.uart_rx != 0 && !self.uart_rx.is_empty() {
            if let Some(ctrl) = self.uart_ctrl_io {
                if self.io_read_raw(ctrl) & (1 << 7) != 0 {
                    self.raise_interrupt(p.uart_rx);
                }
            }
        }

        // ADC conversion complete: ADIE (ADCSRA bit3) + ADIF (ADCSRA bit4).
        if p.adc != 0 {
            if let Some(adc) = self.adc {
                let c = self.io_read_raw(adc.ctrl);
                if c & (1 << 3) != 0 && c & (1 << 4) != 0 {
                    self.raise_interrupt(p.adc);
                }
            }
        }

        // SPI transfer complete: SPIE (SPCR bit7) + SPIF (SPSR bit7).
        if p.spi != 0 {
            if let (Some(ctrl), Some(st)) = (self.spi_ctrl_io, self.spi_status_io) {
                if self.io_read_raw(ctrl) & (1 << 7) != 0 && self.io_read_raw(st) & (1 << 7) != 0 {
                    self.raise_interrupt(p.spi);
                }
            }
        }

        // TWI operation complete: TWIE (TWCR bit0) + TWINT (TWCR bit7).
        if p.twi != 0 {
            if let Some(ctrl) = self.twi_ctrl_io {
                let c = self.io_read_raw(ctrl);
                if c & (1 << 0) != 0 && c & (1 << 7) != 0 {
                    self.raise_interrupt(p.twi);
                }
            }
        }

        // External interrupts INT0/INT1: enable bit (EIMSK/GICR) + flag (EIFR/GIFR).
        for i in 0..self.ext_irqs.len() {
            let e = self.ext_irqs[i];
            if self.io_read_raw(e.enable_reg) & (1 << e.enable_bit) != 0
                && self.io_read_raw(e.flag_reg) & (1 << e.flag_bit) != 0
            {
                self.raise_interrupt(e.vector);
            }
        }

        // Pin-change interrupts: PCIE (PCICR) + PCIF (PCIFR) per group.
        for i in 0..self.pc_ints.len() {
            let g = self.pc_ints[i];
            if self.io_read_raw(g.pcicr) & (1 << g.pcie_bit) != 0
                && self.io_read_raw(g.pcifr) & (1 << g.pcif_bit) != 0
            {
                self.raise_interrupt(g.vector);
            }
        }
    }

    fn service_interrupts(&mut self) {
        // Raise any timer interrupt whose enable bit (TIMSK) and flag bit (TIFR)
        // are both set. Other sources are queued directly via `raise_interrupt`.
        for i in 0..self.timers.len() {
            let t = self.timers[i];
            for irq in [t.ovf, t.compa, t.compb] {
                if irq.vector != 0
                    && (self.io_read_raw(irq.enable_reg) & (1 << irq.bit)) != 0
                    && (self.io_read_raw(irq.flag_reg) & (1 << irq.bit)) != 0
                {
                    self.raise_interrupt(irq.vector);
                }
            }
        }

        // Non-timer (peripheral) interrupt sources, gated by their enable bit and
        // the matching completion flag — driven by events the VM already models.
        self.service_periph_interrupts();

        if !self.get_flag(7) {
            return;
        }

        let Some(vec) = (1..self.irq_pending.len()).find(|&i| self.irq_pending[i]) else {
            return;
        };

        self.irq_pending[vec] = false;
        self.set_flag(7, false);

        // Entering a timer ISR clears its flag in hardware.
        for i in 0..self.timers.len() {
            let t = self.timers[i];
            for irq in [t.ovf, t.compa, t.compb] {
                if irq.vector as usize == vec {
                    self.io_clear_bits(irq.flag_reg, 1 << irq.bit);
                }
            }
        }
        // Clear the peripheral completion flag for the source being serviced, so
        // a level-triggered source does not re-fire every instruction.
        if let Some(p) = self.periph {
            if let (Some(adc), v) = (self.adc, vec) {
                if v == p.adc as usize {
                    self.io_clear_bits(adc.ctrl, 1 << 4); // ADIF
                }
            }
            if let Some(st) = self.spi_status_io {
                if vec == p.spi as usize {
                    self.io_clear_bits(st, 0x80); // SPIF
                }
            }
            if let Some(ctrl) = self.twi_ctrl_io {
                if vec == p.twi as usize {
                    self.io_clear_bits(ctrl, 0x80); // TWINT
                }
            }
        }
        // Entering an external-interrupt ISR clears its EIFR flag (edge modes).
        for i in 0..self.ext_irqs.len() {
            let e = self.ext_irqs[i];
            if e.vector as usize == vec {
                self.io_clear_bits(e.flag_reg, 1 << e.flag_bit);
            }
        }
        // Entering a pin-change ISR clears its PCIFR flag.
        for i in 0..self.pc_ints.len() {
            let g = self.pc_ints[i];
            if g.vector as usize == vec {
                self.io_clear_bits(g.pcifr, 1 << g.pcif_bit);
            }
        }

        self.push16(self.pc as u16);
        self.pc = vec as u32 * self.vector_slot_bytes();
        self.cycles += 5;

        if self.trace {
            self.trace_line(format!("IRQ vector {} -> PC=0x{:06X}", vec, self.pc));
        }
    }

    fn vector_slot_bytes(&self) -> u32 {
        // Parts with 8 KB of flash or less have 1-word (RJMP) vector slots;
        // larger parts have 2-word (JMP) slots. This matches the datasheet
        // tables, not the core family: a classic ATmega8 and an AVRrc tiny
        // both use 1-word vectors.
        if self.flash_bytes <= 8192 {
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

/// Map a timer clock-select (CS2:0) value to its prescaler divisor, or 0 when
/// the clock is stopped or driven from an external pin (not modelled).
fn cs_prescaler(cs: u8) -> u32 {
    match cs & 0x07 {
        1 => 1,
        2 => 8,
        3 => 64,
        4 => 256,
        5 => 1024,
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
