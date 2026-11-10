import 'package:flutter/services.dart';

/// A document picked or shared on the phone.
class PickedFile {
  PickedFile(this.uri, this.name, this.size, [this.path = '']);
  factory PickedFile.from(Map<dynamic, dynamic> m) => PickedFile(
        m['uri'] as String,
        m['name'] as String,
        (m['size'] as num?)?.toInt(),
        (m['path'] as String?) ?? '',
      );

  final String uri;
  final String name;
  final int? size;

  /// Folder path inside a picked folder ('' at its top).
  final String path;
}

/// The network the phone is on, as far as transfers care.
class NetworkState {
  NetworkState(this.id, this.kind, this.vpn, this.bypassing);
  factory NetworkState.from(Map<dynamic, dynamic>? m) => NetworkState(
        (m?['id'] as String?) ?? '',
        (m?['kind'] as String?) ?? 'none',
        m?['vpn'] == true,
        m?['bypassing'] == true,
      );

  /// Changes when the phone moves to another Wi-Fi or to mobile data.
  final String id;
  final String kind;
  final bool vpn;

  /// The app's connections skip the VPN and use the real network.
  final bool bypassing;
}

/// The few Android features the app needs beyond Flutter.
class Native {
  static const _channel = MethodChannel('cc.gdirect/native');

  /// Called when the phone's network changes.
  static void Function(NetworkState state)? onNetwork;

  /// Called when another app shares files with this one while it runs.
  static void Function()? onShared;

  static void listen() {
    _channel.setMethodCallHandler((call) async {
      switch (call.method) {
        case 'network':
          onNetwork?.call(NetworkState.from(call.arguments as Map?));
        case 'shared':
          onShared?.call();
      }
    });
  }

  /// Copies a finished download into Downloads/gdirect; returns its URI.
  static Future<String> saveToDownloads(String path, String name, String? subdir) async =>
      (await _channel.invokeMethod<String>(
        'saveToDownloads',
        {'path': path, 'name': name, 'subdir': subdir},
      ))!;

  /// The public folder downloads can be written to directly, if the system
  /// allows that (Android 11 and later).
  static Future<String?> downloadsDir() => _channel.invokeMethod<String>('downloadsDir');

  /// Makes a file written to Downloads show up there; returns its URI.
  static Future<String> scanFile(String path) async =>
      (await _channel.invokeMethod<String>('scanFile', {'path': path}))!;

  static Future<void> openUri(String uri, String? mime) =>
      _channel.invokeMethod('openUri', {'uri': uri, 'mime': mime});

  /// Keeps the app running (a foreground service) while transfers go on.
  static Future<void> keepAlive(bool on, String text, int progress) =>
      _channel.invokeMethod('keepAlive', {'on': on, 'text': text, 'progress': progress});

  static Future<void> notifyPermission() => _channel.invokeMethod('notifyPermission');

  /// Sends the app's connections around a running VPN (or stops doing so).
  static Future<NetworkState> bypassVpn(bool on) async =>
      NetworkState.from(await _channel.invokeMethod<Map>('bypassVpn', {'on': on}));

  static Future<NetworkState> networkState() async =>
      NetworkState.from(await _channel.invokeMethod<Map>('networkState'));

  static Future<List<PickedFile>> pickFiles() async =>
      ((await _channel.invokeMethod<List>('pickFiles')) ?? const [])
          .map((e) => PickedFile.from(e as Map))
          .toList();

  /// A folder's name and every file below it; null when nothing was picked.
  static Future<(String, List<PickedFile>)?> pickFolder() async {
    final reply = await _channel.invokeMethod<Map>('pickFolder');
    if (reply == null) return null;
    return (
      reply['name'] as String,
      (reply['files'] as List).map((e) => PickedFile.from(e as Map)).toList(),
    );
  }

  /// Opens a document for reading; the descriptor stays open until [closeFd].
  static Future<int> openFd(String uri) async =>
      (await _channel.invokeMethod<int>('openFd', {'uri': uri}))!;

  static Future<void> closeFd(int fd) => _channel.invokeMethod('closeFd', {'fd': fd});

  /// Files other apps shared with this one since the last call.
  static Future<List<PickedFile>> takeShared() async =>
      ((await _channel.invokeMethod<List>('takeShared')) ?? const [])
          .map((e) => PickedFile.from(e as Map))
          .toList();

  /// The text of a small picked document (a client JSON, for instance).
  static Future<String> readText(String uri) async =>
      (await _channel.invokeMethod<String>('readText', {'uri': uri}))!;

  static Future<int> pdfPages(String path) async =>
      (await _channel.invokeMethod<int>('pdfPages', {'path': path}))!;

  /// Renders a page to an image file and returns its path.
  static Future<String> pdfPage(String path, int index, int width) async =>
      (await _channel.invokeMethod<String>(
        'pdfPage',
        {'path': path, 'index': index, 'width': width},
      ))!;

  static Future<void> pdfClose() => _channel.invokeMethod('pdfClose');
}
