use core::cell::RefCell;

use alloc::{format, rc::Rc, string::ToString};
use embassy_time::Timer;
use framework::{error, prelude::Framework};

#[embassy_executor::task]
pub async fn ssdp_broadcast(framework: Rc<RefCell<Framework>>) {
    let stack = framework.borrow().stack;

    let device_name = framework.borrow().device_name.as_ref().unwrap_or(&"".to_string()).clone();

    let local_addr;
    loop {
        if let Some(config) = stack.config_v4() {
            local_addr = config.address;
            break;
        }
        Timer::after_millis(250).await;
    }

    let mut rx_buffer = [0; 0];
    let mut tx_buffer = [0; 512];
    let mut rx_meta = [embassy_net::udp::PacketMetadata::EMPTY; 16];
    let mut tx_meta = [embassy_net::udp::PacketMetadata::EMPTY; 16];

    let ssdp_multicast_endpoint = embassy_net::IpEndpoint {
        addr: embassy_net::Ipv4Address::new(239, 255, 255, 250).into(),
        port: 1990,
    };
    let local_endpoint = embassy_net::IpEndpoint {
        addr: local_addr.address().into(),
        port: 1900,
    };

    let mut socket1 = embassy_net::udp::UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    socket1.bind(local_endpoint).unwrap();

    let buf = format!(
        r#"NOTIFY * HTTP/1.1
HOST: 239.255.255.250:1900
Server: UPnP/1.0
Location: {}
NT: urn:spoolease-io:device:spoolscale:{}
USN: {}
Cache-Control: max-age=1800

"#,
        local_addr.address().to_string(),
        env!("CARGO_PKG_VERSION"),
        device_name,
    );

    let buf = buf.replace("\n", "\r\n");
    loop {
        let res = socket1.send_to(buf.as_bytes(), ssdp_multicast_endpoint).await;
        if res.is_err() {
            error!("Error sending SSDP {:?}", res.err().unwrap());
        }
        Timer::after_secs(5).await;
    }
}
