# Printer Architecture Handoff

This document captures the current backend architecture review and the proposed path for making printer support pluggable while preserving the existing Bambu Lab behavior.

The intent is to make this file the durable handoff point for future work. A new session should be able to read this document and continue without relying on chat history.

## Scope

This document covers only backend code in `console/core` and direct backend dependencies under `console/shared` where they affect printer integration.

Primary reviewed areas:

- `src/bambu.rs`
- `src/bambu/*`
- `src/view_model.rs`
- `src/app_config.rs`
- `src/web_app.rs`
- `src/store.rs`
- `src/spool_record.rs`
- `src/filament_staging.rs`
- `src/tag_v1.rs`
- `src/tag_standards.rs`
- `src/spool_scale.rs`
- `src/ssdp.rs`
- `src/my_mqtt.rs`
- `ui/*.slint`
- `console/shared/src/gcode_analysis*.rs`
- `console/shared/src/scale.rs`
- `console/shared/src/spool_tag.rs`

The client app is intentionally out of scope for this document, except where backend API compatibility is discussed.

## Product Goal

The project currently works well for Bambu Lab printers. The next goal is to support very different printer technologies, starting with likely candidates such as Snapmaker U1, Prusa Core One, and printers exposed through Moonraker.

The architecture must support:

- Multiple printer driver implementations.
- Writable material slot configuration as the most important cross-printer feature.
- Printer status and print tracking where available, but not as mandatory capabilities.
- Driver-specific implementation details without leaking those details into generic inventory code.
- Bambu Lab behavior that remains absolutely compatible with the current implementation during the first migration phase.

## Key Decisions

These decisions incorporate both the review and follow-up product direction.

### Bambu MQTT Is Bambu-Specific

MQTT should not be generalized now.

Current Bambu MQTT behavior is specific enough that making it generic would add abstraction cost without a real second consumer:

- TLS MQTT to printer port `8883`.
- Username `bblp`.
- Password is the Bambu access code.
- Topics are `device/{serial}/report` and `device/{serial}/request`.
- Certificate selection and rotation are Bambu model-specific.
- Simulator behavior is Bambu-specific.

If a future printer also uses MQTT, revisit this then. For now, `src/my_mqtt.rs` can be treated as Bambu transport infrastructure, even if some helper code is technically reusable.

### Print Consumption Tracking Is Not a Generic Algorithm

Bambu currently tracks print consumption by:

- Receiving a `project_file` command.
- Fetching and analyzing the 3MF/G-code.
- Tracking layer changes and active tray changes.
- Mapping gcode filament IDs to Bambu AMS/external slots.
- Incrementing spool consumption from analyzed usage entries.

That is Bambu-specific. G-code/3MF fetching, parsing, layer tracking, Bambu AMS mapping, and print resume state should stay hidden inside the Bambu driver/adapter. Do not make G-code parsing a generic printer feature.

The generic concept is not "fetch G-code and track layers". The generic concept is:

```text
slot X consumed Y grams
```

A driver may produce this by any mechanism:

- Bambu: derive from gcode analysis plus print telemetry.
- Moonraker: possibly read direct extrusion/filament sensor/tool events.
- Another printer: possibly receive direct per-slot usage reports.
- Some printers: no consumption tracking capability at all.

### Pressure Advance Is Optional and Driver-Specific

Bambu stores and manages pressure advance/K calibration in the printer and exposes calibration tables through MQTT commands. Other printers may ignore this completely because pressure advance is embedded in slicer-generated G-code or handled differently.

Pressure advance must therefore be an optional driver capability, not a generic inventory requirement.

The existing Bambu K model should remain as a Bambu-specific extension until there is a real second implementation that needs similar semantics.

### Material Slot Assignment Is Required for First Non-Bambu Work

The first non-Bambu implementation must support writable material slot assignment. This is more important than status monitoring.

The generic printer interface should therefore be designed around material slots and assignment commands first.

### Backend API Can Change With The Client

There is no need to preserve the current printer status API by adding a parallel V2 endpoint if the client will be updated in the same project phase.

During the internal migration, temporary compatibility adapters are still useful to keep Bambu working while Rust and Slint are refactored. But the external API can be replaced when the client is updated.

### Slint Must Be Refactored

Current Slint code is Bambu topology UI. It hardcodes:

- External tray IDs `255` and `254`.
- AMS slots `0..15`.
- AMS-HT slots `16..23`.
- AMS A-D names.
- HT slots.
- `curr-ams-id` and `ams-exists`.
- `tray-id: int` callbacks.

This must become dynamic and slot-group driven before non-Bambu printers can be represented cleanly.

## Current Backend Architecture

The current backend has one concrete printer implementation: Bambu Lab.

High-level flow:

```text
main.rs
  -> app.rs
    -> ViewModel
      -> BambuPrinter instances
      -> Store inventory
      -> Slint AppState/AppBackend
      -> Web API state
      -> SpoolTag local NFC
      -> SpoolScale remote scale/NFC/gcode helper
      -> G-code analysis jobs
```

`ViewModel` is the central orchestrator. It owns or coordinates nearly every domain:

- Printer instances through `bambu_printer_model: SelectedPrinter`.
- Inventory through `Store`.
- Staging through `FilamentStaging`.
- Local NFC tag reader.
- Remote scale.
- Slint UI projection.
- Web API responses.
- Print consumption updates.
- G-code analysis dispatch and completion.
- Printer state persistence scheduling.

This is the main source of coupling. The Bambu protocol code is mostly isolated, but the application layer is Bambu-shaped.

## Current Bambu Implementation

### Main State Object

`src/bambu.rs` defines `BambuPrinter`, which is the concrete runtime state and command surface for a Bambu printer.

Important state includes:

- Config and identity:
  - `printer_number`
  - `printer_index`
  - `printer_serial`
  - `printer_access_code`
  - `configured_printer_name`
  - `inner_printer_name`
  - `printer_selector_name`
  - `configured_printer_ip`
- Behavior settings:
  - `auto_restore_k`
  - `track_print_consume`
  - `fetch_3mf`
  - `ignore_certificates`
  - `printer_mode`
  - `use_ams_scan`
- Runtime connection:
  - `printer_ip`
  - `printer_connectivity_ok`
  - `locked_mode`
- Protocol and channels:
  - `protocol_state`
  - `write_packets`
  - `restart_printer`
- Printer topology:
  - `inner_extruders: [Extruder; 2]`
  - `inner_ams_trays: Vec<Tray>` fixed to 24 slots
  - `inner_virt_trays: [Tray; 2]`
  - `tray_tar`, `tray_now`, `tray_pre`
  - `ams_info`
- Dirty persistence flags:
  - `extruders_dirty`
  - `ams_trays_dirty`
  - `virt_trays_dirty`
  - `tray_exist_bits_dirty`
  - `tray_read_done_bits_dirty`
  - `ams_exist_bits_dirty`
  - `calibrations_dirty`
  - `printer_name_dirty`
  - `relevant_extruder_state_dirty`
- Print status:
  - `gcode_state`
  - `layer_num`
  - `total_layer_num`
  - `mc_percent`
  - `mc_remaining_time`
  - `print_error`
  - `gcode_file_prepare_percent`
  - `subtask_name`
  - `stg_cur`
  - `hms`
- Print consumption tracking:
  - `curr_print_project`
  - `loaded_print_project`
- Calibration:
  - `calibrations: Vec<Calibration>`

### Bambu Model Detection

`BambuPrinter::model()` maps serial prefixes to models:

- `094`: H2D
- `239`: H2DPro
- `00M`: X1C
- `03W`: X1E
- `01P`: P1S
- `01S`: P1P
- `039`: A1
- `030`: A1Mini
- `22E`: P2S
- `31B`: H2C
- `093`: H2S
- `20P`: X2D

