import 'package:flutter/material.dart';

import '../format.dart';
import '../main.dart';
import '../models.dart';

/// Downloads and uploads, running and finished.
class TransfersPage extends StatelessWidget {
  const TransfersPage({super.key});

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: app,
      builder: (context, _) {
        final items = app.transfers.reversed.toList();
        final finished = items.any((t) => t.status == 'done' && (!t.isDownload || t.saved != null));
        return Scaffold(
          appBar: AppBar(
            title: Text(t('传输', 'Transfers')),
            actions: [
              if (finished)
                TextButton(
                  onPressed: app.clearFinished,
                  child: Text(t('清除已完成', 'Clear finished')),
                ),
            ],
          ),
          body: items.isEmpty
              ? Center(
                  child: Column(mainAxisSize: MainAxisSize.min, children: [
                    Icon(Icons.swap_vert_rounded, size: 56, color: Theme.of(context).colorScheme.outline),
                    const SizedBox(height: 12),
                    Text(t('还没有传输', 'No transfers yet')),
                    const SizedBox(height: 4),
                    Text(
                      t('在“文件”里点一个文件即可下载', 'Open a file in Files to download it'),
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                  ]),
                )
              : ListView.builder(
                  padding: const EdgeInsets.fromLTRB(12, 4, 12, 24),
                  itemCount: items.length + 1,
                  itemBuilder: (context, i) =>
                      i == 0 ? _Summary(items: items) : _TransferCard(item: items[i - 1]),
                ),
        );
      },
    );
  }
}

class _TransferCard extends StatelessWidget {
  const _TransferCard({required this.item});
  final TransferItem item;

  String _status() {
    final saveError = app.exportErrors[item.id];
    switch (item.status) {
      case 'queued':
        return t('排队中…', 'Queued…');
      case 'running':
        final parts = <String>[
          item.total > 0 ? '${formatBytes(item.done)} / ${formatBytes(item.total)}' : formatBytes(item.done),
          if (item.speed > 0) formatSpeed(item.speed),
          if (item.speed > 0 && item.total > 0) formatEta(item.total - item.done, item.speed),
          if (item.isDownload && item.connections > 0) t('${item.connections} 连接', '${item.connections} conn.'),
        ];
        return parts.where((s) => s.isNotEmpty).join(' · ');
      case 'done':
        if (!item.isDownload) return t('已上传 · ${formatBytes(item.total)}', 'Uploaded · ${formatBytes(item.total)}');
        if (item.saved != null) return t('已保存到 下载/gdirect · ${formatBytes(item.total)}', 'Saved to Downloads/gdirect · ${formatBytes(item.total)}');
        if (saveError != null) return t('下载完成，但保存失败：$saveError', 'Downloaded, but saving failed: $saveError');
        return t('下载完成，正在保存到“下载”…', 'Downloaded, saving to Downloads…');
      case 'paused':
        return t('已暂停 · ${formatBytes(item.done)} / ${formatBytes(item.total)}', 'Paused · ${formatBytes(item.done)} / ${formatBytes(item.total)}');
      case 'cancelled':
        return t('已取消 · 已保留 ${formatBytes(item.done)}，可继续', 'Cancelled · kept ${formatBytes(item.done)}, can resume');
      default:
        return item.error ?? t('失败', 'Failed');
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final failed = item.status == 'failed' || app.exportErrors.containsKey(item.id);
    final saveError = app.exportErrors[item.id];
    return Card(
      margin: const EdgeInsets.only(top: 8),
      clipBehavior: Clip.antiAlias,
      child: InkWell(
        onTap: () => showTransferDetails(context, item.id),
        child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 8, 12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(item.isDownload ? Icons.download_rounded : Icons.upload_rounded, color: scheme.primary),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(item.name, maxLines: 2, overflow: TextOverflow.ellipsis,
                          style: Theme.of(context).textTheme.titleSmall),
                      const SizedBox(height: 2),
                      Text(
                        _status(),
                        style: Theme.of(context).textTheme.bodySmall?.copyWith(
                              color: failed ? scheme.error : scheme.onSurfaceVariant,
                            ),
                      ),
                    ],
                  ),
                ),
                _actions(context, saveError != null),
              ],
            ),
            if (item.active)
              Padding(
                padding: const EdgeInsets.only(top: 10, right: 8),
                child: ClipRRect(
                  borderRadius: BorderRadius.circular(4),
                  child: LinearProgressIndicator(
                    value: item.status == 'queued' ? null : item.fraction,
                    minHeight: 6,
                  ),
                ),
              ),
          ],
        ),
      ),
      ),
    );
  }

  Widget _actions(BuildContext context, bool saveFailed) {
    if (item.active) {
      return IconButton(
        icon: const Icon(Icons.close_rounded),
        tooltip: t('取消', 'Cancel'),
        onPressed: () => app.cancel(item),
      );
    }
    return PopupMenuButton<String>(
      onSelected: (action) {
        switch (action) {
          case 'open':
            app.open(item).catchError((Object e) {
              if (context.mounted) {
                ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$e')));
              }
            });
          case 'retry':
            app.retry(item);
          case 'save':
            app.saveAgain(item);
          case 'remove':
            app.remove(item);
        }
      },
      itemBuilder: (context) => [
        if (item.isDownload && item.saved != null)
          PopupMenuItem(value: 'open', child: Text(t('打开', 'Open'))),
        if (item.stopped)
          PopupMenuItem(value: 'retry', child: Text(t('继续 / 重试', 'Resume / retry'))),
        if (saveFailed) PopupMenuItem(value: 'save', child: Text(t('重新保存到“下载”', 'Save to Downloads again'))),
        PopupMenuItem(value: 'remove', child: Text(t('移除记录', 'Remove'))),
      ],
    );
  }
}

