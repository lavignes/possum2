pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;

    fn write(&mut self, addr: u16, data: u8);

    fn irq(&mut self);

    fn nmi(&mut self);
}

pub trait BusDevice {
    fn reset<B: Bus>(&mut self, bus: &mut B);

    fn irq(&mut self) {}

    fn nmi(&mut self) {}

    fn tick<B: Bus>(&mut self, bus: &mut B);

    #[allow(unused_variables)]
    fn read(&mut self, addr: u16) -> u8 {
        0
    }

    #[allow(unused_variables)]
    fn write(&mut self, addr: u16, data: u8) {}
}
