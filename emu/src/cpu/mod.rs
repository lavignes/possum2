//! CSG65CE02 Emulation

use crate::bus::{Bus, BusDevice};

#[cfg(test)]
mod tests;

pub enum Flags {}

impl Flags {
    pub const CARRY: u8 = 1 << 0;
    pub const ZERO: u8 = 1 << 1;
    pub const INTERRUPT_DISABLE: u8 = 1 << 2;
    pub const DECIMAL_MODE: u8 = 1 << 3;
    pub const BREAK: u8 = 1 << 4;
    pub const EXTEND_STACK_DISABLE: u8 = 1 << 5;
    pub const OVERFLOW: u8 = 1 << 6;
    pub const NEGATIVE: u8 = 1 << 7;
}

#[derive(Debug, Default)]
pub struct Cpu {
    a: u8,
    b: u8,
    x: u8,
    y: u8,
    z: u8,
    p: u8,
    sp: [u8; 2],
    pc: [u8; 2],

    irq: bool,
    nmi: bool,
    stack_xfer_wait: bool, // delay interrupt handling during stack transfers
}

impl Cpu {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn a(&self) -> u8 {
        self.a
    }

    pub fn b(&self) -> u8 {
        self.b
    }

    pub fn x(&self) -> u8 {
        self.x
    }

    pub fn y(&self) -> u8 {
        self.y
    }

    pub fn z(&self) -> u8 {
        self.z
    }

    pub fn p(&self) -> u8 {
        self.p
    }

    pub fn sp(&self) -> u16 {
        u16::from_le_bytes(self.sp)
    }

    pub fn pc(&self) -> u16 {
        u16::from_le_bytes(self.pc)
    }

    pub fn irq(&mut self) {
        self.irq = true;
    }

    pub fn nmi(&mut self) {
        self.nmi = true;
    }

    fn push<B: Bus>(&mut self, bus: &mut B, data: u8) {
        let addr = if (self.p & Flags::EXTEND_STACK_DISABLE) != 0 {
            self.sp[0] = self.sp[0].wrapping_sub(1);
            u16::from_le_bytes(self.sp)
        } else {
            u16::from_le_bytes(self.sp).wrapping_sub(1)
        };
        bus.write(addr, data)
    }

    fn pull<B: Bus>(&mut self, bus: &mut B) -> u8 {
        let addr = u16::from_le_bytes(self.sp);
        let data = bus.read(addr);
        if (self.p & Flags::EXTEND_STACK_DISABLE) != 0 {
            self.sp[0] = self.sp[0].wrapping_add(1);
        } else {
            self.sp = addr.wrapping_add(1).to_le_bytes();
        }
        data
    }

    fn fetch<B: Bus>(&mut self, bus: &mut B) -> u8 {
        let addr = u16::from_le_bytes(self.pc);
        let data = bus.read(addr);
        self.pc = addr.wrapping_add(1).to_le_bytes();
        data
    }

    fn set_flag(&mut self, mask: u8, value: bool) {
        if value {
            self.p |= mask;
        } else {
            self.p &= !mask;
        }
    }

    fn set_p(&mut self, value: u8) {
        // modifying p does not affect B and E flags
        self.p |= value & !(Flags::BREAK | Flags::EXTEND_STACK_DISABLE);
    }

    // (BP,X)
    fn addr_bp_indirect_x<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let ptr = self.fetch(bus).wrapping_add(self.x);
        let lo = bus.read(u16::from_le_bytes([ptr, self.b]));
        let hi = bus.read(u16::from_le_bytes([ptr.wrapping_add(1), self.b]));
        u16::from_le_bytes([lo, hi])
    }

    // (BP),Y
    fn addr_bp_indirect_y<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let ptr = self.fetch(bus);
        let lo = bus.read(u16::from_le_bytes([ptr, self.b]));
        let hi = bus.read(u16::from_le_bytes([ptr.wrapping_add(1), self.b]));
        u16::from_le_bytes([lo, hi])
            .wrapping_add(self.y as u16) // good lord why carry?
            .wrapping_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 })
    }

    // (BP),Z
    fn addr_bp_indirect_z<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let ptr = self.fetch(bus);
        let lo = bus.read(u16::from_le_bytes([ptr, self.b]));
        let hi = bus.read(u16::from_le_bytes([ptr.wrapping_add(1), self.b]));
        u16::from_le_bytes([lo, hi]).wrapping_add(self.z as u16)
    }

    // BP,X
    fn addr_bp_x<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let ptr = self.fetch(bus).wrapping_add(self.x);
        u16::from_le_bytes([ptr, self.b])
    }

    // BP,Y
    fn addr_bp_y<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let ptr = self.fetch(bus).wrapping_add(self.y);
        u16::from_le_bytes([ptr, self.b])
    }

    // BP
    fn addr_bp<B: Bus>(&mut self, bus: &mut B) -> u16 {
        u16::from_le_bytes([self.fetch(bus), self.b])
    }

    // ABS
    fn addr_abs<B: Bus>(&mut self, bus: &mut B) -> u16 {
        u16::from_le_bytes([self.fetch(bus), self.fetch(bus)])
    }

    // (ABS)
    fn addr_abs_indirect<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let addr = u16::from_le_bytes([self.fetch(bus), self.fetch(bus)]);
        let lo = bus.read(addr);
        let hi = bus.read(addr.wrapping_add(1));
        u16::from_le_bytes([lo, hi])
    }

    // (ABS,X)
    fn addr_abs_indirect_x<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let addr = u16::from_le_bytes([self.fetch(bus), self.fetch(bus)])
            .wrapping_add(self.x as u16)
            .wrapping_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
        let lo = bus.read(addr);
        let hi = bus.read(addr.wrapping_add(1));
        u16::from_le_bytes([lo, hi])
    }

    // ABS,X
    fn addr_abs_x<B: Bus>(&mut self, bus: &mut B) -> u16 {
        u16::from_le_bytes([self.fetch(bus), self.fetch(bus)])
            .wrapping_add(self.x as u16)
            .wrapping_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 })
    }

    // ABS,Y
    fn addr_abs_y<B: Bus>(&mut self, bus: &mut B) -> u16 {
        u16::from_le_bytes([self.fetch(bus), self.fetch(bus)])
            .wrapping_add(self.y as u16)
            .wrapping_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 })
    }

    // (d,SP),Y
    fn addr_sp_indirect_y<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let offset = self.fetch(bus);
        if (self.p & Flags::EXTEND_STACK_DISABLE) != 0 {
            let [lo, hi] = self.sp;
            let lo = lo.wrapping_add(offset).wrapping_add(self.y);
            u16::from_le_bytes([lo, hi])
        } else {
            u16::from_be_bytes(self.sp)
                .wrapping_add(offset as u16)
                .wrapping_add(self.y as u16)
        }
    }
}

