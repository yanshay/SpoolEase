use core::cell::RefCell;

use alloc::rc::Rc;
use embassy_time::{with_timeout, Duration};
use esp_hal::gpio::{AnyPin, Input, Pull};
use framework::{debug, prelude::Framework};

use crate::app::App;
#[embassy_executor::task]
pub async fn scale_button_task(
    app: Rc<RefCell<App>>,
    _framework: Rc<RefCell<Framework>>,
    scale_button_pin: AnyPin,
) {
    let mut button = Input::new(scale_button_pin, Pull::None);

    button.wait_for_high().await; // wait for initial state of button not pressed
    loop {
        button.wait_for_low().await; // wait for presss
        let res = with_timeout(Duration::from_millis(5000), button.wait_for_high()).await; // wait for release
        match res {
            Ok(_) => (), // released before timeout
            Err(_timeout_err) => {
                debug!("Long scale button press");
                app.borrow_mut().notify_button_long_press();
                button.wait_for_low().await;
            }
        }
    }
}
