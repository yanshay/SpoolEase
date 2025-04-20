use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use deku::DekuContainerWrite;
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
const COMMAND_COMPLETE: [u8;2] = [R_APDU_SW1_COMMAND_COMPLETE, R_APDU_SW2_COMMAND_COMPLETE];

const R_APDU_SW1_NDEF_TAG_NOT_FOUND: u8 = 0x6a;
const R_APDU_SW2_NDEF_TAG_NOT_FOUND: u8 = 0x82;
const TAG_NOT_FOUND: [u8;2] = [R_APDU_SW1_NDEF_TAG_NOT_FOUND, R_APDU_SW2_NDEF_TAG_NOT_FOUND];

const R_APDU_SW1_FUNCTION_NOT_SUPPORTED: u8 = 0x6A;
const R_APDU_SW2_FUNCTION_NOT_SUPPORTED: u8 = 0x81;
const FUNCTION_NOT_SUPPORTED: [u8;2] = [R_APDU_SW1_FUNCTION_NOT_SUPPORTED, R_APDU_SW2_FUNCTION_NOT_SUPPORTED];

// const R_APDU_SW1_MEMORY_FAILURE: u8 = 0x65;
// const R_APDU_SW2_MEMORY_FAILURE: u8 = 0x81;

const R_APDU_SW1_END_OF_FILE_BEFORE_REACHED_LE_BYTES: u8 = 0x62;
const R_APDU_SW2_END_OF_FILE_BEFORE_REACHED_LE_BYTES: u8 = 0x82;
const END_OF_FILE_BEFORE_REACHED_LE_BYTES: [u8;2] = [R_APDU_SW1_END_OF_FILE_BEFORE_REACHED_LE_BYTES, R_APDU_SW2_END_OF_FILE_BEFORE_REACHED_LE_BYTES];

const NDEF_TAG_APPLICATION_NAME_V2: [u8; 9] = [0, 0x7, 0xD2, 0x76, 0x00, 0x00, 0x85, 0x01, 0x01];
const CAPABILITY_CONTAINER: [u8;15] = [
    0_u8,                                          // cc len msb
    0x0F,                                          // cc len lsb
    0x20,                                          // version 2.0
    ((NDEF_MAX_READ_LENGTH & 0xFF00) >> 8) as u8,  // Mle msb (Maximum data size that can be read using a single ReadBinary command.)
    (NDEF_MAX_READ_LENGTH & 0xFF) as u8,           // Mle lsb 
    ((NDEF_MAX_WRITE_LENGTH & 0xFF00) >> 8) as u8, // Mlc msb (Maximum data size that can be written using a single UpdateBinary command)
    (NDEF_MAX_WRITE_LENGTH & 0xFF) as u8,          // Mlc lsb
    // NDEF TLV
    0x04,                                          // T - Tag ?
    0x06,                                          // L - Length of the value field
    0xE1,                                          // NDEF File Identifier byte 1
    0x04,                                          // NDEF File Identifier byte 2
    ((NDEF_MAX_LENGTH & 0xFF00) >> 8) as u8,       // Maximum NDEF file size Msb
    (NDEF_MAX_LENGTH & 0xFF) as u8,                // maximum NDEF file size Lsb
    0x00,                                          // read access 0x0 = granted
    0x00,                                          // write access 0x0 = granted | 0xFF = deny
];

#[repr(u8)]
#[derive(Clone, Copy, Debug)]
enum TagFile {
    NONE = 0,
    CC =   1, // Capability Container
    NDEF = 2,
}

const NDEF_MAX_LENGTH: usize = 1024; // arbitrary size, defines tag max size, relevant mostly for write (currently not implemented)
const NDEF_MAX_READ_LENGTH: usize = 254;
const NDEF_MAX_WRITE_LENGTH: usize = 256;

