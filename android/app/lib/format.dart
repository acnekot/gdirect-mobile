import 'dart:ui';

/// Picks Chinese or English text by the phone's language.
String t(String zh, String en) =>
    PlatformDispatcher.instance.locale.languageCode == 'zh' ? zh : en;

String formatBytes(num? bytes) {
  if (bytes == null) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  var value = bytes.toDouble();
  var unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  final digits = unit == 0 || value >= 100 ? 0 : (value >= 10 ? 1 : 2);
  return '${value.toStringAsFixed(digits)} ${units[unit]}';
}

String formatSpeed(num bytesPerSecond) => '${formatBytes(bytesPerSecond)}/s';

String formatEta(int remainingBytes, int speed) {
  if (speed <= 0 || remainingBytes <= 0) return '';
  final seconds = remainingBytes ~/ speed;
  if (seconds < 60) return t('$seconds 秒', '${seconds}s');
  if (seconds < 3600) {
    return t('${seconds ~/ 60} 分 ${seconds % 60} 秒',
        '${seconds ~/ 60}m ${seconds % 60}s');
  }
  return t('${seconds ~/ 3600} 小时 ${seconds % 3600 ~/ 60} 分',
      '${seconds ~/ 3600}h ${seconds % 3600 ~/ 60}m');
}

String two(int n) => n.toString().padLeft(2, '0');

/// An RFC 3339 time from Drive, in the phone's time zone.
String formatTime(String? rfc3339) {
  final time = DateTime.tryParse(rfc3339 ?? '')?.toLocal();
  if (time == null) return '';
  return '${time.year}-${two(time.month)}-${two(time.day)} '
      '${two(time.hour)}:${two(time.minute)}';
}

/// File names that are safe to show as a short title.
String ellipsize(String text, int max) =>
    text.length <= max ? text : '${text.substring(0, max - 1)}…';

String formatDuration(Duration? d) {
  if (d == null) return '—';
  final s = d.inSeconds;
  if (s < 60) return t('$s 秒', '${s}s');
  if (s < 3600) return t('${s ~/ 60} 分 ${s % 60} 秒', '${s ~/ 60}m ${s % 60}s');
  return t('${s ~/ 3600} 小时 ${s % 3600 ~/ 60} 分', '${s ~/ 3600}h ${s % 3600 ~/ 60}m');
}
