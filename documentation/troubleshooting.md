# Troubleshooting Guide

Here are some common issues. Additional ones may be found in the [issues](https://github.com/yanshay/SpoolEase/issues) and [discussions](https://github.com/yanshay/SpoolEase/discussions) areas on GitHub, including closed issues, so be sure to search there as well.

### Flashing
- Device erasing/flashing seem to fail again and again and/or the device appear to get into endless boots and/or appears to be bricked - see #18 for resolution options and more details.

## Initialization
#### SpoolEase fails to establish communication with the NFC Tag Reader, showing a `TimeoutAck` error.
- Check that you configured the DIP switches correctly as described in the documentation.
- Inspect the soldering, verifying both the correctness of wire connections and the quality of the soldering. Look for solder bridges or other issues - see #13 for some more details.
- Initialization fails also if you haven't provided printer Serial/Access Code. It will how a message about that in the terminal log. See #19 for more details.
## Connectivity

#### SpoolEase successfully connects to the printer ("Printer is connected") but fails to establish a TLS connection with the error `TlsError(Eof)`.
- Verify that the printer's serial number and access code are entered correctly.

### Usage
- Scanning Babmbulab filaments doesn't work - Bambulab spools RFID is currently not supported. AMS supports them built in, no need for SpoolEase with those filaments. They don't include the Pressure Advance though. 
