use alloc::{rc::Rc, vec::Vec};
use core::{
    cell::RefCell,
    net::SocketAddr, str::FromStr,
};
use edge_http::{
    io::client::Connection,
    ws::{MAX_BASE64_KEY_LEN, MAX_BASE64_KEY_RESPONSE_LEN, NONCE_LEN},
};
use edge_nal_embassy::{Tcp, TcpBuffers};
use edge_ws::{FrameHeader, FrameType};
use embassy_executor::Spawner;
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embedded_io_async::Write;
use framework::{error, info, term_error, term_info, warn};
use shared::scale::ScaleToConsole;
use embassy_net::Stack;

use crate::ssdp::SSDPPubSubChannel;

//TODO: use the one in the future release of 'framework'
pub fn random_u32() -> u32 {
    let mut buf = [0u8; 4];
    getrandom::getrandom(&mut buf).unwrap();
    u32::from_le_bytes(buf)
}

pub struct SpoolScale {
    pub weight: i32,
    observers: Vec<alloc::rc::Weak<RefCell<dyn SpoolScaleObserver>>>,
}

pub trait SpoolScaleObserver {
    fn on_scale_loaded(&mut self, weight: i32);
    fn on_scale_load_changed(&mut self, weight: i32);
    fn on_scale_load_removed(&mut self);
}

impl SpoolScale {
    pub fn process_message(&mut self, _frame_header: &FrameHeader, payload: &[u8]) {
        let parse_res = serde_json::from_slice::<ScaleToConsole>(payload);
        if let Ok(scale_to_console) = parse_res {
            match scale_to_console {
                ScaleToConsole::NewLoad(weight) => {
                    self.weight = weight;
                    self.notify_scale_loaded(weight);
                }
                ScaleToConsole::LoadChanged(weight) => {
                    self.weight = weight;
                    self.notify_scale_load_changed(weight);
                }
                ScaleToConsole::LoadRemoved => {
                    self.weight = 0;
                    self.notify_scale_load_removed();
                }
                ScaleToConsole::WebConfigEnabled(_web_config_info) => todo!(),
            }
        }
    }

    pub fn subscribe(&mut self, observer: alloc::rc::Weak<RefCell<dyn SpoolScaleObserver>>) {
        self.observers.push(observer);
    }

    pub fn notify_scale_loaded(&self, weight: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_loaded(weight);
        }
    }
    pub fn notify_scale_load_changed(&self, weight: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_load_changed(weight);
        }
    }
    pub fn notify_scale_load_removed(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_load_removed();
        }
    }
}

pub fn init(stack: Stack<'static>, spawner: Spawner, ssdp_pub_sub: &'static SSDPPubSubChannel) -> Rc<RefCell<SpoolScale>> {
    let spool_scale_rc = Rc::new(RefCell::new(SpoolScale {
        weight: 0,
        observers: Vec::new(),
    }));
    spawner.spawn(spool_scale_task(stack, spool_scale_rc.clone(), ssdp_pub_sub)).ok();

    spool_scale_rc
}

