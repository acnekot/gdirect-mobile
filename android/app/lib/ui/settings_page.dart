import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';

import '../engine.dart';
import '../format.dart';
import '../main.dart';
import '../models.dart';
import 'login_page.dart';

const projectUrl = 'https://github.com/mizorewww/gdirect';

class SettingsPage extends StatefulWidget {
  const SettingsPage({super.key, required this.openScan});
  final VoidCallback openScan;

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  Map<String, dynamic>? _about;
  String? _aboutError;
  String? _aboutFor;

  Future<void> _loadAbout() async {
    final account = app.account;
    if (account == null || _aboutFor == account) return;
    _aboutFor = account;
    try {
      final about = Engine.map(await Engine.slow('about'));
      if (mounted) setState(() => _about = about);
    } catch (e) {
      if (mounted) setState(() => _aboutError = friendlyError(e));
    }
  }

  @override
  void initState() {
    super.initState();
    _loadAbout();
  }

  Future<void> _logout(String email) async {
    final ok = await showDialog<bool>(
          context: context,
          builder: (context) => AlertDialog(
            title: Text(t('退出登录？', 'Sign out?')),
            content: Text(email),
            actions: [
              TextButton(onPressed: () => Navigator.pop(context, false), child: Text(t('取消', 'Cancel'))),
              FilledButton(onPressed: () => Navigator.pop(context, true), child: Text(t('退出', 'Sign out'))),
            ],
          ),
        ) ??
        false;
    if (ok) {
      _about = null;
      _aboutFor = null;
      await app.logout(email);
      _loadAbout();
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: app,
      builder: (context, _) {
        _loadAbout();
        final scheme = Theme.of(context).colorScheme;
        final settings = app.settings;
        final connections = (settings['connections'] as num?)?.toInt() ?? 8;
        final parallel = (settings['parallel'] as num?)?.toInt() ?? 2;
        final usage = (_about?['usage'] as num?)?.toInt();
        final limit = (_about?['limit'] as num?)?.toInt();
        return Scaffold(
          appBar: AppBar(title: Text(t('设置', 'Settings'))),
          body: ListView(
            children: [
              _header(context, t('账号', 'Accounts')),
              for (final email in app.accounts)
                ListTile(
                  leading: Icon(
                    email == app.account ? Icons.account_circle_rounded : Icons.account_circle_outlined,
                    color: email == app.account ? scheme.primary : null,
                  ),
                  title: Text(email),
                  subtitle: email == app.account
                      ? Text(usage == null
                          ? (_aboutError ?? t('正在读取存储用量…', 'Reading storage…'))
                          : limit == null
                              ? t('已用 ${formatBytes(usage)}', 'Using ${formatBytes(usage)}')
                              : t('已用 ${formatBytes(usage)} / ${formatBytes(limit)}',
                                  'Using ${formatBytes(usage)} of ${formatBytes(limit)}'))
                      : Text(t('点按切换', 'Tap to switch')),
                  onTap: email == app.account
                      ? null
                      : () {
                          _about = null;
                          _aboutFor = null;
                          app.setAccount(email);
                        },
                  trailing: IconButton(
                    icon: const Icon(Icons.logout_rounded),
                    tooltip: t('退出登录', 'Sign out'),
                    onPressed: () => _logout(email),
                  ),
                ),
              ListTile(
                leading: const Icon(Icons.person_add_alt_1_rounded),
                title: Text(t('添加账号', 'Add account')),
                onTap: () => Navigator.of(context).push(
                  MaterialPageRoute<void>(builder: (_) => const LoginPage(canClose: true)),
                ),
              ),
              const Divider(),
              _header(context, t('网络', 'Network')),
              SwitchListTile(
                secondary: const Icon(Icons.bolt_rounded),
                title: Text(t('直连 Google 节点', 'Direct to Google front ends')),
                subtitle: Text(t('关闭后使用系统网络（含 VPN / 代理）', 'Off: use the system network, VPN and proxy included')),
                value: app.direct,
                onChanged: (v) => app.setSetting('direct', v),
              ),
              ListTile(
                leading: const Icon(Icons.travel_explore_rounded),
                title: Text(t('网络扫描', 'Network scan')),
                subtitle: Text(app.routes > 0
                    ? t('${app.routes} 个可用节点', '${app.routes} usable endpoint(s)')
                    : t('尚未扫描', 'Not scanned yet')),
                trailing: const Icon(Icons.chevron_right_rounded),
                onTap: widget.openScan,
              ),
              const Divider(),
              _header(context, t('传输', 'Transfers')),
              ListTile(
                leading: const Icon(Icons.call_split_rounded),
                title: Text(t('每个下载的连接数：$connections', 'Connections per download: $connections')),
                subtitle: Slider(
                  value: connections.toDouble(),
                  min: 1,
                  max: 16,
                  divisions: 15,
                  label: '$connections',
                  onChanged: (v) => app.setSetting('connections', v.round()),
                ),
              ),
              ListTile(
                leading: const Icon(Icons.layers_rounded),
                title: Text(t('同时进行的传输数：$parallel', 'Simultaneous transfers: $parallel')),
                subtitle: Slider(
                  value: parallel.toDouble(),
                  min: 1,
                  max: 4,
                  divisions: 3,
                  label: '$parallel',
                  onChanged: (v) => app.setSetting('parallel', v.round()),
                ),
              ),
              ListTile(
                leading: const Icon(Icons.folder_rounded),
                title: Text(t('保存位置', 'Save location')),
                subtitle: Text(t('系统“下载”文件夹里的 gdirect', 'gdirect in the system Downloads folder')),
              ),
              const Divider(),
              _header(context, t('关于', 'About')),
              ListTile(
                leading: const Icon(Icons.info_outline_rounded),
                title: Text('gdirect mobile ${app.state['version'] ?? ''}'),
                subtitle: Text(t('基于 mizorewww/gdirect 的 Rust 引擎 · GPL-2.0-or-later',
                    'Built on the Rust engine of mizorewww/gdirect · GPL-2.0-or-later')),
                onTap: () => launchUrl(Uri.parse(projectUrl), mode: LaunchMode.externalApplication),
              ),
              const SizedBox(height: 24),
            ],
          ),
        );
      },
    );
  }

  Widget _header(BuildContext context, String text) => Padding(
        padding: const EdgeInsets.fromLTRB(16, 16, 16, 4),
        child: Text(
          text,
          style: Theme.of(context).textTheme.labelLarge?.copyWith(color: Theme.of(context).colorScheme.primary),
        ),
      );
}
