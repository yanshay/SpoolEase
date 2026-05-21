# SpoolEase System

SpoolEase is a smart add-on system for Bambu Lab 3D printers that adds intelligence and control to every filament spool.

It features:
- NFC tags for automatic spool identification
- [**Comprehensive spool inventory management system**](https://docs.spoolease.io/docs/quickstart/inventory-ui-intro) - keeping your spools organized
- [**Precise Filament Weight Tracking**](https://docs.spoolease.io/docs/full-guide/spool-weight-tracking) using (1) weight scale, (2) print usage monitoring, and (3) a smart workflow - so you can tell how much filament is available in every spool
- [**Storage System Management**](https://docs.spoolease.io/docs/full-guide/storage-system) - quickly find where your spools are using NFC based location tracking and seamless workflow to keep location information up to date
- **Automatic slot configuration** for material, color, pressure advance (K)
- [**Printers Monitoring**](https://docs.spoolease.io/docs/full-guide/printers-monitoring) - the essentials for monitoring printers, prints progress and AMS units status
- [**Configurable Dashboard**](https://docs.spoolease.io/docs/full-guide/dashboard) - get the big picture of your spools inventory
- [**Virtual Spool Label**](https://docs.spoolease.io/docs/quickstart/view-tag-info) for viewing spool info on your mobile device
- Import and support for Bambu Lab Spools RFID tag
- Software support for OpenPrintTag (see support scope in [Register OpenPrintTag Spools](https://docs.spoolease.io/docs/quickstart/register-openprinttag-spools))
- Compatibility with your slicer filament settings
- Even serves as **backup for your pressure advance settings** (for when the printer loses them — and yes, it happens)

- Supports most common NFC tags - NTAG (recommended 215 and above) and Mifare Classic (with Mifare no support yet for virtual label feature)
- Supports data import from and use of Bambu Lab filament RFID tags
- Supports Bambu Lab X1, P1, A1, H2, P2, X2 product lines with AMS-Lite, AMS, AMS2-Pro and AMS-HT and Filament Track Switcher
- Supports multiple printers simultaneously (within resource limits)
- More ...

The system includes two products:  
- **SpoolEase Console** – The main hub with a display, managing inventory, weight tracking, AMS/External slot configuration, and showing AMS/External filament status.
- **SpoolEase Scale** – Measures spool weight and feeds data to the Console.

SpoolEase works well together with the [SpoolEase NFC tag holder](https://makerworld.com/en/models/2050083) that supports easily swappable NFC tag, material type and spool-id labels for spool reuse purpose.

And most importantly, even though it’s an open-source project, it’s fun and easy to build and surprisingly simple to set up!

- [Documentation](https://docs.spoolease.io/docs/welcome)
- [Flashing Web Site](https://www.spoolease.io)
- [Reddit](https://www.reddit.com/r/SpoolEase/)
- [Discord Server](https://discord.gg/6brKUCERcQ)

## Show Your Appreciation  
A **tremendous** amount of effort has gone into this project and continues to go in.  
If you find it valuable or helpful, please **Boost** the 3D models on MakerWorld and ⭐ **Star** the GitHub repo.

<div align="center">
  <a href="https://www.star-history.com/#yanshay/spoolease&Date">
    <img src="https://api.star-history.com/svg?repos=yanshay/spoolease&type=Date)" height="300px">
  </a>
</div>

## Inventory Management (Press to Enlarge)
<a href="readme/inventory-main.png">
  <img src="readme/inventory-main.png">
</a>

## Dashboarding
<a href="readme/dashboard-overview.png">
  <img src="readme/dashboard-overview.png">
</a>

## Printers Monitoring
<a href="readme/printer-monitoring-overview.png">
  <img src="readme/printer-monitoring-overview.png">
</a>

## A Few SpoolEase Console Screenshots
| While Printing (Weight Tracking in AMS) | Weighting Spool for Available  Filament|
|:--------:|:---------:|
| ![Printing](readme/printing.png) | ![Scale](readme/scale-loaded.png) |
| Spool Operations | Spool Information |
| ![Staging Operations](readme/staging-more.png) | ![Spool Information](readme/spool-information.png) |

## Press Below for (outdated) Video Demonstration of SpoolEase Console  
**SpoolEase now offers far more features than shown in these videos! See the latest in the docs.**

<div align="center">
  <a href="https://www.youtube.com/watch?v=WKIBzVbrhOg">
    <img src="https://img.youtube.com/vi/WKIBzVbrhOg/0.jpg" height="400px">
  </a>
  <a href="">
    <img src="readme/virtual-tag.png" height="400px">
  </a>
</div>

## Press Below for Video Demonstration of SpoolEase Scale
<div align="center">
  <a href="https://www.youtube.com/watch?v=3tB1VMCOK6c">
    <img src="readme/scale-youtube-cover.jpg" height="400px">
  </a>
</div>

---

**Notice:** This is a new project - while it has been installed by many happy users, new users should be aware that there are no warranties, liabilities, or guarantees, and they assume all risks involved.

## Collaboration

- For discussions, support and general discussions best to join SpoolEase [Discord Server](https://discord.gg/6brKUCERcQ)
- For questions, feedback, comments, etc. please use the [Repo discussions area](https://github.com/yanshay/SpoolEase/discussions)
- For getting notified on important updates, subscribe to the [Announcements Discussion](https://github.com/yanshay/SpoolEase/discussions/7)
- It would be real cool if you post your build in the [Introduce Your Build Discussion](https://github.com/yanshay/SpoolEase/discussions/8)

**I’d also greatly appreciate it if you could star SpoolEase GitHub repo.**

## Licensing Information

This project (including hardware designs, software, and case files) is freely available for you to build and use for any purpose, including within commercial environments. However, you may not profit from redistributing or commercializing the project itself. Specifically prohibited activities include:

- Selling assembled devices based on this project
- Selling kits or components packaged for this project
- Charging for the software or hardware designs
- Selling modified versions or derivatives
- Integrating the product, with or without modifications, into a commercial server offering, whether cloud-based or on-premise
- Offering paid installation, configuration, or support services specific to this project

To be clear: You CAN use this device in your business operations, even if those operations generate revenue. You CANNOT make money by selling, distributing, or providing services specifically related to this project or its components.

If you're interested in commercial licensing, redistribution rights, or other activities not permitted under these terms, please contact SpoolEase at gmail dot com for potential partnership opportunities.

## Detailed Instructions  

**Important:** Make Sure to Use Follow Docuemntation for Your Version.  

- **SpoolEase Console**  
  [Build](https://docs.spoolease.io/docs/build-setup/console-build)  
  [Setup](https://docs.spoolease.io/docs/build-setup/console-setup)  

- **SpoolEase Scale**  
  [Build](https://docs.spoolease.io/docs/build-setup/scale-build)  
  [Setup](https://docs.spoolease.io/docs/build-setup/scale-setup)

- **System Information**  
  [Usage](https://docs.spoolease.io/docs/quickstart/basic-usage-flows)  
  [Troubleshooting](https://docs.spoolease.io/docs/troubleshooting)

## Third Party Attributions
SpoolScale uses the following sources for it's spools weights catalog:  
- Scuk's "Empty Spool Weight Catalog": https://www.printables.com/model/464663-empty-spool-weight-catalog
- https://www.onlyspoolz.com/portfolio/

## License

SpoolEase Console software is licensed under Apache License, Version 2.0 **with Commons Clause** - see [LICENSE.md](LICENSE.md).

- ✅ Free for use
- ❌ Cannot be sold, offered as a service, or used for consulting, see [LICENSE.md](LICENSE.md) for more details
- 📧 For commercial licensing inquiries about restricted uses, contact: **SpoolEase at Gmail dot Com**

### Contribution Notice

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
the work by you, shall be licensed as above, without any additional terms or conditions.
