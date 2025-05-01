# Troubleshooting Guide

Here are some common issues. Additional ones may be found in the [issues](https://github.com/yanshay/SpoolEase/issues) and [discussions](https://github.com/yanshay/SpoolEase/discussions) areas on GitHub, including closed issues, so be sure to search there as well.

## Flashing
#### Device erasing/flashing seem to fail again and again and/or the device appear to get into endless boots and/or appears to be bricked
- see [Issue #18](https://github.com/yanshay/SpoolEase/issues/18) for resolution options and more details.

## Initialization
#### SpoolEase fails to establish communication with the NFC Tag Reader, showing a `TimeoutAck` error.
- Check that you configured the DIP switches correctly as described in the documentation.
- Inspect the soldering, verifying both the correctness of wire connections and the quality of the soldering. Look for solder bridges or other issues.
- Wires colors arrive in different variants, so colors could be misleading. Verify wiring based on the table by matching pin-number (on display) to signal-name (on PN532) see [Issues #13](https://github.com/yanshay/SpoolEase/issues/13), [Issue #26](https://github.com/yanshay/SpoolEase/issues/26) for some more details. It seems to be usually swapping yellow/green wires.
#### I see 'Initialization failed' message, but don't understand what has failed
- Initialization fails also if you haven't provided printer Serial/Access Code. It will how a message about that in the terminal log. See [Issue #19](https://github.com/yanshay/SpoolEase/issues/19) for more details.

## Connectivity
#### I see SpoolEase connected to wifi but I can't connect to it
- Verify you enabled Web-Config (swipe down until you see a button to enable Web-Config)
- There was a report that Pihole somehow caused communication issues, see [Duscussion #9](https://github.com/yanshay/SpoolEase/discussions/9) for more details.
#### SpoolEase successfully connects to the printer ("Printer is connected") but fails to establish a TLS connection with the error `TlsError(Eof)`.
- Verify that the printer's serial number and access code are entered correctly.

## Usage
#### Scanning Babmbulab filaments doesn't work
- Bambulab spools RFID is currently not supported. AMS supports them built in, no need for SpoolEase with those filaments. They don't include the Pressure Advance though. 

#### Unreliable encoding
- Use a high-quality, stable and strong enough USB power adapter.
- Try a different USB cable.
- Don’t place the tag directly on the PN532 — keep it about 1 cm away for reliable encoding.
- Ensure you’re using a compatible tag: NTAG215 only. Mifare, FeliCa, and the default tag shipped with the PN532 are not supported.
- Upgrade to 0.3.0 (still beta at time of writing)
