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

## 0.3.1
- Easy Web-Config: Access by scanning the NFC module (PN532) with a phone; security key auto-filled
- Web-Config Always Enabled
- Extended Tag Info: Add Brand, Material Subtype, Color Name, and Notes via phone. Access by scanning the NFC module or browsing the URL shown in the Encode page title
- Custom Filaments CSV: Can now be generated directly via Web-Config—no need for SpoolEase-Desktop
- Filament3D.org Update: Custom filament info now uses Web-Config-compatible CSV format and is stored locally on the phone after scanning
- Tag Info Display: Filament and tag data shown when pressing a slot or double-pressing staging (use the custom filaments list)
- Context-Aware Redirects: Console root redirects to Web-Config or Encode app based on context
- Secure Key via URL: Security key can be passed via URL fragment (#sk=...)
- Persistent App Links: Combining context-aware redirects, secure URL fragments, and mDNS allows creating a permanent, app-like mobile shortcut to the right app view
- Spool Core Weight Update: Core weight can be updated without reweighing the spool

## 0.3.2
- Clarify clean install by explaining the security key in web-config and automatically showing the web-config screen on the console when configurations are incomplete

## 0.3.4
- Restore slot data on SpoolEase Console restart (useful for tag info and pressure advance (K) values on X1C) - **requires SD Card installed**
- Automatically detect brand and filament color from slot data when available
- Restore pressure advance (K) values after printer restart (primarily for X1C)
- Refine filament/tag info title display to reduce clutter and distinguish between auto-detected data and data encoded on the tag
- Fixed issue with configuring a large number of custom filaments or spool entries
- Improved tag formatting accuracy when the tag is not pre-formatted
