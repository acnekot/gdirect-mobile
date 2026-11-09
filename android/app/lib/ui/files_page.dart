import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:url_launcher/url_launcher.dart';

import '../engine.dart';
import '../format.dart';
import '../main.dart';
import '../models.dart';

class _Crumb {
  _Crumb(this.id, this.name, this.driveId);
  final String id;
  final String name;
  final String? driveId;
}

String _placeTitle(Place place) => switch (place) {
      Place.root => t('我的云端硬盘', 'My Drive'),
      Place.shared => t('与我共享', 'Shared with me'),
      Place.starred => t('已加星标', 'Starred'),
      Place.recent => t('最近', 'Recent'),
      Place.trash => t('回收站', 'Trash'),
      Place.drives => t('共享云端硬盘', 'Shared drives'),
    };

/// Browsing Drive: places, folders, search and per-file actions.
class FilesPage extends StatefulWidget {
  const FilesPage({
    super.key,
    required this.active,
    required this.openTransfers,
    required this.openScan,
  });

  /// Whether this tab is the one on screen (it stays alive behind the others).
  final bool active;
  final VoidCallback openTransfers;
  final VoidCallback openScan;

  @override
  State<FilesPage> createState() => _FilesPageState();
}

class _FilesPageState extends State<FilesPage> {
  Place _place = Place.root;
  final List<_Crumb> _crumbs = [];
  List<DriveFile> _items = [];
  String? _next;
  bool _loading = false;
  String? _error;
  int _gen = 0;
  bool _searching = false;
  String _query = '';
  final _search = TextEditingController();
  final _scroll = ScrollController();
  String? _account;

  bool get _atRoot => _place == Place.root && _crumbs.isEmpty && _query.isEmpty && !_searching;
  String get _parent => _crumbs.isNotEmpty ? _crumbs.last.id : 'root';
  bool get _canCreate =>
      _query.isEmpty && (_crumbs.isNotEmpty || _place == Place.root);

  @override
  void initState() {
    super.initState();
    _account = app.account;
    _scroll.addListener(() {
      if (_scroll.position.extentAfter < 600 && _next != null && !_loading) {
        _load(more: true);
      }
    });
    app.addListener(_accountChanged);
    _load();
  }

  @override
  void dispose() {
    app.removeListener(_accountChanged);
    _search.dispose();
    _scroll.dispose();
    super.dispose();
  }

  void _accountChanged() {
    if (app.account != _account) {
      _account = app.account;
      _place = Place.root;
      _crumbs.clear();
      _query = '';
      _searching = false;
      _search.clear();
      _load();
    }
  }