impl BusDevice for Cpu {
    fn reset<B: Bus>(&mut self, bus: &mut B) {
        let lo = bus.read(0xFFFC);
        let hi = bus.read(0xFFFD);
        *self = Self {
            a: 0,
            b: 0,
            x: 0,
            y: 0,
            z: 0,
            p: Flags::INTERRUPT_DISABLE | Flags::EXTEND_STACK_DISABLE,
            sp: [0, 1], // stack is placed in page 1, for 6502 compat
            pc: [lo, hi],

            irq: false,
            nmi: false,
            stack_xfer_wait: false,
        };
    }

    fn tick<B: Bus>(&mut self, bus: &mut B) {
        // TXS and TYS instructions require delaying interrupt handling
        // for an extra tick because they need to be ran twice
        // in succession in either order.
        if !self.stack_xfer_wait {
            if self.nmi {
                self.nmi = false;
                let [lo, hi] = self.pc;
                self.push(bus, hi);
                self.push(bus, lo);
                self.push(bus, self.p);
                self.p &= !Flags::DECIMAL_MODE;
                self.p |= Flags::INTERRUPT_DISABLE;
                let lo = bus.read(0xFFFA);
                let hi = bus.read(0xFFFB);
                self.pc = [lo, hi];
                return;
            }

            if self.irq && ((self.p & Flags::INTERRUPT_DISABLE) == 0) {
                self.irq = false;
                let [lo, hi] = self.pc;
                self.push(bus, hi);
                self.push(bus, lo);
                self.push(bus, self.p);
                self.p &= !Flags::DECIMAL_MODE;
                self.p |= Flags::INTERRUPT_DISABLE;
                let lo = bus.read(0xFFFE);
                let hi = bus.read(0xFFFF);
                self.pc = [lo, hi];
                return;
            }
        }
        self.stack_xfer_wait = false;

        match self.fetch(bus) {
            // BRK
            0x00 => {
                // the intent of the extra byte following BRK is to store the BRK reason?
                self.fetch(bus);
                let [lo, hi] = self.pc;
                self.push(bus, hi);
                self.push(bus, lo);
                self.push(bus, self.p);
                self.p &= !Flags::DECIMAL_MODE;
                self.p |= Flags::BREAK | Flags::INTERRUPT_DISABLE;
                let lo = bus.read(0xFFFE);
                let hi = bus.read(0xFFFF);
                self.pc = [lo, hi];
            }

            // ORA (BP,X)
            0x01 => {
                let addr = self.addr_bp_indirect_x(bus);
                let data = bus.read(addr);
                self.a |= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // CLE
            0x02 => {
                self.p &= !Flags::EXTEND_STACK_DISABLE;
            }

            // SEE
            0x03 => {
                self.p |= Flags::EXTEND_STACK_DISABLE;
            }

            // TSB BP
            0x04 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                bus.write(addr, self.a | data);
                self.set_flag(Flags::ZERO, (self.a & data) == 0);
            }

            // ORA BP
            0x05 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                self.a |= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ASL BP
            0x06 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let (data, carry) = data.overflowing_shl(1);
                bus.write(addr, data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (data & 0x80) != 0);
                self.set_flag(Flags::ZERO, data == 0);
            }

            // RMB 0,BP
            0x07 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data & !(1 << 0);
                bus.write(addr, data);
            }

            // PHP
            0x08 => {
                self.push(bus, self.p);
            }

