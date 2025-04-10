use alloc::{boxed::Box, rc::Rc, string::ToString, vec::Vec};
use core::{cell::RefCell, net::SocketAddr, str::FromStr};
use edge_http::{
    io::client::Connection,
    ws::{MAX_BASE64_KEY_LEN, MAX_BASE64_KEY_RESPONSE_LEN, NONCE_LEN},
};
use edge_nal_embassy::{Tcp, TcpBuffers};
use edge_ws::{FrameHeader, FrameType};
use embassy_executor::Spawner;
use embassy_futures::select::select3;
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embassy_time::{Instant, Timer};
use embedded_io_async::Write;
use framework::{debug, error, info, mk_static, term_error, term_info, utils::random_u32, warn};
use shared::scale::{ConsoleToScale, ScaleToConsole};

use crate::{app_config::AppConfig, ssdp::SSDPPubSubChannel};

pub type ConsoleToScaleChannel = Channel<NoopRawMutex, ConsoleToScale, 5>;

#[allow(dead_code)]
pub enum ScaleWeight {
    Unknown,
    Stable(i32),
    Unstable(i32)
}

pub struct SpoolScale {
    pub weight: ScaleWeight,
    observers: Vec<alloc::rc::Weak<RefCell<dyn SpoolScaleObserver>>>,
    console_to_scale: &'static ConsoleToScaleChannel,
}

pub trait SpoolScaleObserver {
    fn on_scale_loaded(&mut self, weight: i32);
    fn on_scale_load_changed_stable(&mut self, weight: i32);
    fn on_scale_load_changed_unstable(&mut self, weight: i32);
    fn on_scale_load_removed(&mut self);
    fn on_scale_raw_samples_avg(&mut self, raw_data: i32);
    fn on_scale_connected(&mut self);
    fn on_scale_disconnected(&mut self);
    fn on_scale_uncalibrated(&mut self);
    fn on_term_text(&mut self, text: &str);
    fn on_tag_status(&mut self, status: &shared::spool_tag::Status);
    fn on_pn532_status(&mut self, status: bool);
}

impl SpoolScale {
    pub fn calibrate(&self, weight: i32) {
        self.console_to_scale
            .try_send(ConsoleToScale::Calibrate(weight))
            .unwrap_or_else(|e| error!("Failed sending calibrate request to scale {e:?}"));
    }

    pub fn process_message(&mut self, _frame_header: &FrameHeader, payload: &[u8]) {
        let parse_res = serde_json::from_slice::<ScaleToConsole>(payload);
        if let Ok(scale_to_console) = parse_res {
            match scale_to_console {
                ScaleToConsole::NewLoad(weight) => {
                    self.weight = ScaleWeight::Unstable(weight);
                    self.notify_scale_loaded(weight);
                }
                ScaleToConsole::LoadChangedUnstable(weight) => {
                    self.weight = ScaleWeight::Unstable(weight);
                    self.notify_scale_load_changed_unstable(weight);
                }
                ScaleToConsole::LoadChangedStable(weight) => {
                    self.weight = ScaleWeight::Stable(weight);
                    self.notify_scale_load_changed_stable(weight);
                }
                ScaleToConsole::LoadRemoved => {
                    self.weight = ScaleWeight::Stable(0);
                    self.notify_scale_load_removed();
                }
                ScaleToConsole::RawSamplesAvg(raw_data) => {
                    self.notify_scale_raw_samples_avg(raw_data);
                }
                ScaleToConsole::WebConfigEnabled(_web_config_info) => todo!(),
                ScaleToConsole::Uncalibrated => {
                    self.notify_scale_uncalibrated();
                }
                ScaleToConsole::Term(text) => {
                    self.notify_term_text(&text);
                }
                ScaleToConsole::TagStatus(status) =>  {
                    self.notify_tag_status(&status);
                }
                ScaleToConsole::PN532Status(status) => {
                    self.notify_pn532_status(status);
                }
            }
        }
    }
    pub fn connected(&mut self) {
        self.notify_scale_connected();
    }
    pub fn disconnected(&mut self) {
        self.notify_scale_disconnected();
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
    pub fn notify_scale_load_changed_stable(&self, weight: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_load_changed_stable(weight);
        }
    }
    pub fn notify_scale_load_changed_unstable(&self, weight: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_load_changed_unstable(weight);
        }
    }
    pub fn notify_scale_load_removed(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_load_removed();
        }
    }
    pub fn notify_scale_raw_samples_avg(&self, raw_data: i32) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_raw_samples_avg(raw_data);
        }
    }
    pub fn notify_scale_connected(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_connected();
        }
    }
    pub fn notify_scale_disconnected(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_disconnected();
        }
    }
    pub fn notify_scale_uncalibrated(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_scale_uncalibrated();
        }
    }
    pub fn notify_term_text(&self, text: &str) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_term_text(text);
        }
    }
    pub fn notify_tag_status(&mut self, status: &shared::spool_tag::Status) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_tag_status(status);
        }
    }
    pub fn notify_pn532_status(&mut self, status: bool) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_pn532_status(status);
        }
    }
}

