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
- Filament3D.org Update: Custom filament info now uses Web-Config-compatible CSV format and is stored locally on the phone after scanning
- Tag Info Display: Filament and tag data shown when pressing a slot or double-pressing staging
- Context-Aware Redirects: Console root redirects to Web-Config or Encode app based on context
- Secure Key via URL: Security key can be passed via URL fragment (#sk=...)
- Persistent App Links: Combining context-aware redirects, secure URL fragments, and mDNS allows creating a permanent, app-like mobile shortcut to the right app view
- Spool Core Weight Update: Core weight can be updated without reweighing the spool
