use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Stdout, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use clap::Parser;
use cpu::Cpu;
use memmap2::MmapMut;
use signal_hook::{consts, flag};
use sys::{Mem, System};
use termion::{
    raw::{IntoRawMode, RawTerminal},
    AsyncReader,
};
use tracing::Level;

use crate::cpu::Flags;

mod bus;
mod cpu;
mod fdc;
mod sys;
mod uart;

struct NoopIo {}

impl Read for NoopIo {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}

impl Write for NoopIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for NoopIo {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Ok(0)
    }
}

struct MemMap {
    inner: MmapMut,
    offset: usize,
}

impl Read for MemMap {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let size = (&self.inner[self.offset..]).read(buf)?;
        self.offset += size;
        Ok(size)
    }
}

impl Write for MemMap {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let size = (&mut self.inner[self.offset..]).write(buf)?;
        self.offset += size;
        Ok(size)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Seek for MemMap {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match pos {
            SeekFrom::End(_) => {
                self.offset = self.inner.len();
            }
            SeekFrom::Start(offset) => {
                self.offset = offset as usize;
            }
            SeekFrom::Current(offset) => match self.offset.checked_add_signed(offset as isize) {
                Some(offset) => self.offset = offset,
                None => {
                    // following the spec, you should return err on seek before start of file
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "attempted to seek outside of memory map",
                    ));
                }
            },
        }
        self.offset = self.offset.clamp(0, self.inner.len());
        Ok(self.offset as u64)
    }
}

struct Tty {
    tx: RawTerminal<Stdout>,
    rx: AsyncReader,
}

impl Tty {
    fn new() -> Self {
        let tx = io::stdout().into_raw_mode().unwrap();
        let rx = termion::async_stdin();
        Self { tx, rx }
    }
}

impl Read for Tty {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.rx.read(buf)
    }
}

impl Write for Tty {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tx.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tx.flush()
    }
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to rom file
    rom: PathBuf,

    /// FD0 image file
    #[arg(long)]
    fd0: PathBuf,

    /// One of `TRACE`, `DEBUG`, `INFO`, `WARN`, or `ERROR`
    #[arg(short, long, default_value_t = Level::INFO)]
    log_level: Level,
}

fn main() -> Result<(), ()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_max_level(args.log_level)
        .with_writer(io::stderr)
        .init();

    let mut rom = Vec::new();
    File::open(&args.rom)
        .map_err(|e| tracing::error!("failed to open ROM file: {e}"))?
        .read_to_end(&mut rom)
        .map_err(|e| tracing::error!("failed to read ROM file: {e}"))?;
    if rom.len() != 0x0F00 {
        tracing::error!(
            "ROM file is {} bytes, but it must be exactly 3840 bytes (3.75KiB) in length!",
            rom.len()
        );
        return Err(());
    }

    let fd0 = File::options()
        .write(true)
        .read(true)
        .open(&args.fd0)
        .map_err(|e| tracing::error!("failed to open FD0 file: {e}"))?;
    let fd0 = (unsafe { MmapMut::map_mut(&fd0) })
        .map_err(|e| tracing::error!("failed to map FD0 file: {e}"))?;
    if fd0.len() != 0xB4000 {
        tracing::error!(
            "FD0 file is {} bytes, but it must be exactly 737280 bytes (720KiB) in length!",
            fd0.len()
        );
        return Err(());
    }
    let fd0 = MemMap {
        inner: fd0,
        offset: 0,
    };

    let debug_mode = Arc::new(AtomicBool::new(false));
    flag::register(consts::SIGUSR1, debug_mode.clone())
        .map_err(|e| {
            tracing::warn!("external debugger unavailable: failed to install SIGUSR1 handler: {e}")
        })
        .ok();

    let mut breakpoints = Vec::new();
    let mut sys = System::new(&rom, Tty::new(), NoopIo {}, fd0, NoopIo {});
    sys.reset();

    'emu: loop {
        if breakpoints.contains(&sys.cpu().pc()) {
            debug_mode.store(true, Ordering::Relaxed);
        }
        if debug_mode.load(Ordering::Relaxed) {
            sys.ser0_mut().handle_mut().tx.suspend_raw_mode().unwrap();
            loop {
                print!("dbg> ");
                sys.ser0_mut().handle_mut().tx.flush().unwrap();
                let mut line = Vec::new();
                // kind of jank, but reads are async, so we busy-wait
                loop {
                    let mut buf = [0];
                    if sys.ser0_mut().handle_mut().rx.read(&mut buf).unwrap() != 1 {
                        continue;
                    }
                    if buf[0] == 0x0A {
                        break;
                    }
                    line.push(buf[0]);
                }

                let line = String::from_utf8(line).unwrap();
                let parts = line.split_whitespace().collect::<Vec<&str>>();
                if !parts.is_empty() {
                    match parts[0] {
                        "q" => break,            // quit debugger
                        "Q" => break 'emu,       // quit emulator
                        "s" | "n" => sys.tick(), // single step
                        "r" => print_cpu_regs(sys.cpu()),
                        "R" => print_cpu_regs_base10(sys.cpu()),
                        "RR" => print_cpu_regs_signed_base10(sys.cpu()),
                        "b" => add_breakpoint(sys.cpu(), &mut breakpoints, parts.get(1).copied()),
                        "B" => {
                            remove_breakpoint(sys.cpu(), &mut breakpoints, parts.get(1).copied())
                        }
                        "x" => examine(sys.mem(), sys.cpu(), parts.get(1).copied()),
                        "X" => examine_base10(sys.mem(), sys.cpu(), parts.get(1).copied()),
                        "XX" => examine_signed_base10(sys.mem(), sys.cpu(), parts.get(1).copied()),
                        "?" => print_help(),
                        _ => println!("unknown command: `{}`. type `?` for help", parts[0]),
                    }
                }
            }
            // restore raw tty
            sys.ser0_mut().handle_mut().tx.activate_raw_mode().unwrap();
            debug_mode.store(false, Ordering::Relaxed);
        }

        sys.tick();
    }

    Ok(())
}

