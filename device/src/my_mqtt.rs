use alloc::boxed::Box;
use alloc::ffi::CString;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use embassy_time::with_timeout;
use core::cell::RefCell;
use core::cmp::min;
use embassy_futures::select::Either;
use embassy_futures::select::Either3;
use embassy_net::tcp::State;
use embassy_net::tcp::TcpSocket;
use embassy_net::IpEndpoint;
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::pubsub::PubSubChannel;
use embassy_time::Duration;
use embassy_time::Timer;
use embedded_io_async::Write;
use esp_mbedtls::TlsError;
use esp_mbedtls::TlsReference;
use esp_mbedtls::X509;
use mqttrust::encoding::v4::decode_slice;
use mqttrust::{
    encoding::v4::{encode_slice, Connect, Pid, Protocol},
    MqttError, Packet, Subscribe, SubscribeTopic,
};

use framework::prelude::*;

use crate::app_config::AppConfig;

#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
#[allow(dead_code)]
pub enum MyMqttError {
    MqttError(MqttError),
    TlsError(TlsError),
    EncodingError(mqttrust::encoding::v4::Error),
    WriteTimeoutError,
    RecvMessageTooLarge(usize),
}

impl From<TlsError> for MyMqttError {
    fn from(err: TlsError) -> Self {
        MyMqttError::TlsError(err)
    }
}

impl From<MqttError> for MyMqttError {
    fn from(err: MqttError) -> Self {
        MyMqttError::MqttError(err)
    }
}

impl From<mqttrust::encoding::v4::utils::Error> for MyMqttError {
    fn from(err: mqttrust::encoding::v4::utils::Error) -> Self {
        MyMqttError::EncodingError(err)
    }
}

const INITIAL_MQTT_BUFFER_SIZE: usize = 32768;
const MAX_MQTT_BUFFER_SIZE: usize = 49152;
const MQTT_BUFFER_SIZE_GROW_STEPS: usize = 8192;

pub struct MyMqtt<'a, T>
where
    T: embedded_io_async::Read + embedded_io_async::Write,
{
    tls: esp_mbedtls::asynch::Session<'a, T>,
    buf: Vec<u8>,
    message_bytes_in_buf: usize,
    data_bytes_in_buf: usize,
    write_timeout: Duration,
}