pub fn init(app_config: Rc<RefCell<AppConfig>>, stack: Stack<'static>, spawner: Spawner, ssdp_pub_sub: &'static SSDPPubSubChannel) -> Rc<RefCell<SpoolScale>> {
    let console_to_scale = mk_static!(ConsoleToScaleChannel, ConsoleToScaleChannel::new());

    let spool_scale_rc = Rc::new(RefCell::new(SpoolScale {
        weight: ScaleWeight::Unknown,
        observers: Vec::new(),
        console_to_scale,
    }));

    if let Some(spool_scale_config) = &app_config.clone().borrow().configured_scale {
        if spool_scale_config.available == true {
            spawner.spawn(spool_scale_task(app_config, stack, spool_scale_rc.clone(), ssdp_pub_sub)).ok();
        }
    }

    spool_scale_rc
}

#[embassy_executor::task]
pub async fn spool_scale_task(app_config: Rc<RefCell<AppConfig>>, stack: Stack<'static>, spool_scale_rc: Rc<RefCell<SpoolScale>>, ssdp_pub_sub: &'static SSDPPubSubChannel) {
    info!("Task spool_scale_task started");
    let console_to_scale = spool_scale_rc.borrow().console_to_scale;
    loop {
        if let Some(_config) = stack.config_v4() {
            break;
        }
        Timer::after_millis(250).await;
    }

    let mut configured_ip = None;
    let mut configured_name = None;
    let spoolscale_ip;

    if let Some(configured_scale) = &app_config.borrow().configured_scale {
        configured_ip = configured_scale.ip;
        configured_name = configured_scale.name.clone();
    }

    if configured_ip.is_none() {
        term_info!("No SpoolScale IP configured, discovering {}", configured_name.as_ref().unwrap_or(&"".to_string()));
        let mut ssdp_subscribe = ssdp_pub_sub.subscriber().unwrap();
        loop {
            let ssdp_info = ssdp_subscribe.next_message().await;
            match ssdp_info {
                embassy_sync::pubsub::WaitResult::Lagged(_) => (),
                embassy_sync::pubsub::WaitResult::Message(ssdp_info) => {
                    if ssdp_info.nt.contains("urn:spoolease-io:device:spoolscale") {
                        if let Some(spoolscale_name) = &configured_name {
                            if ssdp_info.usn != *spoolscale_name {
                                debug!("Found a SpoolScale, but with name {} and not {spoolscale_name}",ssdp_info.usn);
                                continue;
                            }
                        }
                        if let Ok(found_ip) = embassy_net::Ipv4Address::from_str(&ssdp_info.location) {
                            spoolscale_ip = found_ip;
                            term_info!("Discovered SpoolScale at {}", spoolscale_ip);
                            break;
                        }
                    }
                }
            }
        }
    } else {
        spoolscale_ip = configured_ip.unwrap();
    }

    let tcp_buffers = Box::new(TcpBuffers::<1, 1024, 1024>::new());
    let tcp = Tcp::new(stack, &tcp_buffers);
    let tcp = edge_nal::WithTimeout::new(10000, tcp);

    let mut first_connect = true;
    let mut connect_error_counter = 0;
    'connect_loop: loop {
        if first_connect {
            first_connect = false;
        } else {
            Timer::after_secs(2).await;
        }
        // let mut conn_buf = [0_u8; 1024];
        let mut conn_buf = alloc::vec![0_u8; 1024];
        let mut conn: Connection<_> = Connection::new(&mut conn_buf, &tcp, SocketAddr::new(core::net::IpAddr::V4(spoolscale_ip), 81));

        let mut nonce = [0_u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce).unwrap();
        let mut nonce_base64_buf = [0_u8; MAX_BASE64_KEY_LEN];
        if connect_error_counter % 5 == 0 {
            term_info!("Connecting to SpoolScale at {}", spoolscale_ip);
        }
        if let Err(err) = conn
            .initiate_ws_upgrade_request(Some(&spoolscale_ip.to_string()), None, "/ws", None, &nonce, &mut nonce_base64_buf)
            .await
        {
            if connect_error_counter % 5 == 0 && connect_error_counter != 0 {
                term_error!("SpoolScale: Error initiating web socket request {:?}", err);
            }
            connect_error_counter += 1;
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

        connect_error_counter = 0;

        term_info!("Connection with SpoolScale established");

        spool_scale_rc.borrow_mut().connected();

        'send_recv_loop: loop {
            // max timeout_for_ping need to be less than above WithTimeout wrapper
            let timeout_for_ping = (random_u32() % 5000) + 5000;
            let with_timeout_res = select3(
                Timer::after_millis(timeout_for_ping as u64),
                FrameHeader::recv(&mut socket),
                console_to_scale.receive(),
            )
            .await;
            match with_timeout_res {
                embassy_futures::select::Either3::First(_timeout_res) => {
                    // Sending Ping on timeout
                    let now = Instant::now().as_ticks();
                    let ping_header = FrameHeader {
                        frame_type: FrameType::Ping,
                        payload_len: 8,
                        mask_key: None,
                    };
                    let send_ping_header_res = ping_header.send(&mut socket).await;
                    match send_ping_header_res {
                        Ok(_) => {
                            let send_ping_payload_res = ping_header.send_payload(&mut socket, &now.to_le_bytes()).await;
                            match send_ping_payload_res {
                                Ok(_) => {
                                    let res = socket.flush().await;
                                    match res {
                                        Ok(_) => {
                                            debug!("SpoolScale: Sent Ping");
                                        }
                                        Err(send_ping_flush_err) => {
                                            error!("SpoolScale: Error sending Ping payload {send_ping_flush_err:?}, disconnecting");
                                            break 'send_recv_loop;
                                            // continue 'connect_loop;
                                        }
                                    }
                                }
                                Err(send_ping_payload_err) => {
                                    error!("SpoolScale: Error sending Ping payload {send_ping_payload_err:?}");
                                }
                            }
                        }
                        Err(send_ping_header_err) => {
                            error!("SpoolScale: Error sending Ping header {send_ping_header_err:?}");
                        }
                    }
                    // in case of timeut (which is the Err(_timeout_err) case we want to continue send_recv_loop
                    continue 'send_recv_loop;
                }
                embassy_futures::select::Either3::Second(from_scale_res) => {
                    match from_scale_res {
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
                                        let send_pong_header_res = pong_header.send(&mut socket).await;
                                        match send_pong_header_res {
                                            Ok(_) => {
                                                let res = pong_header.send_payload(&mut socket, payload).await;
                                                match res {
                                                    Ok(_) => {
                                                        let flush_res = socket.flush().await;
                                                        match flush_res {
                                                            Ok(_) => {
                                                                debug!("SpoolScale: Received Ping, replied with Pong");
                                                            }
                                                            Err(err) => {
                                                                error!("SpoolScale: Error sending Pong reply {err:?}, disconnecting");
                                                                break 'send_recv_loop;
                                                            }
                                                        }
                                                    }
                                                    Err(err) => {
                                                        error!("SpoolScale: Error sending Pong payload {err:?}");
                                                        break 'send_recv_loop;
                                                    }
                                                }
                                            }
                                            Err(err) => {
                                                error!("SpoolScale: Error sending Pong header {err:?}");
                                                break 'send_recv_loop;
                                            }
                                        }
                                    }
                                    FrameType::Pong => {
                                        let tick_res: Result<&[u8; 8], _> = payload.try_into();
                                        if let Ok(ticks) = tick_res {
                                            let ping_ticks = u64::from_le_bytes(*ticks);
                                            let ping_instant = Instant::from_ticks(ping_ticks);
                                            let elapsed_duration = ping_instant.elapsed();
                                            debug!("SpoolScale: Ping-Pong duration was {} millis", elapsed_duration.as_millis());
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
                                        let close_resp_header_res = close_resp_header.send(&mut socket).await;
                                        match close_resp_header_res {
                                            Ok(_) => {
                                                let close_resp_payload_res = close_resp_header.send_payload(&mut socket, payload).await;
                                                match close_resp_payload_res {
                                                    Ok(_) => {
                                                        let close_resp_flush_res = socket.flush().await;
                                                        match close_resp_flush_res {
                                                            Ok(_) => {
                                                                debug!("SpoolScale: Replied to Close, disconnecting");
                                                                break 'send_recv_loop;
                                                            }
                                                            Err(close_resp_flush_err) => {
                                                                error!(
                                                                    "SpoolScale: Error sending Close reply {close_resp_flush_err:?}, disconnecting"
                                                                );
                                                                break 'send_recv_loop;
                                                            }
                                                        }
                                                    }
                                                    Err(err) => {
                                                        error!("SpoolScale: Error sending Close Response payload {err:?}");
                                                        break 'send_recv_loop;
                                                    }
                                                }
                                            }
                                            Err(close_resp_header_err) => {
                                                error!("SpoolScale: Error sending Close Response header {close_resp_header_err:?}");
                                                break 'send_recv_loop;
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
                                error!("SpoolScale: Error while reading payload {:?}", recv_payload_res.err().unwrap());
                                // can continue, will try to read next header and if will fail, will fail on the header and disconnect
                            }
                        }
                        Err(recv_header_err) => {
                            match recv_header_err {
                                edge_ws::Error::Io(io_err) => {
                                    error!("SpoolScale: IO error while reading header, disconnecting {io_err:?}");
                                    // breaking out of the loop, because when an IO error happens here, it happens continuously and turns to a busy loop
                                    break 'send_recv_loop;
                                }
                                // edge_ws::Error::Incomplete(_) => todo!(),
                                // edge_ws::Error::Invalid => todo!(),
                                // edge_ws::Error::BufferOverflow => todo!(),
                                // edge_ws::Error::InvalidLen => todo!(),
                                _ => {
                                    error!("SpoolScale: Error receiving web-socket header {recv_header_err:?}");
                                    break 'send_recv_loop;
                                }
                            }
                        }
                    }
                }
                embassy_futures::select::Either3::Third(console_to_scale) => {
                    let json_res = serde_json::to_string(&console_to_scale);
                    match json_res {
                        Ok(json) => {
                            let send_to_scale_header = FrameHeader {
                                frame_type: FrameType::Text(false),
                                payload_len: json.len() as u64,
                                mask_key: None,
                            };

                            let send_to_scale_header_res = send_to_scale_header.send(&mut socket).await;
                            match send_to_scale_header_res {
                                Ok(_) => {
                                    let send_to_scale_payload_res = send_to_scale_header.send_payload(&mut socket, json.as_bytes()).await;
                                    match send_to_scale_payload_res {
                                        Ok(_) => {
                                            let res = socket.flush().await;
                                            match res {
                                                Ok(_) => {
                                                    debug!("SpoolScale: Sent message to scale: {json}");
                                                }
                                                Err(send_to_scale_flush_err) => {
                                                    error!("SpoolScale: Error sending message payload {send_to_scale_flush_err:?}, disconnecting");
                                                    break 'send_recv_loop;
                                                }
                                            }
                                        }
                                        Err(send_to_scale_payload_err) => {
                                            error!("SpoolScale: Error sending Ping payload {send_to_scale_payload_err:?}");
                                        }
                                    }
                                }
                                Err(send_to_scale_header_err) => {
                                    error!("SpoolScale: Error sending Ping header {send_to_scale_header_err:?}");
                                }
                            }
                        }
                        Err(err) => {
                            error!("SpoolScale: Error serializing data {:?}, {:?}", console_to_scale, err)
                        }
                    }
                }
            }
        }
        spool_scale_rc.borrow_mut().disconnected();
    }
}
