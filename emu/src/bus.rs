pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;

    fn write(&mut self, addr: u16, data: u8);
}

pub trait BusDevice {
    fn reset<B: Bus>(&mut self, bus: &mut B);

    fn irq<B: Bus>(&mut self, bus: &mut B);

    fn nmi<B: Bus>(&mut self, bus: &mut B);

    fn tick<B: Bus>(&mut self, bus: &mut B);
}