  void _toast(String text, {String? action, VoidCallback? onAction}) {
    if (!mounted) return;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(
        content: Text(text),
        action: action == null ? null : SnackBarAction(label: action, onPressed: onAction ?? () {}),
      ));
  }

  Future<void> _load({bool more = false}) async {
    final gen = ++_gen;
    setState(() {
      _loading = true;
      _error = null;
      if (!more) {
        _items = [];
        _next = null;
      }
    });
    try {
      final Map<String, dynamic> reply;
      if (_query.isNotEmpty) {
        reply = Engine.map(await Engine.slow('search', {
          'text': _query,
          if (more && _next != null) 'pageToken': _next,
        }));
      } else if (_place == Place.drives && _crumbs.isEmpty) {
        final drives = Engine.maps(await Engine.slow('drives'));
        reply = {
          'files': [
            for (final d in drives)
              {'id': d['id'], 'name': d['name'], 'mimeType': folderMime, 'driveId': d['id']},
          ],
        };
      } else {
        final folder = _crumbs.isEmpty ? null : _crumbs.last;
        reply = Engine.map(await Engine.slow('list', {
          'place': folder != null ? 'folder' : _place.name,
          if (folder != null) 'id': folder.id,
          if (folder?.driveId != null) 'driveId': folder!.driveId,
          if (more && _next != null) 'pageToken': _next,
        }));
      }
      if (gen != _gen || !mounted) return;
      final files = Engine.maps(reply['files']).map(DriveFile.new).toList();
      setState(() {
        _items = more ? [..._items, ...files] : files;
        _next = reply['next'] as String?;
        _loading = false;
      });
    } catch (e) {
      if (gen != _gen || !mounted) return;
      setState(() {
        _error = friendlyError(e);
        _loading = false;
      });
    }
  }

  void _goPlace(Place place) {
    if (place == _place && _crumbs.isEmpty && _query.isEmpty) return;
    _place = place;
    _crumbs.clear();
    _query = '';
    _load();
  }

  void _goBack() {
    if (_searching || _query.isNotEmpty) {
      _closeSearch();
    } else if (_crumbs.isNotEmpty) {
      _crumbs.removeLast();
      _load();
    } else if (_place != Place.root) {
      _goPlace(Place.root);
    }
  }

  void _closeSearch() {
    _searching = false;
    _search.clear();
    final hadQuery = _query.isNotEmpty;
    _query = '';
    if (hadQuery) {
      _load();
    } else {
      setState(() {});
    }
  }

  void _open(DriveFile file) {
    if (file.isFolder || file.shortcutToFolder) {
      final id = file.isShortcut ? file.shortcut!['targetId'] as String : file.id;
      final scope = _crumbs.isNotEmpty ? _crumbs.last.driveId : file.driveId;
      _crumbs.add(_Crumb(id, file.name, file.driveId ?? scope));
      _query = '';
      _searching = false;
      _search.clear();
      _load();
    } else {
      _showActions(file);
    }
  }

  Future<void> _run(Future<void> Function() action) async {
    try {
      await action();
    } catch (e) {
      _toast(friendlyError(e));
    }
  }

  Future<void> _download(DriveFile file, {String export = 'pdf'}) => _run(() async {
        await app.download(file, export: export);
        _toast(t('已加入传输队列', 'Added to transfers'),
            action: t('查看', 'View'), onAction: widget.openTransfers);
      });

  Future<String?> _askText(String title, String initial, String action) {
    final controller = TextEditingController(text: initial);
    return showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(title),
        content: TextField(
          controller: controller,
          autofocus: true,
          onSubmitted: (v) => Navigator.pop(context, v),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: Text(t('取消', 'Cancel'))),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text),
            child: Text(action),
          ),
        ],
      ),
    );
  }

  Future<bool> _confirm(String title, String body, String action) async =>
      await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: Text(title),
          content: Text(body),
          actions: [
            TextButton(onPressed: () => Navigator.pop(context, false), child: Text(t('取消', 'Cancel'))),
            FilledButton(onPressed: () => Navigator.pop(context, true), child: Text(action)),
          ],
        ),
      ) ??
      false;

  Future<void> _rename(DriveFile file) async {
    final name = (await _askText(t('重命名', 'Rename'), file.name, t('确定', 'OK')))?.trim();
    if (name == null || name.isEmpty || name == file.name) return;
    await _run(() async {
      await Engine.slow('rename', {'id': file.id, 'name': name});
      await _load();
    });
  }

  Future<void> _newFolder() async {
    final name = (await _askText(t('新建文件夹', 'New folder'), '', t('创建', 'Create')))?.trim();
    if (name == null || name.isEmpty) return;
    await _run(() async {
      await Engine.slow('mkdir', {'parent': _parent, 'name': name});
      await _load();
    });
  }

  Future<void> _upload() => _run(() async {
        final n = await app.pickAndUpload(_parent);
        if (n > 0) {
          _toast(t('已加入 $n 个上传', 'Queued $n upload(s)'),
              action: t('查看', 'View'), onAction: widget.openTransfers);
        }
      });

  Future<void> _trash(DriveFile file) => _run(() async {
        await Engine.slow('trash', {'id': file.id});
        setState(() => _items = _items.where((f) => f.id != file.id).toList());
        _toast(t('已移到回收站', 'Moved to trash'),
            action: t('撤销', 'Undo'),
            onAction: () => _run(() async {
                  await Engine.slow('untrash', {'id': file.id});
                  await _load();
                }));
      });

  Future<void> _restore(DriveFile file) => _run(() async {
        await Engine.slow('untrash', {'id': file.id});
        setState(() => _items = _items.where((f) => f.id != file.id).toList());
        _toast(t('已恢复', 'Restored'));
      });

  Future<void> _deleteForever(DriveFile file) async {
    final ok = await _confirm(
      t('永久删除？', 'Delete forever?'),
      t('“${file.name}”将被永久删除，无法恢复。', '“${file.name}” will be deleted permanently.'),
      t('永久删除', 'Delete'),
    );
    if (!ok) return;
    await _run(() async {
      await Engine.slow('delete', {'id': file.id});
      setState(() => _items = _items.where((f) => f.id != file.id).toList());
    });
  }

  Future<void> _emptyTrash() async {
    final ok = await _confirm(
      t('清空回收站？', 'Empty the trash?'),
      t('回收站里的所有文件将被永久删除。', 'Everything in the trash will be deleted permanently.'),
      t('清空', 'Empty'),
    );
    if (!ok) return;
    await _run(() async {
      await Engine.slow('empty_trash');
      await _load();
    });
  }

  Future<void> _star(DriveFile file) => _run(() async {
        await Engine.slow('star', {'id': file.id, 'on': !file.starred});
        _toast(file.starred ? t('已取消星标', 'Unstarred') : t('已加星标', 'Starred'));
        if (_place == Place.starred) await _load();
      });

  void _showActions(DriveFile file) {
    final scheme = Theme.of(context).colorScheme;
    final kind = file.mimeType.replaceFirst('application/vnd.google-apps.', '');
    final exports = <String, String>{
      'pdf': 'PDF',
      if (kind == 'document') 'docx': 'Word (.docx)',
      if (kind == 'spreadsheet') 'xlsx': 'Excel (.xlsx)',
      if (kind == 'presentation') 'pptx': 'PowerPoint (.pptx)',
    };
    final inTrash = file.trashed || _place == Place.trash;
    Widget tile(IconData icon, String label, VoidCallback onTap, {Color? color}) => ListTile(
          leading: Icon(icon, color: color),
          title: Text(label, style: TextStyle(color: color)),
          onTap: () {
            Navigator.pop(context);
            onTap();
          },
        );
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      isScrollControlled: true,
      builder: (context) => SafeArea(
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              ListTile(
                leading: Icon(file.icon, size: 32, color: scheme.primary),
                title: Text(file.name, maxLines: 2, overflow: TextOverflow.ellipsis),
                subtitle: Text([
                  if (file.size != null) formatBytes(file.size),
                  if (file.owner.isNotEmpty) file.owner,
                  formatTime(file.modified),
                ].where((s) => s.isNotEmpty).join(' · ')),
              ),
              const Divider(height: 1),
              if (inTrash) ...[
                tile(Icons.restore_from_trash_rounded, t('恢复', 'Restore'), () => _restore(file)),
                tile(Icons.delete_forever_rounded, t('永久删除', 'Delete forever'), () => _deleteForever(file),
                    color: scheme.error),
              ] else ...[
                if (file.canDownload && !file.isWorkspace && !file.isShortcut)
                  tile(Icons.download_rounded, t('下载', 'Download'), () => _download(file)),
                if (file.isWorkspace && file.exportable)
                  for (final e in exports.entries)
                    tile(Icons.download_rounded, t('导出为 ${e.value}', 'Export as ${e.value}'),
                        () => _download(file, export: e.key)),
                if (file.isShortcut && !file.shortcutToFolder)
                  tile(Icons.download_rounded, t('下载（快捷方式目标）', 'Download (shortcut target)'),
                      () => _download(file)),
                tile(file.starred ? Icons.star_rounded : Icons.star_border_rounded,
                    file.starred ? t('取消星标', 'Remove star') : t('加星标', 'Add star'), () => _star(file)),
                tile(Icons.drive_file_rename_outline_rounded, t('重命名', 'Rename'), () => _rename(file)),
                if (file.link != null) ...[
                  tile(Icons.link_rounded, t('复制链接', 'Copy link'), () async {
                    await Clipboard.setData(ClipboardData(text: file.link!));
                    _toast(t('链接已复制', 'Link copied'));
                  }),
                  tile(Icons.open_in_new_rounded, t('在浏览器中打开', 'Open in browser'),
                      () => launchUrl(Uri.parse(file.link!), mode: LaunchMode.externalApplication)),
                ],
                tile(Icons.delete_outline_rounded, t('移到回收站', 'Move to trash'), () => _trash(file),
                    color: scheme.error),
              ],
            ],
          ),
        ),
      ),
    );
  }

  void _showCreate() {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: Column(mainAxisSize: MainAxisSize.min, children: [
          ListTile(
            leading: const Icon(Icons.upload_file_rounded),
            title: Text(t('上传文件', 'Upload files')),
            onTap: () {
              Navigator.pop(context);
              _upload();
            },
          ),
          ListTile(
            leading: const Icon(Icons.create_new_folder_rounded),
            title: Text(t('新建文件夹', 'New folder')),
            onTap: () {
              Navigator.pop(context);
              _newFolder();
            },
          ),
        ]),
      ),
    );
  }

  Widget _title() {
    if (_searching) {
      return TextField(
        controller: _search,
        autofocus: true,
        textInputAction: TextInputAction.search,
        decoration: InputDecoration(
          hintText: t('搜索云端硬盘', 'Search Drive'),
          border: InputBorder.none,
          enabledBorder: InputBorder.none,
          focusedBorder: InputBorder.none,
        ),
        onSubmitted: (v) {
          _query = v.trim();
          _load();
        },
      );
    }
    final name = _crumbs.isNotEmpty ? _crumbs.last.name : _placeTitle(_place);
    return Text(name, overflow: TextOverflow.ellipsis);
  }

  Widget _banner() {
    if (!app.direct || app.routes > 0 || !_atRoot) return const SizedBox.shrink();
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 4, 12, 4),
      child: Card(
        color: scheme.secondaryContainer,
        child: ListTile(
          leading: Icon(Icons.travel_explore_rounded, color: scheme.onSecondaryContainer),
          title: Text(t('还没有直连节点', 'No direct endpoint yet'),
              style: TextStyle(color: scheme.onSecondaryContainer)),
          subtitle: Text(t('现在走系统网络。扫描一次即可直连 Google。', 'Using the system network. Scan once to connect to Google directly.'),
              style: TextStyle(color: scheme.onSecondaryContainer)),
          trailing: FilledButton.tonal(onPressed: widget.openScan, child: Text(t('扫描', 'Scan'))),
        ),
      ),
    );
  }

  Widget _body() {
    if (_error != null && _items.isEmpty) {
      return ListView(children: [
        Padding(
          padding: const EdgeInsets.all(32),
          child: Column(children: [
            Icon(Icons.cloud_off_rounded, size: 48, color: Theme.of(context).colorScheme.outline),
            const SizedBox(height: 12),
            Text(_error!, textAlign: TextAlign.center),
            const SizedBox(height: 12),
            FilledButton.tonal(onPressed: _load, child: Text(t('重试', 'Retry'))),
          ]),
        ),
      ]);
    }
    if (_items.isEmpty && !_loading) {
      return ListView(children: [
        Padding(
          padding: const EdgeInsets.all(48),
          child: Center(child: Text(_query.isNotEmpty ? t('没有找到结果', 'No results') : t('这里是空的', 'Nothing here'))),
        ),
      ]);
    }
    return ListView.builder(
      controller: _scroll,
      physics: const AlwaysScrollableScrollPhysics(),
      itemCount: _items.length + (_loading || _next != null ? 1 : 0),
      itemBuilder: (context, index) {
        if (index >= _items.length) {
          return const Padding(
            padding: EdgeInsets.all(20),
            child: Center(child: SizedBox(width: 24, height: 24, child: CircularProgressIndicator(strokeWidth: 2))),
          );
        }
        final file = _items[index];
        final scheme = Theme.of(context).colorScheme;
        final subtitle = [
          if (!file.isFolder && file.size != null) formatBytes(file.size),
          formatTime(file.modified),
        ].where((s) => s.isNotEmpty).join(' · ');
        return ListTile(
          leading: CircleAvatar(
            backgroundColor: scheme.primaryContainer,
            foregroundColor: scheme.onPrimaryContainer,
            child: Icon(file.icon),
          ),
          title: Text(file.name, maxLines: 2, overflow: TextOverflow.ellipsis),
          subtitle: subtitle.isEmpty ? null : Text(subtitle),
          trailing: _place == Place.drives && _crumbs.isEmpty
              ? null
              : IconButton(
                  icon: Icon(file.starred ? Icons.star_rounded : Icons.more_vert_rounded),
                  onPressed: () => _showActions(file),
                ),
          onTap: () => _open(file),
          onLongPress: _place == Place.drives && _crumbs.isEmpty ? null : () => _showActions(file),
        );
      },
    );
  }

  @override
  Widget build(BuildContext context) {
    return PopScope(
      canPop: !widget.active || _atRoot,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop && widget.active) _goBack();
      },
      child: Scaffold(
        appBar: AppBar(
          leading: _atRoot
              ? null
              : IconButton(icon: const Icon(Icons.arrow_back_rounded), onPressed: _goBack),
          title: _title(),
          actions: [
            if (!_searching)
              IconButton(
                icon: const Icon(Icons.search_rounded),
                tooltip: t('搜索', 'Search'),
                onPressed: () => setState(() => _searching = true),
              ),
            if (_place == Place.trash && _crumbs.isEmpty)
              IconButton(
                icon: const Icon(Icons.delete_sweep_rounded),
                tooltip: t('清空回收站', 'Empty trash'),
                onPressed: _emptyTrash,
              ),
            IconButton(
              icon: const Icon(Icons.refresh_rounded),
              tooltip: t('刷新', 'Refresh'),
              onPressed: _load,
            ),
          ],
        ),
        body: Column(
          children: [
            if (_crumbs.isEmpty && _query.isEmpty && !_searching)
              SizedBox(
                height: 52,
                child: ListView(
                  scrollDirection: Axis.horizontal,
                  padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                  children: [
                    for (final place in Place.values)
                      Padding(
                        padding: const EdgeInsets.only(right: 8),
                        child: ChoiceChip(
                          label: Text(_placeTitle(place)),
                          selected: _place == place,
                          onSelected: (_) => _goPlace(place),
                        ),
                      ),
                  ],
                ),
              ),
            _banner(),
            if (_loading && _items.isEmpty) const LinearProgressIndicator(),
            Expanded(
              child: RefreshIndicator(onRefresh: _load, child: _body()),
            ),
          ],
        ),
        floatingActionButton: _canCreate
            ? FloatingActionButton(
                onPressed: _showCreate,
                tooltip: t('上传或新建', 'Upload or create'),
                child: const Icon(Icons.add_rounded),
              )
            : null,
      ),
    );
  }
}