`model_series()` groups models into behavior families:

- X1
- P1
- A1
- H2
- P2
- X2
- Unknown

This affects transport certificates and gcode/FTP handling.

### Bambu Slot Topology

Bambu slots are represented as fixed integer tray IDs:

- `0..15`: standard AMS slots, four AMS units with four slots each.
- `16..23`: AMS-HT slots, corresponding to Bambu AMS IDs `128..135`.
- `255`: right or single external spool, extruder 0.
- `254`: left external spool, extruder 1.

Important helpers:

- `BambuPrinter::get_ams_and_slot_id(tray_id)` maps internal tray ID to Bambu AMS ID and slot ID.
- `BambuPrinter::get_ams_info_index_for_tray(tray_id)` maps slot ID to metadata index.
- `BambuPrinter::get_tray_detailed_ready_state(tray_id)` derives `Ready`, `Loaded`, or `Empty` based on active extruder and tray state.
- `BambuPrinter::get_quad_for_set_filament_from_tray_id(tray_id)` computes `(ams_id, ams_tray_id, slot_id, original_tray_id)` for Bambu commands.

### Tray State

`src/bambu/tray.rs` defines:

```rust
pub enum TrayState {
    Unknown,
    Empty,
    Spool,
    Reading,
    Ready,
    Loading,
    Unloading,
    Loaded,
}
```

`Tray` contains:

- `state`
- `filament`
- `k_from_tray`
- `cali_idx`
- flattened `TrayMetaInfo`

`TrayMetaInfo` now contains only Bambu-private tray metadata:

- `old_tag_info` for migration
- `waiting_for_tag_uid`

`spool_id`, consumption counters, and `used_in_print` are generic app concepts and now live only in `PrinterSnapshot` state. Bambu tray metadata is no longer a source of truth for these fields.

### Incoming Report Compatibility

`src/bambu/process_incoming.rs` contains delicate compatibility logic across Bambu models and firmware versions.

Do not rewrite this in the first migration.

Important compatibility behaviors:

- Locked/cloud mode:
  - `PrinterMode::Auto` reads `print.fun` bit `0x20000000`.
  - `DevOrOldFirmware` forces unlocked.
  - `Cloud` forces locked.
- Nozzle information:
  - New format: `print.device.nozzle.info[]`.
  - Old format: top-level `print.nozzle_diameter`.
- Tray movement:
  - New/H2-style format: `device.extruder.info[*].star/snow/spre`.
  - Old format: `ams.tray_tar/tray_now/tray_pre`.
- External tray reports:
  - Old/single external: `vt_tray`.
  - New/multiple external: `vir_slot`.
  - Fallback derived from tray movement when no external slot report is present.
- AMS bits:
  - `ams_exist_bits`
  - `tray_exist_bits`
  - `tray_read_done_bits`
  - `tray_reading_bits`
- AMS scan:
  - When tray read completes and `tag_uid` is present, `notify_tag_scanned()` can trigger automatic slot configuration.
- Partial external tray update with `id: None`:
  - Current behavior requests a full update rather than merging partial fields.
- Failed printer responses:
  - Messages with `result == "fail"` are ignored.

### Bambu API Schema

`src/bambu/bambu_api.rs` defines Bambu JSON schema and outgoing command structures.

Important incoming models:

- `Message::Print`
- `Message::Info`
- `PrintData`
- `PrintAms`
- `PrintAmsData`
- `PrintTray`
- `PrintDevice`
- `PrintDeviceExtruder`
- `PrintDeviceNozzle`

Important outgoing commands:

- `PushAllCommand`
- `AmsFilamentSettingCommand`
- `ExtrusionCaliGetCommand`
- `ExtrusionCaliSelCommand`
- `ExtrusionCaliSetCommand`
- `GetVersionCommand`
- `PrinterCommand`

Serde compatibility:

- Many numeric fields are strings.
- Some values are hex strings.
- `gcode_file_prepare_percent` supports string integer form.
- `PrintDevice.nozzle` ignores parse failures for known firmware differences.
- `PrintTray::tray_colors()` supports both `tray_color` and multi-color `cols`.

### Bambu Outgoing Commands

`src/bambu/outgoing.rs` implements current command behavior:

- Publish payloads to `device/{serial}/request`.
- Request version info.
- Request full update.
- Pause/resume/stop.
- Fetch calibrations.
- Reset tray.
- Set tray filament.
- Select pressure advance/K calibration.
- Add calibration to printer.

Many mutation commands are skipped while `is_locked()` is true. This behavior must be preserved.

### Bambu Persistence

`src/bambu/printer_state.rs` defines Bambu private restart state. The shared printer-state scheduler stores it inside the per-printer `driver_private` section at the startup path derived from serial:

- `/state/{file_name}.{file_ext}/startup.jsn`

Bambu print resume state remains separate under the same serial-derived directory:

- `/state/{file_name}.{file_ext}/print.jsn`
- `/state/{file_name}.{file_ext}/print.csv`
- `/state/{file_name}.{file_ext}/print.ci0`
- `/state/{file_name}.{file_ext}/print.ci1`

`PrinterPersistentState` includes:

- `ams_trays`
- legacy `virt_tray`
- `virt_trays`
- legacy `nozzle_diameter`
- `ams_exist_bits`
- `tray_exist_bits`
- `tray_read_done_bits`
- `calibrations`
- `printer_name`
- `extruders`
- `extruder_state`

Compatibility behavior:

- Legacy single virtual tray is migrated into `virt_trays[0]`.
- Legacy `nozzle_diameter` is restored into extruder 0.
- Tray vector is resized to 24.
- Missing generic spool IDs are cleared by the generic `PrinterSnapshot` load path if no longer in inventory.

Generic slot assignment and consumption fields are no longer stored in Bambu tray metadata.

### Bambu Print Tracking

`src/bambu/bambu_print.rs` tracks active print projects.

Current flow:

- `project_file` creates `PrintProject`.
- It stores Bambu project fields:
  - `project_id`
  - `subtask_name`
  - `threemf_url`
  - `gcode_filename_in_3mf`
  - `ams_mapping`
  - `ams_mapping2`
  - `use_ams`
- G-code analysis is requested through observers.
- Trays used in print are marked.
- State changes, layer changes, tray changes, and finish/fail events trigger consumption logic.
- Consumption maps gcode filament IDs to Bambu tray IDs.
- Print resume state is persisted separately.

Compatibility:

- Old `ams_mapping` is preferred.
- New `ams_mapping2` is used for external slots.
- `use_ams == false` maps to external spool for some cases.

This entire mechanism is Bambu-specific and should become a Bambu implementation of generic consumption reporting.

Important boundary:

- Keep G-code/3MF analysis under Bambu-specific code.
- Do not require non-Bambu drivers to expose print files, G-code paths, layer telemetry, or Bambu-style AMS mappings.
- The generic printer/application layer should receive consumption notifications, not analysis jobs.
- If Bambu needs internal analysis events during migration, treat them as Bambu adapter internals or Bambu-specific extension events.

### Bambu Calibration

`src/bambu/calibration.rs` models Bambu pressure advance/K data.

Important concepts:

- `Calibration`
- `KInfo`
- `KPrinter`
- `KExtruder`
- `KNozzleDiameter`
- `KNozzleId`
- Bambu `cali_idx`
- Bambu `setting_id`
- Bambu nozzle ID and high-flow/standard detection

The matching logic is delicate:

- Match by printer serial.
- Match by extruder.
- Match by nozzle diameter.
- Match by nozzle type.
- Match by filament ID.
- Fall back from exact calibration name to cleaned name to K value.
- Tolerate H2D missing `setting_id`.

