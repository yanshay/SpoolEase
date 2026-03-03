use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use embassy_time::Duration;
use embassy_time::Instant;
use embassy_time::Timer;
use embassy_time::with_deadline;
use ndef_rs::NdefMessage;
use ndef_rs::tag::NFT2Tag;
use ndef_rs::tag::TlvValue;

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
    AuthenticationError,
}

impl<E: core::fmt::Debug> From<pn532::Error<E>> for Error<E> {
    fn from(v: pn532::Error<E>) -> Self {
        Error::Pn532Error(v)
    }
}

#[derive(Default)]
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

    #[allow(clippy::manual_div_ceil)]
    let num_pages = (buf.len() + 3) / 4; // complement to include partial data on last page

    let end_time = Instant::now() + timeout;
    let last_err;

    /*'single_write:*/
    let mut data_to_write = [0u8; 4];
    for page_offset in 0..num_pages {
        let page_byte_offset = page_offset * 4;
        let n = min(4, buf.len() - page_byte_offset);
        data_to_write[..n].copy_from_slice(&buf[page_byte_offset..page_byte_offset + n]);
        if n < 4 {
            data_to_write[n..].fill(0);
        }

        if Instant::now() > end_time {
            error!("Tag read timeout error");
            return Err(Error::Pn532Error(pn532::Error::TimeoutResponse)); // using the Pn532Error, not sure if good practice
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
    #[allow(clippy::manual_div_ceil)]
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
            &[],
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
    error_on_errnums: &[u8],
) -> Result<usize, Error<I::Error>>
where
    I: pn532::Interface,
{
    let mut last_err;

    loop {
        if Instant::now() > end_time {
            error!("Tag read timeout error");
            return Err(Error::Pn532Error(pn532::Error::TimeoutResponse)); // using the Pn532Error, not sure if good practice
        }

        let read_data = pn532
            .process(
                &pn532::Request::ntag_read(page),
                17,
                end_time - Instant::now(),
            )
            .await?;

        if error_on_errnums.contains(&read_data[0]) {
            // not retrying on these errors
            return Err(Error::Pn532ExtError(read_data[0]));
        }

        if read_data[0] != 0x00 {
            // first byte signals if read was ok
            last_err = read_data[0];
            warn!(
                "Error {} during NFC read of 4 pages starting at {page}, retrying",
                last_err
            );
            continue;
        }

        let n = min(read_data.len() - 1, buf.len());
        buf[..n].copy_from_slice(&read_data[1..n + 1]); // skip the 0 (that represents error or ok) at the beginning
        if n < buf.len() {
            buf[n..].fill(0);
        }
        return Ok(n);
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
    let mut buf = [0u8; 16];

    let end_time = Instant::now() + timeout;
    // page 3 should always be readable, if error, should return it as an error
    read_with_retries(pn532, 3, &mut buf, end_time, &[]).await?;

    if buf[0] == 0xE1 {
        // Magic 0xE1 should be here if formtted (page is [0xE1, 0x10, num_of_pages, 0x00])
        return Ok(());
    }

    // If magic is not here, then we have some unitialized tag.
    // The only reliable way to know its size is to try and read pages until it returns timeout on pages that doesn't exist
    // Only need to check boundary pages of standard ntag sizes

    //           NTAG213 ,  NTAG215  ,  NTAG216
    let tests = [(44, 0x12), (134, 0x3e), (230, 0x6d)]; // (test page, if succeds this is at least the the number of pages)

    let mut num_of_pages_on_tag = 0;

    for test in tests {
        debug!("Testing tag for size - checking page {}", test.0);
        let read_res = read_with_retries(pn532, test.0, &mut buf, end_time, &[19]).await;

        match read_res {
            Ok(_) => {
                info!("  Test passed, at least {} on tag", test.1);
                num_of_pages_on_tag = test.1; // success, so at least NTAG215
            }
            Err(err) => {
                if let Error::Pn532ExtError(err_num) = err {
                    debug!("Error when reading page in ensure_formatted :{err_num}");
                    if err_num != 19 {
                        // this is the error I saw on page not available
                        return Err(Error::Pn532ExtError(err_num));
                    }
                    debug!("Inlisting again to clear error and allow future reading");
                    let res = pn532
                        .process(
                            &pn532::Request::INLIST_ONE_ISO_A_TARGET,
                            17,
                            end_time - Instant::now(),
                        )
                        .await;
                    debug!(
                        "Inner inlist, required after read failure when testing tag, result {res:?}"
                    );

                    break;
                }
            }
        }
    }

    info!("Formatting tag with {num_of_pages_on_tag} pages (writing page 3)");

    let page3_format = [0xe1, 0x10, num_of_pages_on_tag, 0x00];
    // Even if fail, won't fail the encode
    match process_ntag_write_long(pn532, &page3_format, 3, end_time - Instant::now()).await {
        Ok(_) => {
            info!("Formatted tag successfuly");
        }
        Err(err) => {
            error!("Failed to format tag {err:?}");
        }
    }

    Ok(())
}

// const C_APDU_CLA: usize = 0;
const C_APDU_INS: usize = 1; // instruction
const C_APDU_P1: usize = 2; // parameter 1
const C_APDU_P2: usize = 3; // parameter 2
const C_APDU_LC: usize = 4; // length command
const C_APDU_DATA: usize = 5; // data

const ISO7816_SELECT_FILE: u8 = 0xA4;
const ISO7816_READ_BINARY: u8 = 0xB0;
// const ISO7816_UPDATE_BINARY: u8 = 0xD6;

const C_APDU_P1_SELECT_BY_ID: u8 = 0x00;
const C_APDU_P1_SELECT_BY_NAME: u8 = 0x04;

// Response APDU
const R_APDU_SW1_COMMAND_COMPLETE: u8 = 0x90;
const R_APDU_SW2_COMMAND_COMPLETE: u8 = 0x00;
const COMMAND_COMPLETE: [u8; 2] = [R_APDU_SW1_COMMAND_COMPLETE, R_APDU_SW2_COMMAND_COMPLETE];

const R_APDU_SW1_NDEF_TAG_NOT_FOUND: u8 = 0x6a;
const R_APDU_SW2_NDEF_TAG_NOT_FOUND: u8 = 0x82;
const TAG_NOT_FOUND: [u8; 2] = [R_APDU_SW1_NDEF_TAG_NOT_FOUND, R_APDU_SW2_NDEF_TAG_NOT_FOUND];

const R_APDU_SW1_FUNCTION_NOT_SUPPORTED: u8 = 0x6A;
const R_APDU_SW2_FUNCTION_NOT_SUPPORTED: u8 = 0x81;
const FUNCTION_NOT_SUPPORTED: [u8; 2] = [
    R_APDU_SW1_FUNCTION_NOT_SUPPORTED,
    R_APDU_SW2_FUNCTION_NOT_SUPPORTED,
];

// const R_APDU_SW1_MEMORY_FAILURE: u8 = 0x65;
// const R_APDU_SW2_MEMORY_FAILURE: u8 = 0x81;

const R_APDU_SW1_END_OF_FILE_BEFORE_REACHED_LE_BYTES: u8 = 0x62;
const R_APDU_SW2_END_OF_FILE_BEFORE_REACHED_LE_BYTES: u8 = 0x82;
const END_OF_FILE_BEFORE_REACHED_LE_BYTES: [u8; 2] = [
    R_APDU_SW1_END_OF_FILE_BEFORE_REACHED_LE_BYTES,
    R_APDU_SW2_END_OF_FILE_BEFORE_REACHED_LE_BYTES,
];

const NDEF_TAG_APPLICATION_NAME_V2: [u8; 9] = [0, 0x7, 0xD2, 0x76, 0x00, 0x00, 0x85, 0x01, 0x01];
const CAPABILITY_CONTAINER: [u8; 15] = [
    0_u8,                                          // cc len msb
    0x0F,                                          // cc len lsb
    0x20,                                          // version 2.0
    ((NDEF_MAX_READ_LENGTH & 0xFF00) >> 8) as u8, // Mle msb (Maximum data size that can be read using a single ReadBinary command.)
    (NDEF_MAX_READ_LENGTH & 0xFF) as u8,          // Mle lsb
    ((NDEF_MAX_WRITE_LENGTH & 0xFF00) >> 8) as u8, // Mlc msb (Maximum data size that can be written using a single UpdateBinary command)
    (NDEF_MAX_WRITE_LENGTH & 0xFF) as u8,          // Mlc lsb
    // NDEF TLV
    0x04,                                    // T - Tag ?
    0x06,                                    // L - Length of the value field
    0xE1,                                    // NDEF File Identifier byte 1
    0x04,                                    // NDEF File Identifier byte 2
    ((NDEF_MAX_LENGTH & 0xFF00) >> 8) as u8, // Maximum NDEF file size Msb
    (NDEF_MAX_LENGTH & 0xFF) as u8,          // maximum NDEF file size Lsb
    0x00,                                    // read access 0x0 = granted
    0x00,                                    // write access 0x0 = granted | 0xFF = deny
];

#[allow(clippy::upper_case_acronyms)]
#[repr(u8)]
#[derive(Clone, Copy, Debug)]
enum TagFile {
    NONE = 0,
    CC = 1, // Capability Container
    NDEF = 2,
}

const NDEF_MAX_LENGTH: usize = 1024; // arbitrary size, defines tag max size, relevant mostly for write (currently not implemented)
const NDEF_MAX_READ_LENGTH: usize = 254;
const NDEF_MAX_WRITE_LENGTH: usize = 256;

pub async fn emulate_tag<I, T, const N: usize>(
    pn532: &mut pn532::Pn532<I, T, N>,
    message: NdefMessage,
    short_uid: Option<[u8; 3]>,
    timeout: Duration,
) -> Result<bool, String>
where
    I: pn532::Interface,
    T: pn532::CountDown<Time = embassy_time::Duration>,
{
    let tlv = TlvValue::ndef_message(&message)
        .map_err(|err| format!("Emulate tag error - generating TLV {err:?}"))?;

    let tag = NFT2Tag::builder().size_in_bytes(512).add_tlv(tlv).build();

    let mut ndef_bytes = tag
        .to_bytes()
        .map_err(|err| format!("Emulate tag error - getting tag bytes {err:?}"))?;
    if ndef_bytes.len() >= 5 {
        ndef_bytes.drain(0..=3); // remove capability container
    } else {
        return Err("Emulating tag error - NDef Bytes less than 5".to_string());
    }
    ndef_bytes[0] = 0; // need null TLV at the beginning and not NDEF TLV for some reason

    // info!("---- Sending TG_INIT_AS_TARGET");
    match pn532
        .process(
            &pn532::Request::tg_init_as_target(Some(5), short_uid),
            37,
            timeout,
        )
        .await
    {
        Ok(_v) => {
            // info!("TG_INIT_AS_TARGET response: {:x?}", v);
        }
        Err(err) => match err {
            pn532::Error::TimeoutResponse => return Ok(false),
            pn532::Error::TimeoutAck => return Ok(false),
            _ => return Err(format!("Error resopnse emulating tag: {err:?}")),
        },
    }

    let mut current_file = TagFile::NONE;
    let mut send_buf = [0u8; NDEF_MAX_READ_LENGTH + 2];
    let mut send_data;
    let mut sent_entire_ndef = false;

    loop {
        match pn532
            .process(&pn532::Request::TG_GET_DATA, 40, Duration::from_secs(60))
            .await
        {
            Ok(v) => {
                if v.len() <= 1 {
                    if sent_entire_ndef {
                        return Ok(true);
                    } else {
                        return Err(
                            "Received empty tgGetData response before sending entire NDEF"
                                .to_string(),
                        );
                    }
                }

                let status = v[0];
                if status != 0 {
                    if sent_entire_ndef {
                        return Ok(true);
                    } else {
                        return Err(format!(
                            "Received error status 0x{status:x} in tgGetData response before sending entire NDEF"
                        ));
                    }
                }
                let recv_buf = &v[1..];
                if recv_buf.len() < 5 {
                    return Err(format!(
                        "Not enough data in tgGetData response, only {} bytes",
                        v.len()
                    ));
                }
                let p1 = recv_buf[C_APDU_P1];
                let p2 = recv_buf[C_APDU_P2];
                let lc = recv_buf[C_APDU_LC];
                let p1p2_length = ((p1 as usize) << 8) + p2 as usize;

                match recv_buf[C_APDU_INS] {
                    ISO7816_SELECT_FILE => match p1 {
                        C_APDU_P1_SELECT_BY_ID => {
                            if p2 != 0x0c {
                                send_data = get_data_to_set(&mut send_buf, &[], &COMMAND_COMPLETE);
                            } else if lc == 2
                                && recv_buf[C_APDU_DATA] == 0xE1
                                && (recv_buf[C_APDU_DATA + 1] == 0x03
                                    || recv_buf[C_APDU_DATA + 1] == 0x04)
                            {
                                send_data = get_data_to_set(&mut send_buf, &[], &COMMAND_COMPLETE);
                                if recv_buf[C_APDU_DATA + 1] == 0x03 {
                                    current_file = TagFile::CC;
                                } else if recv_buf[C_APDU_DATA + 1] == 0x04 {
                                    current_file = TagFile::NDEF;
                                }
                            } else {
                                send_data = get_data_to_set(&mut send_buf, &[], &TAG_NOT_FOUND);
                            }
                        }
                        C_APDU_P1_SELECT_BY_NAME => {
                            if recv_buf[C_APDU_P2..].starts_with(&NDEF_TAG_APPLICATION_NAME_V2) {
                                send_data = get_data_to_set(&mut send_buf, &[], &COMMAND_COMPLETE);
                            } else {
                                error!("function not supported {:x?}", &recv_buf[C_APDU_P2..]);
                                send_data =
                                    get_data_to_set(&mut send_buf, &[], &FUNCTION_NOT_SUPPORTED);
                            }
                        }
                        _ => {
                            warn!("SELECT-FILE -> Unhandled p1 {p1:x}");
                            return Err(format!(
                                "Unsupported SELECT-FILE command in tag emulator {p1:x}"
                            ));
                        }
                    },
                    ISO7816_READ_BINARY => match &current_file {
                        TagFile::NONE => {
                            send_data = get_data_to_set(&mut send_buf, &[], &TAG_NOT_FOUND);
                        }
                        TagFile::CC => {
                            if p1p2_length > NDEF_MAX_LENGTH
                                || p1p2_length + lc as usize > CAPABILITY_CONTAINER.len()
                            {
                                send_data = get_data_to_set(
                                    &mut send_buf,
                                    &[],
                                    &END_OF_FILE_BEFORE_REACHED_LE_BYTES,
                                );
                            } else {
                                send_data = get_data_to_set(
                                    &mut send_buf,
                                    &CAPABILITY_CONTAINER[p1p2_length..p1p2_length + lc as usize],
                                    &COMMAND_COMPLETE,
                                );
                            }
                        }
                        TagFile::NDEF => {
                            if p1p2_length > NDEF_MAX_LENGTH
                                || p1p2_length + lc as usize > ndef_bytes.len()
                            {
                                send_data = get_data_to_set(
                                    &mut send_buf,
                                    &[],
                                    &END_OF_FILE_BEFORE_REACHED_LE_BYTES,
                                );
                            } else {
                                send_data = get_data_to_set(
                                    &mut send_buf,
                                    &ndef_bytes[p1p2_length..p1p2_length + lc as usize],
                                    &COMMAND_COMPLETE,
                                );
                                if p1p2_length + lc as usize == ndef_bytes.len() {
                                    sent_entire_ndef = true;
                                }
                            }
                        }
                    },
                    _ => {
                        warn!(
                            "Unhandled command in tag emulator {:x}",
                            recv_buf[C_APDU_INS]
                        );
                        return Err(format!(
                            "Unsupported NFC command in tag emulator {:x}",
                            recv_buf[C_APDU_INS]
                        ));
                    }
                }
            }

            Err(err) => {
                return Err(format!("Failed to communicate with Tag Reader: {err:?}"));
            }
        }

        match pn532
            .process(
                pn532::requests::BorrowedRequest::new(
                    pn532::requests::Command::TgSetData,
                    send_data,
                ),
                10,
                Duration::from_secs(1),
            )
            .await
        {
            Ok(v) => {
                if v[0] == 0 {
                    // delay required for slow clients to process data (NFCTools on iPhone)
                    Timer::after_millis(20).await;
                } else {
                    return Err(format!("Error sending TgSetData: {}", v[0]));
                }
            }
            Err(err) => {
                return Err(format!("Error sending TgSetData {err:?}"));
            }
        }
    }
}

fn get_data_to_set<'a>(
    send_buf: &'a mut [u8],
    payload: &'_ [u8],
    command: &'_ [u8; 2],
) -> &'a [u8] {
    send_buf[..payload.len()].copy_from_slice(payload);
    send_buf[payload.len()..payload.len() + command.len()].copy_from_slice(command);

    &send_buf[..payload.len() + command.len()]
}

