import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';

import '../format.dart';
import '../main.dart';
import '../models.dart';

/// Opens a quick look at [file]: fetched into app storage, never saved.
Future<void> openPreview(BuildContext context, DriveFile file) => Navigator.of(context).push(
      MaterialPageRoute<void>(builder: (_) => PreviewPage(file: file)),
    );

class PreviewPage extends StatefulWidget {
  const PreviewPage({super.key, required this.file});
  final DriveFile file;

  @override
  State<PreviewPage> createState() => _PreviewPageState();
}

class _PreviewPageState extends State<PreviewPage> {
  int? _id;
  String? _startError;
  String? _text;
  bool _truncated = false;
  bool _wrap = true;
  bool _reading = false;

  static const _textLimit = 256 * 1024;

  @override
  void initState() {
    super.initState();
    _start();
  }

  Future<void> _start() async {
    setState(() {
      _startError = null;
      _text = null;
    });
    try {
      final id = await app.startPreview(widget.file);
      if (!mounted) {
        app.endPreview(id);
        return;
      }
      setState(() => _id = id);
    } catch (e) {
      if (mounted) setState(() => _startError = friendlyError(e));
    }
  }

  @override
  void dispose() {
    final id = _id;
    if (id != null) app.endPreview(id);
    super.dispose();
  }

  Future<void> _readText(String path) async {
    if (_reading || _text != null) return;
    _reading = true;
    try {
      final file = File(path);
      final length = await file.length();
      final bytes = await file.openRead(0, _textLimit).fold<List<int>>([], (a, b) => a..addAll(b));
      final text = utf8.decode(bytes, allowMalformed: true);
      if (mounted) {
        setState(() {
          _text = text;
          _truncated = length > _textLimit;
        });
      }
    } catch (e) {
      if (mounted) setState(() => _startError = friendlyError(e));
    }
  }

  Future<void> _download() async {
    try {
      await app.download(widget.file, export: 'pdf');
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(t('已加入传输队列', 'Added to transfers'))));
    } catch (e) {
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(friendlyError(e))));
    }
  }

  @override
  Widget build(BuildContext context) {
    final file = widget.file;
    return Scaffold(
      appBar: AppBar(
        title: Text(file.name, overflow: TextOverflow.ellipsis),
        actions: [
          if (file.previewKind == PreviewKind.text && _text != null)
            IconButton(
              tooltip: t('自动换行', 'Wrap lines'),
              icon: Icon(_wrap ? Icons.wrap_text_rounded : Icons.notes_rounded),
              onPressed: () => setState(() => _wrap = !_wrap),
            ),
          if (file.canDownload && !file.isWorkspace)
            IconButton(
              tooltip: t('下载', 'Download'),
              icon: const Icon(Icons.download_rounded),
              onPressed: _download,
            ),
        ],
      ),
      body: ListenableBuilder(
        listenable: app,
        builder: (context, _) => _body(context),
      ),
    );
  }

  Widget _body(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    if (_startError != null) return _message(Icons.error_outline_rounded, _startError!, scheme.error, retry: true);
    final id = _id;
    final item = id == null ? null : app.previews[id];
    if (id == null || item == null) return const Center(child: CircularProgressIndicator());
    switch (item.status) {
      case 'failed':
        return _message(Icons.error_outline_rounded, item.error ?? t('失败', 'Failed'), scheme.error, retry: true);
      case 'cancelled':
      case 'paused':
        return _message(Icons.pause_circle_outline_rounded, t('已中断', 'Interrupted'), scheme.outline, retry: true);
      case 'done':
        break;
      default:
        return Center(
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 40),
            child: Column(mainAxisSize: MainAxisSize.min, children: [
              LinearProgressIndicator(value: item.status == 'queued' ? null : item.fraction),
              const SizedBox(height: 12),
              Text(
                item.total > 0 ? '${formatBytes(item.done)} / ${formatBytes(item.total)}' : formatBytes(item.done),
              ),
              if (item.speed > 0) Text(formatSpeed(item.speed), style: Theme.of(context).textTheme.bodySmall),
            ]),
          ),
        );
    }
    switch (widget.file.previewKind) {
      case PreviewKind.image:
        return InteractiveViewer(
          maxScale: 8,
          child: Center(
            child: Image.file(
              File(item.path),
              fit: BoxFit.contain,
              errorBuilder: (context, error, _) =>
                  _message(Icons.broken_image_rounded, t('无法显示这张图片', 'This image cannot be shown'), scheme.outline),
            ),
          ),
        );
      case PreviewKind.text:
        _readText(item.path);
        final text = _text;
        if (text == null) return const Center(child: CircularProgressIndicator());
        final body = SelectableText(
          text,
          style: const TextStyle(fontFamily: 'monospace', fontSize: 13, height: 1.35),
        );
        return Column(children: [
          if (_truncated)
            Container(
              width: double.infinity,
              color: scheme.secondaryContainer,
              padding: const EdgeInsets.all(8),
              child: Text(
                t('只显示前 256 KB，完整内容请下载', 'Showing the first 256 KB; download for everything'),
                style: TextStyle(color: scheme.onSecondaryContainer),
              ),
            ),
          Expanded(
            child: SingleChildScrollView(
              padding: const EdgeInsets.all(12),
              child: _wrap
                  ? body
                  : SingleChildScrollView(scrollDirection: Axis.horizontal, child: body),
            ),
          ),
        ]);
      case PreviewKind.none:
        return _message(Icons.visibility_off_rounded, t('不支持快速浏览', 'No quick look for this type'), scheme.outline);
    }
  }

  Widget _message(IconData icon, String text, Color color, {bool retry = false}) => Center(
        child: Padding(
          padding: const EdgeInsets.all(32),
          child: Column(mainAxisSize: MainAxisSize.min, children: [
            Icon(icon, size: 48, color: color),
            const SizedBox(height: 12),
            Text(text, textAlign: TextAlign.center),
            if (retry) ...[
              const SizedBox(height: 16),
              FilledButton(
                onPressed: () {
                  final old = _id;
                  if (old != null) app.endPreview(old);
                  _id = null;
                  _start();
                },
                child: Text(t('重试', 'Retry')),
              ),
            ],
          ]),
        ),
      );
}
