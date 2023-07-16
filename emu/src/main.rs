use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Stdout, Write},
    path::PathBuf,
};

use clap::Parser;
use memmap2::MmapMut;
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
            SeekFrom::End(offset) => {
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
            "ROM file is {} bytes, but it must be exactly 3840 bytes in length!",
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
    let fd0 = MemMap {
        inner: fd0,
        offset: 0,
    };

    let mut sys = System::new(&rom, Tty::new(), NoopIo {}, fd0, NoopIo {});
    sys.reset();
    loop {
        sys.tick();
    }

    Ok(())
}
