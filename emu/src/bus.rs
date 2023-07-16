pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;

    fn write(&mut self, addr: u16, data: u8);
}

pub trait BusDevice {
    fn reset<B: Bus>(&mut self, bus: &mut B);

    fn tick<B: Bus>(&mut self, bus: &mut B);

    #[allow(unused_variables)]
    fn read(&mut self, addr: u16) -> u8 {
        0
    }

    #[allow(unused_variables)]
    fn write(&mut self, addr: u16, data: u8) {}
}
