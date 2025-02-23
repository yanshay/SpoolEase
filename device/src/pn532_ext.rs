use embassy_time::with_deadline;
use embassy_time::Duration;
use embassy_time::Instant;
use embassy_time::Timer;

use core::cmp::min;
use core::future::Future;

use framework::prelude::*;

/*

PN532 User Guide (Manual): https://www.nxp.com/docs/en/user-guide/141520.pdf
Error Codes List (the first byte): page 67, 7.1 Error Handling

*/

#[derive(Debug)]
#[allow(dead_code)]
pub enum Error<E: core::fmt::Debug> {
    Pn532Error(pn532::Error<E>),
    Pn532ExtError(u8),
}

impl<E: core::fmt::Debug> From<pn532::Error<E>> for Error<E> {
    fn from(v: pn532::Error<E>) -> Self {
        Error::Pn532Error(v)
    }
}
pub struct Esp32TimerAsync {
    deadline: Option<embassy_time::Instant>,
}

impl Esp32TimerAsync {
    pub fn new() -> Self {
        Self { deadline: None }
    }
}
impl pn532::CountDown for Esp32TimerAsync {
    type Time = embassy_time::Duration;

    fn start<D: Into<Self::Time>>(&mut self, count: D) {
        let duration: embassy_time::Duration = count.into();
        self.deadline = Some(Instant::now().checked_add(duration).unwrap_or(embassy_time::Instant::now()));
    }

    async fn until_timeout<F: Future>(&self, fut: F) -> Result<F::Output, embassy_time::TimeoutError> {
        with_deadline(self.deadline.unwrap(), fut).await
    }
}

pub async fn process_ntag_write_long<I>(
    pn532: &mut pn532::Pn532<I, Esp32TimerAsync>,
    buf: &[u8],
    page: u8,
    timeout: Duration,
) -> Result<(), Error<I::Error>>
where
    I: pn532::Interface,
{
    Timer::after_millis(10).await; // wait for stable RF field
    assert!(buf.len() % 4 == 0);
    let num_pages = buf.len() / 4;

    let end_time = Instant::now() + timeout;
    let mut last_err = 0u8;

    /*'single_write:*/
    for page_offset in 0..num_pages {
        let page_byte_offset = page_offset * 4;
        let data_to_write = [
            buf[page_byte_offset],
            buf[page_byte_offset + 1],
            buf[page_byte_offset + 2],
            buf[page_byte_offset + 3],
        ];
        'retries: loop {
            if Instant::now() > end_time {
                return Err(Error::Pn532ExtError(last_err));
            }
            let res = pn532
                .process(
                    &pn532::Request::ntag_write(page + u8::try_from(page_offset).unwrap(), &data_to_write),
                    1,
                    end_time - Instant::now(),
                )
                .await?;
            if res[0] != 0x00 {
                // first byte signals if read was ok
                last_err = res[0];
                trace!("Error {} during NFC write of page {page_offset}, retrying", last_err);
                // continue 'retries; retries on write might be causing tag bricking? or was it a faulty PN532?
                return Err(Error::Pn532ExtError(last_err));
            }
            break 'retries;
        }
    }
    Ok(())
}

pub async fn process_ntag_read_long<I>(
    pn532: &mut pn532::Pn532<I, Esp32TimerAsync>,
    buf: &mut [u8],
    page: u8,
    len: usize,
    timeout: Duration,
) -> Result<(), Error<I::Error>>
where
    I: pn532::Interface,
{
    assert!(len >= buf.len());
    // read is in 16 bytes chunks
    let num_chunks = u8::try_from((len + 15) / 16).unwrap();

    let end_time = Instant::now() + timeout;
    let mut last_err = 0u8;

    /*'single_read:*/
    for chunk_offset in 0..num_chunks {
        'retries: loop {
            if Instant::now() > end_time {
                return Err(Error::Pn532ExtError(last_err));
            }
            let read_data = pn532
                .process(&pn532::Request::ntag_read(page + chunk_offset * 4), 17, end_time - Instant::now())
                .await?;
            if read_data[0] != 0x00 {
                // first byte signals if read was ok
                last_err = read_data[0];
                trace!("Error {} during NFC read of chunk (4 pages) {chunk_offset}, retrying", last_err);
                continue 'retries;
            }

            let chunk_byte_offset = usize::from(chunk_offset) * 16;
            let copy_bytes = min(16, len - chunk_byte_offset);
            buf[chunk_byte_offset..chunk_byte_offset + copy_bytes].copy_from_slice(&read_data[1..1 + copy_bytes]);

            break 'retries;
        }
    }
    Ok(())
}