This should be treated as a Bambu optional capability, not a generic inventory primitive.

## Current Generic Domains

### Inventory

`src/spool_record.rs` defines `SpoolRecord`, the main inventory row stored as CSV.

Mostly generic fields:

- `id`
- `tag_id`
- `material_type`
- `material_subtype`
- `color_name`
- `color_code`
- `note`
- `brand`
- `weight_advertised`
- `weight_core`
- `weight_new`
- `weight_current`
- `added_time`
- `encode_time`
- `added_full`
- `consumed_since_add`
- `consumed_since_weight`
- `data_origin`
- `tag_type`
- `assigned_location`
- `actual_location`
- `spools_count`

Bambu-coupled fields or semantics:

- `slicer_filament` currently often stores Bambu filament/material IDs.
- `ext_has_k` signals Bambu pressure advance extension presence.
- `SpoolRecordExt.k_info` stores Bambu K information.
- `SpoolRecordExt::get_calibration()` takes Bambu `NozzleType` and Bambu K hierarchy.
- `OriginData::BambuLabTag` stores Bambu tag origin.

Short-term recommendation:

- Keep schema unchanged for compatibility.
- Hide Bambu-specific interpretation behind adapter/service APIs.

Long-term recommendation:

- Replace `k_info` with a generic extension map or driver-specific extension enum.
- Keep `slicer_filament` but clarify it means slicer/material code, not Bambu-specific ID.

### Store

`src/store.rs` owns persistent inventory databases:

- `spools_db`
- `locations_db`
- spool tag ID index
- storage config

The CSV database layer in `src/csvdb.rs` is generic and should remain reusable.

Store coupling to clean up later:

- Imports Bambu `KInfo`.
- Imports `ViewModel`.
- `edit_spool_from_web(..., k_info: Option<KInfo>)` exposes Bambu K directly.
- Upgrade code uses `ViewModel::get_k_info_from_old_tag()`.
- Store emits UI messages through `ViewModel` in upgrade paths.

Target:

- Store should not know `ViewModel`.
- Store should not know Bambu `KInfo` in generic APIs.
- Store should emit domain errors/events and let the application layer decide UI messages.

### Staging

`src/filament_staging.rs` holds the currently staged spool:

- `full_spool_rec`
- `scanned_tag_id`
- `origin`

Origins:

- `Empty`
- `Scanned`
- `Encoded`
- `Unloaded`

This is mostly generic, although the name `FilamentStaging` and some usage patterns are tied to printer tray operations.

Target:

- Keep the concept.
- Consider renaming later to `SpoolStaging` or `MaterialStaging`.
- Route assignment through generic printer slot commands, not Bambu tray functions.

### Tags

`src/tag_v1.rs` and `src/tag_standards.rs` handle tag formats.

Current tag standards:

- SpoolEase V1.
- Bambu Lab RFID tag.
- OpenPrintTag.

Bambu coupling:

- `BambuLabTag` is explicitly Bambu.
- `TagInformationV1` imports Bambu `Calibration` and `FilamentInfo`.
- V1 K handling is Bambu-specific.
- Shared local NFC reports `ReadResult::BambulabTag`.

Target:

- Tag readers remain generic devices.
- Tag format parsers become adapters.
- Bambu Lab RFID remains an inventory tag adapter, not inherently a printer adapter.

### Scale

`src/spool_scale.rs` and `console/shared/src/scale.rs` are mostly generic for weight/NFC/button functions.

Coupling:

- Remote scale protocol includes gcode analysis requests with `printer_index`.
- `GcodeAnalysisRequest` contains Bambu-specific fields such as serial, access code, FTPS details, and filename rules.

Target:

- Keep scale weight and NFC generic.
- Treat remote gcode analysis as a driver-owned optional helper.
- Version the scale protocol before changing non-compatible gcode request types.

## Current UI and API Coupling

### Slint Coupling

Original Slint types and callbacks were Bambu-shaped:

- `UiTray` used integer `id` matching Bambu tray IDs.
- `trays-state` was initialized with Bambu fixed slots.
- `curr-ams-id` and `ams-exists` assumed AMS paging.
- `get_tray_id()` and `get_tray_index()` encoded Bambu mappings.
- Tray operations used `tray-id: int`.
- UI displays K directly.
- New tag scan includes explicit Bambu Lab flows.

The current Slint slot surface now uses `UiSlotGroup` / `UiSlot` with opaque string slot IDs. The Bambu AMS/external visual layout is still specialized, but it is fed by backend slot groups rather than fixed Slint tray rows.

Target Slint model:

```text
PrinterView
  printer_id
  display_name
  connected
  slot_groups[]
    group_id
    display_name
    kind
    slots[]
      slot_id
      display_name
      state
      filament
      spool_id
      weight_display
      used_in_print
      capabilities
```

Callbacks should become:

```text
set-staging-to-slot(printer-id: string, slot-id: string)
configure-slot-with-spool-id(printer-id: string, slot-id: string, spool-id: string)
reset-slot(printer-id: string, slot-id: string)
untag-slot(printer-id: string, slot-id: string)
select-printer(printer-id: string)
```

Bambu can still map string slot IDs to current internal tray IDs.

### Web API Coupling

Important current endpoints:

- `/api/printer-config`
- `/api/printers-status`
- `/api/printer-command`
- `/api/printers-filament-pa`
- `/api/add-printer-pa`
- `/api/spool-kinfo`
- `/api/spools-in-printers`
- inventory and storage endpoints

Bambu-specific API concepts:

- `PrinterConfigDTO.serial`
- `PrinterConfigDTO.access_code`
- `PrinterMode`
- `UseAmsScan`
- `auto_restore_k`
- `fetch_3mf`
- `ignore_certificates`
- `PrintCommand` from Bambu API module
- K/pressure advance DTOs
- Slot status serialized as CSV-like strings
- `SpoolsSlotsKind::Ams | Ext`

Because client changes are acceptable, these endpoints can be redesigned directly when the client work starts. Internal compatibility adapters are still helpful while only backend is changing.

## Proposed Target Architecture

The central change is to introduce a generic printer layer based on material slots and capabilities.

```text
ViewModel / Web API / Slint
  -> PrinterManager
    -> BambuPrinterDriver
      -> existing BambuPrinter and src/bambu/*
    -> future SnapmakerDriver
    -> future MoonrakerDriver
    -> future PrusaDriver

Inventory / Store / Tags / Scale
  -> generic services
  -> optional printer capability calls
```

### Hybrid Snapshot State Model

Every printer driver must expose a shared `PrinterSnapshot` state handle. This state handle is mandatory driver infrastructure, but drivers may choose how much of their runtime model is actually stored there.

Driver state policy:

- Fake and future simple drivers should treat `PrinterSnapshot` as their primary state.
- Bambu keeps its existing protocol/tray/G-code/calibration internals and generates most snapshot fields from those internals.
- Bambu-specific fields such as AMS bits, raw `Tray`, `cali_idx`, `k_from_tray`, and print-analysis bookkeeping remain private to Bambu.
- Generic slot fields such as `spool_id`, `consumed_since_load_g`, `consumed_since_load_saved_g`, `consumed_since_weight_g`, and `used_in_print` are stored in the shared snapshot state, not in Bambu tray metadata.
- The snapshot state handle owns generic dirty tracking and supports store begin/success/failure recovery.

Generic consumption storage now reads printer snapshots and acknowledges saved high-water marks through `PrinterManager`, instead of iterating Bambu trays directly.

### Domain Language

Use generic terms:

