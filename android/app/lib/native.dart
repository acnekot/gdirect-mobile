import 'package:flutter/services.dart';

/// The few Android features the app needs beyond Flutter.
class Native {
  static const _channel = MethodChannel('cc.gdirect/native');

  /// Copies a finished download into Downloads/gdirect; returns its URI.
  static Future<String> saveToDownloads(String path, String name) async =>
      (await _channel.invokeMethod<String>(
        'saveToDownloads',
        {'path': path, 'name': name},
      ))!;

  static Future<void> openUri(String uri, String? mime) =>
      _channel.invokeMethod('openUri', {'uri': uri, 'mime': mime ?? '*/*'});

  /// Keeps the app running (a foreground service) while transfers go on.
  static Future<void> keepAlive(bool on, String text) =>
      _channel.invokeMethod('keepAlive', {'on': on, 'text': text});

  static Future<void> notifyPermission() =>
      _channel.invokeMethod('notifyPermission');
}
