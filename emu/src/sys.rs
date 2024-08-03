//! Overall System Emulation
//!
//! The Possum2 has a:
//! * CSG65CE02 CPU
//! * 2 6551 UARTs
//! * 2 FD179X Floppy Disk Controllers
//! * Simple Interrupt Controller using 74148 and 74574
//! * NES/GBC-ish PPU with external VRAM and DMA
//! * Banked RAM
//!
//! PPU has 2 resolutions? (640x480 and 1024x768) since it
//! internally maintains a 1024x1024 plane of tiles.
//!
//! 32 sprites per line!
//!
//! TODO: Use WGPU to implement PPU in hardware. Scroll effects
//!   can be implemented by rendering the BG and FG first to a
//!   texture, and then sample into the texture in the pixel shader.
//!   The same would have to be done for sprites... so I don't know.
//!   I could just _not_ support such effects for sprites.
//!
//! Memory Map:
//!
//! 0000-0FFF RAM0
//! 1000-1FFF RAM1
//! 2000-2FFF RAM2
//! 3000-3FFF RAM3
//! 4000-4FFF RAM4
//! 5000-5FFF RAM5
//! 6000-6FFF RAM6
//! 7000-6FFF RAM7
//! 8000-7FFF RAM8
//! 9000-9FFF RAM9
//! A000-AFFF RAMA
//! B000-BFFF RAMB
//! C000-CFFF RAMC
//! D000-DFFF RAMD
//! E000-EFFF RAME
//! F000-F0FF IO
//! F100-FFFF ROM
//!
//! IO Addresses:
//!
//! F000-F00E RAM Bank Select
//! F010      SER0 Data
//! F011      SER0 Status
//! F012      SER0 Command
//! F013      SER0 Control
//! F014      SER1 Data
//! F015      SER1 Status
//! F016      SER1 Command
//! F017      SER1 Control
//! F020      PPU Contol/Status (Reads return Status)
//! F021      PPU Data
//! F022      PPU Address (2 writes)
//! F023      PPU DMA Control
//! F024      PPU DMA Src (2 writes)
//! F025      PPU DMA Dst (2 writes)
//! F026      PPU DMA Length (2 writes)
//! F027      PPU BG Scroll-X (2 writes)
//! F028      PPU BG Scroll-Y (2 writes)
//! F029      PPU FG Scroll-X (2 writes)
//! F02A      PPU FG Scroll-Y (2 writes)
//! F030      FDC0 Command/Status
//! F031      FDC0 Track
//! F032      FDC0 Sector
//! F033      FDC0 Data
//! F034      FDC1 Command/Status
//! F035      FDC1 Track
//! F036      FDC1 Sector
//! F037      FDC1 Data
//! F0FF      Interrupt Latch
//!
//! PPU Memory Map:
//!
//! 0000-3FFF BG Map 16K (128x128 tiles)
//! 4000-7FFF FG Map 16K (128x128 tiles)
//! 8000-9FFF BG Map Attributes (128x128 4-bits per tile)
//! A000-BFFF FE Map Attributes (128x128 4-bits per tile)
//! C000-D7FF Tile Bank 0 (256 8x8 tiles, 24 bytes per tile)
//! D800-F000 Tile Bank 1 (256 8x8 tiles, 24 bytes per tile)
//! F000-F0FF Sprite Attributes (128 sprites, 2 byte each)
//! F100-F27F Sprite Positions (128 sprites, 3 bytes each, 20-bits for x and y)
//! F280-F2DF BG/FG Palettes (4 palettes of 8 24-bit colors)
//! F2E0-F33F Sprite Palette (4 palettes of 8 24-bit colors)
use std::io::{Read, Seek, Write};

use crate::{
    bus::{Bus, BusDevice},
    cpu::Cpu,
    fdc::Fdc,
    uart::Uart,
};

pub struct Mem {
    inner: Vec<u8>,
    bank_select: [usize; 16], // we create 16 bank selects, but rom is static
}

impl Mem {
    fn new() -> Self {
        Self {
            inner: vec![0; 65536 * 4],
            bank_select: [0; 16],
        }
    }

    pub fn read(&self, addr: u16) -> u8 {
        // get the high nibble to determine which 4K "chapter" we are in
        let chapter = ((addr & 0xF000) >> 12) as usize;
        let base = chapter * (0x1000 + self.bank_select[chapter]);
        let offset = (addr & 0x0FFF) as usize;
        self.inner[base + offset]
    }

    fn write(&mut self, addr: u16, data: u8) {
        // get the high nibble to determine which 4K "chapter" we are in
        let chapter = ((addr & 0xF000) >> 12) as usize;
        let base = chapter * (0x1000 + self.bank_select[chapter]);
        let offset = (addr & 0x0FFF) as usize;
        self.inner[base + offset] = data;
    }

    pub fn bank(&self, addr: u16) -> usize {
        // get the high nibble to determine which 4K "chapter" we are in
        let chapter = ((addr & 0xF000) >> 12) as usize;
        self.bank_select[chapter]
    }
}

pub struct System<S0, S1, F0, F1> {
    cpu: Cpu,
    ser0: Uart<S0>,
    ser1: Uart<S1>,
    fdc0: Fdc<F0>,
    fdc1: Fdc<F1>,

    irq_latch: u8,
    mem: Mem,
}