- Printer
- Driver
- Slot group
- Material slot
- Slot assignment
- Filament/material metadata
- Print status
- Consumption event
- Capability

Avoid generic use of Bambu terms:

- AMS
- tray
- tray bits
- cali index
- K restore
- Bambu serial access code
- MQTT report/request

Bambu code can continue using Bambu terminology internally.

### Printer Capabilities

Capabilities should be explicit and optional.

Required for first non-Bambu driver:

- Writable material slot assignment.

Likely generic capabilities:

- `MaterialSlotRead`
- `MaterialSlotWrite`
- `MaterialSlotAssign`
- `MaterialSlotSetSpoolId`
- `MaterialSlotClear`
- `MaterialSlotUnassignSpool`
- `MaterialSlotPresenceNotify`
- `PrintStatusRead`
- `PrintControl`
- `ConsumptionTracking`
- `TagScanFromPrinter`
- `DriverManagedPressureAdvance`
- `PrintFileFetch`
- `PersistentSlotState`

Example:

```rust
pub struct PrinterCapabilities {
    pub material_slot_read: bool,
    pub material_slot_write: bool,
    pub material_slot_assign: bool,
    pub material_slot_set_spool_id: bool,
    pub material_slot_clear: bool,
    pub material_slot_unassign_spool: bool,
    pub material_slot_presence_notify: bool,
    pub print_status_read: bool,
    pub print_control: bool,
    pub consumption_tracking: bool,
    pub printer_tag_scan: bool,
    pub pressure_advance: PressureAdvanceCapability,
}

pub enum PressureAdvanceCapability {
    Unsupported,
    DriverManaged,
}
```

### Printer Driver Trait

Avoid `async fn` in the core trait for now. This codebase uses `Rc<RefCell<_>>`, Embassy tasks, and channels heavily. A command-enqueue model is safer for embedded Rust and avoids object-safety/generic task issues.

Conceptual shape:

```rust
pub trait PrinterDriver {
    fn id(&self) -> &PrinterId;
    fn kind(&self) -> PrinterDriverKind;
    fn display_name(&self) -> String;
    fn capabilities(&self) -> PrinterCapabilities;
    fn snapshot(&self) -> PrinterSnapshot;
    fn dispatch(&mut self, command: PrinterCommand) -> Result<(), PrinterError>;
    fn subscribe(&mut self, observer: Weak<RefCell<dyn PrinterObserver>>);
    fn start(&mut self, framework: Rc<RefCell<Framework>>);
}
```

The short one-based printer number is not driver identity. Driver-owned logs use the driver instance's assigned printer number; manager-owned logs derive it from active manager index plus one. `dispatch()` can internally enqueue async commands or call current synchronous Bambu functions. `start()` is where a driver starts background runtime work or adapter-owned event bridges after application observers have subscribed.

### Printer Manager

`PrinterManager` should replace `SelectedPrinter` as the application-facing collection.

Responsibilities:

- Own all configured printers.
- Provide current selected printer.
- Provide snapshots for UI/API.
- Dispatch commands by `PrinterId`.
- Route driver events to application services.
- Hide `Rc<RefCell<BambuPrinter>>` from `ViewModel`.

Conceptual shape:

```rust
pub struct PrinterManager {
    printers: Vec<Rc<RefCell<dyn PrinterDriver>>>,
    selected: Option<PrinterId>,
}
```

If trait objects become painful because of embedded generics or allocation constraints, an enum is acceptable:

```rust
pub enum PrinterInstance {
    Bambu(BambuPrinterDriver),
    Snapmaker(SnapmakerDriver),
    Moonraker(MoonrakerDriver),
}
```

Given the codebase style, the enum approach may be more pragmatic initially. It avoids object-safety problems and keeps concrete methods available during migration.

### Printer Snapshot

The snapshot should be the only object Slint/API projection reads.

Conceptual shape:

```rust
pub struct PrinterSnapshot {
    pub id: PrinterId,
    pub kind: PrinterDriverKind,
    pub identifier: String,
    pub name: String,
    pub connected: bool,
    pub num_extruders: u32,
    pub print_error_code: Option<i32>,
    pub system_error_codes: Vec<(i32, i32)>,
    pub slot_groups: Vec<SlotGroupSnapshot>,
    pub print: PrintSnapshot,
}
```

`identifier` is the driver-facing printer identifier used by the compatibility web API as `printer_serial`; it may be a serial number for Bambu or another stable identifier for other drivers. Empty identifiers are preserved as empty. `print_error_code` and `system_error_codes` are top-level printer status fields because they can outlive an active print job, but they are transient and cleared when restart state is loaded. `PrintSnapshot` contains print-job status including `stage_code`. The compatibility `num_ams` field is derived from `InternalChanger` slot-group count for every printer kind.

Capabilities are printer/driver metadata exposed by `PrinterDriver::capabilities()` and `PrinterManager`, not part of the state snapshot.

Slot groups:

```rust
pub struct SlotGroupSnapshot {
    pub id: String,
    pub name: String,
    pub short_name: String,
    pub kind: SlotGroupKind,
    pub extruder: Option<u32>,
    pub temp: Option<f32>,
    pub humidity: Option<i32>,
    pub slots: Vec<MaterialSlotSnapshot>,
}

pub enum SlotGroupKind {
    InternalChanger,
    External,
    Toolhead,
    Virtual,
    Other,
}
```

Slots:

```rust
pub struct MaterialSlotSnapshot {
    pub id: SlotId,
    pub display_name: String,
    pub short_name: String,
    pub state: SlotState,
    pub filament: PrinterFilament,
    pub spool_id: Option<String>,
    pub consumed_since_load_g: f32,
    pub consumed_since_load_saved_g: f32,
    pub consumed_since_weight_g: f32,
    pub used_in_print: bool,
    pub pressure_advance_value: String,
    pub pressure_advance_meta: String,
}
```

Slot IDs should be opaque strings in the generic API/UI:

```rust
pub struct SlotId(pub String);
```

Bambu can use stable IDs such as:

- `bambu:0`
- `bambu:1`
- `bambu:16`
- `bambu:254`
- `bambu:255`

or simply `0`, `1`, `254`, `255` inside the Bambu adapter. The UI should not rely on their numeric meaning. UI-facing labels must come from `display_name`, `short_name`, group `name`, and group `short_name`.

### Printer Commands

Commands should express user intent, not Bambu protocol.

```rust
pub enum PrinterCommand {
    Refresh,
    PrintControl(PrintControlCommand),
    AssignMaterialToSlot {
        slot_id: SlotId,
        spool: FullSpoolRecord,
        temps: FilamentTemps,
        mode: SlotAssignMode,
    },
    ClearSlot {
        slot_id: SlotId,
    },
    UnassignSpoolFromSlot {
        slot_id: SlotId,
    },
    AddPressureAdvance(PressureAdvanceProfile),
    DriverSpecific(DriverCommand),
}
```

Assignment mode matters because current behavior has two variants:

- Assign only the app's spool ID to the slot.
- Write material/color/K information to the printer and also assign spool ID.

```rust
pub enum SlotAssignMode {
    SpoolIdOnly,
    WritePrinterMaterial,
}
```

For Bambu:

- `SpoolIdOnly` maps to `set_tray_spool_rec()`.
- `WritePrinterMaterial` maps to `set_tray_filament()`.

For another printer:

- It maps to that printer's writable material slot API.

### Printer Events

Drivers should emit generic events to the application layer.

