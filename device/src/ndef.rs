use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::str;
use deku::prelude::*;

///////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
#[deku(id_type = "u8", bits = 3)]
enum TypeNameFormat {
    #[deku(id = 0)]
    Empty,
    #[deku(id = 1)]
    WellKnown,
    #[deku(id = 2)]
    MimeMediaType,
    #[deku(id = 3)]
    AbsoluteURI,
    #[deku(id = 4)]
    External,
    #[deku(id = 5)]
    Unknown,
    #[deku(id = 6)]
    Unchanged,
    #[deku(id = 7)]
    Reserved,
}

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
#[deku(id_type = "u8", bits = 3)]
enum WellKnownFormatType {
    #[deku(id = 0x54)] // T = Text
    Text,
    #[deku(id = 0x55)] // U = URI
    URI,
}
fn payload_length_reader<R: no_std_io::io::Read>(reader: &mut deku::reader::Reader<R>, short_record: bool) -> Result<u32, DekuError> {
    Ok(if short_record {
        u8::from_reader_with_ctx(reader, deku::ctx::Endian::Big)?.into()
    } else {
        u32::from_reader_with_ctx(reader, deku::ctx::Endian::Big)?
    })
}

fn payload_length_writer<W: no_std_io::io::Write>(
    writer: &mut deku::writer::Writer<W>,
    payload_length: u32,
    short_record: bool,
) -> Result<(), DekuError> {
    if short_record {
        let short_val: u8 = payload_length.try_into()?;
        short_val.to_writer(writer, deku::ctx::Endian::Big)?;
    } else {
        payload_length.to_writer(writer, deku::ctx::Endian::Big)?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
// #[deku(endian = "big")]
pub struct Record {
    // TNF and Flags
    #[deku(bits = 1)]
    message_begin: bool,
    #[deku(bits = 1)]
    message_end: bool,
    #[deku(bits = 1)]
    chunk_flag: bool,
    #[deku(bits = 1)]
    #[deku(update = "self.payload_data.len()<=255")]
    short_record: bool,
    #[deku(bits = 1)]
    id_length_is_present: bool,
    // #[deku(bits = 3)]
    type_name_format: TypeNameFormat,
    //
    #[deku(update = "self.type_data.len()")]
    type_length: u8,
    //
    #[deku(update = "self.payload_data.len()")]
    #[deku(
        reader = "payload_length_reader(deku::reader, *short_record)",
        writer = "payload_length_writer(deku::writer, *payload_length, *short_record)"
    )]
    payload_length: u32, // if short record it is one byte, otherwise it is four
    //
    #[deku(skip, cond = "!*id_length_is_present", default = "0")]
    id_length: u8, // exists only if id_length_is_present is true
    //
    #[deku(count = "type_length")]
    type_data: Vec<u8>, // length as specified in type_length field
    //
    #[deku(count = "id_length")]
    id_data: Vec<u8>, // length as specified in id_length, meaning only if id_length_present
    //
    #[deku(count = "payload_length")]
    payload_data: Vec<u8>, // length as specified in payload_length
}

impl Record {
    pub fn new_text_record_en(text: &str) -> Self {
        let mut payload = Vec::<u8>::with_capacity(3 + text.as_bytes().len());
        payload.extend_from_slice(&[0x02, b'e', b'n']);
        payload.extend_from_slice(text.as_bytes());
        Record {
            message_begin: true,
            message_end: true,
            chunk_flag: false,
            short_record: false,
            id_length_is_present: false,
            type_name_format: TypeNameFormat::WellKnown,
            type_length: 1,
            payload_length: 0, //text.len().into(),
            id_length: 0x34,
            type_data: Vec::from([0x54]), // 'T' for Text
            id_data: Vec::new(),
            payload_data: payload,
        }
    }
    pub fn en_text_payload(&self) -> String {
        if self.payload_data.len() > 3 {
            String::from(core::str::from_utf8(&self.payload_data[3..]).unwrap())
        } else {
            String::from("")
        }
    }

    pub fn new_url_record(url: &str) -> Self {
        let mut payload = Vec::<u8>::with_capacity(1 + url.as_bytes().len());
        // payload.extend_from_slice(&[0x02, b'e', b'n']);
        let mut sub_url = &url[0..url.len() - 1];
        if url.starts_with("http://www.") {
            payload.extend_from_slice(&[0x01]);
            sub_url = &url[11..];
        } else if url.starts_with("https://www.") {
            payload.extend_from_slice(&[0x02]);
            sub_url = &url[12..];
        } else if url.starts_with("http://") {
            payload.extend_from_slice(&[0x03]);
            sub_url = &url[7..];
        } else if url.starts_with("https://") {
            payload.extend_from_slice(&[0x04]);
            sub_url = &url[8..];
        } else {
            payload.extend_from_slice(&[0x00]);
        }
        payload.extend_from_slice(sub_url.as_bytes());
        Record {
            message_begin: true,
            message_end: true,
            chunk_flag: false,
            short_record: false,
            id_length_is_present: false,
            type_name_format: TypeNameFormat::WellKnown,
            type_length: 1,
            payload_length: 0, //text.len().into(),
            id_length: 0x34,
            type_data: Vec::from([0x55]), // 'U' for Text
            id_data: Vec::new(),
            payload_data: payload,
        }
    }
    pub fn url_payload(&self) -> String {
        if self.payload_data.len() > 0 {
            let prefix = match self.payload_data[0] {
                0x01 => "http://www.",
                0x02 => "https://www.",
                0x03 => "http://",
                0x04 => "https://",
                _ => "",
            };
            let url = core::str::from_utf8(&self.payload_data[1..]).unwrap();
            format!("{}{}", prefix, url)
        } else {
            String::from("")
        }
    }
}

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
pub struct NDEFStructure {
    // page 3 - capability container
    #[deku(update = "0xe1")]
    magic: u8,
    #[deku(update = "0x10")]
    doc_version: u8,
    #[deku(update = "(self.record.to_bytes().unwrap().len()+7)/8")] // need to set this to entire ndef space / 8
    ndef_size: u8,
    #[deku(update = "0x00")]
    read_write: u8,
    // page 4
    // tlv
    #[deku(update = "0x03")]
    message_start: u8,
    #[deku(update = "self.record.to_bytes().unwrap().len()")]
    message_size: u8,
    pub record: Record,
    #[deku(pad_bytes_after = "(4-((7+message_size)%4))%4")] // align structure to page size of 4
    #[deku(update = "0xfe")]
    termination_tlv: u8,
}
impl NDEFStructure {
    pub fn new(record: Record) -> Self {
        let mut res = NDEFStructure {
            magic: 0,
            doc_version: 0,
            ndef_size: 0,
            read_write: 0,
            message_start: 0,
            message_size: 0,
            record,
            termination_tlv: 0,
        };
        res.record.update().unwrap();
        res.update().unwrap();
        res
    }
}

/////////////////////////////////////////////////////////////////////////////////////////
