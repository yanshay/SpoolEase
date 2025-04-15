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

## Switching Between Multiple Printers

If you have several printers connected, and you have configured them through the web-config, switching between them is simple:

1. **Switch to Printers List**  
   - Swipe left on the display to expose the screen on the right
   - You will see there which printer is currently selected as well as which printer is the default of one is set.
2. **Select Printer to Display**  
   - Press the printer you want to be displayed and it will become the displayed printer.

Additional Notes:
- SpoolEase is connected to and monitors all configured printers simultaneously, switching only switch the display.
- Scanning and tag and loading the spool to an AMS of a printer that isn't displayed also switch display to that printer automatically.

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

- You may find it convenient to use the “Synchronize Filament List from AMS” feature in the slicer after loading tagged spools into the AMS, rather than manually selecting them in the slicer.

- To copy a spool’s tag, scan the source tag to move its data into staging, then encode the staging data onto the new tag.

--

# Viewing Spool Tag Information  

If you want to see the information stored on your NFC tag in a clear and organized way, simply scan the tag with your mobile phone. Ensure NFC is enabled (on Android, it may be disabled by default). You'll be redirected to a web page displaying the encoded tag information—a "Virtual Spool Tag."  
  
<div align="center">
  <img src="virtual-spool-tag.jpeg" alt="Virtual Spool Tag" height="800" >
</div>

## Identifying Filament Names  

- If you're using a standard filament type from BambuStudio slicer, its name will appear automatically.  
- If you've defined custom filaments, you'll initially see **"Custom Filament"** because SpoolEase needs additional information to recognize them.  

## Enabling Custom Filament Names  

By default, custom filaments appear as **"Custom Filament"** because the necessary data isn't available online. To enable proper filament names:  

1. Use **SpoolEase Desktop** to collect filament data from your computer.  
2. Store the exported data in an accessible online location.  
3. Link the stored data to SpoolEase via the "Virtual Spool Tag" web page.  

### Steps:  