```rust
pub trait PrinterObserver {
    fn on_printer_event(&mut self, event: PrinterEvent);
}

pub struct PrinterEvent {
    pub printer_id: PrinterId,
    pub kind: PrinterEventKind,
}

pub enum PrinterEventKind {
    ConnectivityChanged {
        connected: bool,
    },
    SnapshotChanged {
        change: PrinterChange,
        snapshot: Box<PrinterSnapshot>,
    },
    SlotTagScanned {
        slot_id: SlotId,
        tag_id: String,
        only_spool_id: bool,
    },
    MaterialSlotPresenceChanged {
        changes: Vec<MaterialSlotPresenceChange>,
    },
    PrintFileAnalysisRequested {
        request: PrintFileAnalysisRequest,
    },
    PrintFileAnalysisCanceled {
        job_number: i32,
    },
}

pub struct MaterialSlotPresenceChange {
    pub slot_id: SlotId,
    pub change: MaterialSlotPresenceChangeKind,
    pub spool_id: Option<String>,
}

pub enum MaterialSlotPresenceChangeKind {
    Inserted,
    Removed,
}
```

`PrinterEvent` is an envelope. The source printer ID is stored once on the envelope and event-specific data is stored in `PrinterEventKind`. Snapshot events carry a boxed `PrinterSnapshot`; handlers must use that snapshot instead of synchronously re-querying the source driver through `PrinterManager`.

Bambu now bridges its existing `BambuPrinterObserver` through an adapter-owned `BambuPrinterEventBridge`. Tray/snapshot refresh routes through `PrinterEventKind::SnapshotChanged`; the bridge builds the snapshot from the already-borrowed `BambuPrinter` to avoid a `RefCell` re-borrow through `PrinterManager` during the callback. The same bridge emits `MaterialSlotPresenceChanged` batches for physical spool insertion/removal transitions. `ViewModel` handles those presence batches by enqueueing application async work before it configures slots or updates staging, so it does not dispatch back into a printer while Bambu is still notifying observers.

The Fake driver is now a virtual/demo printer runtime rather than a purely synchronous in-memory mock. `PrinterCommand` dispatch queues work to the driver's runtime task, the task waits briefly to make the asynchronous update visible, mutates virtual printer state, and then emits generic `PrinterEventKind::SnapshotChanged` to subscribed `PrinterObserver`s. This keeps command execution closer to real networked printer drivers and avoids observer callbacks from inside a mutable `PrinterManager` dispatch borrow.

## Bambu Adapter Strategy

The first implementation should preserve Bambu internals.

Do not rewrite:

- `process_incoming.rs`
- `tray.rs`
- `bambu_print.rs`
- `calibration.rs`
- `printer_state.rs`
- `bambu_api.rs`
- `mqtt.rs`
- `outgoing.rs`

Instead add a wrapper, conceptually:

```rust
pub struct BambuPrinterDriver {
    inner: Rc<RefCell<BambuPrinter>>,
}
```

Responsibilities:

- Convert Bambu state to generic `PrinterSnapshot`.
- Convert generic `SlotId` to Bambu tray ID.
- Convert generic commands to existing Bambu methods.
- Bridge Bambu observer events into generic printer events through adapter-owned `BambuPrinterEventBridge`.
- Preserve Bambu persistence as-is.

This allows `ViewModel` to stop depending on `BambuPrinter` directly without destabilizing Bambu behavior.

## Persistence Architecture

Persistence must be split carefully because current state mixes generic and Bambu-specific concepts.

### Existing Inventory State

Keep existing inventory CSV and sidecar JSON initially.

Existing files:

- spool CSV database under store paths managed by `Store`
- `SpoolRecordExt` files under `/store/spools.ext/...`
- tag-location DB
- storage config

Generic concepts that remain:

- Spool records.
- Tag links.
- Weight fields.
- Consumption totals.
- Actual and assigned storage locations.
- Stock/split count.

Bambu leakage to tolerate short-term:

- `ext_has_k`
- `SpoolRecordExt.k_info`
- Bambu origin data.

### Bambu Printer State

Bambu persistence now uses the same one-file envelope as other restart-state-capable printer drivers while preserving Bambu private internals.

Current Bambu behavior:

- The state path is still derived from the Bambu serial by `BambuPrinter::printer_state_file_path()`.
- The generic section stores the full `PrinterSnapshot`.
- The driver-private section stores Bambu trays, AMS bits, extruders, calibration, printer name, and related private state as opaque JSON.
- Bambu print-project resume state remains separate because it is print-tracking runtime state, not the printer restart-state envelope.
- Old standalone Bambu restart-state file content is not treated as a compatibility contract for this migration; bad/old reads should fail cleanly rather than panic.

### Generic Printer State

All restart-state-capable drivers now use a generic printer app state file.

The common persistence layer owns scheduling and file handling: SD-card checks, load timing, periodic dirty saves, write verification, retries, path-collision checks, generic snapshot load/store, and generic dirty recovery. Drivers own stable identity, the exact state path, and optional driver-private JSON import/export.

The state path must not depend on UI order, runtime array index, or printer number. For Bambu the stable identity remains the printer serial. For Fake the stable identity is the configured `unique_id` through `fake_printer_{unique_id}`.

Conceptual shape:

```rust
pub struct PrinterStateFile {
    pub version: u32,
    pub printer_id: PrinterId,
    pub driver_kind: PrinterDriverKind,
    pub generic: PrinterSnapshot,
    pub driver_private: Option<serde_json::Value>,
}
```

Volatile fields are sanitized on load: printer connectivity, group temperature/humidity, and print remaining time. Missing spool IDs are cleared during generic state load and mark the snapshot dirty so the corrected state can be persisted.

Current Fake implementation stores only the generic snapshot under a driver-owned FAT 8.3 path like `/state/<8-char-id>.fak/startup.jsn`. The `<8-char-id>` is a deterministic short name derived from the stable Fake printer ID, and the `.fak` extension is owned by the Fake driver.

## Configuration Architecture

Previous `PrinterConfig` was Bambu-specific:

- `ip`
- `name`
- `serial`
- `access_code`
- `log_filter`
- `auto_restore_k`
- `track_print_consume`
- `fetch_3mf`
- `ignore_certificates`
- `printer_mode`
- `use_ams_scan`

Current `PrinterConfig` is driver-specific. It has generic display `name` plus a tagged driver config. `Bambu` and `Fake` exist; all Bambu-specific fields live in `BambuPrinterConfig`.

Current `static/config.html` is transitional. It has a printer type selector for Bambu Lab and Fake Non-Bambu. Slint renders Bambu and Fake slots through the shared standard slot-card view with capability-gated assign material, set spool ID, reset, and untag operations.

Target configuration should be driver-kind based.

Conceptual shape:

```rust
pub struct ConfiguredPrinter {
    pub name: Option<String>,
    pub driver: PrinterDriverConfig,
}

pub enum PrinterDriverConfig {
    Bambu(BambuPrinterConfig),
    Fake(FakePrinterConfig),
    Snapmaker(SnapmakerPrinterConfig),
    Moonraker(MoonrakerPrinterConfig),
    Prusa(PrusaPrinterConfig),
}
```

Bambu config mirrors the old flat Bambu fields.

Printer IDs are derived from the driver config rather than stored as a generic config field. For Bambu, the runtime ID is currently `bambu_printer_{serial}`. For Fake, the runtime ID is currently `fake_printer_{unique_id}`.

Printer numbers are separate from printer IDs. They are short one-based active printer numbers intended for square-bracket log prefixes such as `[1]`. Manager-owned logs derive them from active manager index plus one; driver-owned logs use the same number assigned when the active driver is created. Stable `PrinterId` values remain for routing, persistence, default-printer selection, and API references.

Backward compatibility:

