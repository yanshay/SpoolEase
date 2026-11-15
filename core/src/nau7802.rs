// NAU7802 load cell ADC (I2C): LDO 3.0V, gain 128, 10 SPS

use embassy_time::{Duration, Instant, Timer};
use embedded_hal_async::i2c::I2c;

pub const NAU7802_I2C_ADDRESS: u8 = 0x2A;

const REG_PU_CTRL: u8 = 0x00;
const REG_CTRL1: u8 = 0x01;
const REG_CTRL2: u8 = 0x02;
const REG_ADCO_B2: u8 = 0x12;
const REG_ADC: u8 = 0x15;
const REG_PGA_PWR: u8 = 0x1C;
const REG_REVISION: u8 = 0x1F;

const PU_CTRL_RR: u8 = 1 << 0;
const PU_CTRL_PUD: u8 = 1 << 1;
const PU_CTRL_PUA: u8 = 1 << 2;
const PU_CTRL_PUR: u8 = 1 << 3;
const PU_CTRL_CS: u8 = 1 << 4;
const PU_CTRL_CR: u8 = 1 << 5;
const PU_CTRL_AVDDS: u8 = 1 << 7;

const CTRL1_VLDO_3V0: u8 = 0b101 << 3;
const CTRL1_GAIN_128: u8 = 0b111;

const CTRL2_CRS_MASK: u8 = 0b111 << 4; // 000 = 10 SPS
const CTRL2_CAL_ERR: u8 = 1 << 3;
const CTRL2_CALS: u8 = 1 << 2;
const CTRL2_CALMOD_MASK: u8 = 0b11; // 00 = internal offset

const ADC_CHPS_OFF: u8 = 0b11 << 4; // chopper off
const PGA_PWR_CAP_EN: u8 = 1 << 7; // PGA output cap

#[derive(Debug)]
pub enum Error<E> {
    I2c(E),
    PowerUpTimeout,
    CalibrationFailed,
}

impl<E> From<E> for Error<E> {
    fn from(e: E) -> Self {
        Error::I2c(e)
    }
}

pub struct Nau7802<I2C> {
    i2c: I2C,
}

impl<I2C: I2c> Nau7802<I2C> {
    pub fn new(i2c: I2C) -> Self {
        Self { i2c }
    }

    async fn read_reg(&mut self, reg: u8) -> Result<u8, I2C::Error> {
        let mut buf = [0u8];
        self.i2c.write_read(NAU7802_I2C_ADDRESS, &[reg], &mut buf).await?;
        Ok(buf[0])
    }

    async fn write_reg(&mut self, reg: u8, value: u8) -> Result<(), I2C::Error> {
        self.i2c.write(NAU7802_I2C_ADDRESS, &[reg, value]).await
    }

    async fn update_reg(&mut self, reg: u8, mask: u8, value: u8) -> Result<(), I2C::Error> {
        let current = self.read_reg(reg).await?;
        self.write_reg(reg, (current & !mask) | value).await
    }

    // Returns chip revision
    pub async fn init(&mut self) -> Result<u8, Error<I2C::Error>> {
        // Reset
        self.write_reg(REG_PU_CTRL, PU_CTRL_RR).await?;
        Timer::after_millis(1).await;
        // Power up digital + analog
        self.write_reg(REG_PU_CTRL, PU_CTRL_PUD | PU_CTRL_PUA).await?;
        let deadline = Instant::now() + Duration::from_millis(200);
        while self.read_reg(REG_PU_CTRL).await? & PU_CTRL_PUR == 0 {
            if Instant::now() > deadline {
                return Err(Error::PowerUpTimeout);
            }
            Timer::after_millis(1).await;
        }
        let revision = self.read_reg(REG_REVISION).await? & 0x0F;

        self.write_reg(REG_CTRL1, CTRL1_VLDO_3V0 | CTRL1_GAIN_128).await?;
        self.update_reg(REG_PU_CTRL, PU_CTRL_AVDDS, PU_CTRL_AVDDS).await?;
        self.update_reg(REG_CTRL2, CTRL2_CRS_MASK, 0).await?;
        self.update_reg(REG_ADC, ADC_CHPS_OFF, ADC_CHPS_OFF).await?;
        self.update_reg(REG_PGA_PWR, PGA_PWR_CAP_EN, PGA_PWR_CAP_EN).await?;
        // Start conversions
        self.update_reg(REG_PU_CTRL, PU_CTRL_CS, PU_CTRL_CS).await?;

        Timer::after_millis(600).await; // LDO / AFE settle time
        self.calibrate_internal_offset().await?;
        Ok(revision)
    }

    async fn calibrate_internal_offset(&mut self) -> Result<(), Error<I2C::Error>> {
        self.update_reg(REG_CTRL2, CTRL2_CALS | CTRL2_CALMOD_MASK, CTRL2_CALS).await?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let ctrl2 = self.read_reg(REG_CTRL2).await?;
            if ctrl2 & CTRL2_CALS == 0 {
                if ctrl2 & CTRL2_CAL_ERR != 0 {
                    return Err(Error::CalibrationFailed);
                }
                break;
            }
            if Instant::now() > deadline {
                return Err(Error::CalibrationFailed);
            }
            Timer::after_millis(10).await;
        }
        Timer::after_millis(300).await; // wait for a post-calibration conversion
        Ok(())
    }

    // Returns a sample if a new conversion is available
    pub async fn read_if_ready(&mut self) -> Result<Option<i32>, I2C::Error> {
        if self.read_reg(REG_PU_CTRL).await? & PU_CTRL_CR == 0 {
            return Ok(None);
        }
        let mut buf = [0u8; 3];
        self.i2c.write_read(NAU7802_I2C_ADDRESS, &[REG_ADCO_B2], &mut buf).await?;
        let raw = ((buf[0] as u32) << 16) | ((buf[1] as u32) << 8) | buf[2] as u32;
        Ok(Some(((raw << 8) as i32) >> 8))
    }
}
