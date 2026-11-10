import 'package:flutter_test/flutter_test.dart';
import 'package:gdirect_mobile/engine.dart';
import 'package:gdirect_mobile/format.dart';
import 'package:gdirect_mobile/models.dart';
import 'package:gdirect_mobile/native.dart';

void main() {
  test('byte sizes read naturally', () {
    expect(formatBytes(0), '0 B');
    expect(formatBytes(1536), '1.50 KB');
    expect(formatBytes(10 * 1024 * 1024), '10.0 MB');
    expect(formatBytes(null), '—');
    expect(formatSpeed(2048), '2.00 KB/s');
  });

  test('times show in a stable format', () {
    final text = formatTime('2026-11-09T06:40:00Z');
    expect(RegExp(r'^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$').hasMatch(text), isTrue);
    expect(formatTime(null), '');
    expect(formatTime('nonsense'), '');
  });

  test('the engine envelope is unwrapped', () {
    expect(decodeReply('{"ok": {"a": 1}}'), {'a': 1});
    expect(decodeReply('{"ok": null}'), isNull);
    expect(() => decodeReply('{"error": "boom"}'), throwsA(isA<EngineError>()));
  });

  test('drive files classify themselves', () {
    final folder = DriveFile({'id': 'a', 'name': 'x', 'mimeType': folderMime});
    expect(folder.isFolder, isTrue);
    expect(folder.canDownload, isFalse);

    final doc = DriveFile({'id': 'b', 'name': 'd', 'mimeType': 'application/vnd.google-apps.document'});
    expect(doc.isWorkspace && doc.exportable && doc.canDownload, isTrue);

    final form = DriveFile({'id': 'c', 'name': 'f', 'mimeType': 'application/vnd.google-apps.form'});
    expect(form.canDownload, isFalse);

    final file = DriveFile({'id': 'e', 'name': 'a.zip', 'mimeType': 'application/zip', 'size': '12'});
    expect(file.size, 12);
    expect(file.canDownload, isTrue);

    final link = DriveFile({
      'id': 'f',
      'name': 'l',
      'mimeType': shortcutMime,
      'shortcutDetails': {'targetId': 't', 'targetMimeType': folderMime},
    });
    expect(link.shortcutToFolder, isTrue);
  });

  test('transfers report progress', () {
    final t = TransferItem({
      'id': 1, 'kind': 'download', 'name': 'a', 'path': '/p', 'status': 'running',
      'done': 50, 'total': 200, 'speed': 10, 'connections': 4, 'created': 0,
    });
    expect(t.active, isTrue);
    expect(t.fraction, 0.25);
    expect(t.direct, isFalse);
    expect(t.wantsFd, isFalse);

    final upload = TransferItem({
      'id': 2, 'kind': 'upload', 'name': 'a', 'path': 'content://x', 'status': 'queued',
      'done': 0, 'total': 9, 'speed': 0, 'connections': 0, 'created': 0,
      'source': 'content://x', 'wantsFd': true,
    });
    expect(upload.source, 'content://x');
    expect(upload.wantsFd, isTrue);
  });

  test('picked files and network states decode', () {
    final file = PickedFile.from({'uri': 'content://a', 'name': 'a.txt', 'size': 3, 'path': 'x/y'});
    expect((file.name, file.size, file.path), ('a.txt', 3, 'x/y'));
    expect(PickedFile.from({'uri': 'u', 'name': 'n', 'size': null}).path, '');
    final net = NetworkState.from({'id': '101', 'kind': 'wifi', 'vpn': true, 'bypassing': true});
    expect(net.vpn && net.bypassing, isTrue);
    expect(NetworkState.from(null).kind, 'none');
  });

  test('quick look picks the right kind', () {
    DriveFile f(String name, String mime) => DriveFile({'id': 'x', 'name': name, 'mimeType': mime});
    expect(f('a.png', 'image/png').previewKind, PreviewKind.image);
    expect(f('a.svg', 'image/svg+xml').previewKind, PreviewKind.none);
    expect(f('notes.md', 'application/octet-stream').previewKind, PreviewKind.text);
    expect(f('a.json', 'application/json').previewKind, PreviewKind.text);
    expect(f('a.zip', 'application/zip').previewKind, PreviewKind.none);
    final doc = f('d', 'application/vnd.google-apps.document');
    expect(doc.previewKind, PreviewKind.text);
    expect(doc.previewExport, 'txt');
    expect(f('s', 'application/vnd.google-apps.spreadsheet').previewExport, 'csv');
    expect(f('p', 'application/vnd.google-apps.presentation').previewKind, PreviewKind.none);
    expect(f('a.mp4', 'video/mp4').previewKind, PreviewKind.media);
    expect(f('a.mkv', 'application/octet-stream').previewKind, PreviewKind.media);
    expect(f('a.mkv', 'application/octet-stream').mediaMime, 'video/*');
    expect(f('a.flac', 'audio/flac').previewKind, PreviewKind.media);
    expect(f('a.pdf', 'application/pdf').previewKind, PreviewKind.pdf);
  });
}
