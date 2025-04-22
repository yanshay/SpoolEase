use core::cell::RefCell;

use crate::app::{Pn532State, ScaleState};
use alloc::rc::Rc;
use embassy_time::Timer;
use esp_hal::{gpio::AnyPin, peripherals::RMT, rmt::Rmt, time::RateExtU32};
use esp_hal_smartled::{smartLedBuffer, SmartLedsAdapter};
use framework::{framework::OtaState, prelude::Framework};
use smart_leds::{brightness, colors::*, SmartLedsWrite, RGB, RGB8};

enum LedState {
    Steady(RGB8),
    Flash(RGB8, RGB8),
}

use crate::app::App;
#[embassy_executor::task]
pub async fn rgb_led_task(
    app: Rc<RefCell<App>>,
    framework: Rc<RefCell<Framework>>,
    led_pin: AnyPin,
    rmt: RMT,
) {
    let rmt = Rmt::new(rmt, 80.MHz()).unwrap();

    let rmt_buffer = smartLedBuffer!(1);
    let mut led = SmartLedsAdapter::new(rmt.channel0, led_pin, rmt_buffer);

    #[allow(non_snake_case)]
    let MY_PINK =  brightness([PURPLE].into_iter(), 40).next().unwrap();
    #[allow(non_snake_case)]
    let MY_BLUE = brightness([BLUE].into_iter(), 40).next().unwrap();
    #[allow(non_snake_case)]
    let MY_YELLOW = brightness([RGB{r:0x60, g:0x30, b: 0}].into_iter(), 40).next().unwrap();

    // decide on state based view
    let mut curr_color = BLACK;
    let mut led_state;
    loop {
        if !framework.borrow().wifi_ok.as_ref().unwrap_or(&false) {
            led_state = LedState::Flash(RED, BLACK);
        } else if app.borrow().pn532_state == Pn532State::InitAsTarget {
            led_state = LedState::Steady(MY_PINK);
        }
        else if !app.borrow().connected {
            led_state = LedState::Steady(RED);
        } else if matches!(
            framework.borrow().ota_state,
            Some(OtaState::Started) | Some(OtaState::InProgress(_))
        ) {
            led_state = LedState::Flash(GREEN, BLUE);
        } else {
            match app.borrow().scale_state {
                ScaleState::Uncalibrated => {
                    led_state = LedState::Steady(ORANGE);
                }
                ScaleState::Unknown => {
                    led_state = LedState::Steady(BLACK);
                }
                ScaleState::Empty => {
                    led_state = LedState::Steady(BLACK);
                }
                ScaleState::Loaded(stable, unstable) => {
                    if stable == unstable {
                        led_state = LedState::Steady(MY_BLUE);
                    } else {
                        led_state = LedState::Steady(MY_YELLOW);
                    }
                }
            }
        }

        match led_state {
            LedState::Steady(color) => {
                led.write([color]).unwrap();
                curr_color = color;
            }
            LedState::Flash(color1, color2) => {
                if curr_color == color1 {
                    led.write([color1]).unwrap();
                    curr_color = color2
                } else {
                    led.write([color2]).unwrap();
                    curr_color = color1;
                }
            }
        }
        Timer::after_millis(250).await;
    }
}
