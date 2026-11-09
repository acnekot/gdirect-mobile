import 'package:flutter/material.dart';


const folderMime = 'application/vnd.google-apps.folder';
const shortcutMime = 'application/vnd.google-apps.shortcut';

enum PreviewKind { none, image, text }

/// A Drive item as the engine reports it.
class DriveFile {
  DriveFile(this.json);
  final Map<String, dynamic> json;

  String get id => json['id'] as String;
  String get name => json['name'] as String;
  String get mimeType => json['mimeType'] as String;
  int? get size => int.tryParse('${json['size'] ?? ''}');
  String? get modified => json['modifiedTime'] as String?;
  bool get starred => json['starred'] == true;
  bool get trashed => json['trashed'] == true;
  String? get link => json['webViewLink'] as String?;
  String? get driveId => json['driveId'] as String?;
  Map<String, dynamic>? get shortcut =>
      (json['shortcutDetails'] as Map?)?.cast<String, dynamic>();

  bool get isFolder => mimeType == folderMime;
  bool get isShortcut => mimeType == shortcutMime;
  bool get shortcutToFolder => isShortcut && shortcut?['targetMimeType'] == folderMime;
  bool get isWorkspace =>
      mimeType.startsWith('application/vnd.google-apps.') && !isFolder && !isShortcut;

  /// Google documents can be exported; other Google apps files cannot.
  bool get exportable => const [
        'document',
        'spreadsheet',
        'presentation',
        'drawing',
      ].contains(mimeType.replaceFirst('application/vnd.google-apps.', ''));

  bool get canDownload => !isFolder && (!isWorkspace || exportable);

  String get _ext {
    final dot = name.lastIndexOf('.');
    return dot < 0 ? '' : name.substring(dot + 1).toLowerCase();
  }

  /// What a quick look can show for this file.
  PreviewKind get previewKind {
    if (isFolder || isShortcut || trashed) return PreviewKind.none;
    final m = mimeType;
    if (m == 'application/vnd.google-apps.document' || m == 'application/vnd.google-apps.spreadsheet') {
      return PreviewKind.text;
    }
    if (isWorkspace) return PreviewKind.none;
    if (const ['image/png', 'image/jpeg', 'image/gif', 'image/webp', 'image/bmp'].contains(m)) {
      return PreviewKind.image;
    }
    if (const ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp'].contains(_ext) && m == 'application/octet-stream') {
      return PreviewKind.image;
    }
    const textExt = [
      'txt', 'md', 'markdown', 'json', 'csv', 'tsv', 'log', 'xml', 'yaml', 'yml', 'toml', 'ini', 'conf', 'cfg',
      'html', 'htm', 'css', 'js', 'ts', 'dart', 'py', 'rs', 'go', 'java', 'kt', 'c', 'h', 'cpp', 'hpp', 'sh',
      'sql', 'srt', 'lrc', 'properties', 'gradle', 'tex', 'bib',
    ];
    if (m.startsWith('text/') ||
        const ['application/json', 'application/xml', 'application/x-yaml', 'application/javascript', 'application/x-sh']
            .contains(m) ||
        textExt.contains(_ext)) {
      return PreviewKind.text;
    }
    return PreviewKind.none;
  }

  /// The export format a quick look asks for, for Google documents.
  String? get previewExport => switch (mimeType) {
        'application/vnd.google-apps.document' => 'txt',
        'application/vnd.google-apps.spreadsheet' => 'csv',
        _ => null,
      };

  String get owner {
    final owners = (json['owners'] as List?) ?? const [];
    if (owners.isEmpty) return '';
    final o = (owners.first as Map).cast<String, dynamic>();
    return (o['me'] == true ? null : o['displayName'] as String?) ?? '';
  }

  IconData get icon {
    if (isFolder || shortcutToFolder) return Icons.folder_rounded;
    final m = mimeType;
    if (m.startsWith('image/')) return Icons.image_rounded;
    if (m.startsWith('video/')) return Icons.movie_rounded;
    if (m.startsWith('audio/')) return Icons.audiotrack_rounded;
    if (m == 'application/pdf') return Icons.picture_as_pdf_rounded;
    if (m.contains('zip') || m.contains('rar') || m.contains('7z') || m.contains('tar') || m.contains('gzip')) {
      return Icons.archive_rounded;
    }
    if (m.contains('spreadsheet') || m.contains('excel') || m == 'text/csv') {
      return Icons.table_chart_rounded;
    }
    if (m.contains('presentation') || m.contains('powerpoint')) {
      return Icons.slideshow_rounded;
    }
    if (m.contains('document') || m.contains('word')) return Icons.description_rounded;
    if (m.startsWith('text/')) return Icons.article_rounded;
    if (m == 'application/vnd.android.package-archive') return Icons.android_rounded;
    if (isShortcut) return Icons.shortcut_rounded;
    return Icons.insert_drive_file_rounded;
  }
}

/// One transfer, as the engine reports it.
class TransferItem {
  TransferItem(this.json);
  final Map<String, dynamic> json;

  int get id => json['id'] as int;
  bool get isDownload => json['kind'] == 'download';
  bool get isPreview => json['preview'] == true;
  String get name => json['name'] as String;
  String get path => json['path'] as String;
  String get status => json['status'] as String;
  int get done => (json['done'] as num).toInt();
  int get total => (json['total'] as num).toInt();
  int get speed => (json['speed'] as num).toInt();
  int get connections => (json['connections'] as num).toInt();
  String? get error => json['error'] as String?;
  String? get saved => json['saved'] as String?;
  int get created => (json['created'] as num).toInt();

  bool get active => status == 'queued' || status == 'running';
  bool get stopped => status == 'failed' || status == 'cancelled' || status == 'paused';
  double? get fraction => total > 0 ? (done / total).clamp(0.0, 1.0) : null;
}

/// Maps an engine error to something a person can act on.
String friendlyError(Object error) {
  final text = '$error';
  if (text.contains('not signed in')) return text;
  return text.length > 400 ? '${text.substring(0, 400)}…' : text;
}

/// A named place at the top of Drive.
enum Place { root, shared, starred, recent, trash, drives }

/// What the app observed about one transfer while it ran.
class TransferStats {
  DateTime? started;
  DateTime? finished;
  int peak = 0;
  int maxConnections = 0;
  final List<int> samples = [];

  void observe(TransferItem item) {
    final now = DateTime.now();
    if (item.status == 'running') {
      started ??= now;
      finished = null;
      if (item.speed > peak) peak = item.speed;
      if (item.connections > maxConnections) maxConnections = item.connections;
      samples.add(item.speed);
      if (samples.length > 120) samples.removeAt(0);
    } else if (started != null && finished == null && !item.active) {
      finished = now;
    }
  }

  Duration? get elapsed =>
      started == null ? null : (finished ?? DateTime.now()).difference(started!);

  /// Average speed over the time the transfer ran, given [done] bytes.
  int average(int done) {
    final seconds = elapsed?.inMilliseconds ?? 0;
    return seconds <= 0 ? 0 : done * 1000 ~/ seconds;
  }
}
