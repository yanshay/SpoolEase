// PN532 over I2C without IRQ, polls the ready byte with a delay so a shared bus isn't starved

use embassy_time::Timer;
use embedded_hal_async::i2c::{Error as _, ErrorKind, I2c};
use esp_hal::gpio::Output;

pub const PN532_I2C_ADDRESS: u8 = 0x24;
const PN532_I2C_READY: u8 = 0x01;

pub struct Pn532I2cInterface<I2C> {
    i2c: I2C,
    reset: Output<'static>, // RSTPDN, open drain
}

impl<I2C: I2c> Pn532I2cInterface<I2C> {
    pub fn new(i2c: I2C, reset: Output<'static>) -> Self {
        Self { i2c, reset }
    }

    pub async fn hard_reset(&mut self) {
        self.reset.set_low();
        Timer::after_millis(50).await;
        self.reset.set_high();
        Timer::after_millis(150).await; // PN532 boot time
    }
}

impl<I2C: I2c> pn532::Interface for Pn532I2cInterface<I2C> {
    type Error = I2C::Error;

    async fn write(&mut self, frame: &mut [u8]) -> Result<(), Self::Error> {
        self.i2c.write(PN532_I2C_ADDRESS, frame).await
    }

    async fn wait_ready(&mut self) -> Result<(), Self::Error> {
        // PN532 may NACK while busy (https://github.com/WMT-GmbH/pn532/issues/4),
        // other errors in a row mean it locked up the bus
        let mut bus_errors = 0;
        loop {
            let mut status = [0u8];
            match self.i2c.read(PN532_I2C_ADDRESS, &mut status).await {
                Ok(()) if status[0] == PN532_I2C_READY => return Ok(()),
                Ok(()) => bus_errors = 0,
                Err(e) if matches!(e.kind(), ErrorKind::NoAcknowledge(_)) => bus_errors = 0,
                Err(e) => {
                    bus_errors += 1;
                    if bus_errors >= 5 {
                        return Err(e);
                    }
                }
            }
            Timer::after_millis(1).await;
        }
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        // One read for ready byte and frame, PN532 hangs the bus if a read is split
        let mut frame = [0u8; 1 + 264];
        let frame = &mut frame[..1 + buf.len()];
        let r = self.i2c.read(PN532_I2C_ADDRESS, frame).await;
        if r.is_ok() {
            buf.copy_from_slice(&frame[1..]);
        }
        r
    }

    async fn wake_up(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