- Existing flat `_printers_` config should load as Bambu and convert at the load boundary.
- Existing `_printer_` single-printer fallback should still load as Bambu.
- Old `DefaultPrinterConfig.serial` should convert to generic `DefaultPrinterConfig.printer_id` at the load boundary.

Current migration status:

- [done] `PrinterConfig` is a generic wrapper around `PrinterDriverConfig`.
- [done] Bambu fields moved to `BambuPrinterConfig`.
- [done] Flat legacy printer config is converted in `AppConfig` load only.
- [done] `/api/printer-config` DTO uses `driver_kind` plus `driver_config`.
- [done] Runtime printer initialization walks configured printers in a single config-order pass.
- [done] Short printer log labels use the active printer number, not stable `PrinterId`.
- [done] Missing serial/access-code validation is Bambu-specific.
- [done] Default printer selection stores generic `printer_id` and converts old Bambu serial defaults.
- [done] `static/config.html` has a printer type selector for Bambu Lab and Fake Non-Bambu.
- [done] Fake printer config can be persisted by DTO/model shape and `config.html`.
- [done] Fake runtime driver exists as a virtual/demo printer task, appears in web status, is selectable in the console, uses the shared slot-group UI, supports capability-gated slot operations, emits delayed generic snapshot-change events, and persists generic slot state.

Printer ID policy:

- Bambu: derive `bambu_printer_{serial}`.
- Fake: derive `fake_printer_{unique_id}`.
- Moonraker: configured URL or generated ID.
- Snapmaker: configured ID, serial if available, or generated stable ID.

## Slint Migration

Slint must become dynamic, but this should be staged.

### Original Problem

`ui/app.slint` originally hardcoded Bambu tray state:

- `empty-trays-state()` returned 26 fixed entries: two external plus 24 AMS/HT slots.
- `get_tray_id()` and `get_tray_index()` encode Bambu mappings.
- `curr-ams-id` and `ams-exists` drive AMS paging.
- `Trays` and `AmsButton` assumed Bambu AMS slot counts.

### Implemented UI Model

Fixed tray state was replaced with dynamic printer slot groups.

Current Slint structs:

```slint
export struct UiSlotGroup {
    id: string,
    name: string,
    kind: UiSlotGroupKind,
    slots: [UiSlot],
}

export struct UiSlot {
    id: string,
    name: string,
    state: UiSlotState,
    filament: UiFilament,
    spool-id: string,
    tagged: bool,
    weight-display: string,
    used-in-print: bool,
    k: string,
}
```

The visual layout should render groups generically:

- Horizontal/vertical group cards.
- Group title from backend.
- Slots in the group from backend.
- No assumption of 4 slots per group.
- No assumption of HT slot count.
- No numeric external tray IDs.

### Transitional Slint Plan

Phase 1 kept current Slint and adapted generic Bambu snapshots back into existing `trays-state`. This kept Bambu working while Rust architecture changed.

Phase 2 changed Slint to dynamic groups. Bambu and non-Bambu drivers are now represented through the same slot-group data contract.

## Web API Migration

Because the client can be changed with the backend, the current printer API can be replaced rather than versioned.

### Target Endpoints

Conceptual endpoints:

```text
GET  /api/printers
POST /api/printers/config
GET  /api/printers/status
POST /api/printers/command
POST /api/printers/slots/assign
POST /api/printers/slots/clear
POST /api/printers/slots/unassign-spool
GET  /api/printers/capabilities
```

Status should be typed JSON, not positional slot strings.

Slot assignment payload should be generic:

```rust
pub struct AssignSlotDTO {
    pub printer_id: String,
    pub slot_id: String,
    pub spool_id: String,
    pub mode: SlotAssignMode,
}
```

Pressure advance endpoints should become optional and capability-gated:

```text
GET  /api/printers/{id}/pressure-advance
POST /api/printers/{id}/pressure-advance
```

or be kept as Bambu-specific extension endpoints if the client only shows them for Bambu.

## Print Consumption Architecture

Generic inventory only needs consumption deltas by spool or slot.

Target generic flow:

```text
Driver detects consumption
  -> updates the generic PrinterSnapshotState slot counters
    -> store task reads unsaved snapshot deltas
      -> Store increments SpoolRecord.consumed_since_add
      -> Store increments SpoolRecord.consumed_since_weight
      -> PrinterManager acknowledges the saved high-water mark
```

Bambu implementation:

- Keep current gcode analysis and Bambu print tracking initially.
- Bambu reports final slot/gram consumption increments through its adapter bridge.
- The bridge updates generic snapshot consumption counters directly; the existing store task persists unsaved snapshot deltas into spool records.
- Preserve print resume state while doing so.
- Treat any `PrintFileAnalysisRequested`-style event as transitional Bambu-specific plumbing, not a required generic printer event.

Other drivers:

- May emit direct usage events.
- May have no consumption tracking.
- May need driver-specific print job state.

Do not require all printers to support consumption tracking.

## Pressure Advance Architecture

Pressure advance should be optional.

Bambu support remains:

- Existing `KInfo` in spool extension.
- Existing printer calibration table.
- Existing K matching and restore behavior.
- Existing add/select calibration commands.

Generic layer should expose capability and extension data, but not require all drivers to implement it.

Conceptual capability:

```rust
pub enum PressureAdvanceCapability {
    Unsupported,
    DriverManaged,
}
```

The UI/client should only show pressure advance features when the selected printer supports them.

## Material Slot Assignment Architecture

This is the most important cross-printer feature.

Generic assignment inputs:

- Printer ID.
- Slot ID.
- Spool record.
- Slicer/material code.
- Material type.
- Colors.
- Temperature hints.
- Optional driver-specific extension such as Bambu K.

Driver responsibilities:

- Validate the slot exists and is writable.
- Convert material metadata to printer-specific format.
- Send command to printer if possible.
- Update local slot-spool assignment state if command succeeds or if mode is app-only.
- Emit snapshot change.

Bambu mapping:

- Generic slot ID to tray ID.
- `AssignMaterialToSlot` with `WritePrinterMaterial` to `set_tray_filament()`.
- `AssignMaterialToSlot` with `SpoolIdOnly` to `set_tray_spool_rec()`.
- `ClearSlot` to `reset_tray()`.

## Recommended Migration Phases

### Phase 0: Documentation [done]

Completed by this document.

Purpose:

- Preserve architectural findings.
- Record product decisions.
- Avoid context loss between sessions.

### Phase 1: Add Generic Printer Domain Types [done]

Add `src/printer/` with no behavior changes.

Suggested files:

- `src/printer.rs` or `src/printer/mod.rs`
- `src/printer/types.rs`
- `src/printer/manager.rs`
- `src/printer/events.rs`
- `src/printer/commands.rs`

Add types:

- `PrinterId`
- `PrinterDriverKind`
- `PrinterCapabilities`
- `PrinterSnapshot`
- `SlotGroupSnapshot`
- `MaterialSlotSnapshot`
- `SlotId`
- `SlotState`
- `PrinterFilament`
- `PrinterCommand`
- `PrinterEvent`
- `PrinterObserver`

Acceptance criteria:

- Project builds.
- No Bambu behavior changes.
- No `ViewModel` behavior changes yet.

Current migration status:

- [done] Added generic printer domain scaffold in `src/printer/mod.rs`.
- [done] Added generic IDs, snapshots, slot groups, slots, capabilities, commands, events, observer, and driver trait.
- [done] Registered `mod printer` in `src/main.rs`.

### Phase 2: Add Bambu Adapter Snapshot [done]

Create `BambuPrinterDriver` wrapper.

Responsibilities:

- Hold `Rc<RefCell<BambuPrinter>>`.
- Produce generic snapshots from existing Bambu state.
- Convert generic slot IDs to Bambu tray IDs.
- Expose Bambu capabilities.

