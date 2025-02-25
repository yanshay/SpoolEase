# SpoolEase Setup Guide

## Overview

After building SpoolEase, you’ll need to set it up for your 3D printer. Follow these simple steps to complete the process.

### What You’ll Need

- **WiFi credentials** (network name/SSID and password) for a network that has access to the 3D printer.
- **Printer Serial Number** ([Find it in the Bambu Wiki](https://wiki.bambulab.com/en/general/find-sn)).
- **Printer Access Code** ([Find it in the Bambu Wiki](https://wiki.bambulab.com/en/knowledge-sharing/enable-lan-mode)) - no need to switch to LAN mode, just see where Access Code is shown on the printer.

### Setup Steps

1. **Flash the firmware onto the device**
2. **Configure WiFi settings**
3. **Enter printer information**
4. (Optional) **Configure additional settings** (not described here at this time)

> **Disclaimer**: The setup process is still being refined, so you may encounter some issues. If something doesn't work, try again or explore alternative steps. This is part of being an early adopter! 🙂

---

## Flashing the Firmware

The easiest way to flash the firmware is through Chrome or Edge (other browsers are not supported).

1. Connect the device to your computer using a USB-C cable.
2. Visit [https://yanshay.github.io/SpoolEase-Web/flash.html](https://yanshay.github.io/SpoolEase-Web/flash.html).
3. Click **"Web Flash Firmware"**.
4. Select the correct port (it may contain “JTag” in its name) and click **Connect**.
5. A dialog titled "SpoolEase" should appear. Click **Install SpoolEase** and confirm by pressing **Install**.
6. The device will erase old data (displaying "Erasing") and then install the new firmware (showing progress). This may take a few minutes.
7. Once installation is complete, the device will restart. You’ll see an "Installation complete!" message on the browser and WiFi setup instructions on the device screen.
8. There are now two ways to configure WiFi. If your network is accessible where you installed the firmware, follow the **WiFi Setup - Easy Way** section. Otherwise, skip to **Alternative WiFi Setup**.

---

## Setting Up WiFi - The Easy Way

> **If you encounter issues, retry by visiting** [http://spoolease.io/flash.html](http://spoolease.io/flash.html) **and clicking "Web Flash Firmware" again.**

1. Press **Next** on the "Installation complete!" dialog. This opens the **Configure WiFi** screen.
2. Select your WiFi network or enter it manually, then enter your WiFi password.
3. Press **Connect**.
4. Once connected, the **device** screen will switch to a terminal view. When it stops scrolling, it will display an IP address (it stops because it lacks printer information).
5. A confirmation dialog will appear in the browser stating the device is connected. Click **Visit Device** to open the configuration application.

---

## Setting Up Printer Information - The Easy Way

7. On the device, swipe down twice to reach the configuration instructions screen. You only need the Security Key shown in that screen.
8. Enter the **security key** from the device display into the browser and click **Verify Key**. A "Security Key Validated" message should appear.
9. In the **Printer Settings** section, enter the **Printer Serial Number** and **Access Code**. Optionally, enter the **Printer IP Address** (needed only for advanced network setups).
10. Click **Apply**. A confirmation should appear that settings were applied successfully.
11. Restart the device from the browser by pressing **Disable Web Config** followed by **Restart Device**.
12. After rebooting, if the printer is online, SpoolEase should display the main interface.

**Congratulations! Setup is complete.**

---

## Alternative WiFi Setup

> **Use this if the Easy WiFi Setup fails.**

1. Restart the device by disconnecting and reconnecting the USB power if no instructions appear.
2. Connect to the WiFi network **"SpoolEase"**.
   - On mobile, wait a few moments; the device may auto-open a setup page like airport WiFi.
   - If not, browse to the IP url shown on the device screen using your browser (note it's **http**, not **https**).
3. Enter the security key from the device display and press **Verify Key**.
4. Enter your WiFi credentials and press **Apply**.
5. Once confirmed, restart the device using the **Restart Device** button.

After rebooting, the terminal screen will display an IP address, indicating successful WiFi setup.

You may also encounter an error about a missing SDCard. An SDCard is not required; it’s only supported for cases where an alternative configuration method is needed for specific reasons.

---

## Alternative Printer Information Setup

1. Restart the device after setting up WiFi.
2. Swipe down twice on the terminal screen to access the **Enable Web Config** button.
3. Click **Enable Web Config**.
4. On a device connected to the same network, open a browser and enter the device's IP URL (note to use **http**, not **https**).
5. Enter the security key from the device display and press **Verify Key**.
6. In **Printer Settings**, enter:
   - **Printer Serial Number**
   - **Printer Access Code**
   - (Optional) **Printer IP Address** (only for complex networks)
7. Click **Apply**. A success message should appear.
8. Press **Disable Web Config** followed by **Restart Device**.

After rebooting, the AMS status of your printer should be displayed.

**Congratulations! You’ve completed the SpoolEase setup. Your device is now ready for use with your 3D printer.**

