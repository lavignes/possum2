//! FD179X FDC Emulation

use std::{
    collections::VecDeque,
    io::{Read, Seek, SeekFrom, Write},
};

use crate::bus::{Bus, BusDevice};

const NUM_TRACKS: usize = 80;
const NUM_SECTORS: usize = 9;
const SECTOR_SIZE: usize = 512;

enum StatusFlags {}

impl StatusFlags {
    const BUSY: u8 = 1 << 0;

    const DATA_REQUEST: u8 = 1 << 1;
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

enum State {
    Idle,
    Restore,
    Seek,
    Step,
    ReadSector,
    WriteSector,
    ReadAddress,
    ReadTrack,
    WriteTrack,
}

pub struct Fdc<T> {
    handle: T,
    state: State,
    status: u8,
    command: u8,
    track: u8,
    sector: u8,
    data: u8,

    buf: VecDeque<u8>,
    track_latch: u8,
    track_target: u8,
    sector_count: u8,
    irq: bool,
}

impl<T> Fdc<T> {
    pub fn new(handle: T) -> Self {
        Self {
            handle,
            state: State::Idle,
            status: 0,
            command: 0,
            track: 0,
            sector: 0,
            data: 0,
            buf: VecDeque::with_capacity(SECTOR_SIZE),
            track_latch: 0,
            track_target: 0,
            sector_count: 0,
            irq: false,
        }
    }

    pub fn irq(&self) -> bool {
        self.irq
    }

    pub fn drq(&self) -> bool {
        (self.status & StatusFlags::DATA_REQUEST) != 0
    }
}

impl<T: Read + Write + Seek> BusDevice for Fdc<T> {
    fn reset<B: Bus>(&mut self, bus: &mut B) {
        self.state = State::Idle;
        self.status = 0;
        self.command = 0;
        self.track = 0;
        self.sector = 0;
        self.data = 0;
        self.buf.clear();
        self.track_latch = 0;
        self.track_target = 0;
        self.sector_count = 0;
        self.irq = false;
    }

