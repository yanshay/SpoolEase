use alloc::boxed::Box;
use alloc::ffi::CString;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use esp_mbedtls::TlsReference;
use esp_mbedtls::X509;
use core::cell::RefCell;
use embassy_futures::select::select;
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

pub struct MyMqtt<'a, T>
where
    T: embedded_io_async::Read + embedded_io_async::Write,
{
    tls: esp_mbedtls::asynch::Session<'a, T>,
    buf: [u8; 8192],
    write_timeout: Duration,
}

impl<'a, T> MyMqtt<'a, T>
where
    T: embedded_io_async::Read + embedded_io_async::Write,
{
    pub fn new(tls: esp_mbedtls::asynch::Session<'a, T>, write_timeout: Duration) -> MyMqtt<'a, T> {
        MyMqtt {
            tls,
            buf: [0u8; 8192],
            write_timeout,
        }
    }

    pub async fn connect(&mut self, keep_alive_secs: u16, username: Option<&'a str>, password: Option<&'a [u8]>) -> Result<(), MyMqttError> {
        // Connect MQTT
        let connect = Packet::Connect(Connect {
            protocol: Protocol::MQTT311,
            keep_alive: keep_alive_secs,
            client_id: "", //self.client_id(),
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
        let res = select(self.tls.write_all(&buf[..len]), Timer::after(self.write_timeout)).await;
        match res {
            Either::First(write_res) => Ok(write_res?),
            Either::Second(_) => Err(MyMqttError::WriteTimeoutError),
        }
    }

    async fn write_pingreq(&mut self) -> Result<(), MyMqttError> {
        let packet = mqttrust::Packet::Pingreq;
        self.write(packet).await
    }

    pub async fn read(&mut self) -> Result<Option<Packet>, MyMqttError> {
        let len = match self.tls.read(&mut self.buf).await {
            Ok(n) => n,
            Err(e) => {
                error!("TLS Error {:?}", e);
                return Err(MyMqttError::TlsError(e));
            }
        };

        // TODO: Fix this ugly code ...
        // The code as it is now handles up to two tls/tcp read packets for larger packets
        // This is enough for now for bambu messages, but may not be with more AMS's
        // But need to make it more generic to handle larger mqtt messages
        let decode_val = mqttrust::encoding::v4::decode_slice(&self.buf[..len])?;
        if decode_val.is_some() {
            // due to a limitation in rust borrow checker, need to decode again and return the new value
            // otherwise it reports a false borrow checker isue
            // with RUSTFLAGS="-Zpolonius" cargo check --release  , it compiles ok also w/o this line
            let decode_val = mqttrust::encoding::v4::decode_slice(&self.buf[..len])?;
            return Ok(decode_val);
        }

        let len2 = match self.tls.read(&mut self.buf[len..]).await {
            Ok(n) => n,
            Err(e) => {
                warn!("TLS Error {:?}", e);
                return Err(MyMqttError::TlsError(e));
            }
        };

        let decode_val = mqttrust::encoding::v4::decode_slice(&self.buf[..len + len2])?;
        Ok(decode_val)
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
    tls: TlsReference<'static>
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
                servername: &servername.as_c_str()
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
            let res = embassy_futures::select::select3(my_mqtt.read(), write_packets.receive(), Timer::after_secs(keep_alive_secs.into())).await;

            match res {
                // First : Receive
                Either3::First(res) => match res {
                    Ok(Some(packet)) => match BufferedMqttPacket::try_from(packet) {
                        Ok(p) => {
                            publisher.publish_immediate(p);
                        }
                        Err(e) => {
                            term_error!("Error converting internal packets data on read {:?}", e);
                        }
                    },
                    Ok(None) => {
                        term_error!("Received a None Packet");
                    }
                    Err(MyMqttError::TlsError(e)) => {
                        term_error!("TLS Error on receive {:?}", e);
                        continue 'establish_communication;
                    }
                    Err(e) => {
                        term_error!("Mqtt Receive Error {:?}", e);
                    }
                },
                // Second: Write Request
                Either3::Second(packet) => match mqttrust::Packet::try_from(&packet) {
                    Ok(p) => {
                        if let Err(e) = my_mqtt.write(p).await {
                            term_error!("Error writing mqtt message, error: {:?}", e);
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
                        term_error!("Error writing ping message, error: {:?}", e);
                        // any point retrying?
                        continue 'establish_communication;
                    }
                }
            }
        }
    }
}