fn examine(mem: &Mem, cpu: &Cpu, start: Option<&str>) {
    let start = if let Some(arg) = start {
        match u16::from_str_radix(arg, 16) {
            Ok(addr) => addr,
            Err(e) => {
                println!("error parsing start address: {e}");
                return;
            }
        }
    } else {
        cpu.pc()
    };
    let end = ((start as u32) + 15).min(0xFFFF) as u16;
    print!("{start:04X}  ");
    for addr in start..=end {
        print!("{:02X} ", mem.read(addr));
    }
    print!(" |");
    for addr in start..=end {
        let c = mem.read(addr);
        if c.is_ascii_graphic() {
            print!("{}", c as char);
        } else {
            print!(".");
        }
    }
    println!("|");
}

fn examine_base10(mem: &Mem, cpu: &Cpu, start: Option<&str>) {
    let start = if let Some(arg) = start {
        match u16::from_str_radix(arg, 16) {
            Ok(addr) => addr,
            Err(e) => {
                println!("error parsing start address: {e}");
                return;
            }
        }
    } else {
        cpu.pc()
    };
    let end = ((start as u32) + 15).min(0xFFFF) as u16;
    print!("{start:05}  ");
    for addr in start..=end {
        print!("{:03} ", mem.read(addr));
    }
    print!(" |");
    for addr in start..=end {
        let c = mem.read(addr);
        if c.is_ascii_graphic() {
            print!("{}", c as char);
        } else {
            print!(".");
        }
    }
    println!("|");
}

fn examine_signed_base10(mem: &Mem, cpu: &Cpu, start: Option<&str>) {
    let start = if let Some(arg) = start {
        match u16::from_str_radix(arg, 16) {
            Ok(addr) => addr,
            Err(e) => {
                println!("error parsing start address: {e}");
                return;
            }
        }
    } else {
        cpu.pc()
    };
    let end = ((start as u32) + 15).min(0xFFFF) as u16;
    print!("{start:05}  ");
    for addr in start..=end {
        print!("{:+04} ", mem.read(addr) as i8);
    }
    print!(" |");
    for addr in start..=end {
        let c = mem.read(addr);
        if c.is_ascii_graphic() {
            print!("{}", c as char);
        } else {
            print!(".");
        }
    }
    println!("|");
}

fn add_breakpoint(cpu: &Cpu, breakpoints: &mut Vec<u16>, arg: Option<&str>) {
    let addr = if let Some(arg) = arg {
        match u16::from_str_radix(arg, 16) {
            Ok(addr) => addr,
            Err(e) => {
                println!("error parsing address: {e}");
                return;
            }
        }
    } else {
        cpu.pc()
    };
    if breakpoints.contains(&addr) {
        println!("breakpoint already exists");
    } else {
        breakpoints.push(addr);
        println!("breakpoint added at {addr:04X}");
    }
}

fn remove_breakpoint(cpu: &Cpu, breakpoints: &mut Vec<u16>, arg: Option<&str>) {
    let addr = if let Some(arg) = arg {
        match u16::from_str_radix(arg, 16) {
            Ok(addr) => addr,
            Err(e) => {
                println!("error parsing address: {e}");
                return;
            }
        }
    } else {
        cpu.pc()
    };
    if let Some(index) = breakpoints.iter().position(|&a| a == addr) {
        breakpoints.remove(index);
        println!("breakpoint removed at {addr:04X}");
    } else {
        println!("breakpoint does not exist");
    }
}

