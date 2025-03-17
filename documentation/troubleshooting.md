# Troubleshooting Guide

Here are some common issues. Additional ones may be found in the [issues](https://github.com/yanshay/SpoolEase/issues) and [discussions](https://github.com/yanshay/SpoolEase/discussions) areas on GitHub, including closed issues, so be sure to search there as well.


## Initialization
#### SpoolEase fails to establish communication with the NFC Tag Reader, showing a `TimeoutAck` error.
- Check that you configured the DIP switches correctly as described in the documentation.
- Inspect the soldering, verifying both the correctness of wire connections and the quality of the soldering. Look for solder bridges or other issues.

## Connectivity

#### SpoolEase successfully connects to the printer ("Printer is connected") but fails to establish a TLS connection with the error `TlsError(Eof)`.
- Verify that the printer's serial number and access code are entered correctly.

