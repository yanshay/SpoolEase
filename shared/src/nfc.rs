use alloc::vec::Vec;
use embassy_time::{Duration, Instant};
use hashbrown::HashMap;
use ndef_rs::{
    NdefMessage, NdefRecord, TNF,
    error::NdefError,
    payload::UriPayload,
    tag::{NFT2Tag, TlvValue},
};

use crate::pn532_ext::{self, Esp32TimerAsync, ensure_tag_formatted, process_ntag_write_long};

#[derive(Debug)]
pub enum Error<E: core::fmt::Debug> {
    Pn532ExtError(crate::pn532_ext::Error<E>),
    #[allow(dead_code)]
    NotNdefFormatted,
    NdefSizeError(usize),
    NdefError(NdefError),
    NdefBuildError,
    NotBambulabTag,
    MifareIncompleteBlock(u8),
}

impl<E: core::fmt::Debug> From<crate::pn532_ext::Error<E>> for Error<E> {
    fn from(v: crate::pn532_ext::Error<E>) -> Self {
        Error::Pn532ExtError(v)
    }
}

pub async fn erase_ndef_tag<I>(
    pn532: &mut pn532::Pn532<I, Esp32TimerAsync>,
    timeout: Duration,
) -> Result<(), Error<I::Error>>
where
    I: pn532::Interface,
{
    ensure_tag_formatted(pn532, timeout).await?;
    let empty_page_4 = [0x03, 0x00, 0xFE, 0x00];
    process_ntag_write_long(pn532, &empty_page_4, 4, timeout).await?;
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
    let ndef_record = NdefRecord::builder()
        .tnf(TNF::WellKnown)
        .payload(&UriPayload::from_string(url))
        .build()
        .map_err(|_e| Error::NdefBuildError)?;
    let message = NdefMessage::from(ndef_record);
    let tlv = TlvValue::ndef_message(&message).map_err(|_e| Error::NdefBuildError)?;
    let tag = NFT2Tag::builder()
        .size_in_bytes(512)
        .add_tlv(tlv)
        .add_tlv(TlvValue::terminator())
        .build();
    let tag_bytes = tag.to_bytes().map_err(|_e| Error::NdefBuildError)?;
    if tag_bytes.len() < 4 {
        return Err(Error::NdefBuildError);
    }
    ensure_tag_formatted(pn532, timeout).await?;
    // Don't write page3 - it involves tag manufactured properties, ensure_tag_formatted does its best to take caare of page3
    process_ntag_write_long(pn532, &tag_bytes[4..], 4, timeout).await?;

    Ok(())
}

pub async fn read_bambulab_payload<I>(
    pn532: &mut pn532::Pn532<I, Esp32TimerAsync>,
    timeout: Duration,
    uid: &[u8],
) -> Result<HashMap<i32, Vec<u8>>, Error<I::Error>>
where
    I: pn532::Interface,
{
    let end_time = Instant::now() + timeout;
    let bambulab_keys = crate::pn532_ext::bambulab_keys(uid);
    let mut currently_authenticated_sector = None;
    let blocks_to_read = [1, 2, 4, 5, 6, 13, 16];
    let mut res_map = HashMap::new();
    for block_number in blocks_to_read {
        let mut res_vec = alloc::vec![0u8;16];
        match pn532_ext::mifare_read_with_retries(
            pn532,
            uid,
            block_number,
            &mut currently_authenticated_sector,
            bambulab_keys.block_key(block_number),
            &mut res_vec,
            end_time,
            &[],
        )
        .await
        {
            Ok(bytes_read) => {
                if bytes_read != 16 {
                    return Err(Error::MifareIncompleteBlock(block_number));
                }
                res_map.insert(block_number as i32, res_vec);
            }
            Err(pn532_ext::Error::AuthenticationError) => {
                return Err(Error::NotBambulabTag);
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(res_map)
}

pub async fn read_ndef_payload<I>(
    pn532: &mut pn532::Pn532<I, Esp32TimerAsync>,
    timeout: Duration,
) -> Result<Option<Vec<u8>>, Error<I::Error>>
where
    I: pn532::Interface,
{
    let mut page34 = [0u8; 8];
    // first get ndef first page to extract message length
    crate::pn532_ext::process_ntag_read_long(pn532, &mut page34, 3, 8, timeout).await?;

    let is_ndef_capable = page34[0] == 0xE1;
    let ndef_message_exist = is_ndef_capable && page34[4] == 0x03; // first byte of page 4
    if !ndef_message_exist {
        return Err(Error::NotNdefFormatted);
    }
    let ndef_message_empty = ndef_message_exist && page34[4 + 1] == 0x00; // second byte of page 4
    if ndef_message_empty {
        return Ok(None);
    }
    // read data for message
    let message_size_or_marker = page34[4 + 1];
    let buf_size: usize = if message_size_or_marker == 0xff {
        page34[4 + 2] as usize * 256 + page34[4 + 3] as usize // long TLV
    } else {
        message_size_or_marker as usize // ?? + 3 // short TLV
    };
    if buf_size > 2048 {
        // prevent tag data crashing
        return Err(Error::NdefSizeError(buf_size));
    }
    let mut buf_vec = alloc::vec![0u8;buf_size];
    let mut page5_pos_in_buf_vec = 0;
    let mut bytes_to_read = buf_size;
    if message_size_or_marker != 0xff {
        // short TLV - first two bytes are the two last in page 4 already read
        buf_vec[0..=1].copy_from_slice(&page34[4 + 2..=4 + 3]);
        page5_pos_in_buf_vec = 2;
        bytes_to_read = buf_size - 2;
    }
    let buf: &mut [u8] = &mut buf_vec;
    crate::pn532_ext::process_ntag_read_long(
        pn532,
        &mut buf[page5_pos_in_buf_vec..],
        5,
        bytes_to_read,
        timeout,
    )
    .await?;

    Ok(Some(buf_vec))
}

#[derive(Debug, PartialEq)]
pub enum NfcTagType {
    MifareClassic1K,
    MifareClassic4K,
    NTAG,
    Unknown,
}

pub fn get_nfc_tag_type(inlist_response: &[u8]) -> NfcTagType {
    if inlist_response.len() < 6 {
        return NfcTagType::Unknown;
    }

    let sens_res = inlist_response[3];
    let sak = inlist_response[4];

    match (sens_res, sak) {
        (0x44, 0x00) => NfcTagType::NTAG,
        (0x04, 0x08) | (0x44, 0x08) => NfcTagType::MifareClassic1K,
        (0x04, 0x18) | (0x02, 0x18) => NfcTagType::MifareClassic4K,
        _ => NfcTagType::Unknown,
    }
}