/// Totals over all transfers, shown above the list.
class _Summary extends StatelessWidget {
  const _Summary({required this.items});
  final List<TransferItem> items;

  @override
  Widget build(BuildContext context) {
    final active = items.where((i) => i.active).toList();
    final speed = active.fold<int>(0, (a, i) => a + i.speed);
    final downloaded = items.where((i) => i.isDownload).fold<int>(0, (a, i) => a + i.done);
    final uploaded = items.where((i) => !i.isDownload).fold<int>(0, (a, i) => a + i.done);
    final scheme = Theme.of(context).colorScheme;
    Widget cell(String label, String value) => Expanded(
          child: Column(children: [
            Text(value, style: Theme.of(context).textTheme.titleMedium?.copyWith(color: scheme.onPrimaryContainer)),
            const SizedBox(height: 2),
            Text(label, style: Theme.of(context).textTheme.bodySmall?.copyWith(color: scheme.onPrimaryContainer)),
          ]),
        );
    return Card(
      color: scheme.primaryContainer,
      margin: const EdgeInsets.only(top: 4),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 14),
        child: Row(children: [
          cell(t('当前总速度', 'Speed now'), active.isEmpty ? '—' : formatSpeed(speed)),
          cell(t('进行中', 'Active'), '${active.length}'),
          cell(t('已下载', 'Downloaded'), formatBytes(downloaded)),
          cell(t('已上传', 'Uploaded'), formatBytes(uploaded)),
        ]),
      ),
    );
  }
}

/// Live detail sheet for one transfer; follows the controller while open.
void showTransferDetails(BuildContext context, int id) {
  showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    showDragHandle: true,
    builder: (context) => ListenableBuilder(
      listenable: app,
      builder: (context, _) {
        final matches = app.transfers.where((i) => i.id == id);
        if (matches.isEmpty) return const SizedBox(height: 120);
        return _Details(item: matches.first, stats: app.stats[id] ?? TransferStats());
      },
    ),
  );
}