pub async fn emulate_tag<I, T, const N: usize>(
    pn532: &mut pn532::Pn532<I, T, N>,
    ndef_record: crate::ndef::Record,
    short_uid: Option<[u8; 3]>,
) -> Result<(), String>
where
    I: pn532::Interface,
    T: pn532::CountDown<Time = embassy_time::Duration>,
{
    // info!("---- Sending TG_INIT_AS_TARGET");
    match pn532
        .process(
            &pn532::Request::tg_init_as_target(None, short_uid),
            37,
            Duration::from_secs(60),
        )
        .await
    {
        Ok(_v) => {
            // info!("TG_INIT_AS_TARGET response: {:x?}", v);
        }
        Err(err) => {
            return Err(format!("Failed to communicate with Tag Reader: {err:?}"));
        }
    }


    let ndef_structure = crate::ndef::NDEFStructureType4::new(ndef_record);

    let ndef_bytes = match ndef_structure.to_bytes() {
        Ok(v) => v,
        Err(err) => return Err(format!("Failed to serialize NDEF record: {err:?}"))
    };

    let mut current_file = TagFile::NONE;
    let mut send_buf = [0u8; NDEF_MAX_READ_LENGTH+2];
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
                        break; // and return Ok
                    } else {
                        return Err("Received empty tgGetData response before sending entire NDEF".to_string());
                    }
                }

                let status = v[0];
                if status != 0 {
                    if sent_entire_ndef {
                        break; // and return Ok
                    } else {
                        return Err(format!("Received error status 0x{status:x} in tgGetData response before sending entire NDEF"));
                    }
                }
                let recv_buf = &v[1..];
                if recv_buf.len() < 5 {
                    return Err(format!("Not enough data in tgGetData response, only {} bytes", v.len()));
                }
                let p1 = recv_buf[C_APDU_P1];
                let p2 = recv_buf[C_APDU_P2];
                let lc = recv_buf[C_APDU_LC];
                let p1p2_length = ((p1 as usize) << 8) + p2 as usize;

                match recv_buf[C_APDU_INS] {
                    ISO7816_SELECT_FILE => {
                        match p1 {
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
                                    send_data = get_data_to_set(&mut send_buf, &[], &FUNCTION_NOT_SUPPORTED);
                                }
                            }
                            _ => {
                                warn!("SELECT-FILE -> Unhandled p1 {p1:x}");
                                return Err(format!("Unsupported SELECT-FILE command in tag emulator {p1:x}"));
                            }
                        }
                    }
                    ISO7816_READ_BINARY => {
                        match &current_file {
                            TagFile::NONE => {
                                send_data = get_data_to_set(&mut send_buf, &[], &TAG_NOT_FOUND);
                            }
                            TagFile::CC => {
                                if p1p2_length > NDEF_MAX_LENGTH {
                                    send_data = get_data_to_set(&mut send_buf, &[], &END_OF_FILE_BEFORE_REACHED_LE_BYTES);
                                } else if p1p2_length + lc as usize > CAPABILITY_CONTAINER.len() {
                                    send_data = get_data_to_set(&mut send_buf, &[], &END_OF_FILE_BEFORE_REACHED_LE_BYTES);
                                } else {
                                    send_data = get_data_to_set(&mut send_buf, &CAPABILITY_CONTAINER[p1p2_length..p1p2_length + lc as usize], &COMMAND_COMPLETE);
                                }
                            }
                            TagFile::NDEF => {
                                if p1p2_length > NDEF_MAX_LENGTH {
                                    send_data = get_data_to_set(&mut send_buf, &[], &END_OF_FILE_BEFORE_REACHED_LE_BYTES);
                                } else if p1p2_length + lc as usize > ndef_bytes.len() {
                                    send_data = get_data_to_set(&mut send_buf, &[], &END_OF_FILE_BEFORE_REACHED_LE_BYTES);
                                }
                                else {
                                    send_data = get_data_to_set(&mut send_buf, &ndef_bytes[p1p2_length..p1p2_length + lc as usize], &COMMAND_COMPLETE);
                                    if p1p2_length + lc as usize == ndef_bytes.len() {
                                        sent_entire_ndef = true;
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        warn!("Unhandled command in tag emulator {:x}", recv_buf[C_APDU_INS]);
                        return Err(format!("Unsupported NFC command in tag emulator {:x}", recv_buf[C_APDU_INS]));
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
    Ok(())
}

fn get_data_to_set<'a, 'b, 'c>(send_buf: &'a mut [u8], payload: &'b [u8], command: &'c [u8;2]) -> &'a [u8] {
    send_buf[..payload.len()].copy_from_slice(payload);
    send_buf[payload.len()..payload.len()+command.len()].copy_from_slice(command);

    &send_buf[..payload.len()+command.len()]
}