1. **Get SpoolEase Desktop**  
   - Download **SpoolEase Desktop** from [SpoolEase.io](https://www.spoolease.io).  

2. **Export Filament Data**  
   - Run SpoolEase Desktop on the same computer where BambuStudio and/or OrcaSlicer is installed.  
   - Export the collected filament data as a JSON file.  

3. **Host the JSON File**  
   - Upload the JSON file to an online location that your mobile phone can access without CORS restrictions.  
   - GitHub is a good option (as a file in a repository or a Gist).  

4. **Provide the URL to SpoolEase**  
   - Get the **raw file URL** (not the normal web page view).  
   - On the "Virtual Spool Tag" web page, tap **"Custom Filament"** or the link at the bottom.  
   - Store the URL in your mobile's local storage (accessible only by the application).  

Once set up, scanning your NFC tag will display the actual filament name instead of "Custom Filament."  

### Updating Your Filament Data  

As you add more custom filaments, update the JSON file in the same location. There's no need to update the URL in SpoolEase if the file remains in the same place.  

--

# SpoolEase Scale Usage Instructions

The following instructions are applicable only if you have SpoolEase Scale in addition to SpoolEase Console.

## SpoolEase Scale User Interface

Interaction with SpoolEase Scale occurs primarily through the SpoolEase Console display. Additionally, SpoolEase Scale features an RGB LED that provides direct status information. 

For configuration, SpoolEase Scale offers a web application similar to SpoolEase Console.

## SpoolEase Scale Web Configuration

Since SpoolEase Scale has no display, its web configuration interface is always active. The security key isn't visible on the device, so you'll need to remember it. For convenience, you can use the same fixed key you configured for SpoolEase Console, which is displayed when web configuration is activated on that device.

If you forget your security key, follow the reset procedure described in the [SpoolEase Scale Setup](scale-setup.md) troubleshooting section.

Most configuration steps were completed during initial setup and won't be repeated here.

## Enabling SpoolEase Scale

By default, SpoolEase Console does not assume SpoolEase Scale is present in the system.

To enable it:
1. Access the web configuration of SpoolEase Console (not SpoolEase Scale web configuration)
2. You can enable it without additional information, allowing the system to automatically discover any SpoolEase Scale on the network
3. Alternatively, you can configure it to search for SpoolEase Scale at a specific IP address (useful if you've set a fixed IP) or to connect only to a specifically named SpoolEase Scale (configured during setup)

## LED Status Indicators

The RGB LED communicates the following states, with earlier states taking precedence:

- **Flashing Red** - SpoolEase Scale is not connected to WiFi
- **Constant Red** - SpoolEase Scale is not connected to SpoolEase Console
- **Orange** - The scale is not calibrated
- **Yellow** - Load detected on the scale, but weight is unstable
- **Blue** - Load detected on the scale and reading is stable

## SpoolEase Console Main Screen

A small rectangular information panel appears in the middle of the display, below the AMS slots view and above the message area. This panel shows SpoolEase Scale information with color-coding based on status:

- **Red background** - Indicates an error condition:
  - No communication with SpoolEase Scale
  - SpoolEase Scale is not calibrated

- **Yellow background** with weight value - Load detected but reading is unstable
- **Blue background** with weight value - Load detected and reading is stable

SpoolEase can display the weight of any object (typically a spool) and calculate net filament weight if the spool core weight is known from an NFC tag scan.

If a single value is shown, the system doesn't know the spool core weight.
If the spool core weight is known, it displays two values in the format `net-weight/total-weight` (e.g., `432g / 556g`). This means the total spool weight is 556g, with 432g of usable filament.

## Managing the User Spools List

To add entries to your user spools list, use the web configuration interface. The process is straightforward and explained within the web configuration itself.

## Encoding Weight Information

When SpoolScale is available, you can encode weight information to an NFC tag:
1. Place the spool on the scale and press encode, then pick the tray, or
2. Press encode, pick a tray, then place the spool on the scale

Either method will prompt you to:
1. Specify the spool core weight
2. Indicate whether this is a new unused spool or one that's already been used

**Important:** Keep the spool on the scale until you complete this process.

### Specifying Spool Core Weight

You can specify spool core weight through several methods:
1. **Pick from a list:**
   - User-specified list (entered through web configuration)
   - Previously used list (previously selected from the catalog or manually entered spools, and not in your user list)
   - Spool catalog (aggregated from various sources)
2. **Manual entry** - Enter the weight in grams manually
3. **Calculation** - Calculated for brand new spools, assuming standard filament amounts (1kg/750g/500g only at time of this writing)

After selecting the spool core weight, you'll specify whether this is a new or used spool. For new spools, the total weight will be encoded to track consumption from the original amount rather than relying solely on core weight data.

Once all information is entered, the display returns to the standard encoding process. At this point, remove the spool from the scale to encode the tag.

## Scanning Weight Information

The standard scanning procedure will read weight information if encoded in the NFC tag. The spool core weight will appear in the staging area.

If you place the scanned spool on the scale, both the total weight and net filament weight will display in the center of the screen as described earlier.

## Clearing the Previously Used Spools List

As the previously used spools list grows, you may wish to clear it. This can be done through the Settings screen.

## Recalibrating the Scale

If you notice inaccurate measurements, you can recalibrate the scale at any time by following the procedure in the [SpoolEase Scale Setup Guide](scale-setup.md).

## Finding SpoolEase Scale IP Address

If you need to know the IP address of SpoolEase Scale, even when SpoolEase Console is not connected to it, use the "Scale(s) Information" button in the Settings screen.

## Updating SpoolEase Scale Firmware

You can update SpoolEase Scale firmware through:
1. The initial installation web page you used during setup, or
2. Over the network via the web configuration interface

For network updates, use the web configuration to view the latest version and initiate the update process. To monitor progress, refresh the update page periodically. The device will automatically reboot once the update is complete.