#[allow(clippy::too_many_arguments)]
pub async fn mifare_read_with_retries<I>(
    pn532: &mut pn532::Pn532<I, Esp32TimerAsync>,
    uid: &[u8],
    block_number: u8,
    currently_authenticated_sector: &mut Option<u8>,
    key: &[u8; 6],
    buf: &mut [u8],
    end_time: Instant,
    error_on_errnums: &[u8],
) -> Result<usize, Error<I::Error>>
where
    I: pn532::Interface,
{
    let mut last_err;

    let sector = block_number / 4;
    let need_authenticate = Some(sector) != *currently_authenticated_sector;

    if need_authenticate {
        loop {
            if Instant::now() > end_time {
                error!("Tag read timeout error");
                return Err(Error::Pn532Error(pn532::Error::TimeoutResponse)); // using the Pn532Error, not sure if good practice
            }

            let read_data = pn532
                .process(
                    &pn532::Request::mifare_classic_authenticate_block(
                        uid,
                        block_number,
                        pn532::requests::MifareAuthKey::A(key),
                    ),
                    7,
                    end_time - Instant::now(),
                )
                .await?;

            if error_on_errnums.contains(&read_data[0]) {
                // not retrying on these errors
                return Err(Error::Pn532ExtError(read_data[0]));
            }

            match read_data[0] {
                0 => {
                    *currently_authenticated_sector = Some(block_number / 4);
                    break;
                }
                0x14 => {
                    // not logging since it happens on every non bambu Mifare tag
                    error!("Authentication of block {block_number} (relevant sector) rejected");
                    return Err(Error::AuthenticationError);
                }
                _ => {
                    last_err = read_data[0];
                    warn!(
                        "Error {} during authentication of block {block_number}, retrying",
                        last_err
                    );
                    continue;
                }
            }
        }
    }

    loop {
        if Instant::now() > end_time {
            error!("Tag read timeout error");
            return Err(Error::Pn532Error(pn532::Error::TimeoutResponse)); // using the Pn532Error, not sure if good practice
        }

        let read_data = pn532
            .process(
                &pn532::Request::mifare_classic_read_data_block(block_number),
                17,
                end_time - Instant::now(),
            )
            .await?;

        if error_on_errnums.contains(&read_data[0]) {
            // not retrying on these errors
            return Err(Error::Pn532ExtError(read_data[0]));
        }

        if read_data[0] != 0x00 {
            // first byte signals if read was ok
            last_err = read_data[0];
            warn!(
                "Error {} during NFC read of block {block_number}, retrying",
                last_err
            );
            continue;
        }

        let n = min(read_data.len() - 1, buf.len());
        buf[..n].copy_from_slice(&read_data[1..n + 1]); // skip the 0 (that represents error or ok) at the beginning
        if n < buf.len() {
            buf[n..].fill(0);
        }
        // debug!(">>>> [{block_number:2}] read read_data: {buf:X?}");
        return Ok(n);
    }
}