fn print_help() {
    println!("debugger commands:");
    println!("`q`: quit debugger, continuing emulator");
    println!("`Q`: quit emulator");
    println!("`s` or `n`: single step cpu");
    println!("`r`: print cpu registers");
    println!("`R`: print cpu registers (base 10)");
    println!("`RR`: print cpu registers (signed base 10)");
    println!("`b [addr]`: add breakpoint");
    println!("`B [addr]`: delete breakpoint");
    println!("`x [start]`: examine memory");
    println!("`X [start]`: examine memory (base 10)");
    println!("`XX [start]`: examine memory (signed base 10)");
    println!("`?`: show this help info");
}

fn print_cpu_regs(cpu: &Cpu) {
    print!(
        "A={:02X} B={:02X} X={:02X} Y={:02X} Z={:02X} PC={:04X} SP={:04X} ",
        cpu.a(),
        cpu.b(),
        cpu.x(),
        cpu.y(),
        cpu.z(),
        cpu.pc(),
        cpu.sp()
    );
    let p = cpu.p();
    print!("P={:02X} [", p);
    #[rustfmt::skip]
    {
        print!("{}", if (p & Flags::NEGATIVE) == 0 { "-" } else { "N" });
        print!("{}", if (p & Flags::OVERFLOW) == 0 { "-" } else { "V" });
        print!("{}", if (p & Flags::EXTEND_STACK_DISABLE) == 0 { "-" } else { "E" });
        print!("{}", if (p & Flags::BREAK) == 0 { "-" } else { "B" });
        print!("{}", if (p & Flags::DECIMAL_MODE) == 0 { "-" } else { "D" });
        print!("{}", if (p & Flags::INTERRUPT_DISABLE) == 0 { "-" } else { "I" });
        print!("{}", if (p & Flags::ZERO) == 0 { "-" } else { "Z" });
        print!("{}", if (p & Flags::CARRY) == 0 { "-" } else { "C" });
    };
    println!("]");
}

fn print_cpu_regs_base10(cpu: &Cpu) {
    print!(
        "A={:03} B={:03} X={:03} Y={:03} Z={:03} PC={:05} SP={:05} ",
        cpu.a(),
        cpu.b(),
        cpu.x(),
        cpu.y(),
        cpu.z(),
        cpu.pc(),
        cpu.sp()
    );
    let p = cpu.p();
    print!("P={:03} [", p);
    #[rustfmt::skip]
    {
        print!("{}", if (p & Flags::NEGATIVE) == 0 { "-" } else { "N" });
        print!("{}", if (p & Flags::OVERFLOW) == 0 { "-" } else { "V" });
        print!("{}", if (p & Flags::EXTEND_STACK_DISABLE) == 0 { "-" } else { "E" });
        print!("{}", if (p & Flags::BREAK) == 0 { "-" } else { "B" });
        print!("{}", if (p & Flags::DECIMAL_MODE) == 0 { "-" } else { "D" });
        print!("{}", if (p & Flags::INTERRUPT_DISABLE) == 0 { "-" } else { "I" });
        print!("{}", if (p & Flags::ZERO) == 0 { "-" } else { "Z" });
        print!("{}", if (p & Flags::CARRY) == 0 { "-" } else { "C" });
    };
    println!("]");
}

fn print_cpu_regs_signed_base10(cpu: &Cpu) {
    print!(
        "A={:+04} B={:+04} X={:+04} Y={:+04} Z={:+04} PC={:+06} SP={:+06} ",
        cpu.a() as i8,
        cpu.b() as i8,
        cpu.x() as i8,
        cpu.y() as i8,
        cpu.z() as i8,
        cpu.pc() as i16,
        cpu.sp() as i16
    );
    let p = cpu.p();
    print!("P={:+04} [", p as i8);
    #[rustfmt::skip]
    {
        print!("{}", if (p & Flags::NEGATIVE) == 0 { "-" } else { "N" });
        print!("{}", if (p & Flags::OVERFLOW) == 0 { "-" } else { "V" });
        print!("{}", if (p & Flags::EXTEND_STACK_DISABLE) == 0 { "-" } else { "E" });
        print!("{}", if (p & Flags::BREAK) == 0 { "-" } else { "B" });
        print!("{}", if (p & Flags::DECIMAL_MODE) == 0 { "-" } else { "D" });
        print!("{}", if (p & Flags::INTERRUPT_DISABLE) == 0 { "-" } else { "I" });
        print!("{}", if (p & Flags::ZERO) == 0 { "-" } else { "Z" });
        print!("{}", if (p & Flags::CARRY) == 0 { "-" } else { "C" });
    };
    println!("]");
}