class _Details extends StatelessWidget {
  const _Details({required this.item, required this.stats});
  final TransferItem item;
  final TransferStats stats;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final percent = item.fraction == null ? '—' : '${(item.fraction! * 100).toStringAsFixed(1)}%';
    final average = stats.average(item.done);
    final remaining = item.total - item.done;
    final eta = item.active && item.speed > 0 ? formatEta(remaining, item.speed) : '';
    final rows = <(String, String)>[
      (t('状态', 'Status'), _statusName(item.status)),
      (t('进度', 'Progress'), '$percent · ${formatBytes(item.done)} / ${item.total > 0 ? formatBytes(item.total) : '—'}'),
      (t('剩余', 'Remaining'), item.total > 0 ? formatBytes(remaining < 0 ? 0 : remaining) : '—'),
      (t('当前速度', 'Current speed'), item.active ? formatSpeed(item.speed) : '—'),
      (t('平均速度', 'Average speed'), average > 0 ? formatSpeed(average) : '—'),
      (t('峰值速度', 'Peak speed'), stats.peak > 0 ? formatSpeed(stats.peak) : '—'),
      (t('预计剩余时间', 'ETA'), eta.isEmpty ? '—' : eta),
      (t('已用时间', 'Elapsed'), formatDuration(stats.elapsed)),
      if (item.isDownload)
        (t('连接数', 'Connections'),
            item.active ? '${item.connections}' : (stats.maxConnections > 0 ? t('最多 ${stats.maxConnections}', 'up to ${stats.maxConnections}') : '—')),
      (t('类型', 'Kind'), item.isDownload ? t('下载', 'Download') : t('上传', 'Upload')),
      (t('创建时间', 'Created'), formatTime(DateTime.fromMillisecondsSinceEpoch(item.created * 1000, isUtc: true).toIso8601String())),
      if (item.isDownload) (t('暂存路径', 'Staging path'), item.path),
      if (item.saved != null) (t('已保存到', 'Saved to'), item.saved!),
      if (item.error != null) (t('错误', 'Error'), item.error!),
    ];
    return SafeArea(
      child: SingleChildScrollView(
        padding: const EdgeInsets.fromLTRB(20, 0, 20, 20),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(item.name, style: Theme.of(context).textTheme.titleMedium, maxLines: 3, overflow: TextOverflow.ellipsis),
            const SizedBox(height: 12),
            if (item.active)
              ClipRRect(
                borderRadius: BorderRadius.circular(4),
                child: LinearProgressIndicator(value: item.fraction, minHeight: 8),
              ),
            const SizedBox(height: 12),
            Text(t('速度曲线', 'Speed over time'), style: Theme.of(context).textTheme.labelLarge),
            const SizedBox(height: 6),
            SizedBox(
              height: 72,
              width: double.infinity,
              child: stats.samples.length < 2
                  ? Center(child: Text(t('暂无数据', 'No data yet'), style: Theme.of(context).textTheme.bodySmall))
                  : CustomPaint(painter: _SpeedPainter(stats.samples, scheme.primary, scheme.outlineVariant)),
            ),
            const SizedBox(height: 12),
            for (final (label, value) in rows)
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 5),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    SizedBox(
                      width: 104,
                      child: Text(label, style: Theme.of(context).textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant)),
                    ),
                    Expanded(child: SelectableText(value, style: Theme.of(context).textTheme.bodyMedium)),
                  ],
                ),
              ),
          ],
        ),
      ),
    );
  }

  String _statusName(String s) => switch (s) {
        'queued' => t('排队中', 'Queued'),
        'running' => t('进行中', 'Running'),
        'done' => t('已完成', 'Done'),
        'paused' => t('已暂停', 'Paused'),
        'cancelled' => t('已取消', 'Cancelled'),
        _ => t('失败', 'Failed'),
      };
}

class _SpeedPainter extends CustomPainter {
  _SpeedPainter(this.samples, this.line, this.grid);
  final List<int> samples;
  final Color line;
  final Color grid;

  @override
  void paint(Canvas canvas, Size size) {
    final peak = samples.reduce((a, b) => a > b ? a : b).clamp(1, 1 << 62).toDouble();
    canvas.drawLine(Offset(0, size.height), Offset(size.width, size.height), Paint()..color = grid);
    final step = size.width / (samples.length - 1);
    final path = Path();
    for (var i = 0; i < samples.length; i++) {
      final x = i * step;
      final y = size.height - samples[i] / peak * (size.height - 6) - 2;
      i == 0 ? path.moveTo(x, y) : path.lineTo(x, y);
    }
    final fill = Path.from(path)
      ..lineTo(size.width, size.height)
      ..lineTo(0, size.height)
      ..close();
    canvas.drawPath(fill, Paint()..color = line.withValues(alpha: 0.15));
    canvas.drawPath(
      path,
      Paint()
        ..color = line
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2
        ..strokeJoin = StrokeJoin.round,
    );
  }

  @override
  bool shouldRepaint(_SpeedPainter old) => true;
}
