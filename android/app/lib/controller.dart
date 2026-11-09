import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

import 'engine.dart';
import 'format.dart';
import 'models.dart';
import 'native.dart';

/// App-wide state: the engine's settings, accounts and the transfer list.
class AppController extends ChangeNotifier {
  Map<String, dynamic> state = {};
  List<TransferItem> transfers = [];
  final Map<int, TransferStats> stats = {};
  /// Quick looks in flight, kept apart from the transfer list.
  Map<int, TransferItem> previews = {};
  bool ready = false;
  String? fatal;

  /// Why a finished download could not be put into Downloads, by transfer.
  final exportErrors = <int, String>{};

  final _exporting = <int>{};
  final _busy = <String>{};
  Timer? _timer;
  String _last = '';
  bool _alive = false;
  int _aliveCount = -1;
  bool _asked = false;
  late Directory _cache;

  List<String> get accounts => ((state['accounts'] as List?) ?? const []).cast<String>();
  bool get signedIn => accounts.isNotEmpty;
  String? get account => state['account'] as String?;
  String? get clientId => (state['client'] as Map?)?['id'] as String?;
  int get routes => (state['routes'] as num?)?.toInt() ?? 0;
  Map<String, dynamic> get settings =>
      ((state['settings'] as Map?) ?? const {}).cast<String, dynamic>();
  bool get direct => settings['direct'] != false;
  int get activeTransfers => transfers.where((t) => t.active).length;

  Future<void> init() async {
    try {
      final dir = await getApplicationSupportDirectory();
      _cache = await getTemporaryDirectory();
      state = Engine.map(Engine.fast('init', {'dir': dir.path}));
      ready = true;
      _tick();
      _schedule();
    } catch (e) {
      fatal = '$e';
    }
    notifyListeners();
  }

  void _schedule() {
    _timer?.cancel();
    _timer = Timer(
      Duration(milliseconds: activeTransfers > 0 || previews.values.any((p) => p.active) ? 500 : 2500),
      () {
        _tick();
        _schedule();
      },
    );
  }

  void refreshState() {
    try {
      state = Engine.map(Engine.fast('state'));
    } catch (_) {}
    notifyListeners();
  }

  /// Marks long jobs (sign-in, scans) so the app is kept alive for them.
  void setBusy(String reason, bool on) {
    on ? _busy.add(reason) : _busy.remove(reason);
    _syncKeepAlive();
  }

  void _tick() {
    try {
      final raw = jsonEncode(Engine.fast('transfers'));
      if (raw != _last) {
        _last = raw;
        final all = (jsonDecode(raw) as List)
            .map((e) => TransferItem((e as Map).cast<String, dynamic>()))
            .toList();
        previews = {for (final i in all.where((i) => i.isPreview)) i.id: i};
        transfers = all.where((i) => !i.isPreview).toList();
        for (final item in transfers) {
          stats.putIfAbsent(item.id, TransferStats.new).observe(item);
        }
        notifyListeners();
      }
    } catch (_) {}
    _saveFinished();
    _syncKeepAlive();
  }

  void _syncKeepAlive() {
    final n = activeTransfers;
    final want = n > 0 || _busy.isNotEmpty;
    if (want == _alive && (!want || n == _aliveCount)) return;
    _alive = want;
    _aliveCount = n;
    final text = n > 0
        ? t('$n 个传输正在进行', '$n transfer(s) in progress')
        : t('正在处理…', 'Working…');
    Native.keepAlive(want, text).catchError((_) {});
  }

  /// Moves finished downloads from the app's staging folder to Downloads.
  void _saveFinished() {
    for (final item in transfers) {
      if (item.isDownload &&
          item.status == 'done' &&
          item.saved == null &&
          !exportErrors.containsKey(item.id) &&
          _exporting.add(item.id)) {
        _save(item);
      }
    }
  }

  Future<void> _save(TransferItem item) async {
    try {
      final uri = await Native.saveToDownloads(item.path, item.name);
      Engine.fast('transfer_saved', {'id': item.id, 'uri': uri});
      exportErrors.remove(item.id);
    } catch (e) {
      exportErrors[item.id] = '$e';
    } finally {
      _exporting.remove(item.id);
      _tick();
      notifyListeners();
    }
  }

  void saveAgain(TransferItem item) {
    exportErrors.remove(item.id);
    _tick();
  }

  Future<void> _askNotifications() async {
    if (_asked) return;
    _asked = true;
    await Native.notifyPermission().catchError((_) {});
  }

  // ---- actions --------------------------------------------------------

  Future<void> download(DriveFile file, {String export = 'pdf'}) async {
    await _askNotifications();
    await Engine.slow('download', {'id': file.id, 'export': export});
    _tick();
    _schedule();
  }

  /// Starts fetching [file] for a quick look and returns the engine's id.
  Future<int> startPreview(DriveFile file) async {
    final reply = Engine.map(await Engine.slow('download', {
      'id': file.id,
      'preview': true,
      if (file.previewExport != null) 'export': file.previewExport,
    }));
    _tick();
    _schedule();
    return (reply['id'] as num).toInt();
  }

  void endPreview(int id) {
    try {
      Engine.fast('transfer_remove', {'id': id});
    } catch (_) {}
    _tick();
  }

  /// Lets the person pick files and uploads them into [parentId].
  /// Returns how many were queued.
  Future<int> pickAndUpload(String parentId) async {
    final picked = await FilePicker.pickFiles();
    if (picked.isEmpty) return 0;
    await _askNotifications();
    final uploads = Directory('${_cache.path}/uploads')..createSync(recursive: true);
    var count = 0;
    for (final file in picked) {
      var path = file.path;
      var cleanup = false;
      if (path == null || !File(path).existsSync()) {
        // Android hands out content URIs: copy the data where the engine can read it.
        final copy = File('${uploads.path}/${DateTime.now().microsecondsSinceEpoch}-$count');
        final sink = copy.openWrite();
        try {
          await sink.addStream(file.readAsByteStream());
        } finally {
          await sink.close();
        }
        path = copy.path;
        cleanup = true;
      }
      await Engine.slow('upload', {
        'path': path,
        'parent': parentId,
        'name': file.name,
        'cleanup': cleanup,
      });
      count++;
    }
    _tick();
    _schedule();
    return count;
  }

  void cancel(TransferItem item) {
    Engine.fast('transfer_cancel', {'id': item.id});
    _tick();
  }

  void retry(TransferItem item) {
    Engine.fast('transfer_retry', {'id': item.id});
    exportErrors.remove(item.id);
    _tick();
    _schedule();
  }

  void remove(TransferItem item) {
    Engine.fast('transfer_remove', {'id': item.id});
    exportErrors.remove(item.id);
    _tick();
  }

  void clearFinished() {
    Engine.fast('transfers_clear');
    _tick();
  }

  Future<void> open(TransferItem item) async {
    final uri = item.saved;
    if (uri != null) await Native.openUri(uri, null);
  }

  void setSetting(String key, Object value) {
    Engine.fast('set', {'key': key, 'value': value});
    refreshState();
  }

  Future<void> logout(String email) async {
    Engine.fast('logout', {'email': email});
    refreshState();
  }

  void setAccount(String email) {
    Engine.fast('set_account', {'email': email});
    refreshState();
  }
}