#[embassy_executor::task]
pub async fn spool_scale_task(stack: Stack<'static>, spool_scale_rc: Rc<RefCell<SpoolScale>>, ssdp_pub_sub: &'static SSDPPubSubChannel) {
    info!("Task spool_scale_task started");
    loop {
        if let Some(_config) = stack.config_v4() {
            break;
        }
        Timer::after_millis(250).await;
    }

    // if bambu_printer.borrow().configured_printer_ip.is_none() {
    term_info!("No SpoolScale IP configured, discovering");
    let ip;
    let mut ssdp_subscribe = ssdp_pub_sub.subscriber().unwrap();
    loop {
        let ssdp_info = ssdp_subscribe.next_message().await;
        match ssdp_info {
            embassy_sync::pubsub::WaitResult::Lagged(_) => (),
            embassy_sync::pubsub::WaitResult::Message(ssdp_info) => {
                if ssdp_info.nt.contains("urn:spoolease-io:device:spoolscale") {
                    if let Ok(found_ip) = embassy_net::Ipv4Address::from_str(&ssdp_info.location) {
                        ip = found_ip;
                        term_info!("Discovered SpoolScale at {}", ip);
                        break;
                    }
                }
            }
        }
    }
    // } else {
    //     printer_ip = bambu_printer.borrow().configured_printer_ip.unwrap();
    //     printer_name = bambu_printer.borrow().configured_printer_name.clone().unwrap_or(String::from("Unknown"));
    // }

    let tcp_buffers = TcpBuffers::<1, 4096, 4096>::new();
    let tcp = Tcp::new(stack, &tcp_buffers);

    let mut first_connect = true;
    #[allow(unused_labels)]
    'connect_loop: loop {
        if first_connect {
            first_connect = false;
        } else {
            Timer::after_secs(2).await;
        }
        let mut conn_buf = [0_u8; 4096];
        let mut conn: Connection<_> = Connection::new(&mut conn_buf, &tcp, SocketAddr::new(core::net::IpAddr::V4(ip), 81));

        let mut nonce = [0_u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce).unwrap();
        let mut nonce_base64_buf = [0_u8; MAX_BASE64_KEY_LEN];

        term_info!("Connecting to SpoolScale");
        if let Err(err) = conn
            .initiate_ws_upgrade_request(Some("192.168.10.79"), None, "/ws", None, &nonce, &mut nonce_base64_buf)
            .await
        {
            term_error!("SpoolScale: Error initiating web socket request {:?}", err);
            continue 'connect_loop;
        }
        if let Err(err) = conn.initiate_response().await {
            term_error!("SpoolScale: Error initiating web socket response {:?}", err);
            continue 'connect_loop;
        }

        let mut buf = [0_u8; MAX_BASE64_KEY_RESPONSE_LEN];
        let upgrade_accepted_res = conn.is_ws_upgrade_accepted(&nonce, &mut buf);
        match upgrade_accepted_res {
            Ok(true) => (),
            Ok(false) => {
                term_error!("SpoolScale: Upgrading to websocket rejected");
                continue 'connect_loop;
            }
            Err(err) => {
                term_error!("SpoolScale: Error during websocket upgrade {:?}", err);
                continue 'connect_loop;
            }
        }

        if let Err(err) = conn.complete().await {
            error!("SpoolScale: Error completing the connection {:?}", err);
            return;
        }

        // Now we have the TCP socket in a state where it can be operated as a WS connection
        // Send some traffic to a WS echo server and read it back

        let (mut socket, buf) = conn.release();

        term_info!("Connection with SpoolScale established");

        loop {
            let timeout_for_ping = (random_u32() % 5000) + 5000;
            let with_timeout_res = with_timeout(Duration::from_millis(timeout_for_ping as u64), FrameHeader::recv(&mut socket)).await;
            let recv_header_res = match with_timeout_res {
                Ok(header) => header,
                Err(_timeout_err) => {
                    // Sending Ping on timeout
                    let now = Instant::now().as_ticks();
                    let ping_header = FrameHeader {
                        frame_type: FrameType::Ping,
                        payload_len: 8,
                        mask_key: None,
                    };
                    let res = ping_header.send(&mut socket).await;
                    match res {
                        Ok(_) => {
                            let res = ping_header.send_payload(&mut socket, &now.to_le_bytes()).await;
                            match res {
                                Ok(_) => {
                                    let res = socket.flush().await;
                                    match res {
                                        Ok(_) => {
                                            info!("SpoolScale: Sent Ping");
                                        }
                                        Err(err) => {
                                            error!("SpoolScale: Error sending Ping payload {err:?}, disconnecting");
                                            continue 'connect_loop;
                                        }
                                    }
                                }
                                Err(err) => {
                                    error!("SpoolScale: Error sending Ping payload {err:?}");
                                }
                            }
                        }
                        Err(err) => {
                            error!("SpoolScale: Error sending Ping header {err:?}");
                        }
                    }
                    continue;
                }
            };
            match recv_header_res {
                Ok(header) => {
                    let recv_payload_res = header.recv_payload(&mut socket, buf).await;
                    if let Ok(payload) = recv_payload_res {
                        match header.frame_type {
                            FrameType::Text(_fragmented) => {
                                spool_scale_rc.borrow_mut().process_message(&header, payload);
                            }
                            FrameType::Binary(_) => {
                                error!("Got binary message, header: {header}, payload: {payload:?}");
                            }
                            FrameType::Ping => {
                                let pong_header = FrameHeader {
                                    frame_type: FrameType::Pong,
                                    payload_len: header.payload_len,
                                    mask_key: header.mask_key,
                                };
                                let res = pong_header.send(&mut socket).await;
                                match res {
                                    Ok(_) => {
                                        let res = pong_header.send_payload(&mut socket, payload).await;
                                        match res {
                                            Ok(_) => {
                                                let res = socket.flush().await;
                                                match res {
                                                    Ok(_) => {
                                                        info!("SpoolScale: Received Ping, replied with Pong");
                                                    }
                                                    Err(err) => {
                                                        error!("SpoolScale: Error sending Pong reply {err:?}, disconnecting");
                                                        continue 'connect_loop;
                                                    }
                                                }
                                            }
                                            Err(err) => {
                                                error!("SpoolScale: Error sending Pong payload {err:?}");
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        error!("SpoolScale: Error sending Pong header {err:?}");
                                    }
                                }
                            }
                            FrameType::Pong => {
                                let tick_res: Result<&[u8; 8], _> = payload.try_into();
                                if let Ok(ticks) = tick_res {
                                    let ping_ticks = u64::from_le_bytes(*ticks);
                                    let ping_instant = Instant::from_ticks(ping_ticks);
                                    let elapsed_duration = ping_instant.elapsed();
                                    info!("SpoolScale: Ping-Pong duration was {} millis", elapsed_duration.as_millis());
                                } else {
                                    warn!("SpoolScale: Received pong wrongly formatted, header: {header:?}, payload: {payload:?}");
                                }
                            }
                            FrameType::Close => {
                                let close_resp_header = FrameHeader {
                                    frame_type: FrameType::Close,
                                    payload_len: header.payload_len,
                                    mask_key: header.mask_key,
                                };
                                let res = close_resp_header.send(&mut socket).await;
                                match res {
                                    Ok(_) => {
                                        let res = close_resp_header.send_payload(&mut socket, payload).await;
                                        match res {
                                            Ok(_) => {
                                                let res = socket.flush().await;
                                                match res {
                                                    Ok(_) => {
                                                        info!("SpoolScale: Replied to Close, disconnecting");
                                                        continue 'connect_loop;
                                                    }
                                                    Err(err) => {
                                                        error!("SpoolScale: Error sending Close reply {err:?}, disconnecting");
                                                        continue 'connect_loop;
                                                    }
                                                }
                                            }
                                            Err(err) => {
                                                error!("SpoolScale: Error sending Close Response payload {err:?}");
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        error!("SpoolScale: Error sending Close Response header {err:?}");
                                    }
                                }
                            }
                            FrameType::Continue(_fragmented) => {
                                warn!(
                                    "SpoolScale Recv(continue): header: {header}, payload: {}",
                                    core::str::from_utf8(payload).unwrap()
                                );
                            }
                        }

                        if !header.frame_type.is_final() {
                            warn!("SpoolScale: Unexpected fragmented frame header: {header:?}, payload: {payload:?}");
                        }
                    } else {
                        error!("SpoolScale: Error with websocket payload {:?}", recv_payload_res.err().unwrap());
                    }
                }
                Err(header_err) => {
                    match header_err {
                        edge_ws::Error::Incomplete(_) => todo!(),
                        edge_ws::Error::Invalid => todo!(),
                        edge_ws::Error::BufferOverflow => todo!(),
                        edge_ws::Error::InvalidLen => todo!(),
                        edge_ws::Error::Io(io_err) => {
                            error!("SpoolScale: Connection IO error while reading header, disconnecting {io_err:?}");
                            continue 'connect_loop;
                        } // edge_ws::Error::Io(io_err) => match io_err {
                          //     edge_nal_embassy::TcpError::General(error) => match error {
                          //         embassy_net::tcp::Error::ConnectionReset => todo!(),
                          //     },
                          //     edge_nal_embassy::TcpError::Connect(connect_error) => match connect_error {
                          //         embassy_net::tcp::ConnectError::InvalidState => todo!(),
                          //         embassy_net::tcp::ConnectError::ConnectionReset => todo!(),
                          //         embassy_net::tcp::ConnectError::TimedOut => todo!(),
                          //         embassy_net::tcp::ConnectError::NoRoute => todo!(),
                          //     },
                          //     edge_nal_embassy::TcpError::Accept(accept_error) => match accept_error {
                          //         embassy_net::tcp::AcceptError::InvalidState => todo!(),
                          //         embassy_net::tcp::AcceptError::InvalidPort => todo!(),
                          //         embassy_net::tcp::AcceptError::ConnectionReset => todo!(),
                          //     },
                          //     edge_nal_embassy::TcpError::NoBuffers => todo!(),
                          // },
                    }
                    // error!("SpoolScale: Error with websocket header {:?}", header_err);
                }
            }
        }
    }
}
