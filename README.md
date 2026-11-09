<p align="center"><img src="android/icon.png" width="112" alt="gdirect mobile icon"></p>

# gdirect mobile

[中文](README.zh-CN.md) · [Releases](https://github.com/acnekot/gdirect-mobile/releases) · [Upstream gdirect](https://github.com/mizorewww/gdirect)

An Android client for Google Drive that talks to Google over front-end IPs your network can still reach, with multi-connection resumable downloads. It is a mobile port of [mizorewww/gdirect](https://github.com/mizorewww/gdirect): a Flutter UI on top of the upstream Rust engine.

<p align="center"><a href="https://github.com/acnekot/gdirect-mobile/releases/download/android-v1.0.0/gdirect-mobile-android-arm64.apk"><img alt="Download for Android (arm64)" src="https://img.shields.io/badge/Download%20for%20Android%20%28arm64%29-3DDC84?style=for-the-badge&logo=android&logoColor=white"></a></p>

## Features

- **Browse:** My Drive, shared drives, shared with me, starred, recent and trash; search.
- **Quick look:** tap an image, a text file, a Google Doc or a Google Sheet to view it without downloading.
- **Fast downloads:** up to 16 connections per file, resumable, MD5 checked; Google Docs, Sheets and Slides export to docx, xlsx, pptx or pdf.
- **Transfer details:** live speed graph, average and peak speed, ETA, connections and elapsed time for every transfer.
- **Uploads, file management, several accounts:** rename, star, trash, restore, delete.
- **Network scan:** finds the Google endpoints reachable from your current network.
- **Long transfers:** a foreground service keeps them running with the screen off.

## Install and set up

1. Download the APK from the [latest Android release](https://github.com/acnekot/gdirect-mobile/releases/latest) (arm64, Android 10 or newer; signed with a debug key).
2. Create your own Google OAuth client of type *Desktop app* and enable the Drive API.
3. In the app, paste the client ID and secret (or import the JSON) and sign in in the browser.
4. Open *Settings → Network scan* and scan once per network.

**With a VPN:** Android lets no app bypass a running VPN by itself. Exclude gdirect in your VPN or proxy app's per-app settings, otherwise the scan and downloads go through it instead of directly.

**Storage:** finished downloads are saved to `Download/gdirect`. They are staged in app storage first, so a download needs about twice its size in free space for a while.

## How it differs from upstream

The upstream downloader uses a patched libaria2, which is impractical to cross-compile for Android. [`android/core/src/download.rs`](android/core/src/download.rs) replaces it with a pure Rust downloader: ranged chunks over N connections, a `.gdl` control file for resuming, redirect following with a host allowlist, and MD5 verification. Everything else in `android/core` is the upstream engine. The Flutter app talks to it through a small C interface (`android/ffi`, `dart:ffi`).

The desktop client, CLI and packaging files in the rest of this repository are the unchanged upstream project; see its README for them.

## Build

Needs Rust (stable), the Android SDK with NDK 27, and Flutter.

```sh
cd android
export ANDROID_HOME=$HOME/Android/Sdk
ABIS="arm64-v8a" ./scripts/build-rust-android.sh
cd app && flutter build apk --release --target-platform android-arm64
```

[`.github/workflows/android.yml`](.github/workflows/android.yml) builds three ABIs in CI. More detail is in [`android/README.md`](android/README.md).

## Limits

- Folders cannot be downloaded as a whole.
- The release APK uses the debug signing key; set up your own signing before distributing it.
- On Android 15 the data-sync foreground service is limited to about 6 hours a day.
- There is no per-network binding yet, hence the VPN note above.

## License

GPL-2.0-or-later, like upstream. See [LICENSE](LICENSE). Thanks to [mizorewww](https://github.com/mizorewww) for gdirect.
