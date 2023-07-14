use std::{
    fs::File,
    io::{self, Read, Stdout, Write},
    path::PathBuf,
};

use clap::Parser;
use sys::System;
use termion::{
    raw::{IntoRawMode, RawTerminal},
    AsyncReader,
};
use tracing::Level;

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

struct Tty {
    tx: RawTerminal<Stdout>,
    rx: AsyncReader,
}

impl Tty {
    fn new() -> Self {
        let mut tx = io::stdout().into_raw_mode().unwrap();
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
            "ROM file is {} bytes, but it must be exactly 3840 bytes in length!",
            rom.len()
        );
        return Err(());
    }

    let mut sys = System::new(&rom, Tty::new(), NoopIo {});
    sys.reset();
    loop {
        sys.tick();
    }

    Ok(())
}