impl<S0, S1, F0, F1> System<S0, S1, F0, F1>
where
    S0: Read + Write,
    S1: Read + Write,
    F0: Read + Write + Seek,
    F1: Read + Write + Seek,
{
    pub fn new(rom: &[u8], ser0: S0, ser1: S1, fdc0: F0, fdc1: F1) -> Self {
        let cpu = Cpu::new();
        let ser0 = Uart::new(ser0);
        let ser1 = Uart::new(ser1);
        let fdc0 = Fdc::new(fdc0);
        let fdc1 = Fdc::new(fdc1);
        let mut mem = Mem::new();

        for (i, data) in rom.iter().enumerate() {
            mem.write((0xF100 + i) as u16, *data);
        }

        Self {
            cpu,
            ser0,
            ser1,
            fdc0,
            fdc1,
            irq_latch: 0,
            mem,
        }
    }

    pub fn reset(&mut self) {
        let System {
            cpu,
            ser0,
            ser1,
            fdc0,
            fdc1,
            irq_latch,
            mem,
        } = self;
        cpu.reset(&mut CpuView {
            ser0,
            ser1,
            fdc0,
            fdc1,
            irq_latch,
            mem,
        });
        let mut io_view = IoView {};
        ser0.reset(&mut io_view);
        ser1.reset(&mut io_view);
        fdc0.reset(&mut io_view);
        fdc1.reset(&mut io_view);
        *irq_latch = 0;
    }

    pub fn tick(&mut self) {
        let System {
            cpu,
            ser0,
            ser1,
            fdc0,
            fdc1,
            irq_latch,
            mem,
        } = self;
        cpu.tick(&mut CpuView {
            ser0,
            ser1,
            fdc0,
            fdc1,
            irq_latch,
            mem,
        });
        let mut io_view = IoView {};
        ser0.tick(&mut io_view);
        ser1.tick(&mut io_view);
        fdc0.tick(&mut io_view);
        fdc1.tick(&mut io_view);

        // update IRQ latch (pre-shifting makes implementing the jump table trivial)
        // see http://www.6502.org/mini-projects/priority-interrupt-encoder/priority-interrupt-encoder.html
        if fdc0.drq() {
            *irq_latch = 1 << 1;
        } else if fdc1.drq() {
            *irq_latch = 2 << 1;
        } else if fdc0.irq() {
            *irq_latch = 3 << 1;
        } else if fdc1.irq() {
            *irq_latch = 4 << 1;
        } else if ser0.irq() {
            *irq_latch = 5 << 1;
        } else if ser1.irq() {
            *irq_latch = 6 << 1;
        }
        // the last 2 IRQs: PPU, and Parallel Port

        // tie all IRQs to CPU
        if ser0.irq() || ser1.irq() || fdc0.irq() || fdc0.drq() || fdc1.irq() || fdc1.drq() {
            cpu.irq();
        }
    }

    pub fn ser0_mut(&mut self) -> &mut Uart<S0> {
        &mut self.ser0
    }

    pub fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    pub fn mem(&self) -> &Mem {
        &self.mem
    }
}

struct IoView {}

impl Bus for IoView {
    fn read(&mut self, _addr: u16) -> u8 {
        0
    }

    fn write(&mut self, _addr: u16, _data: u8) {}
}

pub struct CpuView<'a, S0, S1, F0, F1> {
    ser0: &'a mut Uart<S0>,
    ser1: &'a mut Uart<S1>,
    fdc0: &'a mut Fdc<F0>,
    fdc1: &'a mut Fdc<F1>,

    irq_latch: &'a mut u8,
    mem: &'a mut Mem,
}

impl<'a, S0, S1, F0, F1> Bus for CpuView<'a, S0, S1, F0, F1>
where
    S0: Read + Write,
    S1: Read + Write,
    F0: Read + Write + Seek,
    F1: Read + Write + Seek,
{
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0xF000..=0xF00E => self.mem.bank_select[(addr as usize) - 0xF000] as u8,
            0xF00F => 0,
            0xF010..=0xF013 => self.ser0.read(addr - 0xF010),
            0xF014..=0xF017 => self.ser1.read(addr - 0xF014),
            0xF024..=0xF029 => todo!("reading io address {addr:04X}"),
            0xF030..=0xF033 => self.fdc0.read(addr - 0xF030),
            0xF034..=0xF037 => self.fdc1.read(addr - 0xF034),
            0xF038..=0xF0FE => todo!("reading io address {addr:04X}"),
            0xF0FF => {
                let irq = *self.irq_latch;
                *self.irq_latch = 0;
                irq
            }
            _ => self.mem.read(addr),
        }
    }

    fn write(&mut self, addr: u16, data: u8) {
        match addr {
            0xF000..=0xF00E => {
                self.mem.bank_select[(addr as usize) - 0xF000] = (data & 0b11) as usize
            }
            0xF00F => {}
            0xF010..=0xF013 => self.ser0.write(addr - 0xF010, data),
            0xF014..=0xF017 => self.ser1.write(addr - 0xF014, data),
            0xF024..=0xF029 => todo!("writing to io address {addr:04X}"),
            0xF030..=0xF033 => self.fdc0.write(addr - 0xF030, data),
            0xF034..=0xF037 => self.fdc1.write(addr - 0xF034, data),
            0xF038..=0xF0FE => todo!("writing to io address {addr:04X}"),
            0xF0FF => {}
            _ => self.mem.write(addr, data),
        }
    }
}
