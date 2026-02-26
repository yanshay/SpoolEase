use core::cell::RefCell;

use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
};
use embassy_futures::select::{Either, select};
use embassy_net::Ipv4Address;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, pubsub::PubSubChannel};
use embassy_time::{Duration, Timer};
use framework::prelude::*;

use crate::{
    bambu::{BambuPrinter, bambu_ssdp::BambuSSDPInfo, default_printer_name},
    my_mqtt::BufferedMqttPacket,
    ssdp::SSDPPubSubChannel,
};

pub(crate) type WritePacketsChannel = Channel<NoopRawMutex, crate::my_mqtt::BufferedMqttPacket, 20>;
pub(crate) type ReadPacketsPubSub = PubSubChannel<NoopRawMutex, BufferedMqttPacket, 20, 2, 1>;

#[allow(clippy::too_many_arguments)]
// #[embassy_executor::task(pool_size = 5)]
pub async fn restartable_mqtt_task(
    framework: Rc<RefCell<Framework>>,
    rx_socket_buffer_size: usize,
    tx_socket_buffer_size: usize,
    read_packets: Rc<ReadPacketsPubSub>,
    write_packets: Rc<WritePacketsChannel>,
    bambu_printer: Rc<RefCell<BambuPrinter>>,
    restart_printer: Rc<embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, i32>>,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
) {
    loop {
        let printer_mqtt_task = bambu_mqtt_task(
            framework.clone(),
            bambu_printer.clone(),
            rx_socket_buffer_size,
            tx_socket_buffer_size,
            read_packets.clone(),
            write_packets.clone(),
            ssdp_pub_sub,
        );
        match select(printer_mqtt_task, restart_printer.wait()).await {
            Either::First(_) => {
                // we arrive here only if something is wrong with config, so the only thing to do
                // is wait for printer restart
                restart_printer.wait().await;
            }
            Either::Second(_) => {}
        }
        write_packets.clear();
        read_packets.clear();
    }
}

// Usage example, this should be in the client code using the generic_mqtt_task, specific per scenario
// This indirection is because embassy can't have generic functions as tasks
// https://github.com/embassy-rs/embassy/issues/2454#issuecomment-2336644031
// This is specific to the hw and required detailes (buffer sizes, etc.)
pub async fn bambu_mqtt_task(
    framework: Rc<RefCell<Framework>>,
    bambu_printer: Rc<RefCell<BambuPrinter>>,
    rx_socket_buffer_size: usize,
    tx_socket_buffer_size: usize,
    read_packets: Rc<ReadPacketsPubSub>,
    write_packets: Rc<WritePacketsChannel>,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
) {
    let stack = framework.borrow().stack;
    let printer_serial = bambu_printer.borrow().printer_serial.clone();
    let printer_log_id = bambu_printer.borrow().printer_number;
    let log_level = bambu_printer.borrow().log_filter;

    let subscribe_topics = [mqttrust::SubscribeTopic {
        topic_path: &format!("device/{}/report", printer_serial),
        qos: mqttrust::QoS::AtLeastOnce,
    }];

    if log_level >= log::Level::Info {
        info!("[{}] Waiting for IP in Bambu Mqtt Task", printer_log_id);
    }
    // let mut wait_counter = 0;
    // const SKIP_CHECKS: i32 = 4;
    loop {
        if let Some(_config) = stack.config_v4() {
            break;
        }
        Timer::after(Duration::from_millis(250)).await;
    }
    if log_level >= log::Level::Info {
        info!("[{}] From Bambu MQTT - got IP", printer_log_id);
    }
    Timer::after(Duration::from_millis(250)).await; // So log will come after wifi log

    let printer_ip: Ipv4Address;
    let printer_name: String;

    if bambu_printer.borrow().configured_printer_ip.is_none() {
        term_info!("[{}] No Printer IP configured, discovering Printer", printer_log_id);
        let mut ssdp_subscribe = ssdp_pub_sub.subscriber().unwrap();
        loop {
            let ssdp_info = ssdp_subscribe.next_message().await;
            match ssdp_info {
                embassy_sync::pubsub::WaitResult::Lagged(_) => (),
                embassy_sync::pubsub::WaitResult::Message(ssdp_info) => {
                    if let Ok(ssdp_info) = TryInto::<BambuSSDPInfo>::try_into(ssdp_info)
                        && printer_serial == ssdp_info.serial.unwrap_or("".to_string())
                    {
                        printer_ip = ssdp_info.ip.unwrap();
                        printer_name = ssdp_info.name.unwrap();
                        term_info!("[{}] Discovered printer {}", printer_log_id, printer_name);
                        break;
                    }
                }
            }
        }
    } else {
        printer_ip = bambu_printer.borrow().configured_printer_ip.unwrap();
        printer_name = bambu_printer.borrow().configured_printer_name.clone().unwrap_or(default_printer_name());
    }

    // Final name, theoretically if name explicitly supplied and IP not,  this could override the supplied name
    bambu_printer.borrow_mut().printer_ip = printer_ip;
    bambu_printer.borrow_mut().set_printer_name(&printer_name);

    let remote_endpoint = (printer_ip, 8883);
    let password = {
        let bambu_printer_borrow = bambu_printer.borrow();
        Some(bambu_printer_borrow.printer_access_code.clone().into_bytes())
    };

    crate::my_mqtt::generic_mqtt_task(
        framework,
        remote_endpoint,
        &printer_serial,
        Some("bblp"),
        password,
        0,
        &subscribe_topics,
        rx_socket_buffer_size,
        tx_socket_buffer_size,
        write_packets,
        read_packets,
        Duration::from_secs(20),
        bambu_printer,
    )
    .await
}