Acceptance criteria:

- Tests or debug logs can show generic snapshots for Bambu printers.
- Current UI still uses old direct Bambu path.
- Bambu protocol untouched.

Current migration status:

- [done] Added `src/printer/bambu_adapter.rs` with `BambuPrinterDriver`.
- [done] Adapter holds `Rc<RefCell<BambuPrinter>>`.
- [done] Adapter produces generic `PrinterSnapshot` from existing Bambu state.
- [done] Adapter maps generic `SlotId` values such as `bambu:255` to Bambu tray IDs.
- [done] Adapter exposes Bambu capabilities.
- [done] Adapter preserves compact `/api/printers-status` slot string output through compatibility conversion in `ViewModel`.

### Phase 3: Introduce PrinterManager Behind ViewModel [partial]

Add `PrinterManager` and initialize Bambu printers through it.

During this phase, `ViewModel` may still use compatibility methods that internally call the Bambu adapter.

Acceptance criteria:

- `ViewModel` no longer stores `SelectedPrinter<Vec<Rc<RefCell<BambuPrinter>>>>` as its primary printer-facing field.
- Current Bambu UI still works.
- Existing Bambu state restore/store still works.

Current migration status:

- [done] Added `src/printer/manager.rs` with a minimal `PrinterManager` bridge.
- [done] `ViewModel` owns a `RefCell<PrinterManager>`.
- [done] Bambu printers are added to `PrinterManager` during existing Bambu initialization.
- [done] Selected printer index is mirrored into `PrinterManager` when the UI selects a printer.
- [done] `/api/printers-status`, `ui_untag_slot`, and `ui_reset_slot` use `PrinterManager` instead of constructing `BambuPrinterDriver` directly.
- [done] Web print-command dispatch uses `PrinterManager::dispatch_by_id` instead of scanning `SelectedPrinter` directly.
- [not started] `ViewModel` still stores `SelectedPrinter<Vec<Rc<RefCell<BambuPrinter>>>>`.
- [not done] `ViewModel` still uses direct Bambu access for many unreworked flows.
- [not done] `SelectedPrinter<Vec<Rc<RefCell<BambuPrinter>>>>` is still the primary printer-facing field for those unreworked flows.

### Phase 4: Route Commands Through Generic Printer Commands [done]

Refactor these paths:

- [done] `ui_untag_slot` dispatches `PrinterCommand::UnassignSpoolFromSlot`.
- [done] `ui_reset_slot` dispatches `PrinterCommand::ClearSlot`.
- [done] `set_staging_to_slot` / `set_staging_to_tray_direct` dispatch `PrinterCommand::AssignMaterialToSlot` through `PrinterManager`.
- [done] `configure_slot_with_spool_async` routes by generic printer ID and slot ID, then dispatches `PrinterCommand::AssignMaterialToSlot` through `PrinterManager`.
- [done] web `/api/printer-command` dispatches `PrinterCommand::PrintControl` through `PrinterManager`.

Additional read-path migration:

- [done] `/api/printers-status` builds from `PrinterManager` snapshots only while preserving the existing compact response format.

They should dispatch `PrinterCommand` instead of calling `BambuPrinter` methods directly.

Acceptance criteria:

- Bambu slot configuration works exactly as before.
- Bambu AMS scan auto-configuration works.
- Reset/untag behavior works.

### Phase 5: Generic Printer Events [partial]

Bridge `BambuPrinterObserver` into generic `PrinterEvent`.

Refactor `ViewModel` to handle generic events:

- [done] Connectivity changes route through `PrinterEventKind::ConnectivityChanged`.
- [done] Slot tag scans route through `PrinterEventKind::SlotTagScanned`.
- [done] Snapshot/tray refresh routes through `PrinterEventKind::SnapshotChanged` and the generic selected-printer slot-group refresh path.
- [done] Generic `PrinterObserver` subscription is wired through `PrinterManager`.
- [done] Fake/Demo slot changes are emitted from a runtime task as `PrinterEventKind::SnapshotChanged`, not synchronously from inside `dispatch`.
- [done] `PrinterEvent` is a source-printer envelope with event-specific `PrinterEventKind` payloads.
- [done] `SnapshotChanged` events carry `Box<PrinterSnapshot>`, and `ViewModel` consumes the event snapshot instead of re-querying the source printer.
- [done] `BambuPrinterDriver::start(...)` installs an adapter-owned `BambuPrinterEventBridge` that converts Bambu tray/connect/tag callbacks into generic printer events.
- [done] Bambu physical spool insertion/removal transitions route through generic `PrinterEventKind::MaterialSlotPresenceChanged` batches.
- [done] Staging-on-insert, configure-existing-spool-on-insert, unload-to-staging, and connect/disconnect terminal logs are handled from generic printer events.
- [done] `ViewModel` no longer forwards or handles tray/connect/tag behavior from its direct `BambuPrinterObserver` implementation.
- [done] Generic consumption storage reads snapshot slot consumption fields and acknowledges saved high-water marks through `PrinterManager`.
- [done] Bambu consumption increments route through the adapter bridge, which updates generic snapshot slot consumption counters directly.
- [not done] `ViewModel` still uses direct `BambuPrinterObserver` subscription for G-code analysis request/cancel dispatch.
- [not done] Bambu still derives consumption from its internal G-code tracking; only the final slot/gram report is generic.
- [not done] G-code analysis request/cancel remains Bambu-specific and should not become a required generic printer event.

Acceptance criteria:

- `ViewModel` no longer implements product logic directly in `BambuPrinterObserver`, except inside a bridge.
- Bambu UI updates still work.

### Phase 6: Dynamic Slint Slot Groups [mostly done]

Replace hardcoded Bambu tray arrays with backend-provided slot groups.

Required Slint changes:

- Replace `trays-state` fixed list with selected printer slot groups.
- Replace integer tray callbacks with string printer/slot IDs.
- Remove generic dependency on `curr-ams-id` and `ams-exists`.
- Keep Bambu-specific visual grouping only as data from the backend, not fixed UI structure.

Acceptance criteria:

- [done] Bambu AMS and external slots render from `PrinterSnapshot.slot_groups` through unified `UiSlotGroup` / `UiSlot` Slint data.
- [done] The fixed `trays-state` / `empty-trays-state()` Slint model was removed.
- [done] Main slot UI operations use opaque string slot IDs at the Slint/Rust boundary.
- [done] UI can render a fake printer with arbitrary slot groups and basic capability-gated slot operations.
- [done] Bambu AMS and external slots render through the same backend slot-group model while preserving the current Bambu visual layout.
- [done] Fake and Bambu now use the same standard circular slot-card UI path; group selector naming is generic (`SlotGroupTab`, primary/external groups).
- [not done] Generic slot view supports pagination/scrolling for large slot counts.

### Phase 7: Replace Printer Status API and Client [not started]

Replace `/api/printers-status` response shape with dynamic printer/slot groups if needed by the client work.

Keep compact encoded fields where needed for payload size; do not remove them unless the replacement preserves the size goal.

Acceptance criteria:

- Client updated to consume new typed shape.
- Bambu printer status displays correctly.
- Non-Bambu fake driver status can display.

### Phase 8: Generic Configuration and State for New Drivers [done]

Introduce driver-kind config.

Keep old Bambu config migration.

Introduce generic state for new drivers.

Acceptance criteria:

- [done] Existing flat Bambu config loads as Bambu through load-boundary migration.
- [done] Bambu config is stored as driver-specific `BambuPrinterConfig`.
- [done] Default printer selection works by derived generic printer ID.
- [done] New fake/non-Bambu config can be persisted by DTO/model shape and `config.html`.
- [done] Common printer-state scheduling loads/saves one state file per printer with a full generic `PrinterSnapshot` plus optional driver-private JSON.

