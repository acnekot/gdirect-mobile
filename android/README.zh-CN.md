# gdirect mobile

gdirect 的 Android 版：通过可直连的 Google 前端 IP 访问 Google Drive，并支持多连接下载。移植自 [mizorewww/gdirect](https://github.com/mizorewww/gdirect)。

- **界面**：Flutter（Material 3），中英双语。
- **引擎**：上游 Rust 代码（`core/`），经 `ffi/` 的 C 接口由 Dart 通过 `dart:ffi` 调用。
- **下载**：上游依赖打过补丁的 libaria2，很难交叉编译到 Android。`core/src/download.rs` 用纯 Rust 重写：分块 Range 请求、N 连接并行、`.gdl` 控制文件断点续传、带主机白名单的重定向跟随、MD5 校验。

## 功能

- **浏览**：我的云端硬盘、共享云端硬盘、与我共享、已加星标、最近、回收站；搜索；列表或缩略图网格。
- **快速浏览**：图片、文本、PDF（原生渲染）、Google 文档/表格；**视频和音频边下边播**（本地流，可拖动进度）。
- **传输**：并行队列（可设并行数）、通知栏进度、断点续传。下载直接写入 `Download/gdirect`，没有暂存副本，不占双倍空间；上传直接读取所选文档，不复制。支持**整个文件夹**下载和上传；从任意应用“分享”文件到 gdirect 即可上传。
- **WebDAV**：在设置里打开后，可在文件管理器、Kodi、VLC 等挂载云端硬盘（`http://127.0.0.1:8765/dav/`，用户名 `gdirect`，密码在设置里，随机生成）。这是明文 HTTP；打开“局域网”开关会暴露给同一网络，请只在可信 Wi-Fi 使用。
- **登录**：用自己的 OAuth 客户端，或者在 Google 登录页打不开时用**自己的令牌服务器**（`server/`）。
- **网络**：扫描可用的 Google 节点，网络变化时自动重扫，开着 VPN 时绑定到真实网络以保持直连。
- 多账号、重命名、加星、回收站、恢复、永久删除、文档导出为 docx/xlsx/pptx/pdf。

## 使用前准备

二选一：自建 Google OAuth 客户端（类型“桌面应用”，启用 Drive API）并粘贴到应用里；或者按 `server/README.md` 部署令牌服务器，在登录页选“我的服务器”。

## 与 VPN 共存

普通 VPN 下应用会自行绑定真实网络（设置 → 绕过 VPN）。如果 VPN 开了“始终开启并阻止未经 VPN 的连接”，这一招无效，请在 VPN 应用里让 gdirect 绕过。

## 构建

需要 Rust、Android SDK + NDK 27、Flutter。

```sh
export ANDROID_HOME=$HOME/Android/Sdk
ABIS="arm64-v8a" ./scripts/build-rust-android.sh
cd app && flutter build apk --release --target-platform android-arm64
```

`.github/workflows/android.yml` 会为三种 ABI 做同样的构建。

## 发布与签名

推送 `android-vX.Y.Z` 标签后，CI 会构建 arm64-v8a、armeabi-v7a、x86_64 三种 APK 并挂到 release。在仓库 Secrets 里添加 `ANDROID_KEYSTORE_BASE64`、`ANDROID_KEYSTORE_PASSWORD`、`ANDROID_KEY_ALIAS`、`ANDROID_KEY_PASSWORD` 即可用自己的密钥签名；本地构建用 `app/android/key.properties`（`storeFile`、`storePassword`、`keyAlias`、`keyPassword`）。都没配时用 debug 密钥。

## 已知限制

- 未在所有机型上测试，网络绑定和 WebDAV 的表现因厂商而异。
- WebDAV 不加密。
- Android 15 对 `dataSync` 前台服务有每天约 6 小时的限制。

## 许可证

与上游相同，GPL-2.0-or-later，见 `LICENSE`。
