# SpoolEase Usage Instructions

## SpoolEase User Interface

SpoolEase's user interface consists of three vertically stacked screens, with only one visible at a time. You can navigate between them by swiping up or down on the display. In some cases, such as during an OTA update, navigation may be temporarily disabled.  

### Screens (from top to bottom):
- **Terminal** – Displays logs  
- **Main Spools View** – The primary interface for managing spools  
- **Settings** – Configuration options  

After setup, the device starts on the terminal screen. Once the boot process completes successfully, it automatically switches to the main spools view.

## Encoding an NFC Tag

To encode an NFC tag, follow these steps:

1. **Set Tag Information**  
   In BambuStudio or Orca Slicers, set the required spool information for the NFC tag:
   - Filament type (material/vendor)
   - Color
   - PA profile (if applicable, but not mandatory)

   For easier encoding without affecting your AMS spools, it's recommended to use the **External Spool** option.

2. **Encode the Tag**  
   - Press the **'Encode'** button on the SpoolEase device. All available slots will flash.
   - Select the slot you set up in step 1.
   - A message will appear prompting you to place the spool tag to encode.
   - Place the NFC tag next to the right side of SpoolEase.
   - Once the encoding is successful, a confirmation message will appear. If it fails, repeat the process.

> **Note**: NFC tags have varying ranges depending on factors like the PN532 module, the NFC tag itself, and the USB power supply. Typically, the tag needs to be placed around 1 cm from the sensor. The exact placement may require some trial and error to find the optimal spot.

---

## Loading a Spool into AMS

Loading a spool into AMS is a hands-free process. Here's how to do it:

1. **Scan the Tag**  
   - Place the spool tag next to SpoolEase.
   - The information will be automatically loaded into the **‘Staging’** box located at the bottom left of the display.

2. **Wait for Confirmation**  
   - The information will remain in the Staging box for one minute. During this time, place the spool into the AMS.

3. **Automatic Slot Configuration**  
   - Once the spool is placed in the slot, SpoolEase will automatically recognize it and configure the slot with the corresponding information. No further action is needed on the SpoolEase display.

---

## Loading an External Spool

To load an external spool:

1. **Scan the Tag**  
   - Start the process just like loading a spool into AMS: place the spool tag next to SpoolEase.

2. **Configure the External Spool**  
   - Press the **‘Staging’** box at the bottom left of the display. All available slots will flash.
   - Then select **External Spool**.

This method can also be used with AMS, which is helpful when loading multiple spools at once. After scanning and configuring the slots manually, you can load all the spools together without waiting.

---

## Switching Between Multiple AMS Devices

If you have several AMS devices connected, switching between them is simple:

1. **Select AMS**  
   - Press the top area of the display where it shows the sets of four boxes, each representing an AMS.


## Operations in the Settings Screen

- Enable/Disable Web Config - Enable/Disable the application used for configuring SpoolEase
- Reset WiFi Credentials and Restart
- Restart Device
- Update Firmware Over Network


## Operations in the Settings Screen

The **Settings Screen** allows you to manage and configure SpoolEase. Below are the available options and their functions:

### Enable/Disable Web Config
This option enables or disables the web-based configuration interface for SpoolEase. When enabled, you can access the configuration page from a browser by following the instructions that will appear on the screen. Disabling it ensures that no further modifications can be made remotely until re-enabled.

### Reset WiFi Credentials and Restart
Selecting this option will erase the stored WiFi credentials and restart the device. After restarting, SpoolEase will enter WiFi setup mode, allowing you to connect it to a new network. This is useful if you need to switch networks or troubleshoot connectivity issues.

### Restart Device
This option simply reboots SpoolEase. It is helpful when applying new settings or troubleshooting minor issues without powering the device off manually.

### Update Firmware Over Network (OTA Update)
This feature allows SpoolEase to download and install the latest firmware updates directly over the network. When selected, the device will check for updates, download them if available, and proceed with the installation. During this process, the device may become temporarily unresponsive. After the update is complete, SpoolEase will automatically restart with the latest firmware.

> **Note:** During an OTA update, navigation between screens will be disabled until the process is complete.

## Additional Usage Tips

You may find it convenient to use the “Synchronize Filament List from AMS” feature in the slicer after loading tagged spools into the AMS, rather than manually selecting them in the slicer.
