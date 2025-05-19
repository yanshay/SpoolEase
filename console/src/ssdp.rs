use alloc::string::{String, ToString};
use embassy_futures::select::{select, Either};
use embassy_net::Stack;
use framework::error;
use hashbrown::HashMap;

use crate::app::MAX_NUM_SSDP_LISTENERS;

pub type SSDPPubSubChannel =
    embassy_sync::pubsub::PubSubChannel<embassy_sync::blocking_mutex::raw::NoopRawMutex, SSDPInfo, 3, MAX_NUM_SSDP_LISTENERS, 1>;

#[derive(Clone, Debug, Default)]
pub struct SSDPInfo {
    pub nt: String,
    pub usn: String,
    pub location: String,
    pub custom: HashMap<String, String>,
}
impl SSDPInfo {
    pub fn is_valid(&self) -> bool {
        !(self.nt.is_empty() || self.location.is_empty())
    }
}

#[embassy_executor::task]
pub async fn ssdp_task(
    stack: Stack<'static>,
    ssdp_pub_sub: &'static embassy_sync::pubsub::PubSubChannel<
        embassy_sync::blocking_mutex::raw::NoopRawMutex,
        SSDPInfo,
        3,
        MAX_NUM_SSDP_LISTENERS,
        1,
    >,
) {
    let (mut rx_buffer1, mut rx_buffer2) = (alloc::vec![0; 512], alloc::vec![0; 512]);
    let (mut tx_buffer1, mut tx_buffer2) = ([0; 0], [0; 0]);
    let (mut rx_meta1, mut rx_meta2) = (
        [embassy_net::udp::PacketMetadata::EMPTY; 16],
        [embassy_net::udp::PacketMetadata::EMPTY; 16],
    );
    let (mut tx_meta1, mut tx_meta2) = (
        [embassy_net::udp::PacketMetadata::EMPTY; 16],
        [embassy_net::udp::PacketMetadata::EMPTY; 16],
    );
    let (mut buf1, mut buf2) = (alloc::vec![0; 512], alloc::vec![0; 512]);

    stack.join_multicast_group(embassy_net::Ipv4Address::new(239, 255, 255, 250)).unwrap();
    let recv_source_endpoint1 = embassy_net::IpEndpoint {
        addr: embassy_net::Ipv4Address::UNSPECIFIED.into(),
        port: 1990,
    };
    let mut recv_socket1 = embassy_net::udp::UdpSocket::new(stack, &mut rx_meta1, &mut rx_buffer1, &mut tx_meta1, &mut tx_buffer1);
    recv_socket1.bind(recv_source_endpoint1).unwrap();

    let recv_source_endpoint2 = embassy_net::IpEndpoint {
        addr: embassy_net::Ipv4Address::UNSPECIFIED.into(),
        port: 2021,
    };
    let mut recv_socket2 = embassy_net::udp::UdpSocket::new(stack, &mut rx_meta2, &mut rx_buffer2, &mut tx_meta2, &mut tx_buffer2);
    recv_socket2.bind(recv_source_endpoint2).unwrap();

    loop {
        // debug!("Waiting for SSDP UDP");

        let data = match select(recv_socket1.recv_from(&mut buf1), recv_socket2.recv_from(&mut buf2)).await {
            Either::First(Ok(inner_res)) => {
                let data = &buf1[0..inner_res.0];
                Ok(data)
            }
            Either::Second(Ok(inner_res)) => {
                let data = &buf2[0..inner_res.0];
                Ok(data)
            }
            _ => {
                error!("There was some error");
                Err("Error waiting for data")
            }
        };

        if let Ok(data) = data {
            if let Ok(s) = core::str::from_utf8(data) {
                let mut ssdp_notify = SSDPInfo::default();

                for line in s.lines() {
                    if let Some((first, second)) = line.split_once(" ") {
                        match first {
                            "NOTIFY" => (),
                            "HOST:" => (),
                            "Server:" => (),
                            "NT:" => ssdp_notify.nt = second.to_string(),
                            "Location:" => ssdp_notify.location = second.to_string(),
                            "USN:" => ssdp_notify.usn = second.to_string(),
                            _ => {
                                ssdp_notify.custom.insert(first.to_string(), second.to_string());
                            }
                        }
                    }
                }
                if ssdp_notify.is_valid() {
                    ssdp_pub_sub.publisher().unwrap().publish_immediate(ssdp_notify);
                }
            }
        }
    }
}