### Phase 9: Add Fake Non-Bambu Driver [done]

Before Snapmaker, add a fake driver with arbitrary writable slots.

Purpose:

- Validate dynamic Slint.
- Validate API shape.
- Validate generic material assignment.
- Validate generic persistence.

Acceptance criteria:

- [done] Fake printer has configurable slot count.
- [done] Fake printer initializes through generic `PrinterManager`, starts a virtual-printer runtime task, appears in web status, and appears in the console printer selector with a generic slot view.
- [done] Fake slots are writable through `PrinterCommand::AssignMaterialToSlot`, including a Slint path to assign staging spool to a fake slot; commands are queued to the virtual runtime and reflected back via `PrinterEventKind::SnapshotChanged`.
- [done] Fake slots can be reset and unassigned from Slint through capability-gated generic slot operations.
- [done] Slot-spool/material state persists through the generic `PrinterSnapshot` state file.
- [done] Fake printer uses shared `PrinterSnapshot` state as its primary slot/material state.
- [done] Bambu still works.

### Phase 10: Add First Real Non-Bambu Driver [deferred]

This phase is intentionally out of scope until the remaining generic-printer migration work is complete. Do not start real non-Bambu driver work unless explicitly instructed by the user.

Start with material slot assignment.

Do not require print status or consumption tracking immediately.

Acceptance criteria:

- Driver can configure printer material slots from SpoolEase inventory.
- Driver-specific connection/configuration is isolated.
- Unsupported capabilities are not shown or are disabled in UI/client.

### Phase 11: Generic Pressure Advance / K Management [not started]

Move pressure-advance management out of Bambu-specific web/ViewModel paths when there is a real second driver need.

Acceptance criteria:

- Pressure-advance query/add APIs route through generic printer capability and driver interfaces.
- Bambu calibration internals remain inside the Bambu driver/adapter.
- Unsupported printers do not expose pressure-advance actions.
- Slot pressure-advance display continues to use generic snapshot fields.

## Risks

### RefCell Borrowing

Current code has several comments about avoiding nested `RefCell` borrows, especially around Bambu callbacks and async commands.

The generic event layer should avoid passing mutable printer references into observers. Prefer value events and snapshots.

### Persistence Migration

Restart-state persistence now writes the generic/private envelope. Old standalone Bambu restart-state content is not guaranteed to load; failures should be reported cleanly and must not panic. Bambu print-project resume persistence is separate and should stay intact.

### Slint Rewrite Size

Dynamic slot groups are a meaningful UI change. Do not combine this with Bambu protocol refactoring.

### API and Client Coordination

Because API can change, backend and client must be coordinated. Avoid leaving half-updated compatibility layers unless there is a clear transition plan.

### Scale Protocol Compatibility

Remote scale gcode analysis messages are serialized. Changing them can break older scale firmware. Version this protocol if it changes.

### Pressure Advance Scope Creep

Do not design a broad generic calibration system until there is a second real driver with similar requirements.

### Over-Generalizing Transport

Do not generalize MQTT now. Keep Bambu transport in the Bambu driver.

## Guardrails

- Preserve Bambu behavior until the adapter path is proven.
- Prefer adding wrappers over rewriting Bambu internals.
- Make slots opaque in generic code.
- Make capabilities explicit.
- Keep inventory generic.
- Keep pressure advance optional.
- Keep print consumption optional and driver-produced.
- Avoid driver-specific terms in generic UI/API names.
- Add fake driver before real non-Bambu driver to validate abstractions.

## Files Most Likely To Change

Early phases:

- `src/main.rs` to add `mod printer`.
- new `src/printer/*`.
- new Bambu adapter module, likely `src/bambu/driver.rs` or `src/printer/bambu_adapter.rs`.
- `src/view_model.rs` to introduce `PrinterManager` and command/event routing.

Middle phases:

- `ui/app.slint`
- `ui/trays.slint`
- `ui/slots.slint`
- `ui/tray-operations.slint`
- `ui/slot-display.slint`
- `src/web_app.rs`
- `src/app_config.rs`

Later phases:

- `src/store.rs`
- `src/spool_record.rs`
- `src/tag_v1.rs`
- `src/spool_scale.rs`
- `console/shared/src/gcode_analysis_task.rs`
- `console/shared/src/scale.rs`

Files to avoid changing early unless necessary:

- `src/bambu/process_incoming.rs`
- `src/bambu/tray.rs`
- `src/bambu/bambu_print.rs`
- `src/bambu/calibration.rs`
- `src/bambu/printer_state.rs`
- `src/bambu/bambu_api.rs`

## Suggested Immediate Next Steps

1. [not done] Add scrolling/pagination or another large-topology layout for generic slot groups.
2. [partial] Continue generic consumption migration: storage and snapshot counters are generic, but Bambu still derives consumption from internal G-code tracking and direct G-code analysis callbacks.
3. [not done] Continue reducing direct `SelectedPrinter<Vec<Rc<RefCell<BambuPrinter>>>>` use where a migrated generic path exists.
4. [deferred] Start the first real non-Bambu driver only after explicit user instruction.

## Session Handoff Instructions

For future sessions, start with:

```text
Read console/core/docs/printer-architecture.md. Continue from the next unfinished phase in the printer architecture migration. Preserve Bambu behavior and avoid rewriting src/bambu internals unless explicitly required.
```

If context is limited, read these files next:

- `src/bambu.rs`
- `src/bambu/tray.rs`
- `src/bambu/process_incoming.rs`
- `src/bambu/outgoing.rs`
- `src/view_model.rs`
- `src/app_config.rs`
- `src/web_app.rs`
- `ui/app.slint`
- `ui/trays.slint`

## Current Status

Migration code has started. Completed work: generic printer domain types, Bambu snapshot/command adapter, adapter-owned Bambu generic event bridge, generic `PrinterManager` storage, single-pass active-printer initialization, short generic printer-number log labels, `/api/printers-status` read projection through `PrinterManager` snapshots only while preserving compact output, slot unassign/reset/configure paths through `PrinterCommand`, web `/api/printer-command` through `PrinterCommand::PrintControl`, generic event routing for connectivity/tag-scan/snapshot-refresh events with boxed snapshot payloads, generic material-slot presence events for physical insert/remove transitions, adapter-applied generic snapshot consumption updates, driver-specific printer config with `BambuPrinterConfig` and `FakePrinterConfig`, generic derived default printer IDs, config UI driver-kind selection, explicit assign/set-spool-id/reset/untag slot capabilities, a fake/demo non-Bambu virtual printer runtime visible in web status, console-safe selection of generic printers, generic `PrinterObserver` subscription through `PrinterManager`, unified Slint `UiSlotGroup` / `UiSlot` rendering for Bambu and non-Bambu printers, standard circular slot-card UI for Bambu and Fake, backend-driven primary/external slot groups, opaque string slot IDs for main Slint slot actions, one-file generic/private printer restart-state persistence, Fake `PrinterSnapshot`-backed state, mandatory driver snapshot-state handles with dirty tracking, Bambu snapshot-backed spool/consumption/used-in-print/status fields, generic snapshot-based consumption storage with high-water acknowledgement, driver-provided slot/group display names, explicit slot pressure-advance display fields, and generic async configure-slot-with-spool routing by printer ID plus slot ID.

Still not done: full `PrinterManager` ownership replacement, Bambu-specific G-code analysis request/cancel callback migration, and paginated/scrollable dynamic Slint slot groups for large topologies. First real non-Bambu driver work is deferred until explicit user instruction.