use core::convert::TryInto;
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub struct BambulabKeys {
    okm: Vec<u8>,
}

impl BambulabKeys {
    pub fn sector_key(&self, sector_number: u8) -> &[u8; 6] {
        self.okm
            .get(sector_number as usize * 6..(sector_number as usize + 1) * 6)
            .expect("slice out of bounds")
            .try_into()
            .expect("slice not length 6")
    }
    pub fn block_key(&self, block_number: u8) -> &[u8; 6] {
        self.sector_key(block_number / 4)
    }
}
// impl BambulabKeys {
//     pub fn sector_key(&self, sector_number: usize) -> &[u8;6] {
//         self.okm.get(sector_number*6 .. (sector_number+1) *6).try_into().unwrap()
//     }
// }

pub fn bambulab_keys(uid: &[u8]) -> BambulabKeys {
    let master = [
        0x9a, 0x75, 0x9c, 0xf2, 0xc4, 0xf7, 0xca, 0xff, 0x22, 0x2c, 0xb9, 0x76, 0x9b, 0x41, 0xbc,
        0x96,
    ];
    let context = b"RFID-A\0";
    let num_keys = 16;
    let key_length = 6;
    let total_length = num_keys * key_length; // 96 bytes

    // HKDF-Extract: PRK = HMAC-Hash(salt, IKM)
    let mut extract = Hmac::<Sha256>::new_from_slice(&master).unwrap();
    extract.update(uid);
    let prk = extract.finalize().into_bytes();

    // HKDF-Expand: Generate enough blocks
    let hash_len = 32; // SHA256 output length
    #[allow(clippy::manual_div_ceil)]
    let n = (total_length + hash_len - 1) / hash_len; // Number of blocks needed
    let mut okm = Vec::with_capacity(total_length);
    let mut t = Vec::new();

    for i in 1..=n {
        let mut expand = Hmac::<Sha256>::new_from_slice(&prk).unwrap();
        expand.update(&t);
        expand.update(context);
        expand.update(&[i as u8]);
        t = expand.finalize().into_bytes().to_vec();
        okm.extend_from_slice(&t);
    }
    BambulabKeys { okm }
}
