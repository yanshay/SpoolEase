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
        self.deadline = Some(
            Instant::now()
                .checked_add(duration)
                .unwrap_or(embassy_time::Instant::now()),
        );
    }

    async fn until_timeout<F: Future>(
        &self,
        fut: F,
    ) -> Result<F::Output, embassy_time::TimeoutError> {
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
                    &pn532::Request::ntag_write(
                        page + u8::try_from(page_offset).unwrap(),
                        &data_to_write,
                    ),
                    1,
                    end_time - Instant::now(),
                )
                .await?;
            if res[0] != 0x00 {
                // first byte signals if read was ok
                last_err = res[0];
                trace!("Error {} during NFC write of page {page_offset}", last_err);
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

    /*'single_read:*/
    for chunk_offset in 0..num_chunks {
        let chunk_byte_offset = usize::from(chunk_offset) * 16;
        let copy_bytes = min(16, len - chunk_byte_offset);
        read_with_retries(
            pn532,
            page + chunk_offset * 4,
            &mut buf[chunk_byte_offset..chunk_byte_offset + copy_bytes],
            end_time,
            &[]
        )
        .await?;
    }
    Ok(())
}

pub async fn read_with_retries<I>(
    pn532: &mut pn532::Pn532<I, Esp32TimerAsync>,
    page: u8,
    buf: &mut [u8],
    end_time: Instant,
    error_on_errnums: &[u8]
) -> Result<(), Error<I::Error>>
where
    I: pn532::Interface,
{
    let mut last_err = 0;

    loop {
        if Instant::now() > end_time {
            return Err(Error::Pn532ExtError(last_err));
        }

        let read_data = pn532
            .process(
                &pn532::Request::ntag_read(page),
                17,
                end_time - Instant::now(),
            )
            .await?;
        if error_on_errnums.contains(&read_data[0]) {
            return Err(Error::Pn532ExtError(read_data[0]));
        }
        if read_data[0] != 0x00 {
            // first byte signals if read was ok
            last_err = read_data[0];
            debug!(
                "Error {} during NFC read of 4 pages starting at {page}, retrying",
                last_err
            );
            continue;
        }
        buf.copy_from_slice(&read_data[1..buf.len()+1]); // skip the 0 (that represents error or ok) at the beginning
        return Ok(());
    }
}

// This method is theoretical - and effective only in case of unformatted tags
// So in case of such tags there may be issues and need to debug it for real
pub async fn ensure_tag_formatted<I>(
    pn532: &mut pn532::Pn532<I, Esp32TimerAsync>,
    timeout: Duration,
) -> Result<(), Error<I::Error>>
where
    I: pn532::Interface,
{
    let mut buf = [0u8;16];

    let end_time = Instant::now() + timeout;
    // page 3 should always be readable, if error, should return it as an error
    read_with_retries(pn532, 3, &mut buf, end_time, &[]).await?;

    if buf[0] == 0xE1 { // Magic 0xE1 should be here if formtted (page is [0xE1, 0x10, num_of_pages, 0x00])
        return Ok(())
    } 

    // If magic is not here, then we have some unitialized tag. 
    // The only reliable way to know its size is to try and read pages until it returns timeout on pages that doesn't exist
    // Only need to check boundary pages of standard ntag sizes

    //           NTAG213 ,  NTAG215  ,  NTAG216
    let tests = [(44, 45), (134, 135), (230, 231)]; // (test page, if succeds this is at least the the number of pages)

    let mut num_of_pages_on_tag = 0;

    for test in tests {
        debug!("Testing tag for size - checking page {}",test.0);
        let read_res = read_with_retries(pn532, test.0, &mut buf, end_time, &[19]).await;

        match read_res {
            Ok(_) => {
                info!("  Test passed, at least {} on tag", test.1);
                num_of_pages_on_tag = test.1; // success, so at least NTAG215
            }
            Err(err) => {
            if let  Error::Pn532ExtError(err_num) = err {
                    debug!("Error when reading page in ensure_formatted :{err_num}");
                    if err_num != 19 { // this is the error I saw on page not available
                        return Err(Error::Pn532ExtError(err_num));
                    }
                    debug!("Inlisting again to clear error and allow future reading");
                    let res = pn532.process(&pn532::Request::INLIST_ONE_ISO_A_TARGET, 17, end_time - Instant::now()).await;
                    debug!("Inner inlist, required after read failure when testing tag, result {res:?}");

                    break;
                }
            }
        }
    }

    info!("Formatting tag with {num_of_pages_on_tag} pages (writing page 3)");

    let page3_format = [0xe1, 0x10, num_of_pages_on_tag, 0x00];
    // Even if fail, won't fail the encode
    match process_ntag_write_long(pn532, &page3_format, 3, end_time - Instant::now()).await {
        Ok(_) =>  {
            info!("Formatted tag successfuly");
        }
        Err(err) => {
            error!("Failed to format tag {err:?}");
        }
    }

    Ok(())
}
