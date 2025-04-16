# SpoolEase Scale Setup Guide

## Overview

***Important:** SpoolEase Scale requires the SpoolEase Console to be installed with version 0.3.0 or later, and the Scale must be enabled via SpoolEase Console Web-Config.

The setup for SpoolEase Scale should begin partway through the build process—before the case is closed and the final two screws are installed. The instructions below specify the exact point where it’s best to pause the build and perform the setup.

The setup process is similar to the one you followed for the SpoolEase Console, with a few differences since the SpoolEase Scale doesn’t have a display.

### What You'll Need

- **WiFi credentials** (network name/SSID and password) for a network that has access to your 3D printer.

### Setup Steps

1. **Flash the firmware onto the device**
2. **Configure Connectivity settings**

---

## Flashing the Firmware

The easiest way to flash the firmware is through Chrome or Edge (other browsers are not supported).

1. Connect the SpoolEase Scale device to your computer using a USB-C cable.
2. Visit [https://www.spoolease.io/scale.html#firmware](https://www.spoolease.io/scale.html#firmware)
3. Press the "New Install" button for the version you want to flash at the bottom of the page.
4. Select the correct port for the board and click **Connect**.
5. A dialog titled "SpoolEaseScale" should appear. Click **Install SpoolEaseScale** and confirm by pressing **Install**.
6. The device will erase old data (displaying "Erasing") and then install the new firmware (showing progress). This may take a few minutes.
7. Once installation is complete, the device will restart. You'll see an "Installation complete!" message in the browser and WiFi setup instructions will follow.
8. There are now two ways to configure WiFi. If your network is accessible where you installed the firmware, follow the **WiFi Setup - Easy Way** section. Otherwise, skip to **Alternative WiFi Setup**.

---

## Setting Up WiFi - The Easy Way

> **If you encounter issues, retry by visiting** [https://www.spoolease.io/scale.html#firmware](https://www.spoolease.io/scale.html#firmware) **and clicking "New Install" again.**

1. Press **Next** on the "Installation complete!" dialog. This opens the **Configure WiFi** screen.
2. Select your WiFi network or enter it manually, then enter your WiFi password.
3. Press **Connect**.
4. A confirmation dialog will appear in the browser stating the device is connected. Click **Visit Device** to open the configuration application.

---

## Setting Up Additional Configuration - The Easy Way

1. Enter "Change-Me!" as Security Key and click **Verify Key**. A "Security Key Validated" message should appear.
2. It is recommended to change the security key by filling in the "Set a Fixed Key" field and pressing "Apply".
3. It is recommended to give the device a name so it can be identified as "<devicename>.local" on the network (this will work depending on the client you use) instead of using its IP address, which would be more difficult to find.
4. Enable or disable the NFC Module option (the one in SpoolEase Scale), depending on whether you installed it (by default it is enabled).
5. Restart the device from the browser by pressing **Restart Device**.
6. After rebooting, the front RGB LED will flash red until the device connects to the network, then it will remain steady red until SpoolEase Console connects to it.

**Congratulations! Setup is complete.** Please continue to verify that everything is working properly.

---

## Alternative Setup

> **Use this if the Easy WiFi Setup fails.**

1. Restart the device by disconnecting and reconnecting the USB power or pressing the Reset button on the board.
2. Connect to the WiFi network **"SpoolEaseScale"**.
   - On mobile devices, wait a few moments; the device may auto-open a setup page similar to airport WiFi.
   - If not, browse to http://192.168.2.1/config.html (note: use http NOT https, you must type http).
3. Enter "Change-Me!" as Security Key and click **Verify Key**. A "Security Key Validated" message should appear.
4. It is recommended to change the security key by filling in the "Set a Fixed Key" field and pressing "Apply" before configuring WiFi information.
5. Fill in the WiFi information (SSID and Password) and press Apply.
6. It is recommended to give the device a name so it can be identified as "<devicename>.local" on the network (this will work depending on the client you use) instead of using its IP address, which would be more difficult to find.
7. Restart the device from the browser by pressing **Restart Device**.
8. After rebooting, the front RGB LED will flash red until the device connects to the network, then it will remain steady red until SpoolEase Console connects to it.

**Congratulations! Setup is complete.** Please continue to verify that everything is working properly.

---

## Verifying that the Load Cell is Working Properly

1. On SpoolEase Console, go to the settings screen by swiping up from the bottom of the screen.
2. On page 2, press "Calibrate Scale".
3. On the title, you should see a number (negative or positive, it doesn't matter) - press the load cell and verify this number changes.
4. At this stage don't press anything else on this screen!

## Verifying that the NFC Reader is Working Properly

If you installed the NFC Module, scan a tag using SpoolEase Scale, and it should appear on SpoolEase Console as if you scanned it there.

---

**Congratulations! You've completed the SpoolEase connectivity setup.** 

But you aren't done yet!
After completing the case assembly, it's essential to calibrate the scale for accurate measurements.

## Calibrating the Scale
To ensure accurate weight measurements, you must calibrate the system to your specific load cell. This calibration needs to be performed only once after completing the case assembly, or whenever you suspect the accuracy has drifted.
Follow these steps to calibrate:

1. Find an object weighing approximately 750g - 1kg and measure its exact weight on a trusted scale.
2. On SpoolEase Console, go to the settings screen by swiping up from the bottom of the screen.
3. On page 2, press "Calibrate Scale".
4. Place SpoolEase Scale on a steady surface with nothing on it to establish the zero point.
5. Press the "Tare" button.
6. Place the calibration object on the scale and wait until the LED on SpoolEase Scale turns blue (indicating a stable reading). Note that it will initially show yellow, indicating an unstable reading.
7. Enter the object's exact weight in grams using the keypad on SpoolEase Console and press "Ok". The dialog will close.
8. Return to the main screen and verify the object's weight appears accurately (within ±1g tolerance).

## Troubleshooting

In case you made a mistake when entering WiFi information or don't remember the Security Key used, you can reset WiFi credentials and Security Key by pressing the Reset Button 5 times, with less than 5 seconds between presses (but not too quickly).

SpoolEase Scale will identify this sequence and clear both WiFi credentials and Security Key.

You can then reenter the information as described in the "Alternative Setup" section.