impl<'a, T> MyMqtt<'a, T>
where
    T: embedded_io_async::Read + embedded_io_async::Write,
{
    pub fn new(tls: esp_mbedtls::asynch::Session<'a, T>, write_timeout: Duration) -> MyMqtt<'a, T> {
        MyMqtt {
            tls,
            buf: vec![0u8; INITIAL_MQTT_BUFFER_SIZE],
            message_bytes_in_buf: 0,
            data_bytes_in_buf: 0,
            write_timeout,
        }
    }

    pub async fn connect(&mut self, keep_alive_secs: u16, username: Option<&'a str>, password: Option<&'a [u8]>) -> Result<(), MyMqttError> {
        // Connect MQTT

        // let mac: [u8;6] = esp_hal::efuse::Efuse::mac_address();
        // let _mac_hex = alloc::format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        //                   mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);

        let connect = Packet::Connect(Connect {
            protocol: Protocol::MQTT311,
            keep_alive: keep_alive_secs,
            client_id: "", // &mac_hex
            clean_session: true,
            last_will: None,
            username,
            password,
        });

        self.write(connect).await?;
        let resp = self.read().await?;
        // TODO: handle various connack response options
        match resp {
            Some(mqttrust::Packet::Connack(mqttrust::encoding::v4::Connack { session_present: _, code: _ })) => {}
            _ => {
                warn!("Unexpected connect response {:?}", resp);
            }
        }

        Ok(())
    }
    pub async fn subscribe<'b: 'a>(&mut self, _pid: Option<Pid>, topics: &[SubscribeTopic<'_>]) -> Result<(), MyMqttError> {
        let subscribe = Subscribe::new(topics);
        let packet = Packet::Subscribe(subscribe);

        self.write(packet).await?;
        let resp = self.read().await?;
        // TODO: handle various connack response options
        match resp {
            Some(mqttrust::Packet::Suback(mqttrust::encoding::v4::Suback { pid, return_codes })) => {
                warn!("Suback received with {:?}, {:?}", pid, return_codes);
            }
            _ => {
                warn!("Unexpected subscribe response {:?}", resp);
            }
        }

        // TODO: Need to wait to response before Ok?

        Ok(())
    }

    async fn write(&mut self, packet: mqttrust::Packet<'_>) -> Result<(), MyMqttError> {
        let mut buf = [0u8; 1024];
        let len = encode_slice(&packet, &mut buf)?;
        let write_timeout_res = with_timeout(self.write_timeout, self.tls.write_all(&buf[..len])).await;
        match write_timeout_res {
            Ok(write_res) => {
                match write_res {
                    Ok(_) => {
                        let flush_res = with_timeout(self.write_timeout, self.tls.flush()).await;
                        match flush_res {
                            Ok(v) => {
                                return Ok(v?);
                            }
                            Err(_) => {
                                return Err(MyMqttError::WriteTimeoutError)
                            }
                        }
                    }
                    Err(e) => return Err(e.into())
                }
            }
            Err(_) => return Err(MyMqttError::WriteTimeoutError),
        }
    }

    async fn write_pingreq(&mut self) -> Result<(), MyMqttError> {
        let packet = mqttrust::Packet::Pingreq;
        self.write(packet).await
    }

    pub async fn read(&mut self) -> Result<Option<Packet>, MyMqttError> {
        self.buf.copy_within(self.message_bytes_in_buf.., 0);
        self.data_bytes_in_buf -= self.message_bytes_in_buf;
        self.message_bytes_in_buf = 0;

        loop {
            // Start by checking if there's data from previous round (unlikely, but theoretically could)

            let mut offset = 0;
            if self.data_bytes_in_buf >= 4 {
                // minimal size is 4 bytes, so no point waisting time on less
                let read_header_res = mqttrust::encoding::v4::decoder::read_header(&self.buf[..self.data_bytes_in_buf], &mut offset);
                // read_header returns Some only if we have a full packet
                // but will return error if invalid header
                match read_header_res {
                    Ok(Some((_header, remaining_len))) =>  {
                        let decode_val_res = mqttrust::encoding::v4::decode_slice(&self.buf[..self.data_bytes_in_buf]);
                        match decode_val_res {
                            Ok(decode_val) => {
                                self.message_bytes_in_buf = offset + remaining_len;
                                return Ok(decode_val);
                            }
                            Err(decode_err) =>  {
                                error!("MQTT body parse issues, throwing read data, {} bytes", self.data_bytes_in_buf);
                                self.message_bytes_in_buf = 0;
                                self.data_bytes_in_buf = 0;
                                return Err(decode_err.into());
                            }
                        } 
                    }
                    Ok(None) => (),
                    Err(e) => {
                        error!("MQTT header parse issues, throwing read data, {} bytes", self.data_bytes_in_buf);
                        self.message_bytes_in_buf = 0;
                        self.data_bytes_in_buf = 0;
                        return Err(e.into());
                    }
                }
            }

            // increase buffer if no room
            if self.data_bytes_in_buf >= self.buf.len() {
                if self.buf.len() < MAX_MQTT_BUFFER_SIZE {
                    let add_capacity = min(MQTT_BUFFER_SIZE_GROW_STEPS, MAX_MQTT_BUFFER_SIZE - self.buf.len());
                    debug!("Adding {add_capacity} to MQTT Buffer, from {} to {}", self.buf.len(), self.buf.len()+add_capacity);
                    self.buf.resize(self.buf.len() + add_capacity, 0);
                } else {
                    let data_thrown = self.data_bytes_in_buf;
                    self.data_bytes_in_buf = 0;
                    self.message_bytes_in_buf = 0;
                    return Err(MyMqttError::RecvMessageTooLarge(data_thrown));
                }
            }
            // read data, theoretically if we are stuck waiting for data for some time and datat exists but not valid
            // then probably need to throw it a way, but so far didn't encounter situations to susect this happened
            let read_len = match self.tls.read(&mut self.buf[self.data_bytes_in_buf..]).await {
                Ok(n) => n,
                Err(e) => {
                    error!("TLS Error {:?}", e);
                    return Err(MyMqttError::TlsError(e));
                }
            };
            self.data_bytes_in_buf += read_len;
        }
    }
}

#[derive(Clone, Debug)]
pub struct Publish {
    pub dup: bool,
    pub qos: mqttrust::QoS,
    pub pid: Option<Pid>,
    pub retain: bool,
    pub topic_name: String,
    pub payload: Box<[u8]>,
}

impl<'a> From<mqttrust::Publish<'a>> for Publish {
    fn from(v: mqttrust::Publish) -> Self {
        Self {
            dup: v.dup,
            qos: v.qos,
            pid: v.pid,
            retain: v.retain,
            topic_name: String::from(v.topic_name),
            payload: Vec::<u8>::from(v.payload).into_boxed_slice(),
        }
    }
}

impl<'a> From<&'a Publish> for mqttrust::Publish<'a> {
    fn from(v: &'a Publish) -> Self {
        Self {
            dup: v.dup,
            qos: v.qos,
            pid: v.pid,
            retain: v.retain,
            topic_name: &v.topic_name,
            payload: &v.payload,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BufferedMqttPacket {
    raw: Vec<u8>,
}

impl<'a> TryFrom<mqttrust::Packet<'a>> for BufferedMqttPacket {
    type Error = mqttrust::encoding::v4::Error;

    fn try_from(v: mqttrust::Packet) -> Result<Self, Self::Error> {
        let mut raw = vec![0u8; v.len()];
        match encode_slice(&v, &mut raw) {
            Err(e) => Err(e),
            Ok(_) => Ok(Self { raw }),
        }
    }
}
impl<'a> TryFrom<&'a BufferedMqttPacket> for mqttrust::Packet<'a> {
    type Error = mqttrust::encoding::v4::Error;
    fn try_from(v: &'a BufferedMqttPacket) -> Result<Self, Self::Error> {
        match decode_slice(&v.raw) {
            Err(e) => Err(e),
            Ok(Some(p)) => Ok(p),
            Ok(None) => {
                panic!()
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum PacketOnChannel {
    Unknown(),
    Publish(Publish),
}

impl<'a> From<mqttrust::Packet<'a>> for PacketOnChannel {
    fn from(v: mqttrust::Packet) -> Self {
        match v {
            mqttrust::Packet::Publish(publish) => PacketOnChannel::Publish(Publish::from(publish)),
            _ => PacketOnChannel::Unknown(),
        }
    }
}
impl<'a> From<&'a PacketOnChannel> for mqttrust::Packet<'a> {
    fn from(v: &'a PacketOnChannel) -> Self {
        match v {
            PacketOnChannel::Publish(publish) => mqttrust::Packet::Publish(mqttrust::Publish::from(publish)),
            _ => {
                panic!()
            }
        }
    }
}

// Not Embassy Task since use generics
#[allow(clippy::too_many_arguments)]
pub async fn generic_mqtt_task<
    E: Into<IpEndpoint> + core::fmt::Debug + core::marker::Copy,
    const SOCKET_RX_SIZE: usize,
    const SOCKET_TX_SIZE: usize,
    M: RawMutex,
    const N: usize,
    const CAP: usize,
    const SUBS: usize,
    const PUBS: usize,
>(
    remote_endpoint: E,
    printer_serial: &String,
    username: Option<&str>,
    password: Option<Vec<u8>>,
    keep_alive_secs: u16,
    subscribe_topics: &[SubscribeTopic<'_>],
    stack: Stack<'static>,
    write_packets: &'static Channel<M, BufferedMqttPacket, N>,
    read_packets: &'static PubSubChannel<M, BufferedMqttPacket, CAP, SUBS, PUBS>,
    socket_rx_buffer: &'static mut [u8; SOCKET_RX_SIZE],
    socket_tx_buffer: &'static mut [u8; SOCKET_TX_SIZE],
    write_timeout: Duration,
    // mut rsa: esp_hal::peripherals::RSA,
    app_config: Rc<RefCell<AppConfig>>,
    // mut sha: impl esp_hal::peripheral::Peripheral<P = esp_hal::peripherals::SHA>,
    tls: TlsReference<'static>,
) -> ! {
    // let tls = Tls::new(&mut sha)
    //     .unwrap()
    //     .with_hardware_rsa(&mut rsa);

    'establish_communication: loop {
        let mut socket = TcpSocket::new(stack, socket_rx_buffer, socket_tx_buffer);

        loop {
            if let Some(_config) = stack.config_v4() {
                break;
            }
            Timer::after(Duration::from_millis(500)).await;
        }

        if socket.state() != State::Closed {
            socket.abort();
        }

        let endpoint: IpEndpoint = remote_endpoint.into();
        let port = endpoint.port;
        let embassy_net::IpAddress::Ipv4(addr) = endpoint.addr else { todo!() }; // Ipv6 should not happen
        let octets = addr.octets();

        term_info!("Connecting to Printer {}.{}.{}.{}:{}", octets[0], octets[1], octets[2], octets[3], port);
        match socket.connect(remote_endpoint).await {
            Ok(()) => (),
            Err(e) => {
                // match e {
                //     ConnectError::InvalidState | ConnectError::ConnectionReset => {
                //     }
                //     ConnectError::TimedOut => (),
                //     ConnectError::NoRoute => (),
                // }
                term_error!("Unexpected error connecting socket {:?}", e);
                Timer::after(Duration::from_millis(500)).await;
                continue;
            }
        }

        term_info!("Connected to Printer");

        let servername = CString::new(printer_serial.clone()).unwrap();

        let mut session = match esp_mbedtls::asynch::Session::new(
            socket,
            esp_mbedtls::Mode::Client {
                servername: &servername.as_c_str(),
            },
            esp_mbedtls::TlsVersion::Tls1_2,
            esp_mbedtls::Certificates {
                ca_chain: X509::pem(concat!(include_str!("./certs/bambulab.pem"), "\0").as_bytes()).ok(),
                ..Default::default()
            },
            tls,
        ) {
            Ok(tls_starter) => tls_starter,
            Err(e) => {
                term_error!("Error establishing TLS Connection {:?}", e);
                Timer::after(Duration::from_millis(500)).await;
                continue;
            }
        };

        term_info!("Establishing TLS connection with Printer");

        if let Err(e) = session.connect().await {
            // any point in retrying several times when tls fail?
            term_error!("Unexpected error during tls handshake {:?}", e);
            Timer::after(Duration::from_millis(500)).await;
            continue;
        }

        term_info!("TLS connection with Printer established");

        term_info!("Establishing MQTT connection with Printer");
        let mut my_mqtt = MyMqtt::new(session, write_timeout);

        if let Err(e) = my_mqtt.connect(keep_alive_secs, username, password.as_deref()).await {
            // any point in retrying mqtt connect ?
            term_error!("Unexpected error during mqtt connect {:?}", e);
            Timer::after(Duration::from_millis(500)).await;
            continue;
        }
        term_info!("MQTT connection with Printer established");

        term_info!("Subscribing to Printer reports");
        if let Err(e) = my_mqtt.subscribe(None, subscribe_topics).await {
            // any point in retrying mqtt subscribe ?
            term_error!("Unexpected error during mqtt subscribe {:?}", e);
            Timer::after(Duration::from_millis(500)).await;
            continue;
        }

        term_info!("Subscription to Printer reports confirmed");
        app_config.borrow_mut().report_printer_connectivity(true);

        let publisher = read_packets.immediate_publisher();

        loop {
            let res = if keep_alive_secs != 0 {
                embassy_futures::select::select3(my_mqtt.read(), write_packets.receive(), Timer::after_secs(keep_alive_secs.into())).await
            } else {
                let res = embassy_futures::select::select(my_mqtt.read(), write_packets.receive()).await;
                let res_as_select3_res = match res {
                    Either::First(v) => Either3::First(v),
                    Either::Second(v) => Either3::Second(v),
                };
                res_as_select3_res
            };

            match res {
                // First : Receive
                Either3::First(res) => match res {
                    Ok(Some(packet)) => match BufferedMqttPacket::try_from(packet) {
                        Ok(p) => {
                            // publish internally the received packet
                            publisher.publish_immediate(p);
                        }
                        Err(e) => {
                            term_error!("Error converting internal packets data on read {:?}", e);
                        }
                    },
                    Ok(None) => {
                        term_error!("MQTT Recv:  None Packet");
                    }
                    Err(MyMqttError::TlsError(e)) => {
                        term_error!("TLS Error on receive {:?}", e);
                        continue 'establish_communication;
                    }
                    Err(e) => {
                        term_error!("MQTT Recv: Error {:?}", e);
                    }
                },
                // Second: Write Request
                Either3::Second(packet) => match mqttrust::Packet::try_from(&packet) {
                    Ok(p) => {
                        if let Err(e) = my_mqtt.write(p).await {
                            term_error!("MQTT write error: {:?}\nReconnecting...", e);
                            // any point retrying?
                            continue 'establish_communication;
                        }
                    }
                    Err(e) => {
                        term_error!("Error converting between internal packets on write {:?}", e);
                    }
                },
                Either3::Third(()) => {
                    if let Err(e) = my_mqtt.write_pingreq().await {
                        term_error!("MQTT Send: ping message error: {:?}", e);
                        // any point retrying?
                        continue 'establish_communication;
                    }
                }
            }
        }
    }
}
