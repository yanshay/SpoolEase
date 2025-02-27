# SpoolEase

This project is an ESP32S3 based add-on device for Bambulab 3D printers to encode and decode NFC tags attached to filament spools. The tags store filament information, which can be used to automatically configure printer settings when loading spools, whether through AMS or an external spool. It also provides visibility to the filaments currently loaded into the AMS's and the External Spool. 

This is a new project currently in its early stages, with testing limited to personal use. It is tested on a P1S printer but it reslies on the same protocols for X1C and probably also the A1 line. Users should be aware that there are no warranties, liabilities, or guarantees, and they assume all risks involved.

> [!Note]
> *Status Update*
>
> P1S: OK - Validated to work
>
> X1C: Current version doesn't work, but all issues resolved and will be release soon.
>
> A1: Partially tested, what was tested worked. Either way, will be released soon officially as well.

This project is intended for personal use only. Commercial use, distribution, or any alteration of the device or its components for commercial purposes is strictly prohibited. This includes modifying the hardware or software to create derivative works or using the project in any commercial product. By using this project, you agree to use it solely for non-commercial purposes.

## Press Below for Video Demonstration

[![SpoolEase](https://img.youtube.com/vi/WKIBzVbrhOg/0.jpg)](https://www.youtube.com/watch?v=WKIBzVbrhOg)
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

## Discussions 

For questions, feedback, comments, etc. please use the [repo discussions area](https://github.com/yanshay/SpoolEase/discussions)

## Licensing
This software is licensed under Apache License, Version 2.0  **with Commons Clause** - see [LICENSE.md](LICENSE.md).
- ✅ Free for personal/non-commercial use
- ❌ Cannot be sold, offered as a service, or used for consulting, see [LICENSE.md](LICENSE.md) for more details
- 📧 For commercial licensing inquiries about restricted uses, contact: **SpoolEase at Gmail dot Com** 