    fn tick<B: Bus>(&mut self, bus: &mut B) {
        match self.state {
            State::Idle => {}

            State::Restore => {
                if self.track > 0 {
                    self.track -= 1;
                    self.track_latch = self.track;
                } else {
                    self.state = State::Idle;
                    self.status &= !StatusFlags::BUSY;
                    self.status |= StatusFlags::TRACK_0;
                    self.irq = true;
                }
            }

            State::Seek => {
                if self.data > self.track {
                    self.track -= 1;
                    self.track_latch = self.track;
                } else if self.data < self.track {
                    self.track += 1;
                    self.track_latch = self.track;
                } else {
                    self.status &= !StatusFlags::BUSY;
                    self.irq = true;
                }
                if self.track == 0 {
                    self.status |= StatusFlags::TRACK_0;
                }
            }

            State::Step => {
                self.state = State::Idle;
                self.status &= !StatusFlags::BUSY;
                self.irq = true;
                if self.track < self.track_target {
                    self.track += 1;
                } else if self.track > self.track_target {
                    self.track -= 1;
                }
                if (self.command & CommandFlags::UPDATE_TRACK) != 0 {
                    self.track_latch = self.track;
                }
                if self.track == 0 {
                    self.status |= StatusFlags::TRACK_0;
                }
            }

            State::ReadSector => {
                if self.buf.is_empty() {
                    if self.sector_count > 0 {
                        let side_offset = if (self.command & CommandFlags::SIDE_SELECT) == 0 {
                            0
                        } else {
                            SECTOR_SIZE * NUM_SECTORS * NUM_TRACKS
                        };
                        let track_offset = (self.track as usize) * SECTOR_SIZE * NUM_SECTORS;
                        let sector_offset = (self.sector as usize) * SECTOR_SIZE;
                        self.handle
                            .seek(SeekFrom::Start(
                                (side_offset + track_offset + sector_offset) as u64,
                            ))
                            .unwrap();
                        let mut buf = vec![0; SECTOR_SIZE];
                        self.handle.read_exact(&mut buf).unwrap();
                        self.buf.extend(buf.drain(..));
                        self.sector_count -= 1;
                        self.sector += 1;
                    } else {
                        self.state = State::Idle;
                        self.status &= !StatusFlags::BUSY;
                        self.irq = true;
                    }
                }
                if (self.status & StatusFlags::DATA_REQUEST) == 0 {
                    self.data = self.buf.pop_front().unwrap();
                    self.status |= StatusFlags::DATA_REQUEST;
                }
            }

            State::WriteSector => {
                if self.buf.len() == SECTOR_SIZE {
                    if self.sector_count > 0 {
                        let side_offset = if (self.command & CommandFlags::SIDE_SELECT) == 0 {
                            0
                        } else {
                            SECTOR_SIZE * NUM_SECTORS * NUM_TRACKS
                        };
                        let track_offset = (self.track as usize) * SECTOR_SIZE * NUM_SECTORS;
                        let sector_offset = (self.sector as usize) * SECTOR_SIZE;
                        self.handle
                            .seek(SeekFrom::Start(
                                (side_offset + track_offset + sector_offset) as u64,
                            ))
                            .unwrap();
                        let mut buf = Vec::with_capacity(SECTOR_SIZE);
                        buf.extend(self.buf.drain(..));
                        self.handle.write_all(&buf).unwrap();
                        self.handle.flush().unwrap();
                        self.sector_count -= 1;
                        self.sector += 1;
                    } else {
                        self.status &= !StatusFlags::BUSY;
                        self.irq = true;
                    }
                }
            }

            _ => unimplemented!(),
        }
    }

    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0 => self.status,
            1 => self.track_latch,
            2 => self.sector,
            3 => {
                self.status &= !StatusFlags::DATA_REQUEST;
                self.data
            }
            _ => unreachable!(),
        }
    }

    fn write(&mut self, addr: u16, data: u8) {
        match addr {
            0 => {
                self.command = data;
                match (data & 0b1110_0000) >> 5 {
                    0 => {
                        if (data & 0b0001_0000) == 0 {
                            self.state = State::Restore;
                        } else {
                            self.state = State::Seek;
                        }
                        self.status |= StatusFlags::BUSY;
                        if (data & CommandFlags::HEAD_LOAD) != 0 {
                            self.status |= StatusFlags::HEAD_LOADED;
                        } else {
                            self.status &= !StatusFlags::HEAD_LOADED;
                        }
                    }
                    1 => {
                        self.state = State::Step;
                        self.status |= StatusFlags::BUSY;
                        self.track_target = self.data;
                        if self.track_target > ((NUM_TRACKS - 1) as u8) {
                            self.track_target = (NUM_TRACKS - 1) as u8;
                        }
                        if (data & CommandFlags::HEAD_LOAD) != 0 {
                            self.status |= StatusFlags::HEAD_LOADED;
                        } else {
                            self.status &= !StatusFlags::HEAD_LOADED;
                        }
                    }
                    2 => {
                        self.state = State::Step;
                        self.status |= StatusFlags::BUSY;
                        self.track_target = self.track;
                        if self.track < ((NUM_SECTORS - 1) as u8) {
                            self.track_target += 1;
                        }
                        if (data & CommandFlags::HEAD_LOAD) != 0 {
                            self.status |= StatusFlags::HEAD_LOADED;
                        } else {
                            self.status &= !StatusFlags::HEAD_LOADED;
                        }
                    }
                    3 => {
                        self.state = State::Step;
                        self.status |= StatusFlags::BUSY;
                        self.track_target = self.track;
                        if self.track > 0 {
                            self.track_target -= 1;
                        }
                        if (data & CommandFlags::HEAD_LOAD) != 0 {
                            self.status |= StatusFlags::HEAD_LOADED;
                        } else {
                            self.status &= !StatusFlags::HEAD_LOADED;
                        }
                    }
                    4 => {
                        self.state = State::ReadSector;
                        self.status |= StatusFlags::BUSY | StatusFlags::HEAD_LOADED;
                        self.buf.clear();
                        if self.track_latch != self.track {
                            todo!("emit error when trying to read track that isnt under head");
                        }
                        if (data & CommandFlags::MULTIPLE_RECORD) != 0 {
                            self.sector_count = (NUM_SECTORS as u8) - self.sector;
                        } else {
                            self.sector_count = 1;
                        }
                    }
                    5 => {
                        self.state = State::WriteSector;
                        self.status |= StatusFlags::BUSY | StatusFlags::HEAD_LOADED;
                        self.buf.clear();
                        if self.track_latch != self.track {
                            todo!("emit error when trying to write track that isnt under head");
                        }
                        if (data & CommandFlags::MULTIPLE_RECORD) != 0 {
                            self.sector_count = (NUM_SECTORS as u8) - self.sector;
                        } else {
                            self.sector_count = 1;
                        }
                    }
                    6 => {
                        if (data & 0b0001_0000) == 0 {
                            self.state = State::ReadAddress;
                            todo!();
                        } else {
                            // Forcing Interrupt
                            todo!();
                        }
                    }
                    7 => todo!(),
                    _ => unreachable!(),
                }
            }

            1 => self.track_latch = data,

            2 => {
                if data > 8 {
                    todo!("implement errors of not finding correct track/sector number");
                }
                self.sector = data;
            }

            3 => {
                // push data into output buffer during write
                if matches!(self.state, State::WriteSector)
                    && ((self.status & StatusFlags::DATA_REQUEST) != 0)
                {
                    self.buf.push_back(data);
                }
                self.status &= !StatusFlags::DATA_REQUEST;
                self.data = data;
            }

            _ => unreachable!(),
        }
    }
}
