//! FD179X FDC Emulation

use std::io::{Read, Seek, Write};

use crate::bus::BusDevice;

enum StatusFlags {}

enum CommandFlags {}

pub struct Fdc<T> {
    handle: T,
    status: u8,
    command: u8,
    track: u8,
    sector: u8,
    buffer: Vec<u8>,
    buffer_offset: usize,
}

impl<T> Fdc<T> {
    pub fn new(handle: T) -> Self {
        Self {
            handle,
            status: 0,
            command: 0,
            track: 0,
            sector: 0,
            buffer: Vec::new(),
            buffer_offset: 0,
        }
    }
}

impl<T: Read + Write + Seek> BusDevice for Fdc<T> {
    fn reset<B: crate::bus::Bus>(&mut self, bus: &mut B) {
        todo!()
    }

    fn tick<B: crate::bus::Bus>(&mut self, bus: &mut B) {
        todo!()
    }

    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0 => self.status,
            1 => self.track,
            2 => self.sector,
            3 => {
                // TODO: not correct! need to handle the buffer size
                let data = self.buffer[self.buffer_offset];
                self.buffer_offset += 1;
                data
            }
            _ => unreachable!(),
        }
    }

    fn write(&mut self, addr: u16, data: u8) {
        match addr {
            0 => self.command = data,
            1 => self.track = data,
            2 => self.sector = data,
            3 => {
                // TODO: not correct! need to handle the buffer size
                self.buffer[self.buffer_offset] = data;
                self.buffer_offset += 1;
            }
            _ => unreachable!(),
        }
    }
}