            // ORA IMM
            0x09 => {
                let data = self.fetch(bus);
                self.a |= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ASL A
            0x0A => {
                let (result, carry) = self.a.overflowing_shl(1);
                self.a = result;
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // TSY
            0x0B => {
                self.y = self.sp[1]; // transfer hi byte
                self.set_flag(Flags::NEGATIVE, (self.y & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.y == 0);
            }

            // TSB ABS
            0x0C => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                bus.write(addr, self.a | data);
                self.set_flag(Flags::ZERO, (self.a & data) == 0);
            }

            // ORA ABS
            0x0D => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                self.a |= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ASL ABS
            0x0E => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                let (data, carry) = data.overflowing_shl(1);
                bus.write(addr, data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (data & 0x80) != 0);
                self.set_flag(Flags::ZERO, data == 0);
            }

            // BBR 0,BP
            0x0F => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 0)) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // BPL REL
            0x10 => {
                let branch = self.fetch(bus) as i8;
                if (self.p & Flags::NEGATIVE) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // ORA (BP),Y
            0x11 => {
                let addr = self.addr_bp_indirect_y(bus);
                let data = bus.read(addr);
                self.a |= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ORA (BP),Z
            0x12 => {
                let addr = self.addr_bp_indirect_z(bus);
                let data = bus.read(addr);
                self.a |= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // BPL WREL
            0x13 => {
                let lo = self.fetch(bus);
                let hi = self.fetch(bus);
                let branch = i16::from_le_bytes([lo, hi]);
                if (self.p & Flags::NEGATIVE) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch)
                        .to_le_bytes();
                }
            }

            // TRB BP
            0x14 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                bus.write(addr, !self.a & data);
                self.set_flag(Flags::ZERO, (self.a & data) == 0);
            }

            // ORA BP,X
            0x15 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                self.a |= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ASL BP,X
            0x16 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                let (data, carry) = data.overflowing_shl(1);
                bus.write(addr, data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (data & 0x80) != 0);
                self.set_flag(Flags::ZERO, data == 0);
            }

            // RMB 1,BP
            0x17 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data & !(1 << 1);
                bus.write(addr, data);
            }

            // CLC
            0x18 => {
                self.p &= !Flags::CARRY;
            }

            // ORA ABS,Y
            0x19 => {
                let addr = self.addr_abs_y(bus);
                let data = bus.read(addr);
                self.a |= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // INC A
            0x1A => {
                self.a = self.a.wrapping_add(1);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // INZ
            0x1B => {
                self.z = self.z.wrapping_add(1);
                self.set_flag(Flags::NEGATIVE, (self.z & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.z == 0);
            }

            // TRB ABS
            0x1C => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                bus.write(addr, !self.a & data);
                self.set_flag(Flags::ZERO, (self.a & data) == 0);
            }

            // ORA ABS,X
            0x1D => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                self.a |= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ASL ABS,X
            0x1E => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                let (data, carry) = data.overflowing_shl(1);
                bus.write(addr, data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (data & 0x80) != 0);
                self.set_flag(Flags::ZERO, data == 0);
            }

            // BBR 1,BP
            0x1F => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 1)) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // JSR ABS
            0x20 => {
                let addr = self.addr_abs(bus);
                self.push(bus, self.pc[1]);
                self.push(bus, self.pc[0]);
                self.pc = addr.to_le_bytes();
            }

            // AND (BP,X)
            0x21 => {
                let addr = self.addr_bp_indirect_x(bus);
                let data = bus.read(addr);
                self.a &= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // JSR (ABS)
            0x22 => {
                let addr = self.addr_abs_indirect(bus);
                self.push(bus, self.pc[1]);
                self.push(bus, self.pc[0]);
                self.pc = addr.to_le_bytes();
            }

            // JSR (ABS,X)
            0x23 => {
                let addr = self.addr_abs_indirect_x(bus);
                self.push(bus, self.pc[1]);
                self.push(bus, self.pc[0]);
                self.pc = addr.to_le_bytes();
            }

            // BIT BP
            0x24 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (data & Flags::NEGATIVE) != 0);
                self.set_flag(Flags::OVERFLOW, (data & Flags::OVERFLOW) != 0);
                self.set_flag(Flags::ZERO, (self.a & data) == 0);
            }

            // AND BP
            0x25 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                self.a &= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ROL BP
            0x26 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let (result, carry) = data.overflowing_shl(1);
                let result = result | (self.p & Flags::CARRY);
                bus.write(addr, result);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // RMB 2,BP
            0x27 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data & !(1 << 2);
                bus.write(addr, data);
            }

            // PLP
            0x28 => {
                let data = self.pull(bus);
                self.set_p(data);
            }

            // AND IMM
            0x29 => {
                let data = self.fetch(bus);
                self.a &= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ROL A
            0x2A => {
                let (result, carry) = self.a.overflowing_shl(1);
                self.a = result | (self.p & Flags::CARRY);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // TYS
            0x2B => {
                self.sp[1] = self.y; // transfer hi byte
                self.stack_xfer_wait = true;
            }

            // BIT ABS
            0x2C => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (data & Flags::NEGATIVE) != 0);
                self.set_flag(Flags::OVERFLOW, (data & Flags::OVERFLOW) != 0);
                self.set_flag(Flags::ZERO, (self.a & data) == 0);
            }

            // AND ABS
            0x2D => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                self.a &= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ROL ABS
            0x2E => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                let (result, carry) = data.overflowing_shl(1);
                let result = result | (self.p & Flags::CARRY);
                bus.write(addr, result);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // BBR 2,BP
            0x2F => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 2)) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // BMI REL
            0x30 => {
                let branch = self.fetch(bus) as i8;
                if (self.p & Flags::NEGATIVE) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // AND (BP),Y
            0x31 => {
                let addr = self.addr_bp_indirect_y(bus);
                let data = bus.read(addr);
                self.a &= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // AND (BP),Z
            0x32 => {
                let addr = self.addr_bp_indirect_z(bus);
                let data = bus.read(addr);
                self.a &= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // BMI WREL
            0x33 => {
                let lo = self.fetch(bus);
                let hi = self.fetch(bus);
                let branch = i16::from_le_bytes([lo, hi]);
                if (self.p & Flags::NEGATIVE) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch)
                        .to_le_bytes();
                }
            }

            // BIT BP,X
            0x34 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (data & Flags::NEGATIVE) != 0);
                self.set_flag(Flags::OVERFLOW, (data & Flags::OVERFLOW) != 0);
                self.set_flag(Flags::ZERO, (self.a & data) == 0);
            }

            // AND BP,X
            0x35 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                self.a &= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ROL BP,X
            0x36 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                let (result, carry) = data.overflowing_shl(1);
                let result = result | (self.p & Flags::CARRY);
                bus.write(addr, result);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // RMB 3,BP
            0x37 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data & !(1 << 3);
                bus.write(addr, data);
            }

            // SEC
            0x38 => {
                self.p |= Flags::CARRY;
            }

            // AND ABS,Y
            0x39 => {
                let addr = self.addr_abs_y(bus);
                let data = bus.read(addr);
                self.a &= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // DEC A
            0x3A => {
                self.a = self.a.wrapping_sub(1);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // DEZ
            0x3B => {
                self.z = self.z.wrapping_sub(1);
                self.set_flag(Flags::NEGATIVE, (self.z & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.z == 0);
            }

            // BIT ABS,X
            0x3C => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (data & Flags::NEGATIVE) != 0);
                self.set_flag(Flags::OVERFLOW, (data & Flags::OVERFLOW) != 0);
                self.set_flag(Flags::ZERO, (self.a & data) == 0);
            }

            // AND ABS,X
            0x3D => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                self.a &= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ROL ABS,X
            0x3E => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                let (result, carry) = data.overflowing_shl(1);
                let result = result | (self.p & Flags::CARRY);
                bus.write(addr, result);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // BBR 3,BP
            0x3F => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 3)) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // RTI
            0x40 => {
                let data = self.pull(bus);
                self.set_p(data);
                let lo = self.pull(bus);
                let hi = self.pull(bus);
                self.pc = [lo, hi];
            }

            // EOR (BP,X)
            0x41 => {
                let addr = self.addr_bp_indirect_x(bus);
                let data = bus.read(addr);
                self.a ^= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // NEG A
            0x42 => {
                self.a = (-(self.a as i8)) as u8;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ASR A
            0x43 => {
                let (result, carry) = (self.a as i8).overflowing_shr(1);
                self.a = result as u8;
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ASR BP
            0x44 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let (data, carry) = (data as i8).overflowing_shr(1);
                let data = data as u8;
                bus.write(addr, data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (data & 0x80) != 0);
                self.set_flag(Flags::ZERO, data == 0);
            }

            // EOR BP
            0x45 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                self.a ^= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LSR BP
            0x46 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let (data, carry) = data.overflowing_shr(1);
                bus.write(addr, data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (data & 0x80) != 0);
                self.set_flag(Flags::ZERO, data == 0);
            }

            // RMB 4,BP
            0x47 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data & !(1 << 4);
                bus.write(addr, data);
            }

            // PHA
            0x48 => {
                self.push(bus, self.a);
            }

            // EOR IMM
            0x49 => {
                let data = self.fetch(bus);
                self.a ^= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LSR A
            0x4A => {
                let (result, carry) = self.a.overflowing_shr(1);
                self.a = result;
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // TAZ
            0x4B => {
                self.z = self.a;
                self.set_flag(Flags::NEGATIVE, (self.z & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.z == 0);
            }

            // JMP ABS
            0x4C => {
                let addr = self.addr_abs(bus);
                self.pc = addr.to_le_bytes();
            }

            // EOR ABS
            0x4D => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                self.a ^= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LSR ABS
            0x4E => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                let (data, carry) = data.overflowing_shr(1);
                bus.write(addr, data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (data & 0x80) != 0);
                self.set_flag(Flags::ZERO, data == 0);
            }

            // BBR 4,BP
            0x4F => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 4)) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // BVC REL
            0x50 => {
                let branch = self.fetch(bus) as i8;
                if (self.p & Flags::OVERFLOW) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // EOR (BP),Y
            0x51 => {
                let addr = self.addr_bp_indirect_y(bus);
                let data = bus.read(addr);
                self.a ^= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // EOR (BP),Z
            0x52 => {
                let addr = self.addr_bp_indirect_z(bus);
                let data = bus.read(addr);
                self.a ^= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // BVC WREL
            0x53 => {
                let lo = self.fetch(bus);
                let hi = self.fetch(bus);
                let branch = i16::from_le_bytes([lo, hi]);
                if (self.p & Flags::OVERFLOW) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch)
                        .to_le_bytes();
                }
            }

            // ASR BP,X
            0x54 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                let (data, carry) = (data as i8).overflowing_shr(1);
                let data = data as u8;
                bus.write(addr, data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (data & 0x80) != 0);
                self.set_flag(Flags::ZERO, data == 0);
            }

            // EOR BP,X
            0x55 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                self.a ^= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LSR BP,X
            0x56 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                let (data, carry) = data.overflowing_shr(1);
                bus.write(addr, data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (data & 0x80) != 0);
                self.set_flag(Flags::ZERO, data == 0);
            }

            // RMB 5,BP
            0x57 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data & !(1 << 5);
                bus.write(addr, data);
            }

            // CLI
            0x58 => {
                self.p &= !Flags::INTERRUPT_DISABLE;
            }

            // EOR ABS,Y
            0x59 => {
                let addr = self.addr_abs_y(bus);
                let data = bus.read(addr);
                self.a ^= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // PHY
            0x5A => {
                self.push(bus, self.y);
            }

            // TAB
            0x5B => {
                self.b = self.a;
            }

            // AUG
            0x5C => {
                self.fetch(bus);
                self.fetch(bus);
                self.fetch(bus);
            }

            // EOR ABS,X
            0x5D => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                self.a ^= data;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LSR ABS,X
            0x5E => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                let (data, carry) = data.overflowing_shr(1);
                bus.write(addr, data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (data & 0x80) != 0);
                self.set_flag(Flags::ZERO, data == 0);
            }

            // BBR 5,BP
            0x5F => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 5)) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // RTS
            0x60 => {
                let lo = self.pull(bus);
                let hi = self.pull(bus);
                self.pc = [lo, hi];
            }

            // ADC (BP,X)
            0x61 => {
                let addr = self.addr_bp_indirect_x(bus);
                let data = bus.read(addr);
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // RTN IMM
            0x62 => {
                // this instruction is basically for popping whole stack frames
                // the argument is the location of the return address relative to
                // top of stack
                let offset = self.fetch(bus);
                self.sp = u16::from_le_bytes(self.sp)
                    .wrapping_add(offset as u16)
                    .to_le_bytes();
                let lo = self.pull(bus);
                let hi = self.pull(bus);
                self.pc = [lo, hi];
            }

            // BSR WREL
            0x63 => {
                let lo = self.fetch(bus);
                let hi = self.fetch(bus);
                let branch = i16::from_le_bytes([lo, hi]);
                self.push(bus, self.pc[1]);
                self.push(bus, self.pc[0]);
                self.pc = u16::from_le_bytes(self.pc)
                    .wrapping_add_signed(branch)
                    .to_le_bytes();
            }

            // STZ BP
            0x64 => {
                let addr = self.addr_bp(bus);
                bus.write(addr, self.z);
            }

            // ADC BP
            0x65 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ROR BP
            0x66 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let (result, carry) = data.overflowing_shr(1);
                let result = result | ((self.p & Flags::CARRY) << 7);
                bus.write(addr, result);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // RMB 6,BP
            0x67 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data & !(1 << 6);
                bus.write(addr, data);
            }

            // PLA
            0x68 => {
                self.a = self.pull(bus);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ADC IMM
            0x69 => {
                let data = self.fetch(bus);
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ROR A
            0x6A => {
                let (result, carry) = self.a.overflowing_shr(1);
                self.a = result | ((self.p & Flags::CARRY) << 7);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // TZA
            0x6B => {
                self.a = self.z;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // JMP (ABS)
            0x6C => {
                let addr = self.addr_abs_indirect(bus);
                self.pc = addr.to_le_bytes();
            }

            // ADC ABS
            0x6D => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ROR ABS
            0x6E => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                let (result, carry) = data.overflowing_shr(1);
                let result = result | ((self.p & Flags::CARRY) << 7);
                bus.write(addr, result);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // BBR 6,BP
            0x6F => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 6)) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // BVS REL
            0x70 => {
                let branch = self.fetch(bus) as i8;
                if (self.p & Flags::OVERFLOW) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // ADC (BP),Y
            0x71 => {
                let addr = self.addr_bp_indirect_y(bus);
                let data = bus.read(addr);
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ADC (BP),Z
            0x72 => {
                let addr = self.addr_bp_indirect_z(bus);
                let data = bus.read(addr);
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // BVS WREL
            0x73 => {
                let lo = self.fetch(bus);
                let hi = self.fetch(bus);
                let branch = i16::from_le_bytes([lo, hi]);
                if (self.p & Flags::OVERFLOW) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch)
                        .to_le_bytes();
                }
            }

            // STZ BP,X
            0x74 => {
                let addr = self.addr_bp_x(bus);
                bus.write(addr, self.z);
            }

            // ADC BP,X
            0x75 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ROR BP,X
            0x76 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                let (result, carry) = data.overflowing_shr(1);
                let result = result | ((self.p & Flags::CARRY) << 7);
                bus.write(addr, result);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // RMB 7,BP
            0x77 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data & !(1 << 7);
                bus.write(addr, data);
            }

            // SEI
            0x78 => {
                self.p |= Flags::INTERRUPT_DISABLE;
            }

            // ADC ABS,Y
            0x79 => {
                let addr = self.addr_abs_y(bus);
                let data = bus.read(addr);
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // PLY
            0x7A => {
                self.y = self.pull(bus);
                self.set_flag(Flags::NEGATIVE, (self.y & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.y == 0);
            }

            // TBA
            0x7B => {
                self.a = self.b;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // JMP (ABS,X)
            0x7C => {
                let addr = self.addr_abs_indirect_x(bus);
                self.pc = addr.to_le_bytes();
            }

            // ADC ABS,X
            0x7D => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // ROR ABS,X
            0x7E => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                let (result, carry) = data.overflowing_shr(1);
                let result = result | ((self.p & Flags::CARRY) << 7);
                bus.write(addr, result);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // BBR 7,BP
            0x7F => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 7)) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // BRU REL / BRA REL
            0x80 => {
                let branch = self.fetch(bus) as i8;
                self.pc = u16::from_le_bytes(self.pc)
                    .wrapping_add_signed(branch as i16)
                    .to_le_bytes();
            }

            // STA (BP,X)
            0x81 => {
                let addr = self.addr_bp_indirect_x(bus);
                bus.write(addr, self.a);
            }

            // STA (d,SP),Y
            0x82 => {
                let addr = self.addr_sp_indirect_y(bus);
                bus.write(addr, self.a);
            }

            // BRU WREL / BRA WREL
            0x83 => {
                let lo = self.fetch(bus);
                let hi = self.fetch(bus);
                let branch = i16::from_le_bytes([lo, hi]);
                self.pc = u16::from_le_bytes(self.pc)
                    .wrapping_add_signed(branch)
                    .to_le_bytes();
            }

            // STY BP
            0x84 => {
                let addr = self.addr_bp(bus);
                bus.write(addr, self.y);
            }

            // STA BP
            0x85 => {
                let addr = self.addr_bp(bus);
                bus.write(addr, self.a);
            }

            // STX BP
            0x86 => {
                let addr = self.addr_bp(bus);
                bus.write(addr, self.x);
            }

            // SMB 0,BP
            0x87 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data | (1 << 0);
                bus.write(addr, data);
            }

            // DEY
            0x88 => {
                self.y = self.y.wrapping_sub(1);
                self.set_flag(Flags::NEGATIVE, (self.y & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.y == 0);
            }

            // BIT IMM
            0x89 => {
                let data = self.fetch(bus);
                self.set_flag(Flags::NEGATIVE, (data & Flags::NEGATIVE) != 0);
                self.set_flag(Flags::OVERFLOW, (data & Flags::OVERFLOW) != 0);
                self.set_flag(Flags::ZERO, (self.a & data) == 0);
            }

            // TXA
            0x8A => {
                self.a = self.x;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // STY ABS,X
            0x8B => {
                let addr = self.addr_abs_x(bus);
                bus.write(addr, self.y);
            }

            // STY ABS
            0x8C => {
                let addr = self.addr_abs(bus);
                bus.write(addr, self.y);
            }

            // STA ABS
            0x8D => {
                let addr = self.addr_abs(bus);
                bus.write(addr, self.a);
            }

            // STX ABS
            0x8E => {
                let addr = self.addr_abs(bus);
                bus.write(addr, self.x);
            }

            // BBS 0,BP
            0x8F => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 0)) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // BCC REL
            0x90 => {
                let branch = self.fetch(bus) as i8;
                if (self.p & Flags::CARRY) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // STA (BP),Y
            0x91 => {
                let addr = self.addr_bp_indirect_y(bus);
                bus.write(addr, self.a);
            }

            // STA (BP),Z
            0x92 => {
                let addr = self.addr_bp_indirect_z(bus);
                bus.write(addr, self.a);
            }

            // BCC WREL
            0x93 => {
                let lo = self.fetch(bus);
                let hi = self.fetch(bus);
                let branch = i16::from_le_bytes([lo, hi]);
                if (self.p & Flags::CARRY) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch)
                        .to_le_bytes();
                }
            }

            // STY BP,X
            0x94 => {
                let addr = self.addr_bp_x(bus);
                bus.write(addr, self.y);
            }

            // STA BP,X
            0x95 => {
                let addr = self.addr_bp_x(bus);
                bus.write(addr, self.a);
            }

            // STX BP,Y
            0x96 => {
                let addr = self.addr_bp_y(bus);
                bus.write(addr, self.a);
            }

            // SMB 1,BP
            0x97 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data | (1 << 1);
                bus.write(addr, data);
            }

            // TYA
            0x98 => {
                self.a = self.y;
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // STA ABS,Y
            0x99 => {
                let addr = self.addr_abs_y(bus);
                bus.write(addr, self.a);
            }

            // TXS
            0x9A => {
                self.sp[0] = self.x;
                self.stack_xfer_wait = true;
            }

            // STX ABS,Y
            0x9B => {
                let addr = self.addr_abs_y(bus);
                bus.write(addr, self.x);
            }

            // STZ ABS
            0x9C => {
                let addr = self.addr_abs(bus);
                bus.write(addr, self.z);
            }

            // STA ABS,X
            0x9D => {
                let addr = self.addr_abs_x(bus);
                bus.write(addr, self.a);
            }

            // STZ ABS,X
            0x9E => {
                let addr = self.addr_abs_x(bus);
                bus.write(addr, self.z);
            }

            // BBS 1,BP
            0x9F => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 1)) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // LDY IMM
            0xA0 => {
                self.y = self.fetch(bus);
                self.set_flag(Flags::NEGATIVE, (self.y & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.y == 0);
            }

            // LDA (BP,X)
            0xA1 => {
                let addr = self.addr_bp_indirect_x(bus);
                self.a = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LDX IMM
            0xA2 => {
                self.x = self.fetch(bus);
                self.set_flag(Flags::NEGATIVE, (self.x & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.x == 0);
            }

            // LDZ IMM
            0xA3 => {
                self.z = self.fetch(bus);
                self.set_flag(Flags::NEGATIVE, (self.z & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.z == 0);
            }

            // LDY BP
            0xA4 => {
                let addr = self.addr_bp(bus);
                self.y = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.y & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.y == 0);
            }

            // LDA BP
            0xA5 => {
                let addr = self.addr_bp(bus);
                self.a = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LDX BP
            0xA6 => {
                let addr = self.addr_bp(bus);
                self.x = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.x & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.x == 0);
            }

            // SMB 2,BP
            0xA7 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data | (1 << 2);
                bus.write(addr, data);
            }

            // TAY
            0xA8 => {
                self.y = self.a;
                self.set_flag(Flags::NEGATIVE, (self.y & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.y == 0);
            }

            // LDA IMM
            0xA9 => {
                self.a = self.fetch(bus);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // TAX
            0xAA => {
                self.x = self.a;
                self.set_flag(Flags::NEGATIVE, (self.x & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.x == 0);
            }

            // LDZ ABS
            0xAB => {
                let addr = self.addr_abs(bus);
                self.z = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.z & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.z == 0);
            }

            // LDY ABS
            0xAC => {
                let addr = self.addr_abs(bus);
                self.y = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.y & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.y == 0);
            }

            // LDA ABS
            0xAD => {
                let addr = self.addr_abs(bus);
                self.a = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LDX ABS
            0xAE => {
                let addr = self.addr_abs(bus);
                self.x = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.x & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.x == 0);
            }

            // BBS 2,BP
            0xAF => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 2)) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // BCS REL
            0xB0 => {
                let branch = self.fetch(bus) as i8;
                if (self.p & Flags::CARRY) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // LDA (BP),Y
            0xB1 => {
                let addr = self.addr_bp_indirect_y(bus);
                self.a = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LDA (BP),Z
            0xB2 => {
                let addr = self.addr_bp_indirect_z(bus);
                self.a = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // BCS WREL
            0xB3 => {
                let lo = self.fetch(bus);
                let hi = self.fetch(bus);
                let branch = i16::from_le_bytes([lo, hi]);
                if (self.p & Flags::CARRY) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch)
                        .to_le_bytes();
                }
            }

            // LDY BP,X
            0xB4 => {
                let addr = self.addr_bp_x(bus);
                self.y = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.y & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.y == 0);
            }

            // LDA BP,X
            0xB5 => {
                let addr = self.addr_bp_x(bus);
                self.a = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LDX BP,Y
            0xB6 => {
                let addr = self.addr_bp_x(bus);
                self.x = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.x & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.x == 0);
            }

            // SMB 3,BP
            0xB7 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data | (1 << 3);
                bus.write(addr, data);
            }

            // CLV
            0xB8 => {
                self.p &= !Flags::OVERFLOW;
            }

            // LDA ABS,Y
            0xB9 => {
                let addr = self.addr_abs_y(bus);
                self.a = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // TSX
            0xBA => {
                self.x = self.sp[0]; // transfer lo byte
                self.set_flag(Flags::NEGATIVE, (self.x & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.x == 0);
            }

            // LDZ ABS,X
            0xBB => {
                let addr = self.addr_abs_x(bus);
                self.z = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.z & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.z == 0);
            }

            // LDY ABS,X
            0xBC => {
                let addr = self.addr_abs_x(bus);
                self.y = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.y & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.y == 0);
            }

            // LDA ABS,X
            0xBD => {
                let addr = self.addr_abs_x(bus);
                self.a = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LDX ABS,Y
            0xBE => {
                let addr = self.addr_abs_y(bus);
                self.x = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.x & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.x == 0);
            }

            // BBS 3,BP
            0xBF => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 3)) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // CPY IMM
            0xC0 => {
                let data = self.fetch(bus);
                let (result, carry) = self.y.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // CMP (BP,X)
            0xC1 => {
                let addr = self.addr_bp_indirect_x(bus);
                let data = bus.read(addr);
                let (result, carry) = self.a.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // CPZ IMM
            0xC2 => {
                let data = self.fetch(bus);
                let (result, carry) = self.z.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // DEW BP
            0xC3 => {
                let addr = self.addr_bp(bus);
                let lo = bus.read(addr);
                let hi = bus.read(addr.wrapping_add(1));
                let result = u16::from_le_bytes([lo, hi]).wrapping_sub(1);
                let [lo, hi] = result.to_le_bytes();
                bus.write(addr, lo);
                bus.write(addr.wrapping_add(1), hi);
                self.set_flag(Flags::NEGATIVE, (result & 0x8000) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // CPY BP
            0xC4 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let (result, carry) = self.y.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // CMP BP
            0xC5 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let (result, carry) = self.a.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // DEC BP
            0xC6 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let result = data.wrapping_sub(1);
                bus.write(addr, result);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // SMB 4,BP
            0xC7 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data | (1 << 4);
                bus.write(addr, data);
            }

            // INY
            0xC8 => {
                self.y = self.y.wrapping_add(1);
                self.set_flag(Flags::NEGATIVE, (self.y & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.y == 0);
            }

            // CMP IMM
            0xC9 => {
                let data = self.fetch(bus);
                let (result, carry) = self.a.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // DEX
            0xCA => {
                self.x = self.x.wrapping_sub(1);
                self.set_flag(Flags::NEGATIVE, (self.x & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.x == 0);
            }

            // ASW ABS
            0xCB => {
                let addr = self.addr_bp(bus);
                let lo = bus.read(addr);
                let hi = bus.read(addr.wrapping_add(1));
                let (result, carry) = u16::from_le_bytes([lo, hi]).overflowing_shl(1);
                let [lo, hi] = result.to_le_bytes();
                bus.write(addr, lo);
                bus.write(addr.wrapping_add(1), hi);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x8000) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // CPY ABS
            0xCC => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                let (result, carry) = self.y.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // CMP ABS
            0xCD => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                let (result, carry) = self.a.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // DEC ABS
            0xCE => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                let result = data.wrapping_sub(1);
                bus.write(addr, result);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // BBS 4,BP
            0xCF => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 4)) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // BNE REL
            0xD0 => {
                let branch = self.fetch(bus) as i8;
                if (self.p & Flags::ZERO) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // CMP (BP),Y
            0xD1 => {
                let addr = self.addr_bp_indirect_y(bus);
                let data = bus.read(addr);
                let (result, carry) = self.a.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // CMP (BP),Z
            0xD2 => {
                let addr = self.addr_bp_indirect_z(bus);
                let data = bus.read(addr);
                let (result, carry) = self.a.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // BNE WREL
            0xD3 => {
                let lo = self.fetch(bus);
                let hi = self.fetch(bus);
                let branch = i16::from_le_bytes([lo, hi]);
                if (self.p & Flags::ZERO) == 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch)
                        .to_le_bytes();
                }
            }

            // CPZ BP
            0xD4 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let (result, carry) = self.z.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // CMP BP,X
            0xD5 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                let (result, carry) = self.a.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // DEC BP,X
            0xD6 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                let result = data.wrapping_sub(1);
                bus.write(addr, result);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // SMB 5,BP
            0xD7 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data | (1 << 5);
                bus.write(addr, data);
            }

            // CLD
            0xD8 => {
                self.p &= !Flags::DECIMAL_MODE;
            }

            // CMP ABS,Y
            0xD9 => {
                let addr = self.addr_abs_y(bus);
                let data = bus.read(addr);
                let (result, carry) = self.a.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // PHX
            0xDA => {
                self.push(bus, self.x);
            }

            // PHZ
            0xDB => {
                self.push(bus, self.z);
            }

            // CPZ ABS
            0xDC => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                let (result, carry) = self.z.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // CMP ABS,X
            0xDD => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                let (result, carry) = self.a.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // DEC ABS,X
            0xDE => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                let result = data.wrapping_sub(1);
                bus.write(addr, result);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // BBS 5,BP
            0xDF => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 5)) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // CPX IMM
            0xE0 => {
                let data = self.fetch(bus);
                let (result, carry) = self.x.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // SBC (BP,X)
            0xE1 => {
                let addr = self.addr_bp_indirect_x(bus);
                let data = !bus.read(addr); // invert arg and adc
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // LDA (d,SP),Y
            0xE2 => {
                let addr = self.addr_sp_indirect_y(bus);
                self.a = bus.read(addr);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // INW BP
            0xE3 => {
                let addr = self.addr_bp(bus);
                let lo = bus.read(addr);
                let hi = bus.read(addr.wrapping_add(1));
                let result = u16::from_le_bytes([lo, hi]).wrapping_add(1);
                let [lo, hi] = result.to_le_bytes();
                bus.write(addr, lo);
                bus.write(addr.wrapping_add(1), hi);
                self.set_flag(Flags::NEGATIVE, (result & 0x8000) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // CPX BP
            0xE4 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let (result, carry) = self.x.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // SBC BP
            0xE5 => {
                let addr = self.addr_bp(bus);
                let data = !bus.read(addr); // invert arg and adc
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // INC BP
            0xE6 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let result = data.wrapping_add(1);
                bus.write(addr, result);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // SMB 6,BP
            0xE7 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data | (1 << 6);
                bus.write(addr, data);
            }

            // INX
            0xE8 => {
                self.x = self.x.wrapping_add(1);
                self.set_flag(Flags::NEGATIVE, (self.x & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.x == 0);
            }

            // SBC IMM
            0xE9 => {
                let data = !self.fetch(bus); // invert arg and adc
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // NOP
            0xEA => {}

            // ROW
            0xEB => {
                let addr = self.addr_bp(bus);
                let lo = bus.read(addr);
                let hi = bus.read(addr.wrapping_add(1));
                let (result, carry) = u16::from_le_bytes([lo, hi]).overflowing_shl(1);
                let [lo, hi] = result.to_le_bytes();
                bus.write(addr, lo);
                bus.write(addr.wrapping_add(1), hi);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x8000) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // CPX ABS
            0xEC => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                let (result, carry) = self.x.overflowing_sub(data);
                self.set_flag(Flags::CARRY, carry);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // SBC ABS
            0xED => {
                let addr = self.addr_abs(bus);
                let data = !bus.read(addr); // invert arg and adc
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // INC ABS
            0xEE => {
                let addr = self.addr_abs(bus);
                let data = bus.read(addr);
                let result = data.wrapping_add(1);
                bus.write(addr, result);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // BBS 6,BP
            0xEF => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 6)) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // BEQ REL
            0xF0 => {
                let branch = self.fetch(bus) as i8;
                if (self.p & Flags::ZERO) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }

            // SBC (BP),Y
            0xF1 => {
                let addr = self.addr_bp_indirect_y(bus);
                let data = !bus.read(addr); // invert arg and adc
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // SBC (BP),Z
            0xF2 => {
                let addr = self.addr_bp_indirect_z(bus);
                let data = !bus.read(addr); // invert arg and adc
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // BEQ WREL
            0xF3 => {
                let lo = self.fetch(bus);
                let hi = self.fetch(bus);
                let branch = i16::from_le_bytes([lo, hi]);
                if (self.p & Flags::ZERO) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch)
                        .to_le_bytes();
                }
            }

            // PHW WIMM
            0xF4 => {
                let lo = self.fetch(bus);
                let hi = self.fetch(bus);
                self.push(bus, hi);
                self.push(bus, lo);
            }

            // SBC BP,X
            0xF5 => {
                let addr = self.addr_bp_x(bus);
                let data = !bus.read(addr); // invert arg and adc
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // INC BP,X
            0xF6 => {
                let addr = self.addr_bp_x(bus);
                let data = bus.read(addr);
                let result = data.wrapping_add(1);
                bus.write(addr, result);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // SMB 7,BP
            0xF7 => {
                let addr = self.addr_bp(bus);
                let data = bus.read(addr);
                let data = data | (1 << 7);
                bus.write(addr, data);
            }

            // SED
            0xF8 => {
                self.p |= Flags::DECIMAL_MODE;
            }

            // SBC ABS,Y
            0xF9 => {
                let addr = self.addr_abs_y(bus);
                let data = !bus.read(addr); // invert arg and adc
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // PLX
            0xFA => {
                self.x = self.pull(bus);
                self.set_flag(Flags::NEGATIVE, (self.x & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.x == 0);
            }

            // PLZ
            0xFB => {
                self.z = self.pull(bus);
                self.set_flag(Flags::NEGATIVE, (self.z & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.z == 0);
            }

            // PHW WABS
            0xFC => {
                let addr = self.addr_abs_indirect(bus);
                let lo = bus.read(addr);
                let hi = bus.read(addr.wrapping_add(1));
                self.push(bus, hi);
                self.push(bus, lo);
            }

            // SBC ABS,X
            0xFD => {
                let addr = self.addr_abs_x(bus);
                let data = !bus.read(addr); // invert arg and adc
                let (result, carry1) = self.a.overflowing_add(data);
                let (result, carry2) =
                    result.overflowing_add(if (self.p & Flags::CARRY) != 0 { 1 } else { 0 });
                let overflow = ((!(self.a ^ data)) & (self.a ^ result) & 0x80) != 0;
                self.a = result;
                self.set_flag(Flags::OVERFLOW, overflow);
                self.set_flag(Flags::CARRY, carry1 || carry2);
                self.set_flag(Flags::NEGATIVE, (self.a & 0x80) != 0);
                self.set_flag(Flags::ZERO, self.a == 0);
            }

            // INC ABS,X
            0xFE => {
                let addr = self.addr_abs_x(bus);
                let data = bus.read(addr);
                let result = data.wrapping_add(1);
                bus.write(addr, result);
                self.set_flag(Flags::NEGATIVE, (result & 0x80) != 0);
                self.set_flag(Flags::ZERO, result == 0);
            }

            // BBS 7,BP
            0xFF => {
                let addr = self.addr_bp(bus);
                let branch = self.fetch(bus) as i8;
                let data = bus.read(addr);
                if (data & (1 << 7)) != 0 {
                    self.pc = u16::from_le_bytes(self.pc)
                        .wrapping_add_signed(branch as i16)
                        .to_le_bytes();
                }
            }
        }
    }
}
