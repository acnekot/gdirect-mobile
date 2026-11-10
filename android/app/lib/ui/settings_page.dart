import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
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
  List<String> _addresses = [];

  /// The phone's addresses on the local network, for WebDAV from other devices.
  Future<void> _loadAddresses() async {
    try {
      final list = await NetworkInterface.list(type: InternetAddressType.IPv4);
      final found = [
        for (final interface in list)
          for (final address in interface.addresses)
            if (!address.isLoopback) address.address,
      ];
      if (mounted) setState(() => _addresses = found);
    } catch (_) {}
  }

  void _copy(String text) {
    Clipboard.setData(ClipboardData(text: text));
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(t('已复制', 'Copied'))));
  }

  Future<void> _changePassword(String current) async {
    final controller = TextEditingController(text: current);
    final next = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(t('WebDAV 密码', 'WebDAV password')),
        content: TextField(
          controller: controller,
          autofocus: true,
          autocorrect: false,
          decoration: InputDecoration(helperText: t('至少 6 个字符', 'At least 6 characters')),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: Text(t('取消', 'Cancel'))),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text),
            child: Text(t('保存', 'Save')),
          ),
        ],
      ),
    );
    if (next == null || next.trim() == current) return;
    try {
      app.setSetting('webdavPassword', next.trim());
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(friendlyError(e))));
      }
    }
  }

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
    _loadAddresses();
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
        final network = app.network;
        final port = app.server['port'];
        final password = (app.server['webdavPassword'] as String?) ?? '';
        final lan = settings['webdavLan'] == true;
        final kind = switch (network.kind) {
          'wifi' => 'Wi-Fi',
          'cellular' => t('移动数据', 'mobile data'),
          'ethernet' => t('有线网络', 'Ethernet'),
          _ => t('当前网络', 'the current network'),
        };
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
                      : Text(app.serverAccounts.contains(email)
                          ? t('通过我的服务器登录 · 点按切换', 'Signed in through my server · tap to switch')
                          : t('点按切换', 'Tap to switch')),
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
              SwitchListTile(
                secondary: const Icon(Icons.vpn_lock_rounded),
                title: Text(t('绕过 VPN 直连', 'Go around a VPN')),
                subtitle: Text(!network.vpn
                    ? t('没有检测到 VPN', 'No VPN detected')
                    : network.bypassing
                        ? t('检测到 VPN，本应用正通过 $kind 直连', 'VPN detected; this app is using $kind directly')
                        : t('检测到 VPN，本应用的流量会经过它', 'VPN detected; this app’s traffic goes through it')),
                value: settings['bypassVpn'] != false,
                onChanged: (v) => app.setSetting('bypassVpn', v),
              ),
              SwitchListTile(
                secondary: const Icon(Icons.autorenew_rounded),
                title: Text(t('换网络后自动扫描', 'Scan again after changing networks')),
                subtitle: Text(t('切换 Wi-Fi 或移动数据时重新寻找可用节点', 'Looks for endpoints again when Wi-Fi or mobile data changes')),
                value: settings['autoScan'] != false,
                onChanged: (v) => app.setSetting('autoScan', v),
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
              _header(context, 'WebDAV'),
              SwitchListTile(
                secondary: const Icon(Icons.share_rounded),
                title: Text(t('开启 WebDAV', 'Serve WebDAV')),
                subtitle: Text(t('让播放器、文件管理器通过本应用访问“我的云端硬盘”', 'Lets players and file managers reach My Drive through this app')),
                value: app.webdav,
                onChanged: (v) {
                  app.setSetting('webdav', v);
                  _loadAddresses();
                },
              ),
              if (app.webdav && port != null) ...[
                ListTile(
                  leading: const Icon(Icons.link_rounded),
                  title: Text('http://127.0.0.1:$port/dav/'),
                  subtitle: Text(t('本机地址 · 用户名随意填写', 'On this phone · any user name')),
                  trailing: const Icon(Icons.copy_rounded, size: 20),
                  onTap: () => _copy('http://127.0.0.1:$port/dav/'),
                ),
                ListTile(
                  leading: const Icon(Icons.password_rounded),
                  title: Text(password),
                  subtitle: Text(t('密码 · 点按复制，长按修改', 'Password · tap to copy, hold to change')),
                  trailing: const Icon(Icons.copy_rounded, size: 20),
                  onTap: () => _copy(password),
                  onLongPress: () => _changePassword(password),
                ),
                SwitchListTile(
                  secondary: const Icon(Icons.lan_rounded),
                  title: Text(t('允许局域网内的其他设备访问', 'Allow other devices on the local network')),
                  subtitle: Text(!lan
                      ? t('关闭时只有这部手机上的应用能访问', 'Off: only apps on this phone can connect')
                      : _addresses.isEmpty
                          ? t('已开启；没有找到本机的局域网地址', 'On; no local address found')
                          : _addresses.map((a) => 'http://$a:$port/dav/').join('\n')),
                  value: lan,
                  onChanged: (v) {
                    app.setSetting('webdavLan', v);
                    _loadAddresses();
                  },
                ),
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
                  child: Text(
                    t('WebDAV 没有加密，只应在可信的网络里对其他设备开放。开启期间应用会保持在后台运行。删除的文件会进入云端硬盘的回收站。',
                        'WebDAV is not encrypted: open it to other devices only on a network you trust. The app keeps running in the background while it is on. Deleted files go to the Drive trash.'),
                    style: Theme.of(context).textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
                  ),
                ),
              ],
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
