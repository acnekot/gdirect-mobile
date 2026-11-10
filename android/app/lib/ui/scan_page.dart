import 'dart:async';

import 'package:flutter/material.dart';

import '../engine.dart';
import '../format.dart';
import '../main.dart';
import '../models.dart';

/// Opens the network scan page.
Future<void> openScanPage(BuildContext context) => Navigator.of(context).push(
      MaterialPageRoute<void>(builder: (_) => const ScanPage()),
    );

/// Finds the Google front ends that answer from this network.
class ScanPage extends StatefulWidget {
  const ScanPage({super.key});

  @override
  State<ScanPage> createState() => _ScanPageState();
}

class _ScanPageState extends State<ScanPage> {
  Map<String, dynamic> _status = const {'running': false};
  List<Map<String, dynamic>> _endpoints = [];
  String? _error;
  Timer? _timer;
  bool _wasRunning = false;

  @override
  void initState() {
    super.initState();
    _refresh();
    _timer = Timer.periodic(const Duration(milliseconds: 600), (_) => _refresh());
  }

  @override
  void dispose() {
    _timer?.cancel();
    if (_wasRunning) app.setBusy('scan', false);
    super.dispose();
  }

  void _refresh() {
    try {
      final s = Engine.map(Engine.fast('scan_status'));
      final running = s['running'] == true;
      if (running != _wasRunning) {
        _wasRunning = running;
        app.setBusy('scan', running);
        if (!running) {
          app.refreshState();
          _endpoints = Engine.maps(Engine.fast('endpoints'));
        }
      }
      if (!mounted) return;
      setState(() {
        _status = s;
        _error = s['error'] as String?;
        if (_endpoints.isEmpty && !running) {
          _endpoints = Engine.maps(Engine.fast('endpoints'));
        }
      });
    } catch (e) {
      if (mounted) setState(() => _error = friendlyError(e));
    }
  }

  void _start() {
    try {
      Engine.fast('scan_start');
      _refresh();
    } catch (e) {
      setState(() => _error = friendlyError(e));
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final running = _status['running'] == true;
    final verified = ((_status['verified'] as List?) ?? const []).cast<String>();
    final good = (_status['good'] as num?)?.toInt() ?? 0;
    final probed = (_status['probed'] as num?)?.toInt() ?? 0;
    final open = (_status['open'] as num?)?.toInt() ?? 0;
    final usable = _endpoints.where((e) => e['error'] == null && e['latencyMs'] != null).toList();
    return Scaffold(
      appBar: AppBar(title: Text(t('网络扫描', 'Network scan'))),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          Text(
            t('扫描会在当前网络下寻找可直连的 Google 节点，并保留最快的 24 小时。换了网络（Wi-Fi / 流量）请重新扫描。',
                'The scan looks for Google front ends reachable from this network and keeps the fastest for 24 hours. Scan again after changing networks.'),
            style: Theme.of(context).textTheme.bodyMedium,
          ),
          const SizedBox(height: 12),
          Card(
            color: scheme.tertiaryContainer,
            child: Padding(
              padding: const EdgeInsets.all(12),
              child: Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
                Icon(Icons.info_outline_rounded, color: scheme.onTertiaryContainer),
                const SizedBox(width: 12),
                Expanded(
                  child: Text(
                    t('开着 VPN / 代理时，“设置 → 绕过 VPN 直连”会让本应用改走真实网络。如果 VPN 开了“始终开启并阻止无 VPN 的连接”，这个办法会失效，请改在 VPN 的分应用设置里让 gdirect 绕过。',
                        'With a VPN on, “Settings → Go around a VPN” sends this app over the real network. If the VPN is set to always-on with “block connections without VPN”, that cannot work: exclude gdirect in the VPN’s per-app settings instead.'),
                    style: TextStyle(color: scheme.onTertiaryContainer),
                  ),
                ),
              ]),
            ),
          ),
          const SizedBox(height: 16),
          if (running)
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16),
                child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
                  Row(children: [
                    const SizedBox(width: 20, height: 20, child: CircularProgressIndicator(strokeWidth: 2)),
                    const SizedBox(width: 12),
                    Text(t('正在扫描…', 'Scanning…')),
                  ]),
                  const SizedBox(height: 8),
                  Text(t('已探测 $probed 个，$open 个响应，$good 个可用', 'Probed $probed, $open answered, $good usable')),
                  if (verified.isNotEmpty) ...[
                    const SizedBox(height: 8),
                    for (final line in verified.reversed.take(5))
                      Text(line, style: Theme.of(context).textTheme.bodySmall),
                  ],
                  const SizedBox(height: 8),
                  OutlinedButton(
                    onPressed: () => Engine.fast('scan_cancel'),
                    child: Text(t('停止', 'Stop')),
                  ),
                ]),
              ),
            )
          else
            FilledButton.icon(
              onPressed: _start,
              icon: const Icon(Icons.travel_explore_rounded),
              label: Text(usable.isEmpty ? t('开始扫描', 'Start scan') : t('重新扫描', 'Scan again')),
            ),
          if (_status['note'] == 'intercepted') ...[
            const SizedBox(height: 12),
            Text(
              t('提示：几乎所有地址都能连通，可能有 VPN / 防火墙在拦截，结果反映的是代理而非你的直连网络。',
                  'Note: almost every address answered, so a VPN or firewall is probably intercepting; results reflect it, not your direct network.'),
              style: TextStyle(color: scheme.error),
            ),
          ],
          if (_error != null) ...[
            const SizedBox(height: 12),
            Text(_error!, style: TextStyle(color: scheme.error)),
          ],
          const SizedBox(height: 20),
          Text(t('已验证的节点', 'Verified endpoints'), style: Theme.of(context).textTheme.titleMedium),
          const SizedBox(height: 8),
          if (usable.isEmpty)
            Text(t('暂无', 'None yet'), style: TextStyle(color: scheme.onSurfaceVariant))
          else
            for (final e in usable)
              ListTile(
                dense: true,
                contentPadding: EdgeInsets.zero,
                leading: const Icon(Icons.check_circle_rounded, color: Colors.green),
                title: Text('${e['ip']}'),
                subtitle: (e['sni'] as String?)?.isNotEmpty == true ? Text('SNI ${e['sni']}') : null,
                trailing: Text('${e['latencyMs']} ms'),
              ),
        ],
      ),
    );
  }
}
