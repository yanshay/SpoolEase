use embedded_hal::delay::DelayNs;
use esp_hal::delay::Delay;
use esp_hal::gpio::{AnyPin, Input, InputConfig, Level, Output, OutputConfig, Pull};

const POWER_DOWN_US: u32 = 70;
const CLOCK_HIGH_US: u32 = 1;
const CLOCK_LOW_US: u32 = 1;

pub struct Hx711Gpio {
    dt: Input<'static>,
    sck: Output<'static>,
    delay: Delay,
    gain_pulses: u8,
}

impl Hx711Gpio {
    pub fn new(sck: AnyPin<'static>, dt: AnyPin<'static>) -> Self {
        let mut sck = Output::new(sck, Level::Low, OutputConfig::default());
        let _ = sck.set_low();
        let dt = Input::new(dt, InputConfig::default().with_pull(Pull::None));
        Self {
            dt,
            sck,
            delay: Delay::new(),
            gain_pulses: 1,
        }
    }

    pub fn reset(&mut self) {
        let _ = self.sck.set_high();
        self.delay.delay_us(POWER_DOWN_US);
        let _ = self.sck.set_low();
    }

    pub fn is_ready(&mut self) -> bool {
        self.dt.is_low()
    }

    pub fn read_raw(&mut self) -> Option<i32> {
        if !self.is_ready() {
            return None;
        }

        let value = critical_section::with(|_| {
            let mut value: i32 = 0;
            for _ in 0..24 {
                value <<= 1;
                let _ = self.sck.set_high();
                self.delay.delay_us(CLOCK_HIGH_US);
                if self.dt.is_high() {
                    value |= 1;
                }
                let _ = self.sck.set_low();
                self.delay.delay_us(CLOCK_LOW_US);
            }

            for _ in 0..self.gain_pulses {
                let _ = self.sck.set_high();
                self.delay.delay_us(CLOCK_HIGH_US);
                let _ = self.sck.set_low();
                self.delay.delay_us(CLOCK_LOW_US);
            }
            value
        });

        Some(sign_extend_24(value))
    }
}

fn sign_extend_24(value: i32) -> i32 {
    if value & 0x80_0000 != 0 { value | !0xFF_FFFF } else { value }
}
