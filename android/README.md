# gdirect mobile

> Android port of [mizorewww/gdirect](https://github.com/mizorewww/gdirect), living in the `android/` folder of this fork.

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

Quick look (tap an image, text file, Google Doc or Sheet to view it without downloading);
browse My Drive, shared drives, shared with me, starred, recent and trash;
search; download (Google Docs/Sheets/Slides export to docx/xlsx/pptx/pdf);
upload; rename, star, trash, restore, delete; several accounts; network scan
for usable Google endpoints; foreground service so transfers continue with
the screen off.

Finished downloads are saved to `Download/gdirect` through MediaStore (no
storage permission). They are staged in app storage first, so a download
temporarily needs about twice its size in free space.

## Setup

You need your own Google OAuth client (type *Desktop app*) with the Drive API
enabled. In the app, paste the client ID and secret (or import the JSON),
then sign in in the browser. The app listens on `127.0.0.1` for the redirect.

## Using with a VPN

Android has no way for the app to bypass a running VPN by itself. In your
VPN/proxy app, exclude gdirect (per-app proxy / bypass list); otherwise the
network scan and downloads go through the VPN instead of directly.

## Build

Requirements: Rust (stable), Android SDK + NDK 27, Flutter.

```sh
export ANDROID_HOME=$HOME/Android/Sdk
ABIS="arm64-v8a" ./scripts/build-rust-android.sh
cd app && flutter build apk --release --target-platform android-arm64
```

The CI workflow in `.github/workflows/android.yml` does the same for three ABIs.

## Limits

- Folders cannot be downloaded as a whole.
- The release APK is signed with the debug key; configure your own signing
  for distribution.
- On Android 15 the `dataSync` foreground service is limited to about 6 hours
  per day.
- No per-network binding yet (see the VPN note).

## License

GPL-2.0-or-later, as the upstream project. See `LICENSE`.
