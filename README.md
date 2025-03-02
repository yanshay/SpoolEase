# SpoolEase

This project is an ESP32S3 based add-on device for Bambulab 3D printers to encode and decode NFC tags attached to filament spools. The tags store filament information, which can be used to automatically configure printer settings when loading spools, whether through AMS or an external spool. It also provides visibility to the filaments currently loaded into the AMS's and the External Spool. 

> [!Note]
> This device coverage question keeps coming up, so I leave this update here for some time:
>
> ✅ P1S: OK - Tested to work
>
> ✅ X1C: OK - Tested to work
>
> ✅ A1/A1 Mini: OK - Tested to work

## Press Below for Video Demonstration

[![SpoolEase](https://img.youtube.com/vi/WKIBzVbrhOg/0.jpg)](https://www.youtube.com/watch?v=WKIBzVbrhOg)

Notice: This is a new project currently in its early stages, with testing limited to personal use. Users should be aware that there are no warranties, liabilities, or guarantees, and they assume all risks involved.

## Recommendations for Use at This Stage
- Please ensure you read through this page fully before building or using the device—several important guidelines and tips are provided.
- Since the project is still in its early stages with only a few users, I recommend using it with filaments you regularly work with. As the project gains more exposure, the NFC tag data format may need adjustments. Re-encoding a large number of tags could be inconvenient, and addressing backward compatibility at this stage may slow development. It’s best to wait a couple of weeks for stabilization and to address non-critical data format issues before using it on a large number of spools. Eventually, the project will transition to a state where tags are always backward compatible, but patience is advised until then.
- If you encounter issues, please report them. If you believe it’s a bug, use the issues section; if you’re unsure about the behavior, raise it in the discussions section.
- Bambulab printers are not without their quirks, particularly with K Factor handling. The X1C behaves differently than the P1S, and even the P1S has been observed to experience issues that require a restart. While slicer-only use may obscure these problems, SpoolEase makes them more visible. If you notice issues, try restarting the printer to see if they persist.
- Understanding the K Factor / Flow Dynamic Calibrations / Pressure Advance (all referring to the same concept) is crucial for achieving quality prints, but configuring it on Bambulab printers (especially with Bambu Studio) is not very intuitive. You can read more about it here. It’s important to focus on this if you want to get the best performance from your printer and maximize the value of SpoolEase, which offers an advantage over other solutions in this regard, including Bambulab filaments own RFID tags.

This project (including hardware designs, software, and case files) is freely available for you to build and use for any purpose, including within commercial environments. However, you may not profit from redistributing or commercializing the project itself. Specifically prohibited activities include:

- Selling assembled devices based on this project
- Selling kits or components packaged for this project
- Charging for the software or hardware designs
- Selling modified versions or derivatives
- Offering paid installation, configuration, or support services specific to this project

To be clear: You CAN use this device in your business operations, even if those operations generate revenue. You CANNOT make money by selling, distributing, or providing services specifically related to this project or its components.

If you're interested in commercial licensing, redistribution rights, or other activities not permitted under these terms, please contact SpoolEase at gmail dot com for potential partnership opportunities.

## Required Components

- [WT32-SC01 Plus](https://www.aliexpress.com/item/3256805864064800.html) (**make sure to pick the board and not accessories**)
- 7 wire cable with JST 1.25mm connector (I received one in the box together with WT32-SC01-Plus)
- [PN532 NFC reader module](https://www.aliexpress.com/item/3256806852006648.html) (**make sure to pick the module and not accessories**)
- [8-wire cable with JST 1.25mm connector](https://www.aliexpress.com/item/1005007079265201.html) - Optional but recommended in case of cable fault/soldering/different WT32-SC01 Plus packaging, instead of the 7-wire that's supposed to come with the WT32-SC01 Plus (**make sure to pick the 1.25mm connector size and 8 pins**)
- Soldering tools
- Power adapter capable of 2A current at 5V + USBC Cable (don't use the USB port on the printer!)
- 3D Model of SpoolEase case - [https://makerworld.com/en/models/1138678](https://makerworld.com/en/models/1138678)
- Four M2x10 screws to securely hold the display in place (not mandatory)

- NFC Tags (Ntag215) – Available in different types and qualities, including paper and PET stickers, typically round with a 25mm diameter. It’s recommended to test a few before purchasing in bulk. If using a dryer, ensure the adhesive is durable enough or choose a mounting method that prevents the stickers from falling off.

- (Optional) 3D Model of spool with place for NFC sticker tags - TBD

## Detailed Instructions
- [Build](documentation/build.md)
- [Setup](documentation/setup.md)
- [Usage](documentation/usage.md)

## Collaboration

- For questions, feedback, comments, etc. please use the [repo discussions area](https://github.com/yanshay/SpoolEase/discussions)
- For getting notified on important updates, subscribe to the [Announcements Discussion](https://github.com/yanshay/SpoolEase/discussions/7)
- If you want to try your luck with immediate online response, try the [Discord Channel](https://discord.com/channels/1344027434571272252/1344027676461105234)
- It would be real cool if you post your build in the [Introduce Your Build Discussion](https://github.com/yanshay/SpoolEase/discussions/8) 
## Licensing
This software is licensed under Apache License, Version 2.0  **with Commons Clause** - see [LICENSE.md](LICENSE.md).
- ✅ Free for use
- ❌ Cannot be sold, offered as a service, or used for consulting, see [LICENSE.md](LICENSE.md) for more details
- 📧 For commercial licensing inquiries about restricted uses, contact: **SpoolEase at Gmail dot Com** 
