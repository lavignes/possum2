use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Stdout, Write},
    num::ParseIntError,
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
    color::{Fg, LightBlue, LightMagenta, LightRed, LightYellow, Reset},
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

    /// Start with debugger enabled
    #[arg(short, long)]
    debug: bool,

    /// Debugger symbol file
    #[arg(short, long)]
    sym: Option<PathBuf>,
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
    if fd0.len() != 0xA0000 {
        tracing::error!(
            "FD0 file is {} bytes, but it must be exactly 655360 bytes (640KiB) in length!",
            fd0.len()
        );
        return Err(());
    }
    let fd0 = MemMap {
        inner: fd0,
        offset: 0,
    };

    let debug_mode = Arc::new(AtomicBool::new(args.debug));
    flag::register(consts::SIGUSR1, debug_mode.clone())
        .map_err(|e| {
            tracing::warn!("external debugger unavailable: failed to install SIGUSR1 handler: {e}")
        })
        .ok();

    let mut symbols = HashMap::<u16, Vec<String>>::new();
    if let Some(sym) = args.sym {
        let sym_file =
            File::open(&sym).map_err(|e| tracing::error!("failed to open SYM file: {e}"))?;
        for (line_no, line_result) in BufReader::new(sym_file).lines().enumerate() {
            let line = line_result.map_err(|e| tracing::error!("failed to read SYM file: {e}"))?;
            let (label, addr) = line
                .split_once(':')
                .ok_or_else(|| format!("{}:{line_no}: malformed entry", sym.display()))
                .map_err(|e| tracing::error!("failed to parse SYM file: {e}"))?;
            let addr = u16::from_str_radix(addr, 16).map_err(|e| {
                tracing::error!("failed to parse SYM file: {}:{line_no}: {e}", sym.display())
            })?;
            match symbols.get_mut(&addr) {
                Some(labels) => labels.push(label.to_string()),
                None => {
                    symbols.insert(addr, vec![label.to_string()]);
                }
            }
        }
    }

    let mut breakpoints = Vec::new();
    let mut sys = System::new(&rom, Tty::new(), NoopIo {}, fd0, NoopIo {});
    sys.reset();

    'emu: loop {
        if breakpoints.contains(&sys.cpu().pc()) {
            debug_mode.store(true, Ordering::Relaxed);
        }
        if debug_mode.load(Ordering::Relaxed) {
            sys.ser0_mut().handle_mut().tx.suspend_raw_mode().unwrap();
            dissasemble(sys.mem(), sys.cpu(), &symbols, None, 1);
            let mut cached_parts = Vec::new();
            loop {
                print!("dbg>");
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
                let parts = line
                    .split_whitespace()
                    .map(String::from)
                    .collect::<Vec<String>>();
                let parts = if parts.is_empty() {
                    cached_parts.clone()
                } else {
                    cached_parts = parts.clone();
                    parts
                };
                if !parts.is_empty() {
                    let arg = parts.get(1).map(String::as_str);
                    match parts[0].as_str() {
                        "c" => break,      // continue emulator
                        "q" => break 'emu, // quit emulator
                        "s" | "n" => {
                            // single step
                            sys.tick();
                            dissasemble(sys.mem(), sys.cpu(), &symbols, None, 1);
                        }
                        "r" => print_cpu_regs(sys.cpu()),
                        "R" => print_cpu_regs_base10(sys.cpu()),
                        "RR" => print_cpu_regs_signed_base10(sys.cpu()),
                        "b" => add_breakpoint(sys.cpu(), &mut breakpoints, &symbols, arg),
                        "B" => remove_breakpoint(sys.cpu(), &mut breakpoints, &symbols, arg),
                        "x" => examine(sys.mem(), sys.cpu(), &symbols, arg),
                        "X" => examine_base10(sys.mem(), sys.cpu(), &symbols, arg),
                        "XX" => examine_signed_base10(sys.mem(), sys.cpu(), &symbols, arg),
                        "d" => dissasemble(sys.mem(), sys.cpu(), &symbols, arg, 24),
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

fn examine(mem: &Mem, cpu: &Cpu, symbols: &HashMap<u16, Vec<String>>, start: Option<&str>) {
    let start = if let Some(arg) = start {
        match parse_addr(symbols, arg) {
            Ok(addr) => addr,
            Err(e) => {
                println!("error parsing start address: {e}");
                return;
            }
        }
    } else {
        cpu.pc()
    };
    let end = ((start as u32) + 16).min(0xFFFF) as u16;
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

fn examine_base10(mem: &Mem, cpu: &Cpu, symbols: &HashMap<u16, Vec<String>>, start: Option<&str>) {
    let start = if let Some(arg) = start {
        match parse_addr(symbols, arg) {
            Ok(addr) => addr,
            Err(e) => {
                println!("error parsing start address: {e}");
                return;
            }
        }
    } else {
        cpu.pc()
    };
    let end = ((start as u32) + 16).min(0xFFFF) as u16;
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

fn examine_signed_base10(
    mem: &Mem,
    cpu: &Cpu,
    symbols: &HashMap<u16, Vec<String>>,
    start: Option<&str>,
) {
    let start = if let Some(arg) = start {
        match parse_addr(symbols, arg) {
            Ok(addr) => addr,
            Err(e) => {
                println!("error parsing start address: {e}");
                return;
            }
        }
    } else {
        cpu.pc()
    };
    let end = ((start as u32) + 24).min(0xFFFF) as u16;
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

fn add_breakpoint(
    cpu: &Cpu,
    breakpoints: &mut Vec<u16>,
    symbols: &HashMap<u16, Vec<String>>,
    arg: Option<&str>,
) {
    let addr = if let Some(arg) = arg {
        match parse_addr(symbols, arg) {
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

fn remove_breakpoint(
    cpu: &Cpu,
    breakpoints: &mut Vec<u16>,
    symbols: &HashMap<u16, Vec<String>>,
    arg: Option<&str>,
) {
    let addr = if let Some(arg) = arg {
        match parse_addr(symbols, arg) {
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
    println!("`c`: continue emulator (exiting debugger)");
    println!("`q`: quit emulator");
    println!("`s` or `n`: single step cpu");
    println!("`r`: print cpu registers");
    println!("`R`: print cpu registers (base 10)");
    println!("`RR`: print cpu registers (signed base 10)");
    println!("`b [addr]`: add breakpoint");
    println!("`B [addr]`: delete breakpoint");
    println!("`x [start]`: examine memory");
    println!("`X [start]`: examine memory (base 10)");
    println!("`XX [start]`: examine memory (signed base 10)");
    println!("`d [start]`: disassemble memory");
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

fn dissasemble(
    mem: &Mem,
    cpu: &Cpu,
    symbols: &HashMap<u16, Vec<String>>,
    start: Option<&str>,
    count: usize,
) {
    let mut addr = if let Some(arg) = start {
        match parse_addr(symbols, arg) {
            Ok(addr) => addr,
            Err(e) => {
                println!("error parsing start address: {e}");
                return;
            }
        }
    } else {
        cpu.pc()
    };
    for _ in 0..count {
        if let Some(labels) = symbols.get(&addr) {
            println!("{};  {}:{}  ", Fg(LightBlue), labels[0], Fg(Reset));
        }
        let bank = mem.bank(addr);
        let byte = mem.read(addr);
        print!(
            "{bank}:{}{addr:04X}  {}{byte:02X}",
            Fg(LightYellow),
            Fg(Reset)
        );
        addr += 1;
        let (name, mode) = find_op(byte).unwrap();
        match mode {
            IMM => {
                let byte = mem.read(addr);
                addr += 1;
                print!(" {byte:02X}      ");
                print!(
                    "  {}{name} {}#{}${byte:02X}{}               ",
                    Fg(LightMagenta),
                    Fg(Reset),
                    Fg(LightRed),
                    Fg(Reset),
                );
            }

            ABS => {
                let lo = mem.read(addr);
                addr += 1;
                let hi = mem.read(addr);
                addr += 1;
                print!(" {lo:02X} {hi:02X}   ");
                print!(
                    "  {}{name} {}${hi:02X}{lo:02X}{}          ",
                    Fg(LightMagenta),
                    Fg(LightRed),
                    Fg(Reset),
                );
                let addr = ((hi as u16) << 8) | (lo as u16);
                if let Some(labels) = symbols.get(&addr) {
                    print!("  {}; {}{}", Fg(LightBlue), labels[0], Fg(Reset));
                }
            }

            B => {
                let byte = mem.read(addr);
                addr += 1;
                print!(" {byte:02X}      ");
                print!(
                    "  {}{name} {}${byte:02X}{}                ",
                    Fg(LightMagenta),
                    Fg(LightRed),
                    Fg(Reset),
                );
            }

            ACCUM => {
                print!("         ");
                print!(
                    "  {}{name} A{}                          ",
                    Fg(LightMagenta),
                    Fg(Reset)
                );
            }

            IMPL if name == "AUG" => {
                let lo = mem.read(addr);
                addr += 1;
                let mid = mem.read(addr);
                addr += 1;
                let hi = mem.read(addr);
                addr += 1;
                print!(" {lo:02X} {mid:02X} {hi:02X}");
                print!(
                    "  {}{name} {}${hi:02X}${mid:02X}{lo:02X}{}",
                    Fg(LightMagenta),
                    Fg(LightRed),
                    Fg(Reset)
                );
            }

            IMPL if name == "BRK" => {
                let byte = mem.read(addr);
                addr += 1;
                print!(" {byte:02X}      ");
                print!(
                    "  {}{name} {}#{}${byte:02X}{}               ",
                    Fg(LightMagenta),
                    Fg(Reset),
                    Fg(LightRed),
                    Fg(Reset)
                );
            }

            IMPL if name == "RTN" => {
                let byte = mem.read(addr);
                addr += 1;
                print!(" {byte:02X}      ");
                print!(
                    "  {}{name} {}#{}${byte:02X}{}               ",
                    Fg(LightMagenta),
                    Fg(Reset),
                    Fg(LightRed),
                    Fg(Reset)
                );
            }

            IMPL => {
                print!("         ");
                print!(
                    "  {}{name}{}                            ",
                    Fg(LightMagenta),
                    Fg(Reset)
                );
            }

            IND_X => {
                let byte = mem.read(addr);
                addr += 1;
                print!(" {byte:02X}      ");
                print!(
                    "  {}{name} {}({}${byte:02X}{},{}X{})            ",
                    Fg(LightMagenta),
                    Fg(Reset),
                    Fg(LightRed),
                    Fg(Reset),
                    Fg(LightMagenta),
                    Fg(Reset),
                );
            }

            IND_Y => {
                let byte = mem.read(addr);
                addr += 1;
                print!(" {byte:02X}      ");
                print!(
                    "  {}{name} {}({}${byte:02X}{}),{}Y{}            ",
                    Fg(LightMagenta),
                    Fg(Reset),
                    Fg(LightRed),
                    Fg(Reset),
                    Fg(LightMagenta),
                    Fg(Reset),
                );
            }

            IND_Z => {
                let byte = mem.read(addr);
                addr += 1;
                print!(" {byte:02X}      ");
                print!(
                    "  {}{name} {}({}${byte:02X}{}),{}Z{}            ",
                    Fg(LightMagenta),
                    Fg(Reset),
                    Fg(LightRed),
                    Fg(Reset),
                    Fg(LightMagenta),
                    Fg(Reset),
                );
            }

            IND_SP => {
                let byte = mem.read(addr);
                addr += 1;
                print!(" {byte:02X}      ");
                print!(
                    "  {}{name} {}({}${byte:02X}{},{}SP{}),{}Y{}         ",
                    Fg(LightMagenta),
                    Fg(Reset),
                    Fg(LightRed),
                    Fg(Reset),
                    Fg(LightMagenta),
                    Fg(Reset),
                    Fg(LightMagenta),
                    Fg(Reset),
                );
            }

            B_X => {
                let byte = mem.read(addr);
                addr += 1;
                print!(" {byte:02X}      ");
                print!(
                    "  {}{name} {}${byte:02X}{},{}X{}              ",
                    Fg(LightMagenta),
                    Fg(LightRed),
                    Fg(Reset),
                    Fg(LightMagenta),
                    Fg(Reset),
                );
            }

            B_Y => {
                let byte = mem.read(addr);
                addr += 1;
                print!(" {byte:02X}      ");
                print!("  {name} ${byte:02X},Y              ");
            }

            ABS_X => {
                let lo = mem.read(addr);
                addr += 1;
                let hi = mem.read(addr);
                addr += 1;
                print!(" {lo:02X} {hi:02X}   ");
                print!(
                    "  {}{name} {}${hi:02X}{lo:02X}{},{}X{}        ",
                    Fg(LightMagenta),
                    Fg(LightRed),
                    Fg(Reset),
                    Fg(LightMagenta),
                    Fg(Reset)
                );
                let addr = ((hi as u16) << 8) | (lo as u16);
                if let Some(labels) = symbols.get(&addr) {
                    print!("  {}; {}{}", Fg(LightBlue), labels[0], Fg(Reset));
                }
            }

            ABS_Y => {
                let lo = mem.read(addr);
                addr += 1;
                let hi = mem.read(addr);
                addr += 1;
                print!(" {lo:02X} {hi:02X}   ");
                print!(
                    "  {}{name} {}${hi:02X}{lo:02X}{},{}Y{}        ",
                    Fg(LightMagenta),
                    Fg(LightRed),
                    Fg(Reset),
                    Fg(LightMagenta),
                    Fg(Reset)
                );
                let addr = ((hi as u16) << 8) | (lo as u16);
                if let Some(labels) = symbols.get(&addr) {
                    print!("  ; {}", labels[0]);
                }
            }

            REL => {
                let byte = mem.read(addr);
                addr += 1;
                print!(" {byte:02X}      ");
                print!(
                    "  {}{name} {}${byte:02X}{}            ",
                    Fg(LightMagenta),
                    Fg(LightRed),
                    Fg(Reset)
                );
                let addr = addr.wrapping_add_signed((byte as i8) as i16);
                if let Some(labels) = symbols.get(&addr) {
                    print!("  {}; {}{}", Fg(LightBlue), labels[0], Fg(Reset));
                } else {
                    print!("  {}; {addr:04X}{}", Fg(LightBlue), Fg(Reset));
                }
            }

            WREL => {
                let lo = mem.read(addr);
                addr += 1;
                let hi = mem.read(addr);
                addr += 1;
                print!(" {lo:02X} {hi:02X}   ");
                print!(
                    "  {}{name} {}${hi:02X}{lo:02X}{}          ",
                    Fg(LightMagenta),
                    Fg(LightRed),
                    Fg(Reset)
                );
                let addr = addr.wrapping_add_signed((((hi as u16) << 8) | (lo as u16)) as i16);
                if let Some(labels) = symbols.get(&addr) {
                    print!("  {}; {}{}", Fg(LightBlue), labels[0], Fg(Reset));
                } else {
                    print!("  {}; {addr:04X}{}", Fg(LightBlue), Fg(Reset));
                }
            }

            IND_ABS => {
                let lo = mem.read(addr);
                addr += 1;
                let hi = mem.read(addr);
                addr += 1;
                print!(" {lo:02X} {hi:02X}   ");
                print!(
                    "  {}{name} {}({}${hi:02X}{lo:02X}{})        ",
                    Fg(LightMagenta),
                    Fg(Reset),
                    Fg(LightRed),
                    Fg(Reset)
                );
                let addr = ((hi as u16) << 8) | (lo as u16);
                if let Some(labels) = symbols.get(&addr) {
                    print!("  {}; {}{}", Fg(LightBlue), labels[0], Fg(Reset));
                }
            }

            B_REL => {
                let lo = mem.read(addr);
                addr += 1;
                let hi = mem.read(addr);
                addr += 1;
                print!(" {lo:02X} {hi:02X}   ");
                print!(
                    "  {}{name} {}${hi:02X}{},{}${lo:02X}{}        ",
                    Fg(LightMagenta),
                    Fg(LightRed),
                    Fg(Reset),
                    Fg(LightRed),
                    Fg(Reset)
                );
                let addr = ((hi as u16) << 8) | (lo as u16);
                if let Some(labels) = symbols.get(&addr) {
                    print!("  {}; {}{}", Fg(LightBlue), labels[0], Fg(Reset));
                }
            }

            IND_ABS_X => {
                let lo = mem.read(addr);
                addr += 1;
                let hi = mem.read(addr);
                addr += 1;
                print!(" {lo:02X} {hi:02X}   ");
                print!(
                    "  {}{name} {}({}${hi:02X}{lo:02X}{},{}X{})      ",
                    Fg(LightMagenta),
                    Fg(Reset),
                    Fg(LightRed),
                    Fg(Reset),
                    Fg(LightMagenta),
                    Fg(Reset)
                );
                let addr = ((hi as u16) << 8) | (lo as u16);
                if let Some(labels) = symbols.get(&addr) {
                    print!("  {}; {}{}", Fg(LightBlue), labels[0], Fg(Reset));
                }
            }
            _ => unreachable!(),
        }
        println!();
    }
}

fn parse_addr(symbols: &HashMap<u16, Vec<String>>, arg: &str) -> Result<u16, ParseIntError> {
    match u16::from_str_radix(arg, 16) {
        Ok(addr) => Ok(addr),
        Err(e) => {
            for (addr, labels) in symbols {
                for label in labels {
                    if label == arg {
                        return Ok(*addr);
                    }
                }
            }
            Err(e)
        }
    }
}

fn find_op(byte: u8) -> Option<(&'static str, u8)> {
    for (op, modes) in OPS {
        for (mode, opcode) in *modes {
            if *opcode == byte {
                return Some((op, *mode));
            }
        }
    }
    None
}

const IMM: u8 = 0;
const ABS: u8 = 1;
const B: u8 = 2;
const ACCUM: u8 = 3;
const IMPL: u8 = 4;
const IND_X: u8 = 5; // (B,X)
const IND_Y: u8 = 6; // (B),Y
const IND_Z: u8 = 7; // (B),Z
const IND_SP: u8 = 8; // (d,SP),Y
const B_X: u8 = 9; // B,X
const B_Y: u8 = 10; // B,Y
const ABS_X: u8 = 11;
const ABS_Y: u8 = 12;
const REL: u8 = 13;
const WREL: u8 = 14;
const IND_ABS: u8 = 15; // (ABS)
const B_REL: u8 = 16;
const IND_ABS_X: u8 = 17; // (ABS,X)

type Op = (&'static str, &'static [(u8, u8)]);

#[rustfmt::skip]
const OPS: &[Op] = &[
    ("AUG", &[(IMPL, 0x5C)]), // special
    ("BRK", &[(IMPL, 0x00)]), // special
    ("CLC", &[(IMPL, 0x18)]),
    ("CLD", &[(IMPL, 0xD8)]),
    ("CLE", &[(IMPL, 0x02)]),
    ("CLI", &[(IMPL, 0x58)]),
    ("CLV", &[(IMPL, 0xB8)]),
    ("DEX", &[(IMPL, 0xCA)]),
    ("DEY", &[(IMPL, 0x88)]),
    ("DEZ", &[(IMPL, 0x3B)]),
    ("INX", &[(IMPL, 0xE8)]),
    ("INY", &[(IMPL, 0xC8)]),
    ("INZ", &[(IMPL, 0x1B)]),
    ("NOP", &[(IMPL, 0xEA)]),
    ("PHA", &[(IMPL, 0x48)]),
    ("PHP", &[(IMPL, 0x08)]),
    ("PHX", &[(IMPL, 0xDA)]),
    ("PHY", &[(IMPL, 0x5A)]),
    ("PHZ", &[(IMPL, 0xDB)]),
    ("PLA", &[(IMPL, 0x68)]),
    ("PLP", &[(IMPL, 0x28)]),
    ("PLX", &[(IMPL, 0xFA)]),
    ("PLY", &[(IMPL, 0x7A)]),
    ("PLZ", &[(IMPL, 0xFB)]),
    ("RTI", &[(IMPL, 0x40)]),
    ("RTN", &[(IMPL, 0x62)]), // special
    ("RTS", &[(IMPL, 0x60)]),
    ("SEC", &[(IMPL, 0x38)]),
    ("SED", &[(IMPL, 0xF8)]),
    ("SEE", &[(IMPL, 0x03)]),
    ("SEI", &[(IMPL, 0x78)]),
    ("TAB", &[(IMPL, 0x5B)]),
    ("TAX", &[(IMPL, 0xAA)]),
    ("TAY", &[(IMPL, 0xA8)]),
    ("TBA", &[(IMPL, 0x7B)]),
    ("TSX", &[(IMPL, 0xBA)]),
    ("TSY", &[(IMPL, 0x0B)]),
    ("TXA", &[(IMPL, 0x8A)]),
    ("TXS", &[(IMPL, 0x9A)]),
    ("TYA", &[(IMPL, 0x98)]),
    ("TYS", &[(IMPL, 0x2B)]),
    ("TZA", &[(IMPL, 0x6B)]),

    ("ADC", &[(IMM, 0x69), (ABS, 0x6D), (B, 0x65), (IND_X, 0x61), (IND_Y, 0x71), (IND_Z, 0x72), (B_X, 0x75), (ABS_X, 0x7D), (ABS_Y, 0x79)]),
    ("AND", &[(IMM, 0x29), (ABS, 0x2D), (B, 0x25), (IND_X, 0x21), (IND_Y, 0x31), (IND_Z, 0x32), (B_X, 0x35), (ABS_X, 0x3D), (ABS_Y, 0x39)]),
    ("ASL", &[(ABS, 0x0E), (B, 0x06), (ACCUM, 0x0A), (B_X, 0x16), (ABS_X, 0x1E)]),
    ("ASR", &[(B, 0x44), (ACCUM, 0x43), (B_X, 0x54)]),
    ("ASW", &[(ABS, 0xCB)]),
    ("BIT", &[(IMM, 0x89), (ABS, 0x2C), (B, 0x24), (B_X, 0x34), (ABS_X, 0x3C)]),
    ("BBR", &[(B_REL, 0x0F), (B_REL, 0x1F), (B_REL, 0x2F), (B_REL, 0x3F), (B_REL, 0x4F), (B_REL, 0x5F), (B_REL, 0x6F), (B_REL, 0x7F)]), // special
    ("BBS", &[(B_REL, 0x8F), (B_REL, 0x9F), (B_REL, 0xAF), (B_REL, 0xBF), (B_REL, 0xCF), (B_REL, 0xDF), (B_REL, 0xEF), (B_REL, 0xFF)]), // special
    ("BCC", &[(REL, 0x90), (WREL, 0x93)]),
    ("BCS", &[(REL, 0xB0), (WREL, 0xB3)]),
    ("BEQ", &[(REL, 0xF0), (WREL, 0xF3)]),
    ("BMI", &[(REL, 0x30), (WREL, 0x33)]),
    ("BNE", &[(REL, 0xD0), (WREL, 0xD3)]),
    ("BPL", &[(REL, 0x10), (WREL, 0x13)]),
    ("BRU", &[(REL, 0x80), (WREL, 0x83)]),
    ("BSR", &[(WREL, 0x63)]),
    ("BVC", &[(REL, 0x50), (WREL, 0x53)]),
    ("BVS", &[(REL, 0x70), (WREL, 0x73)]),
    ("CMP", &[(IMM, 0xC9), (ABS, 0xCD), (B, 0xC5), (IND_X, 0xC1), (IND_Y, 0xD1), (IND_Z, 0xD2), (B_X, 0xD5), (ABS_X, 0xDD), (ABS_Y, 0xD9)]),
    ("CPX", &[(IMM, 0xE0), (ABS, 0xEC), (B, 0xE4)]),
    ("CPY", &[(IMM, 0xC0), (ABS, 0xCC), (B, 0xC4)]),
    ("CPZ", &[(IMM, 0xC2), (ABS, 0xDC), (B, 0xD4)]),
    ("DEC", &[(ABS, 0xCE), (B, 0xC6), (ACCUM, 0x3A), (B_X, 0xD6), (ABS_X, 0xDE)]),
    ("EOR", &[(IMM, 0x49), (ABS, 0x4D), (B, 0x45), (IND_X, 0x41), (IND_Y, 0x51), (IND_Z, 0x52), (B_X, 0x55), (ABS_X, 0x5D), (ABS_Y, 0x59)]),
    ("INC", &[(ABS, 0xEE), (B, 0xE6), (ACCUM, 0x1A), (B_X, 0xF6), (ABS_X, 0xFE)]),
    ("INW", &[(B, 0xE3)]),
    ("JMP", &[(ABS, 0x4C), (IND_ABS, 0x6C), (IND_ABS_X, 0x7C)]),
    ("JSR", &[(ABS, 0x20), (IND_ABS, 0x22), (IND_ABS_X, 0x23)]),
    ("LDA", &[(IMM, 0xA9), (ABS, 0xAD), (B, 0xA5), (IND_X, 0xA1), (IND_Y, 0xB1), (IND_Z, 0xB2), (IND_SP, 0xE2), (B_X, 0xB5), (ABS_X, 0xBD), (ABS_Y, 0xB9)]),
    ("LDX", &[(IMM, 0xA2), (ABS, 0xAE), (B, 0xA6), (B_Y, 0xB6), (ABS_Y, 0xBE)]),
    ("LDY", &[(IMM, 0xA0), (ABS, 0xAC), (B, 0xA4), (B_X, 0xB4), (ABS_X, 0xBC)]),
    ("LDZ", &[(IMM, 0xA3), (ABS, 0xAB), (ABS_X, 0xBB)]),
    ("LSR", &[(ABS, 0x4E), (B, 0x46), (ACCUM, 0x4A), (B_X, 0x56), (ABS_X, 0x5E)]),
    ("NEG", &[(ACCUM, 0x42)]),
    ("ORA", &[(IMM, 0x09), (ABS, 0x0D), (B, 0x05), (IND_X, 0x01), (IND_Y, 0x11), (IND_Z, 0x12), (B_X, 0x15), (ABS_X, 0x1D), (ABS_Y, 0x19)]),
    ("RMB", &[(B, 0x07), (B, 0x17), (B, 0x27), (B, 0x37), (B, 0x47), (B, 0x57), (B, 0x67), (B, 0x77)]), // special
    ("ROL", &[(ABS, 0x2E), (B, 0x26), (ACCUM, 0x2A), (B_X, 0x36), (ABS_X, 0x3E)]),
    ("ROR", &[(ABS, 0x6E), (B, 0x66), (ACCUM, 0x6A), (B_X, 0x76), (ABS_X, 0x7E)]),
    ("ROW", &[(ABS, 0xEB)]),
    ("SBC", &[(IMM, 0xE9), (ABS, 0xED), (B, 0xE5), (IND_X, 0xE1), (IND_Y, 0xF1), (IND_Z, 0xF2), (B_X, 0xF5), (ABS_X, 0xFD), (ABS_Y, 0xF9)]),
    ("SMB", &[(B, 0x87), (B, 0x97), (B, 0xA7), (B, 0xB7), (B, 0xC7), (B, 0xD7), (B, 0xE7), (B, 0xF7)]), // special
    ("STA", &[(ABS, 0x8D), (B, 0x85), (IND_X, 0x81), (IND_Y, 0x91), (IND_Z, 0x92), (IND_SP, 0x82), (B_X, 0x95), (ABS_X, 0x9D), (ABS_Y, 0x99)]),
    ("STX", &[(ABS, 0x8E), (B, 0x86), (ABS_Y, 0x96), (ABS_Y, 0x9B)]),
    ("STY", &[(ABS, 0x8C), (B, 0x84), (ABS_X, 0x94), (ABS_X, 0x8B)]),
    ("STZ", &[(ABS, 0x9C), (B, 0x64), (ABS_X, 0x74), (ABS_X, 0x9E)]),
    ("TRB", &[(ABS, 0x1C), (B, 0x14)]), // xfer reset bits, M[addr] &= ~A
    ("TSB", &[(ABS, 0x0C), (B, 0x04)]), // xfer set bits, M[addr] |= A
];
