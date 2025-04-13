use deku::{DekuContainerRead, DekuContainerWrite, DekuError};
use embassy_time::Duration;

use crate::pn532_ext::{ensure_tag_formatted, process_ntag_write_long, Esp32TimerAsync};

#[derive(Debug)]
pub enum Error<E: core::fmt::Debug> {
    Pn532ExtError(crate::pn532_ext::Error<E>),
    #[allow(dead_code)]
    NdefReadError(DekuError),
}

impl<E: core::fmt::Debug> From<crate::pn532_ext::Error<E>> for Error<E> {
    fn from(v: crate::pn532_ext::Error<E>) -> Self {
        Error::Pn532ExtError(v)
    }
}

#[allow(dead_code)]
pub async fn write_ndef_text_record<I>(
    pn532: &mut pn532::Pn532<I, Esp32TimerAsync>,
    text: &str,
    timeout: Duration,
) -> Result<(), Error<I::Error>>
where
    I: pn532::Interface,
{
    let a_record = crate::ndef::Record::new_text_record_en(text);
    let ndef_struct = crate::ndef::NDEFStructure::new(a_record);
    ensure_tag_formatted(pn532, timeout).await?;
    process_ntag_write_long(pn532, &ndef_struct.to_bytes().unwrap(), 4, timeout).await?;
    Ok(())
}

pub async fn write_ndef_url_record<I>(
    pn532: &mut pn532::Pn532<I, Esp32TimerAsync>,
    url: &str,
    timeout: Duration,
) -> Result<(), Error<I::Error>>
where
    I: pn532::Interface,
{
    let a_record = crate::ndef::Record::new_url_record(url);
    let ndef_struct = crate::ndef::NDEFStructure::new(a_record);
    ensure_tag_formatted(pn532, timeout).await?;
    process_ntag_write_long(pn532, &ndef_struct.to_bytes().unwrap(), 4, timeout).await?;
    Ok(())
}

pub async fn read_ndef_record<I>(
    pn532: &mut pn532::Pn532<I, Esp32TimerAsync>,
    timeout: Duration,
) -> Result<crate::ndef::Record, Error<I::Error>>
where
    I: pn532::Interface,
{
    let mut page4 = [0u8; 4];
    // first get ndef first page to extract message length
    crate::pn532_ext::process_ntag_read_long(pn532, &mut page4, 4, 4, timeout).await?;

    // read data for message
    let message_size = page4[1];
    let mut buf_vec = alloc::vec![0u8;(message_size+2).into()];
    let buf: &mut [u8] = &mut buf_vec;
    crate::pn532_ext::process_ntag_read_long(pn532, buf, 4, (message_size + 2).into(), timeout)
        .await?;

    match crate::ndef::Record::from_bytes((&buf[2..], 0)) {
        Err(e) => Err(Error::NdefReadError(e)),
        Ok(record) => Ok(record.1),
    }
}
