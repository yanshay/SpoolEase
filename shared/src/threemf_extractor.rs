use core::error::Error;
use core::mem;

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec::Vec,
};
use miniz_oxide::{
    MZFlush, MZStatus, StreamResult,
    inflate::{self, stream::InflateState},
};

#[derive(Default)]
enum State {
    #[default]
    Signature,
    FileName,
    ExtraField,
    FileData,
    Done,
}

#[derive(Default)]
pub struct ThreemfExtractor {
    buffer: Vec<u8>,
    out_buffer: Vec<u8>,
    extract_filename: String,
    state: State,
    extra_field_length: usize,
    inflate_state: InflateState,
}

#[derive(Debug, PartialEq)]
pub enum FeedStatus {
    NeedMoreData,
    StreamEnded,
    OutputProcessorEnded,
}

impl ThreemfExtractor {
    pub fn new(extract_filename: &str, out_buffer_size: usize) -> Self {
        Self {
            extract_filename: extract_filename.to_string(),
            out_buffer: alloc::vec![0u8; out_buffer_size],
            ..Default::default()
        }
    }
    pub fn feed_data<F: FnMut(&[u8]) -> Result<bool, Box<dyn Error>>>(
        &mut self,
        buf: &[u8],
        mut process_output: F,
    ) -> Result<FeedStatus, Box<dyn Error>> {
        self.buffer.extend_from_slice(buf);
        let response = loop {
            match self.state {
                State::Signature => {
                    // println!(">>>> State::Signature");
                    // println!("{:x?}", self.buffer.windows(4).next().unwrap());
                    if self.buffer.len() >= 4 {
                        if let Some(pos) = self
                            .buffer
                            .windows(4)
                            .position(|w| w == LOCAL_FILE_HEADER_SIGNATURE_BYTES)
                        {
                            self.buffer.drain(..pos);
                            self.state = State::FileName;
                            continue;
                        } else {
                            self.buffer.drain(..self.buffer.len() - 3);
                            break FeedStatus::NeedMoreData;
                        }
                    } else {
                        break FeedStatus::NeedMoreData;
                    }
                }
                State::FileName => {
                    // println!(">>>> State::FileName");
                    if self.buffer.len() >= LOCAL_FILE_HEADER_LEN + self.extract_filename.len() {
                        if self.buffer[LOCAL_FILE_HEADER_LEN
                            ..LOCAL_FILE_HEADER_LEN + self.extract_filename.len()]
                            == *self.extract_filename.as_bytes()
                        {
                            // println!(">>>>>>>> Found filename");
                            let local_file_header =
                                unsafe { LocalFileHeader::from_bytes(&self.buffer) }.unwrap(); // header tested earlier so unwrap is ok
                            if local_file_header.file_name_length as usize
                                != self.extract_filename.len()
                            {
                                // in case matched part of a filename
                                self.buffer.drain(..4);
                                self.state = State::Signature;
                                continue;
                            }
                            self.extra_field_length = local_file_header.extra_field_length as usize;
                            self.state = State::ExtraField;
                            self.buffer
                                .drain(..LOCAL_FILE_HEADER_LEN + self.extract_filename.len());
                            continue;
                        } else {
                            self.buffer.drain(..4);
                            self.state = State::Signature;
                            continue;
                        }
                    } else {
                        break FeedStatus::NeedMoreData;
                    }
                }
                State::ExtraField => {
                    // println!(">>>> State::ExtraField");
                    if self.buffer.len() >= self.extra_field_length {
                        self.buffer.drain(..self.extra_field_length);
                        self.state = State::FileData;
                    } else {
                        // println!("Not enough data for extra field");
                        break FeedStatus::NeedMoreData;
                    }
                }
                State::FileData => {
                    // println!(">>>> State::FileData");
                    let mut total_consumed = 0;
                    let mut done = false;
                    loop {
                        // let mut output = [0u8; 10];
                        let stream_res = inflate::stream::inflate(
                            &mut self.inflate_state,
                            &self.buffer[total_consumed..],
                            &mut self.out_buffer,
                            MZFlush::None,
                        );
                        let StreamResult {
                            bytes_consumed,
                            bytes_written,
                            status,
                        } = stream_res;
                        // println!("bytes_consumed: {bytes_consumed}, bytes_written {bytes_written}, Status: {status:?}");
                        total_consumed += bytes_consumed;
                        if bytes_written != 0 {
                            // let output_str =
                            // core::str::from_utf8(&output[..bytes_written]).unwrap();
                            // print!("{output_str}");
                            let need_continue = process_output(&self.out_buffer[..bytes_written])?;
                            if !need_continue {
                                return Ok(FeedStatus::OutputProcessorEnded);
                            }
                            if total_consumed == self.buffer.len() {
                                break;
                            }
                            continue;
                        }
                        match status {
                            Ok(ok_status) => match ok_status {
                                MZStatus::Ok => (),
                                MZStatus::StreamEnd => {
                                    done = true;
                                    break;
                                }
                                MZStatus::NeedDict => {
                                    return Err("Deflate unexpected status 'NeedDict'"
                                        .to_string()
                                        .into());
                                }
                            },
                            Err(err) => {
                                return Err(format!("Deflate error : {err:?}").into());
                            }
                        }
                        if total_consumed == self.buffer.len() {
                            break;
                        }
                    }
                    self.buffer.drain(..self.buffer.len());
                    if done {
                        self.state = State::Done;
                        break FeedStatus::StreamEnded;
                    } else {
                        self.state = State::FileData;
                        break FeedStatus::NeedMoreData;
                    }
                }
                State::Done => {
                    // println!("Done");
                    self.buffer.clear();
                    break FeedStatus::StreamEnded;
                }
            }
        };
        Ok(response)
    }
}

const LOCAL_FILE_HEADER_LEN: usize = mem::size_of::<LocalFileHeader>();
const LOCAL_FILE_HEADER_SIGNATURE: u32 = 0x04034b50;
const LOCAL_FILE_HEADER_SIGNATURE_BYTES: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];

#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
struct LocalFileHeader {
    signature: u32,
    version_needed_to_extract: u16,
    general_purpose_bit_flag: u16,
    compression_method: u16,
    last_mod_file_time: u16,
    last_mod_file_date: u16,
    crc32: u32,
    compressed_size: u32,
    uncompressed_size: u32,
    file_name_length: u16,
    extra_field_length: u16,
}

impl LocalFileHeader {
    pub unsafe fn from_bytes(ptr: &[u8]) -> Option<&Self> {
        unsafe {
            (ptr.as_ptr() as *const Self).as_ref().and_then(|h| {
                if matches!(h.signature, LOCAL_FILE_HEADER_SIGNATURE) {
                    Some(h)
                } else {
                    None
                }
            })
        }
    }
}
