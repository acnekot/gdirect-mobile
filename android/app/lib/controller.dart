import 'dart:async';
import 'dart:convert';

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

  /// The network the phone is on, and whether the app goes around a VPN.
  NetworkState network = NetworkState('', 'none', false, false);

  /// Files another app shared with this one, waiting for a destination.
  List<PickedFile> shared = [];

  /// The Drive folder open in the Files tab: where shared files go by default.
  String folderId = 'root';
  String folderName = '';

  final _exporting = <int>{};
  final _busy = <String>{};
  final _fds = <int, int>{};
  final _opening = <int>{};
  Timer? _timer;
  Timer? _scanPoll;
  Timer? _scanDebounce;
  String _last = '';
  String _alive = '';
  bool _asked = false;
  String? _publicDir;
  String? _networkId;
  int _bypassHold = 0;

  List<String> get accounts => ((state['accounts'] as List?) ?? const []).cast<String>();
  List<String> get serverAccounts =>
      ((state['serverAccounts'] as List?) ?? const []).cast<String>();
  bool get signedIn => accounts.isNotEmpty;
  String? get account => state['account'] as String?;
  String? get clientId => (state['client'] as Map?)?['id'] as String?;
  int get routes => (state['routes'] as num?)?.toInt() ?? 0;
  Map<String, dynamic> get settings =>
      ((state['settings'] as Map?) ?? const {}).cast<String, dynamic>();
  Map<String, dynamic> get server =>
      ((state['server'] as Map?) ?? const {}).cast<String, dynamic>();
  bool get direct => settings['direct'] != false;
  bool get grid => settings['grid'] == true;
  bool get webdav => settings['webdav'] == true;
  int get activeTransfers => transfers.where((t) => t.active).length;

  String? get _base {
    final port = server['port'];
    return port == null ? null : 'http://127.0.0.1:$port/${server['secret']}';
  }

  /// Address of [file] on the app's own server, for players: bytes are
  /// fetched from Drive as they are asked for.
  String? streamUrl(DriveFile file) {
    final base = _base;
    return base == null ? null : '$base/file/${file.id}/${Uri.encodeComponent(file.name)}';
  }

  /// Address of the small preview image of [file], if Drive has one.
  String? thumbUrl(DriveFile file) {
    final base = _base;
    final link = file.thumbnail;
    if (base == null || link == null) return null;
    return '$base/thumb/${file.id}?u=${Uri.encodeQueryComponent(link)}';
  }

  Future<void> init() async {
    try {
      final dir = await getApplicationSupportDirectory();
      state = Engine.map(Engine.fast('init', {'dir': dir.path}));
      ready = true;
      Native.listen();
      Native.onNetwork = _networkChanged;
      Native.onShared = takeShared;
      _tick();
      _schedule();
      unawaited(_afterStart());
    } catch (e) {
      fatal = '$e';
    }
    notifyListeners();
  }

  Future<void> _afterStart() async {
    try {
      _publicDir = await Native.downloadsDir();
    } catch (_) {}
    await applyBypass();
    await takeShared();
    setBusy('webdav', webdav);
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

  /// Marks long jobs (sign-in, scans, serving WebDAV) so the app is kept
  /// alive for them.
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
    _serveDescriptors();
    _syncKeepAlive();
  }

  void _syncKeepAlive() {
    final active = transfers.where((t) => t.active).toList();
    final want = active.isNotEmpty || _busy.isNotEmpty;
    var text = t('正在处理…', 'Working…');
    var progress = -1;
    if (active.isNotEmpty) {
      final total = active.fold<int>(0, (a, i) => a + i.total);
      final done = active.fold<int>(0, (a, i) => a + i.done);
      final speed = active.fold<int>(0, (a, i) => a + i.speed);
      progress = total > 0 ? (done * 100 ~/ total).clamp(0, 100) : -1;
      text = [
        t('${active.length} 个传输', '${active.length} transfer(s)'),
        if (progress >= 0) '$progress%',
        if (speed > 0) formatSpeed(speed),
      ].join(' · ');
    } else if (_busy.contains('webdav') && _busy.length == 1) {
      text = t('WebDAV 正在运行', 'WebDAV is running');
    }
    // The speed changes every tick; refresh the notification on what a
    // glance can use.
    final key = want ? '${active.length}|$progress|${_busy.join(',')}' : '';
    if (key == _alive) return;
    _alive = key;
    Native.keepAlive(want, text, progress).catchError((_) {});
  }

  /// Puts finished downloads where the person finds them: Downloads/gdirect.
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
      final uri = item.direct
          // Already in place: only the system's index has to learn about it.
          ? await Native.scanFile(item.path)
          : await Native.saveToDownloads(item.path, item.name, item.subdir);
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

  /// Opens the documents of the uploads that are next in line, and closes
  /// those of uploads that have ended.
  void _serveDescriptors() {
    final live = <int>{};
    for (final item in transfers) {
      if (item.source == null) continue;
      if (item.active) live.add(item.id);
      if (item.wantsFd && !_fds.containsKey(item.id) && _opening.length < 3 && _opening.add(item.id)) {
        _open(item);
      }
    }
    for (final id in _fds.keys.toList()) {
      if (!live.contains(id)) {
        Native.closeFd(_fds.remove(id)!).catchError((_) {});
      }
    }
  }

  Future<void> _open(TransferItem item) async {
    try {
      final fd = await Native.openFd(item.source!);
      _fds[item.id] = fd;
      Engine.fast('transfer_fd', {'id': item.id, 'fd': fd});
    } catch (e) {
      try {
        Engine.fast('transfer_fail', {
          'id': item.id,
          'error': t('无法读取所选文件，请重新选择后上传：$e', 'Cannot read the picked file; pick it again: $e'),
        });
      } catch (_) {}
    } finally {
      _opening.remove(item.id);
      _tick();
    }
  }

  Future<void> _askNotifications() async {
    if (_asked) return;
    _asked = true;
    await Native.notifyPermission().catchError((_) {});
  }

  // ---- network --------------------------------------------------------

  /// Applies the "go around a VPN" setting, unless something (sign-in) needs
  /// the VPN for the moment.
  Future<void> applyBypass() async {
    try {
      final want = settings['bypassVpn'] != false && _bypassHold == 0;
      _setNetwork(await Native.bypassVpn(want), scan: false);
    } catch (_) {}
  }

  /// Sign-in pages need whatever reaches Google, which may be the VPN.
  Future<void> holdBypass(bool hold) async {
    _bypassHold = (_bypassHold + (hold ? 1 : -1)).clamp(0, 99);
    await applyBypass();
  }

  void _networkChanged(NetworkState next) => _setNetwork(next, scan: true);

  void _setNetwork(NetworkState next, {required bool scan}) {
    final moved = _networkId != null && next.id.isNotEmpty && next.id != _networkId;
    if (next.id.isNotEmpty) _networkId = next.id;
    network = next;
    notifyListeners();
    if (scan && moved && settings['autoScan'] != false && direct && signedIn) {
      // Networks flap while the phone settles on one.
      _scanDebounce?.cancel();
      _scanDebounce = Timer(const Duration(seconds: 4), startScan);
    }
  }

  /// Looks for reachable Google endpoints on the current network.
  void startScan() {
    try {
      if (Engine.map(Engine.fast('scan_status'))['running'] == true) return;
      Engine.fast('scan_start');
    } catch (_) {
      return;
    }
    setBusy('autoscan', true);
    _scanPoll?.cancel();
    _scanPoll = Timer.periodic(const Duration(seconds: 1), (timer) {
      var running = false;
      try {
        running = Engine.map(Engine.fast('scan_status'))['running'] == true;
      } catch (_) {}
      if (!running) {
        timer.cancel();
        setBusy('autoscan', false);
        refreshState();
      }
    });
  }

  // ---- actions --------------------------------------------------------

  Future<void> download(DriveFile file, {String? export}) async {
    await _askNotifications();
    await Engine.slow('download', {
      'id': file.id,
      'export': ?export,
      'dir': ?_publicDir,
    });
    _tick();
    _schedule();
  }

  /// Queues every file below a folder; returns (queued, skipped).
  Future<(int, int)> downloadFolder(DriveFile folder) async {
    await _askNotifications();
    final id = folder.isShortcut ? folder.shortcut!['targetId'] as String : folder.id;
    final reply = Engine.map(await Engine.slow('download_folder', {
      'id': id,
      'dir': ?_publicDir,
    }));
    _tick();
    _schedule();
    return ((reply['count'] as num).toInt(), (reply['skipped'] as num).toInt());
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

  /// Queues uploads of documents into [parentId]; nothing is copied, the
  /// engine reads each document when its turn comes.
  Future<int> uploadPicked(List<PickedFile> files, String parentId) async {
    if (files.isEmpty) return 0;
    await _askNotifications();
    for (final file in files) {
      Engine.fast('upload', {
        'uri': file.uri,
        'name': file.name,
        'size': file.size ?? 0,
        'parent': parentId,
      });
    }
    _tick();
    _schedule();
    return files.length;
  }

  /// Lets the person pick files and uploads them into [parentId].
  Future<int> pickAndUpload(String parentId) async =>
      uploadPicked(await Native.pickFiles(), parentId);

  /// Lets the person pick a folder and uploads it, with its subfolders,
  /// into [parentId]. Returns how many files were queued, or null.
  Future<int?> pickAndUploadFolder(String parentId) async {
    final picked = await Native.pickFolder();
    if (picked == null) return null;
    final (name, files) = picked;
    await _askNotifications();
    Future<String> mkdir(String parent, String name) async =>
        Engine.map(await Engine.slow('mkdir', {'parent': parent, 'name': name}))['id'] as String;
    // Drive folder for each path inside the picked folder.
    final folders = <String, String>{'': await mkdir(parentId, name)};
    Future<String> folderFor(String path) async {
      final known = folders[path];
      if (known != null) return known;
      final cut = path.lastIndexOf('/');
      final above = await folderFor(cut < 0 ? '' : path.substring(0, cut));
      return folders[path] = await mkdir(above, path.substring(cut + 1));
    }

    for (final file in files) {
      final parent = await folderFor(file.path);
      Engine.fast('upload', {
        'uri': file.uri,
        'name': file.name,
        'size': file.size ?? 0,
        'parent': parent,
      });
    }
    _tick();
    _schedule();
    return files.length;
  }

  /// Collects what other apps shared with this one.
  Future<void> takeShared() async {
    try {
      final files = await Native.takeShared();
      if (files.isEmpty) return;
      shared = [...shared, ...files];
      notifyListeners();
    } catch (_) {}
  }

  void clearShared() {
    shared = [];
    notifyListeners();
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
    if (key == 'bypassVpn') applyBypass();
    if (key == 'webdav') setBusy('webdav', webdav);
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
