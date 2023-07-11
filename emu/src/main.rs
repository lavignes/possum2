use std::io::{self, Read, Stdout, Write};

use sys::System;
use termion::AsyncReader;

mod bus;
mod cpu;
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
    rx: AsyncReader,
    tx: Stdout,
}

impl Tty {
    fn new() -> Self {
        Self {
            rx: termion::async_stdin(),
            tx: io::stdout(),
        }
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

fn main() {
    let mut sys = System::new(Tty::new(), NoopIo {});
    sys.reset();
    loop {
        sys.tick();
    }
}
