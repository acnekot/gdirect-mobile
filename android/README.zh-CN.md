# gdirect mobile

gdirect 的 Android 版：通过可直连的 Google 前端 IP 访问 Google Drive，并支持多连接下载。移植自 [mizorewww/gdirect](https://github.com/mizorewww/gdirect)。

- **界面**：Flutter（Material 3），中英双语。
- **引擎**：上游 Rust 代码（`core/`），经 `ffi/` 的 C 接口由 Dart 通过 `dart:ffi` 调用。
- **下载**：上游依赖打过补丁的 libaria2，很难交叉编译到 Android。`core/src/download.rs` 用纯 Rust 重写：分块 Range 请求、N 连接并行、`.gdl` 控制文件断点续传、带主机白名单的重定向跟随、MD5 校验。

安装：到 [Android 发布页](https://github.com/acnekot/gdirect/releases/tag/android-v1.0.0) 下载 APK。

## 功能

快速浏览（点图片、文本、Google 文档/表格即可查看，无需下载）；浏览我的云端硬盘、共享云端硬盘、与我共享、已加星标、最近、回收站；搜索；下载（Google 文档/表格/幻灯片可导出为 docx/xlsx/pptx/pdf）；上传；重命名、加星、移到回收站、恢复、永久删除；多账号；网络扫描可用节点；前台服务保证息屏后传输不中断。

下载完成后通过 MediaStore 保存到 `Download/gdirect`，无需存储权限。文件先暂存在应用目录，所以下载期间需要约两倍文件大小的空闲空间。

## 使用前准备

需要自己的 Google OAuth 客户端（类型选“桌面应用”），并启用 Drive API。在应用里粘贴客户端 ID 和密钥（或导入 JSON），然后在浏览器里登录，应用会在 `127.0.0.1` 监听回调。

## 与 VPN 共存

Android 上应用自己无法绕过正在运行的 VPN。请在 VPN / 代理软件的分应用设置里让 gdirect 绕过，否则扫描和下载会走 VPN 而不是直连。

## 构建

需要 Rust、Android SDK + NDK 27、Flutter。

```sh
export ANDROID_HOME=$HOME/Android/Sdk
ABIS="arm64-v8a" ./scripts/build-rust-android.sh
cd app && flutter build apk --release --target-platform android-arm64
```

`.github/workflows/android.yml` 会为三种 ABI 做同样的构建。

## 已知限制

- 不能整个下载文件夹。
- release 包用 debug 密钥签名，正式分发请配置自己的签名。
- Android 15 对 `dataSync` 前台服务有每天约 6 小时的限制。
- 暂未做按网络绑定（见 VPN 一节）。

## 许可证

与上游相同，GPL-2.0-or-later，见 `LICENSE`。
