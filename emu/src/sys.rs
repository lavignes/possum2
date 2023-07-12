//! Overall System Emulation
//!
//! The Possum2 has a:
//! * 65CE02 CPU
//! * 2 6551 UARTs
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
//! F020      SER1 Data
//! F021      SER1 Status
//! F022      SER1 Command
//! F023      SER1 Control
//! F030      PPU Contol
//! F031      PPU Status
//! F032-F033 PPU Address
//! F034      PPU Data
//! F035      PPU DMA Control
//! F036-F037 PPU DMA Address (src in RAM, dst is the PPU address)
//! F038-F039 PPU DMA Length
//! F03A      PPU BG Scroll-X
//! F03B      PPU BG Scroll-Y
//! F03C      PPU FG Scroll-X
//! F03D      PPU FG Scroll-Y
//! F03E-F03F PPU Line (current h-line being drawn)
//! F040-F04F SND?
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
use std::io::{Read, Write};

use crate::{
    bus::{Bus, BusDevice},
    cpu::Cpu,
    uart::Uart,
};

struct Mem {
    inner: Vec<u8>,
    bank_select: [usize; 16], // we create 16 bank selects, but rom is static
}

impl Mem {
    fn new() -> Self {
        Self {
            inner: vec![0; 65536 * 255],
            bank_select: [0; 16],
        }
    }

    fn read(&self, addr: u16) -> u8 {
        // get the high nibble to determine which 4K "chapter" we are in
        let chapter = ((addr & 0xF000) >> 12) as usize;
        let base = chapter * (0x1000 + self.bank_select[chapter]);
        let offset = (addr & 0x0FFF) as usize;
        self.inner[base + offset]
    }

    fn write(&mut self, addr: u16, data: u8) {
        let chapter = ((addr & 0xF000) >> 12) as usize;
        let base = chapter * (0x1000 + self.bank_select[chapter]);
        let offset = (addr & 0x0FFF) as usize;
        self.inner[base + offset] = data;
    }
}

pub struct System<S0, S1> {
    cpu: Cpu,
    ser0: Uart<S0>,
    ser1: Uart<S1>,

    mem: Mem,
}

impl<S0, S1> System<S0, S1>
where
    S0: Read + Write,
    S1: Read + Write,
{
    pub fn new(rom: &[u8], ser0: S0, ser1: S1) -> Self {
        let cpu = Cpu::new();
        let ser0 = Uart::new(ser0);
        let ser1 = Uart::new(ser1);
        let mut mem = Mem::new();

        for (i, data) in rom.iter().enumerate() {
            mem.write((0xF100 + i) as u16, *data);
        }

        Self {
            cpu,
            ser0,
            ser1,
            mem,
        }
    }

    pub fn reset(&mut self) {
        let System {
            cpu,
            ser0,
            ser1,
            mem,
        } = self;
        cpu.reset(&mut CpuView { ser0, ser1, mem });
        let mut uart_view = UartView { cpu };
        ser0.reset(&mut uart_view);
        ser1.reset(&mut uart_view);
    }

    pub fn tick(&mut self) {
        let System {
            cpu,
            ser0,
            ser1,
            mem,
        } = self;
        cpu.tick(&mut CpuView { ser0, ser1, mem });
    }

    pub fn view(&mut self) -> (&'_ mut Cpu, CpuView<'_, S0, S1>) {
        let System {
            cpu,
            ser0,
            ser1,
            mem,
        } = self;
        (cpu, CpuView { ser0, ser1, mem })
    }
}

struct UartView<'a> {
    cpu: &'a mut Cpu,
}

impl<'a> Bus for UartView<'a> {
    fn read(&mut self, _addr: u16) -> u8 {
        0
    }

    fn write(&mut self, _addr: u16, _data: u8) {}

    fn irq(&mut self) {
        self.cpu.irq();
    }

    fn nmi(&mut self) {
        self.cpu.nmi();
    }
}

pub struct CpuView<'a, S0, S1> {
    ser0: &'a mut Uart<S0>,
    ser1: &'a mut Uart<S1>,

    mem: &'a mut Mem,
}

impl<'a, S0, S1> Bus for CpuView<'a, S0, S1>
where
    S0: Read + Write,
    S1: Read + Write,
{
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0xF000..=0xF00E => self.mem.bank_select[(addr as usize) - 0xF000] as u8,
            0xF004..=0xF00F => 0,
            0xF010..=0xF013 => self.ser0.read(addr - 0xF010),
            0xF014..=0xF01F => 0,
            0xF020..=0xF023 => self.ser1.read(addr - 0xF020),
            0xF024..=0xF0FF => todo!("reading io address {addr:04X}"),
            _ => self.mem.read(addr),
        }
    }

    fn write(&mut self, addr: u16, data: u8) {
        match addr {
            0xF000..=0xF00E => self.mem.bank_select[(addr as usize) - 0xF000] = data as usize,
            0xF004..=0xF00F => {}
            0xF010..=0xF013 => self.ser0.write(addr - 0xF010, data),
            0xF014..=0xF01F => {}
            0xF020..=0xF023 => self.ser1.write(addr - 0xF020, data),
            0xF024..=0xF0FF => todo!("writing to io address {addr:04X}"),
            _ => self.mem.write(addr, data),
        }
    }

    fn irq(&mut self) {}

    fn nmi(&mut self) {}
}
