# Console Metrics

When the `metrics` feature is enabled, the console writes a pair of `METRICS[n]` lines at each periodic snapshot. The framework line covers shared framework work. The console line covers printer MQTT processing and the HTTPS slots API.

The periodic snapshot resets only its own 30-second metric window. The same observations also accumulate in a second window until `GET /api/debug/metrics` is fetched. That endpoint returns the last periodic snapshot followed by the accumulated metrics, then begins a new accumulated window.

The Debug panel prepends its browser clock to the returned text. The console reports the accumulated window's best available start time and its monotonic elapsed duration. Heap information belongs only to the periodic snapshot and is not accumulated.

`n` is the periodic snapshot sequence number since boot.

## Statistic Format

Duration and byte metrics use this format:

```text
(n=<samples> min=<minimum> avg=<average> max=<maximum>)
```

- `n` is the number of observations in the reported period.
- `min`, `avg`, and `max` are integer values.
- Duration names ending in `_us` use microseconds.
- Names ending in `_bytes` use bytes.
- A metric with `n=0` had no observations in the reported period.

Plain `..._count` and `mqtt_change_sources` values are counters, not duration statistics.

## Framework Metrics

### `ui_draw_us`

Time spent performing a completed embedded Slint render, including the display backend work needed to render and transfer the dirty display region.

It measures each completed draw. It does not identify what requested the draw.

### `ui_redraw_request_to_draw_us`

Time from the selected printer's slot-model update to completion of the next successful UI draw.

This metric is currently marked only after the ViewModel handles a selected-printer slot snapshot change. It includes the wait for the next draw and the draw itself. It does not measure all possible Slint redraw sources.

### `sd_write_us`

Time spent in instrumented SD-card write operations.

It covers byte writes through the framework SD store, including appends, file creation with bytes, and non-skipped file writes. It does not represent deletes, renames, reads, or an application state change that did not result in a write.

### `sd_write_bytes`

Number of bytes written by the same SD write operations measured by `sd_write_us`.

### `http_web_request_us`

Time spent handling a request through the normal console web application server. It includes route handling and response writing.

It starts after the request reaches the device router. It does not include desktop DNS lookup, TCP connection establishment, or reading the request before routing begins. It does not measure the separate bearer-token HTTPS API server.

### `tls_accept_to_first_read_us`

Time from accepting a TLS connection until the first request read becomes available. It includes TLS setup and the time before the peer sends request data.

This is a connection metric, not an application route metric.

## Console MQTT Metrics

### `mqtt_queue_len`

Number of internally queued MQTT packets immediately after a packet is enqueued for processing.

The queue capacity is 20 packets. This is queue occupancy, not message age or processing time. A maximum near 1 means the consumer is keeping up. A maximum near 20 means the queue reached capacity; the immediate publisher can discard the oldest packet in that situation.

### `mqtt_payload_bytes`

Size of the JSON payload of each internally delivered MQTT `Publish` packet.

It excludes MQTT framing, topic bytes, TLS, and TCP overhead. It includes payloads that later fail JSON parsing.

### `mqtt_json_parse_us`

Time to deserialize a MQTT JSON payload into the Bambu message type.

It includes JSON deserialization and the associated boxed result allocation. It excludes TLS receive, MQTT framing, internal queue wait, state application, notification, and display rendering.

### `mqtt_state_apply_us`

Time to apply a parsed Bambu `Print` message to the Bambu printer state.

It includes the synchronous state-update code. It excludes JSON parsing, subsequent observer notification, Slint model updates, and display rendering.

### `mqtt_changed_count`

Number of processed MQTT Print messages for which the existing Bambu processing code reported a state change.

This is not a count of all MQTT messages and is not necessarily a count of visible UI changes.

### `mqtt_change_sources`

Counters identifying the state mutations that contributed to changed MQTT messages:

- `fila_switch`: filament-switch state changed.
- `command`: a supported Bambu command changed state, such as filament-setting or calibration commands.
- `extruder`: nozzle or extruder metadata changed.
- `tray_routing`: tray target/current/previous routing fields changed.
- `ams_topology`: AMS existence, tray presence, tray read-done, or tray-reading bits changed.
- `ams_environment`: AMS humidity or temperature state changed.
- `ams_info`: AMS type or routing metadata changed.
- `ams_tray`: an AMS tray's state, filament, color, calibration, or metadata changed.
- `external_tray`: an external tray changed.
- `print_project`: tracked print-project state changed.

These counters record mutation sources, not messages. One MQTT message can increment several counters, and one source can increment more than once when it changes several values in one message. Their sum can therefore exceed `mqtt_changed_count`.

### `mqtt_changed_notify_us`

Time spent notifying Bambu observers after a changed MQTT message.

It includes synchronous generic printer snapshot work and observer callbacks, including any synchronous ViewModel work. It excludes JSON parsing, state application, and actual display rendering.

## HTTPS Slots API Metrics

These metrics cover `GET /api/internal/printers/slots` on the separate bearer-token HTTPS API server.

### `api_slots_build_us`

Time to build the API printer-slots response data from the ViewModel.

### `api_slots_json_encode_us`

Time to serialize that response data into JSON.

### `api_slots_response_write_us`

Time to write the JSON response to the client.

A high value with low build and encode times usually means the delay is in the network response path or the client accepting the response, not in response construction.

### `api_slots_response_bytes`

Size of the JSON response written by the slots API.

### `api_slots_error_count`

Number of slots API serialization failures or response-write failures.

## Example Interpretation

```text
mqtt_queue_len=(n=9 min=1 avg=1 max=1)
mqtt_changed_count=5
mqtt_change_sources=(... ams_environment=24 ...)
```

Nine MQTT packets reached the processing queue and it did not build backlog. Five messages were classified as changed. The environment source recorded 24 individual humidity or temperature mutations; that count can be larger than five because a single message can update multiple AMS environment values.
