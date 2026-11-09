<p align="center"><img src="android/icon.png" width="112" alt="gdirect mobile 图标"></p>

# gdirect mobile

[English](README.md) · [发布页](https://github.com/acnekot/gdirect/releases) · [上游 gdirect](https://github.com/mizorewww/gdirect)

Google 云盘的 Android 客户端：通过当前网络仍能直连的 Google 前端 IP 访问云盘，下载支持多连接和断点续传。移植自 [mizorewww/gdirect](https://github.com/mizorewww/gdirect)，Flutter 界面加上上游的 Rust 引擎。

<p align="center"><a href="https://github.com/acnekot/gdirect/releases/download/android-v1.0.0/gdirect-mobile-android-arm64.apk"><img alt="下载 Android 版（arm64）" src="https://img.shields.io/badge/%E4%B8%8B%E8%BD%BD%20Android%20%E7%89%88%EF%BC%88arm64%EF%BC%89-3DDC84?style=for-the-badge&logo=android&logoColor=white"></a></p>

## 功能

- **浏览：** 我的云端硬盘、共享云端硬盘、与我共享、已加星标、最近、回收站，支持搜索。
- **快速浏览：** 点图片、文本文件、Google 文档或表格，不用下载就能直接查看。
- **快速下载：** 每个文件最多 16 连接，断点续传，MD5 校验；Google 文档、表格、幻灯片可导出为 docx、xlsx、pptx 或 pdf。
- **传输详情：** 每个传输都有实时速度曲线、平均和峰值速度、预计剩余时间、连接数和已用时间。
- **上传、文件管理、多账号：** 重命名、加星、移到回收站、恢复、删除。
- **网络扫描：** 找出当前网络能连通的 Google 节点。
- **长时间传输：** 前台服务让息屏后传输不中断。

## 安装与设置

1. 从[最新 Android 发布页](https://github.com/acnekot/gdirect/releases/latest)下载 APK（arm64，Android 10 及以上，用 debug 密钥签名）。
2. 创建自己的 Google OAuth 客户端，类型选“桌面应用”，并启用 Drive API。
3. 在应用里粘贴客户端 ID 和密钥（或导入 JSON），再在浏览器里登录。
4. 打开“设置 → 网络扫描”，每换一个网络扫描一次。

**与 VPN 共存：** Android 上应用自己无法绕过正在运行的 VPN。请在 VPN 或代理软件的分应用设置里让 gdirect 绕过，否则扫描和下载会走 VPN，不是直连。

**存储：** 下载完成后保存到 `Download/gdirect`。文件先暂存在应用目录，所以下载期间需要约两倍文件大小的空闲空间。

## 和上游的区别

上游的下载器依赖打过补丁的 libaria2，很难交叉编译到 Android。[`android/core/src/download.rs`](android/core/src/download.rs) 用纯 Rust 重写了它：分块 Range 请求、N 连接并行、`.gdl` 控制文件断点续传、带主机白名单的重定向跟随和 MD5 校验。`android/core` 里其余部分都是上游引擎。Flutter 应用通过一个小的 C 接口（`android/ffi`，`dart:ffi`）调用它。

仓库其余部分的桌面客户端、命令行和打包文件都是未改动的上游内容，请看上游 README。

## 构建

需要 Rust（stable）、带 NDK 27 的 Android SDK 和 Flutter。

```sh
cd android
export ANDROID_HOME=$HOME/Android/Sdk
ABIS="arm64-v8a" ./scripts/build-rust-android.sh
cd app && flutter build apk --release --target-platform android-arm64
```

[`.github/workflows/android.yml`](.github/workflows/android.yml) 在 CI 里构建三种 ABI。更多细节见 [`android/README.zh-CN.md`](android/README.zh-CN.md)。

## 已知限制

- 不能整个下载文件夹。
- release 包用 debug 密钥签名，正式分发前请配置自己的签名。
- Android 15 对 dataSync 前台服务有每天约 6 小时的限制。
- 暂未做按网络绑定，所以才有上面的 VPN 说明。

## 许可证

与上游相同，GPL-2.0-or-later，见 [LICENSE](LICENSE)。感谢 [mizorewww](https://github.com/mizorewww) 的 gdirect。
