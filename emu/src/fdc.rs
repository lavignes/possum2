//! FD179X FDC Emulation

use std::io::{Read, Seek, Write};

use crate::bus::BusDevice;

enum StatusFlags {}

impl StatusFlags {
    const BUSY: u8 = 1 << 0;

    const DRQ: u8 = 1 << 1;
    const INDEX: u8 = 1 << 1;

    const LOST_DATA: u8 = 1 << 2;
    const TRACK_0: u8 = 1 << 2;

    const CRC_ERROR: u8 = 1 << 3;

    const RECORD_NOT_FOUND: u8 = 1 << 4;
    const SEEK_ERROR: u8 = 1 << 4;

    const WRITE_FAULT: u8 = 1 << 5;
    const RECORD_TYPE: u8 = 1 << 5;
    const HEAD_LOADED: u8 = 1 << 5;

    const WRITE_PROTECT: u8 = 1 << 6;

    const NOT_READY: u8 = 1 << 7;
}

enum CommandFlags {}

impl CommandFlags {
    const STEPPING_MOTOR_RATE_MASK: u8 = 0b0000_0011;
    const VERIFY: u8 = 1 << 2;
    const HEAD_LOAD: u8 = 1 << 3;
    const UPDATE_TRACK: u8 = 1 << 4;

    const SIDE_COMPARE: u8 = 1 << 1;
    const DELAY: u8 = 1 << 2;
    const SIDE_SELECT: u8 = 1 << 3;
    const MULTIPLE_RECORD: u8 = 1 << 4;
    const DATA_ADDRESS_MARK: u8 = 1 << 0;

    const INTERRUPT_NOT_READY_TO_READY: u8 = 1 << 0;
    const INTERRUPT_READY_TO_NOT_READY: u8 = 1 << 1;
    const INTERRUPT_INDEX_PULSE: u8 = 1 << 2;
    const INTERRUPT_IMMEDIATE: u8 = 1 << 3;
}

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
