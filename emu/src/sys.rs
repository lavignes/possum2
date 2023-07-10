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
//! 0000-0FFF RAM0 (Fixed)
//! 1000-3FFF RAM1
//! 4000-7FFF RAM2
//! 8000-BFFF RAM3
//! C000-EFFF RAM4
//! F000-F0FF IO
//! F100-FFFF ROM
//!
//! IO Addresses:
//!
//! F000-F003 RAM Bank Select
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
//! 8000-9FFF Tile Bank 0 (256 8x8 tiles, 2 pixels per byte)
//! A000-BFFF Tile Bank 1 (256 8x8 tiles, 2 pixels per byte)
//! C000-C0FF Sprite Attributes (128 sprites, 2 byte each)
//! C100-C27F Sprite Positions (128 sprites, 3 bytes each, 20-bits for x and y)
//! D000-D02F BG/FG Palette (16 24-bit colors)
//! E000-D02F Sprite Palette (16 24-bit colors)
use std::mem;

use crate::{
    bus::{Bus, BusDevice},
    cpu::Cpu,
};

#[derive(Debug)]
struct Mem(Vec<u8>);

impl Mem {
    const RAM0_SIZE: usize = 4096;
    const SMALL_RAM_SIZE: usize = 12288;
    const BIG_RAM_SIZE: usize = 16384;
    const ROM_SIZE: usize = 3840;
    const MAX_BANKS: usize = 256;

    fn new() -> Self {
        Self(vec![
            0;
            Self::RAM0_SIZE
                + ((Self::SMALL_RAM_SIZE * Self::MAX_BANKS) * 2)
                + ((Self::BIG_RAM_SIZE * Self::MAX_BANKS) * 2)
                + Self::ROM_SIZE
        ])
    }

    fn view(&mut self) -> MemView<'_> {
        // Safety: No overlapping slices
        let ram0 = unsafe { mem::transmute(self.0.as_mut_ptr()) };
        let ram1 = unsafe { mem::transmute(self.0.as_mut_ptr().add(Self::RAM0_SIZE)) };
        let ram2 = unsafe {
            mem::transmute(
                self.0
                    .as_mut_ptr()
                    .add(Self::RAM0_SIZE + (Self::SMALL_RAM_SIZE * Self::MAX_BANKS)),
            )
        };
        let ram3 = unsafe {
            mem::transmute(self.0.as_mut_ptr().add(
                Self::RAM0_SIZE
                    + (Self::SMALL_RAM_SIZE * Self::MAX_BANKS)
                    + (Self::BIG_RAM_SIZE * Self::MAX_BANKS),
            ))
        };
        let ram4 = unsafe {
            mem::transmute(self.0.as_mut_ptr().add(
                Self::RAM0_SIZE
                    + (Self::SMALL_RAM_SIZE * Self::MAX_BANKS)
                    + ((Self::BIG_RAM_SIZE * Self::MAX_BANKS) * 2),
            ))
        };
        let rom = unsafe {
            mem::transmute(self.0.as_mut_ptr().add(
                Self::RAM0_SIZE
                    + ((Self::SMALL_RAM_SIZE * Self::MAX_BANKS) * 2)
                    + ((Self::BIG_RAM_SIZE * Self::MAX_BANKS) * 2),
            ))
        };
        MemView {
            ram0,
            ram1,
            ram2,
            ram3,
            ram4,
            rom,
        }
    }
}

#[derive(Debug)]
struct MemView<'a> {
    ram0: &'a mut [u8; Mem::RAM0_SIZE],
    ram1: &'a mut [[u8; Mem::SMALL_RAM_SIZE]; Mem::MAX_BANKS],
    ram2: &'a mut [[u8; Mem::BIG_RAM_SIZE]; Mem::MAX_BANKS],
    ram3: &'a mut [[u8; Mem::BIG_RAM_SIZE]; Mem::MAX_BANKS],
    ram4: &'a mut [[u8; Mem::SMALL_RAM_SIZE]; Mem::MAX_BANKS],
    rom: &'a mut [u8; Mem::ROM_SIZE],
}

#[derive(Debug)]
pub struct Sys {
    cpu: Cpu,

    banks: [usize; 4],
    mem: Mem,
}

impl Sys {
    pub fn new() -> Self {
        let cpu = Cpu::new();
        let mem = Mem::new();
        Self {
            cpu,
            banks: [0; 4],
            mem,
        }
    }

    pub fn reset(&mut self) {
        let Sys { cpu, banks, mem } = self;
        let mem_view = mem.view();
        cpu.reset(&mut CpuView { banks, mem_view })
    }

    pub fn tick(&mut self) {
        let Sys { cpu, banks, mem } = self;
        let mem_view = mem.view();
        cpu.tick(&mut CpuView { banks, mem_view })
    }

    pub fn view(&mut self) -> (&'_ mut Cpu, CpuView<'_>) {
        let Sys { cpu, banks, mem } = self;
        let mem_view = mem.view();
        (cpu, CpuView { banks, mem_view })
    }
}

#[derive(Debug)]
pub struct CpuView<'a> {
    banks: &'a mut [usize; 4],
    mem_view: MemView<'a>,
}

impl<'a> Bus for CpuView<'a> {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x0FFF => self.mem_view.ram0[addr as usize],
            0x1000..=0x3FFF => self.mem_view.ram1[self.banks[0]][(addr as usize) - 0x1000],
            0x4000..=0x7FFF => self.mem_view.ram2[self.banks[1]][(addr as usize) - 0x4000],
            0x8000..=0xBFFF => self.mem_view.ram3[self.banks[2]][(addr as usize) - 0x8000],
            0xC000..=0xEFFF => self.mem_view.ram4[self.banks[3]][(addr as usize) - 0xC000],
            0xF000..=0xF003 => self.banks[(addr as usize) - 0xF000] as u8,
            0xF004..=0xF0FF => todo!(),
            0xF100..=0xFFFF => self.mem_view.rom[(addr as usize) - 0xF100],
        }
    }

    fn write(&mut self, addr: u16, data: u8) {
        match addr {
            0x0000..=0x0FFF => self.mem_view.ram0[addr as usize] = data,
            0x1000..=0x3FFF => self.mem_view.ram1[self.banks[0]][(addr as usize) - 0x1000] = data,
            0x4000..=0x7FFF => self.mem_view.ram2[self.banks[1]][(addr as usize) - 0x4000] = data,
            0x8000..=0xBFFF => self.mem_view.ram3[self.banks[2]][(addr as usize) - 0x8000] = data,
            0xC000..=0xEFFF => self.mem_view.ram4[self.banks[3]][(addr as usize) - 0xC000] = data,
            0xF000..=0xF003 => self.banks[(addr as usize) - 0xF000] = data as usize,
            0xF004..=0xF0FF => todo!(),
            0xF100..=0xFFFF => {}
        }
    }
}
