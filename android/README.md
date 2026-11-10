# gdirect mobile

An Android client for Google Drive that talks to Google over directly
reachable front-end IPs, with multi-connection downloads. It is a mobile port
of [mizorewww/gdirect](https://github.com/mizorewww/gdirect).

- **UI**: Flutter (Material 3), Chinese and English.
- **Engine**: the upstream Rust code (`core/`), called from Dart through a
  small C interface (`ffi/`, `dart:ffi`).
- **Downloads**: upstream uses a patched libaria2, which is impractical to
  cross-compile for Android. `core/src/download.rs` replaces it with a pure
  Rust downloader: ranged chunks over N connections, a `.gdl` control file for
  resuming, redirect following with a host allowlist, MD5 verification.

## Features

- **Browse**: My Drive, shared drives, shared with me, starred, recent, trash; search; list or thumbnail grid.
- **Quick look**: images, text, PDF (native renderer), Google Docs/Sheets; **video and audio play while they download** through a local stream (seek works).
- **Transfers**: parallel queue with a configurable limit, notification progress, resume. Downloads write straight into `Download/gdirect` (no staging copy, no double space). Uploads read the picked document directly (no copy). Whole **folders** can be downloaded and uploaded. Share files from any app to gdirect to upload them.
- **WebDAV**: turn it on in Settings to mount Drive in file managers, Kodi, VLC and so on (`http://127.0.0.1:8765/dav/`, user `gdirect`, random password shown in Settings). It is plain HTTP; the LAN switch exposes it to your network, so use it only on trusted Wi-Fi.
- **Sign in**: with your own OAuth client, or through **your own token server** (`server/`) when Google's sign-in pages are unreachable.
- **Network**: scans usable Google endpoints, re-scans when the network changes, and under a VPN binds to the real network so traffic stays direct.
- Several accounts, rename, star, trash, restore, delete, Docs export to docx/xlsx/pptx/pdf.

## Setup

Either create your own Google OAuth client (type *Desktop app*, Drive API enabled) and paste it in the app, or run the token server in `server/` (see `server/README.md`) and choose *My server* on the login page.

## Using with a VPN

With a normal VPN the app binds itself to the real network (Settings → Bypass VPN). This cannot work when the VPN is *always-on with "block connections without VPN"*; then exclude gdirect in your VPN app instead.

## Build

Requirements: Rust (stable), Android SDK + NDK 27, Flutter.

```sh
export ANDROID_HOME=$HOME/Android/Sdk
ABIS="arm64-v8a" ./scripts/build-rust-android.sh
cd app && flutter build apk --release --target-platform android-arm64
```

The CI workflow in `.github/workflows/android.yml` does the same for three ABIs.

## Releases and signing

Pushing a tag `android-vX.Y.Z` makes CI build arm64-v8a, armeabi-v7a and x86_64 APKs and attach them to a release. Add the repository secrets `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD` to sign with your own key; locally use `app/android/key.properties` (`storeFile`, `storePassword`, `keyAlias`, `keyPassword`). Without them the debug key signs the build.

## Limits

- Not tested on every device; network binding and WebDAV behaviour vary by vendor.
- WebDAV is not encrypted.
- On Android 15 the `dataSync` foreground service is limited to about 6 hours per day.

## License

GPL-2.0-or-later, as the upstream project. See `LICENSE`.
