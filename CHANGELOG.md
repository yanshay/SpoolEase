# Changelog

## v0.1.3
- First public version

## v0.1.4 - 0.1.12
- Incremental features and fixes based on first users feedback
- Support all printers - P1S, X1C, A1/Mini
- Improve PN532 initialization
- Add fixed security key
- Updated tag encoding format
- Buttons to scroll terminal window
- Improve MQTT robustness
- Confirmation to all operations in settings screen
- Fix GitHub certificate expiry for OTA

## 0.2.1 
- Multi-printer support


## 0.3.0 
- Introduced SpoolEase Scale
- Improved Encoding to be more reliable
- Various bug fixes

## Next (not released yet)
- Easy Web-Config: Web config can be accessed by scanning the NFC module (PN532) with a mobile phone and security-key is filled automatically
- Web-Config always enabled
- Add optional manually entered Brand, Material Subtype, Color Name and Note to tag information, through mobile phone. Can scan NFC module with mobile to reach the app.
- Changed adding custom filament information in filament3d.org (tag scan on mobile) to use the csv format of web-config and storing in the mobile itself
- Add display of filament&tag information when pressing a slot or pressing staging twice
- Console web-application root redirects to relevant application depending on context (web config or encode)
- Can supply the security-key through url in a secure way (after #sk=sec-key)
- The previous two features + the mDNS support together allow to have a permanent link and set it as an app on mobile phone to access context sensitive application
