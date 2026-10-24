use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use core::cell::RefCell;

use embassy_futures::yield_now;
use embassy_time::Timer;
use embedded_storage::{ReadStorage, Storage};
use esp_partition_table::{PartitionBuffer, PartitionError, PartitionReaderState};
use esp_storage::FlashStorage;
use framework::framework::Framework;
use framework::ota::{
    OtaObserver, OtaPartition, OtaPartitionLayout, OtaPhase, activate_ota_partition, download_url_to_buffer, fetch_ota_metadata,
    ota_partition_layout, write_ota_to_raw_slot,
};
use framework::{error, info};
use shared::settings::{OTA_DOMAIN_STABLE, OTA_TLS_CERTIFICATE};
use slint::ComponentHandle;

use crate::app::{AppWindow, FrameworkState};
use crate::settings::{CONSOLE_PARTITION_TABLE_PATH, OTA_TOML_FILENAME, UPDATER_STABLE_OTA_PATH};

const PARTITION_TABLE_OFFSET: u32 = 0x8000;
const PARTITION_TABLE_BINARY_SIZE: usize = 0x0c00;
const PARTITION_TABLE_SECTOR_SIZE: usize = 0x1000;
const COPY_BUFFER_SIZE: usize = 4096;
const UPDATER_PROJECT_NAME: &str = "esp-hal-app-updater";

struct MigrationObserver<'a> {
    ui: &'a AppWindow,
    progress_percent: u8,
}

impl OtaObserver for MigrationObserver<'_> {
    fn on_ota_start(&mut self) {}

    fn on_ota_status(&mut self, text: &str) {
        migration_status(self.ui, text);
    }

    fn on_ota_failed(&mut self, text: &str) {
        migration_failed(self.ui, text);
    }

    fn on_ota_completed(&mut self, _text: &str) {}

    fn on_ota_version_available(&mut self, _version: &str, _newer: bool) {}

    fn on_ota_progress(&mut self, phase: OtaPhase, _filename: &str, completed: u32, total: u32) {
        let percent = if total == 0 {
            0
        } else {
            ((u64::from(completed) * 100) / u64::from(total)) as u8
        };
        if percent == self.progress_percent {
            return;
        }
        self.progress_percent = percent;
        let phase = match phase {
            OtaPhase::Downloading => "Downloading updater",
            OtaPhase::Flashing => "Installing updater",
            OtaPhase::Completing => "Finalizing updater",
        };
        migration_status(self.ui, &format!("{phase}: {percent}%"));
    }
}

pub async fn run(framework: Rc<RefCell<Framework>>, ui: &AppWindow) {
    let layout = match ota_partition_layout() {
        Ok(layout) => layout,
        Err(err) => {
            error!("Migration: failed to inspect OTA layout: {err}");
            return;
        }
    };

    if !is_legacy_layout(layout) {
        info!("Migration: unequal OTA layout already installed");
        return;
    }

    migration_started(ui, "Preparing flash layout update");
    info!(
        "Migration: legacy OTA layout detected; ota_0={} bytes ota_1={} bytes running={:?}",
        layout.ota_0.size, layout.ota_1.size, layout.running
    );

    let result = match layout.running {
        Some(0) => {
            migration_status(ui, "Waiting for network");
            Framework::wait_for_wifi(&framework).await;
            migrate_from_ota_0(framework, ui).await
        }
        Some(1) => migrate_from_ota_1(layout, framework, ui).await,
        _ => Err("Cannot identify the running OTA partition".into()),
    };

    match result {
        Ok(()) => {
            migration_completed(ui, "Flash layout update complete");
            Timer::after_secs(2).await;
            migration_clear(ui);
        }
        Err(err) => {
            error!("Migration: failed: {err}");
            migration_failed(ui, &format!("Flash layout update failed: {err}"));
        }
    }
}

fn is_legacy_layout(layout: OtaPartitionLayout) -> bool {
    let smaller = u64::from(layout.ota_0.size.min(layout.ota_1.size));
    let larger = u64::from(layout.ota_0.size.max(layout.ota_1.size));
    smaller * 4 >= larger * 3
}

async fn migrate_from_ota_1(layout: OtaPartitionLayout, framework: Rc<RefCell<Framework>>, ui: &AppWindow) -> Result<(), alloc::string::String> {
    let length = layout.ota_0.size.min(layout.ota_1.size);
    migration_phase_started(ui, "Migrating firmware to main slot");
    info!(
        "Migration: copying {} bytes from ota_1 at 0x{:08x} to ota_0 at 0x{:08x}",
        length, layout.ota_1.offset, layout.ota_0.offset
    );

    let mut flash = FlashStorage::new();
    let mut buffer = Box::new([0_u8; COPY_BUFFER_SIZE]);
    let mut copied = 0_u32;
    let mut source_crc = 0_u32;
    let mut progress_percent = 0_u8;
    while copied < length {
        let chunk_len = ((length - copied) as usize).min(buffer.len());
        flash
            .read(layout.ota_1.offset + copied, &mut buffer[..chunk_len])
            .map_err(|err| format!("Failed reading ota_1 during migration copy: {err:?}"))?;
        source_crc = esp_hal_ota::crc32::calc_crc32(&buffer[..chunk_len], source_crc);
        flash
            .write(layout.ota_0.offset + copied, &buffer[..chunk_len])
            .map_err(|err| format!("Failed writing ota_0 during migration copy: {err:?}"))?;
        copied += chunk_len as u32;
        let percent = ((u64::from(copied) * 100) / u64::from(length)) as u8;
        if percent != progress_percent {
            progress_percent = percent;
            migration_status(ui, &format!("Migrating firmware to main slot: {percent}%"));
            yield_now().await;
        }
    }

    let mut verified = 0_u32;
    let mut destination_crc = 0_u32;
    while verified < length {
        let chunk_len = ((length - verified) as usize).min(buffer.len());
        flash
            .read(layout.ota_0.offset + verified, &mut buffer[..chunk_len])
            .map_err(|err| format!("Failed verifying ota_0 migration copy: {err:?}"))?;
        destination_crc = esp_hal_ota::crc32::calc_crc32(&buffer[..chunk_len], destination_crc);
        verified += chunk_len as u32;
    }
    if source_crc != destination_crc {
        return Err(format!(
            "Migration copy CRC mismatch: source {source_crc:08x}, destination {destination_crc:08x}"
        ));
    }

    migration_phase_finished(ui);
    info!("Migration: bridge copy verified; selecting ota_0");
    activate_ota_partition(0)?;
    migration_status(ui, "Restarting from main slot");
    Framework::reset_device_safer_async(framework, None).await;
    Ok(())
}

async fn migrate_from_ota_0(framework: Rc<RefCell<Framework>>, ui: &AppWindow) -> Result<(), alloc::string::String> {
    migration_status(ui, "Downloading partition table");
    info!("Migration: downloading partition table");
    let mut table = Box::new([0_u8; PARTITION_TABLE_BINARY_SIZE]);
    download_url_to_buffer(
        OTA_DOMAIN_STABLE,
        CONSOLE_PARTITION_TABLE_PATH,
        OTA_TLS_CERTIFICATE,
        &mut *table,
        framework.clone(),
    )
    .await?;
    let updater_partition = future_updater_partition(&table)?;
    info!("Migration: future updater offset is 0x{:08x}", updater_partition.offset);

    let mut observer = MigrationObserver {
        ui,
        progress_percent: u8::MAX,
    };
    migration_status(ui, "Fetching updater metadata");
    let metadata = fetch_ota_metadata(
        OTA_DOMAIN_STABLE,
        UPDATER_STABLE_OTA_PATH,
        OTA_TOML_FILENAME,
        OTA_TLS_CERTIFICATE,
        framework.clone(),
    )
    .await?;
    migration_phase_started(ui, "Downloading updater");
    write_ota_to_raw_slot(
        OTA_DOMAIN_STABLE,
        UPDATER_STABLE_OTA_PATH,
        OTA_TLS_CERTIFICATE,
        &metadata,
        updater_partition,
        UPDATER_PROJECT_NAME,
        framework,
        &mut observer,
    )
    .await?;
    info!(
        "Migration: updater {} version {} written and verified",
        metadata.filename, metadata.version
    );

    migration_phase_finished(ui);
    migration_status(ui, "Applying flash layout. Keep power connected");
    info!("Migration: writing partition table");
    write_partition_table(&table)?;
    info!("Migration: partition table written and verified");
    Ok(())
}

fn future_updater_partition(table: &[u8; PARTITION_TABLE_BINARY_SIZE]) -> Result<OtaPartition, alloc::string::String> {
    let mut reader = PartitionReaderState::new(0, table.len(), true);
    let mut ota_1 = None;
    for bytes in table.chunks_exact(core::mem::size_of::<PartitionBuffer>()) {
        let entry_data: &PartitionBuffer = bytes
            .try_into()
            .map_err(|_| alloc::string::String::from("Invalid partition table entry size"))?;
        match reader.read(entry_data) {
            Ok(entry) if entry.name() == "ota_1" => {
                ota_1 = Some(OtaPartition {
                    offset: entry.offset,
                    size: entry.size as u32,
                });
            }
            Ok(_) => {}
            Err(PartitionError::NotEnoughData) => break,
            Err(err) => return Err(format!("Invalid partition table: {err:?}")),
        }
    }
    if reader.check_md5() != Some(true) {
        return Err("Partition table MD5 validation failed".into());
    }
    ota_1.ok_or_else(|| "Partition ota_1 is missing from the new table".into())
}

fn write_partition_table(table: &[u8; PARTITION_TABLE_BINARY_SIZE]) -> Result<(), alloc::string::String> {
    let mut sector = Box::new([0xff_u8; PARTITION_TABLE_SECTOR_SIZE]);
    sector[..table.len()].copy_from_slice(table);
    let mut flash = FlashStorage::new();
    flash
        .write(PARTITION_TABLE_OFFSET, &*sector)
        .map_err(|err| format!("Failed writing partition table: {err:?}"))?;
    let mut readback = Box::new([0_u8; PARTITION_TABLE_SECTOR_SIZE]);
    flash
        .read(PARTITION_TABLE_OFFSET, &mut *readback)
        .map_err(|err| format!("Failed reading partition table back: {err:?}"))?;
    if *sector != *readback {
        return Err("Partition table read-back does not match".into());
    }
    Ok(())
}

fn migration_started(ui: &AppWindow, text: &str) {
    ui.global::<FrameworkState>().invoke_migration_started(text.into());
}

fn migration_status(ui: &AppWindow, text: &str) {
    ui.global::<FrameworkState>().invoke_migration_status(text.into());
}

fn migration_completed(ui: &AppWindow, text: &str) {
    ui.global::<FrameworkState>().invoke_migration_completed(text.into());
}

fn migration_failed(ui: &AppWindow, text: &str) {
    ui.global::<FrameworkState>().invoke_migration_failed(text.into());
}

fn migration_phase_started(ui: &AppWindow, text: &str) {
    ui.global::<FrameworkState>().invoke_migration_phase_started(text.into());
}

fn migration_phase_finished(ui: &AppWindow) {
    ui.global::<FrameworkState>().invoke_migration_phase_finished();
}

fn migration_clear(ui: &AppWindow) {
    ui.global::<FrameworkState>().invoke_migration_clear();
}
