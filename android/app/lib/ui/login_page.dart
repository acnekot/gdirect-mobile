import 'dart:async';
import 'dart:convert';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:url_launcher/url_launcher.dart';

import '../engine.dart';
import '../format.dart';
import '../main.dart';
import '../models.dart';

const setupGuide =
    'https://github.com/mizorewww/gdirect/blob/main/docs/configuration/README.zh-CN.md';

/// Sign in with the person's own Google OAuth client.
class LoginPage extends StatefulWidget {
  const LoginPage({super.key, this.canClose = false});

  /// Shown on top of the app (adding an account) rather than as the front door.
  final bool canClose;

  @override
  State<LoginPage> createState() => _LoginPageState();
}

class _LoginPageState extends State<LoginPage> {
  final _id = TextEditingController(text: app.clientId ?? '');
  final _secret = TextEditingController();
  bool _show = false;
  bool _waiting = false;
  bool _opened = false;
  String? _url;
  String? _error;
  Timer? _poll;

  @override
  void dispose() {
    _poll?.cancel();
    if (_waiting) app.setBusy('login', false);
    _id.dispose();
    _secret.dispose();
    super.dispose();
  }

  bool get _hasSavedClient => app.clientId != null && _secret.text.isEmpty;

  void _fill(String text) {
    try {
      final r = Engine.map(Engine.fast('parse_client', {'text': text}));
      setState(() {
        _error = null;
        if (r['clientId'] != null) _id.text = r['clientId'] as String;
        if (r['clientSecret'] != null) _secret.text = r['clientSecret'] as String;
      });
    } catch (e) {
      setState(() => _error = friendlyError(e));
    }
  }

  Future<void> _paste() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text?.trim() ?? '';
    if (text.isEmpty) {
      setState(() => _error = t('剪贴板是空的', 'The clipboard is empty'));
      return;
    }
    _fill(text);
  }

  Future<void> _importJson() async {
    final file = await FilePicker.pickFile(
      type: FileType.custom,
      allowedExtensions: const ['json'],
    );
    if (file == null) return;
    final bytes = await file.readAsBytes();
    _fill(utf8.decode(bytes, allowMalformed: true));
  }

  Future<void> _start() async {
    setState(() {
      _error = null;
      _waiting = true;
      _opened = false;
      _url = null;
    });
    app.setBusy('login', true);
    try {
      Engine.fast('login', {
        if (!_hasSavedClient) 'clientId': _id.text.trim(),
        if (!_hasSavedClient) 'clientSecret': _secret.text.trim(),
      });
    } catch (e) {
      _stop(friendlyError(e));
      return;
    }
    _poll?.cancel();
    _poll = Timer.periodic(const Duration(milliseconds: 500), (_) => _check());
  }

  void _stop(String? error) {
    _poll?.cancel();
    app.setBusy('login', false);
    if (!mounted) return;
    setState(() {
      _waiting = false;
      _error = error;
    });
  }

  Future<void> _openBrowser() async {
    final url = _url;
    if (url == null) return;
    final ok = await launchUrl(Uri.parse(url), mode: LaunchMode.externalApplication);
    if (!ok && mounted) {
      setState(() => _error = t('无法打开浏览器，请复制链接手动打开', 'Cannot open a browser; copy the link instead'));
    }
  }

  void _check() {
    Map<String, dynamic> s;
    try {
      s = Engine.map(Engine.fast('login_status'));
    } catch (e) {
      _stop(friendlyError(e));
      return;
    }
    switch (s['status']) {
      case 'url':
        _url = s['url'] as String?;
        if (!_opened) {
          _opened = true;
          _openBrowser();
        }
      case 'done':
        _poll?.cancel();
        app.setBusy('login', false);
        app.refreshState();
        if (mounted) {
          setState(() => _waiting = false);
          if (widget.canClose) Navigator.of(context).pop();
        }
      case 'error':
        _stop(s['error'] as String?);
      case 'cancelled':
        _stop(null);
    }
  }

  void _cancel() {
    Engine.fast('login_cancel');
    _stop(null);
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final canStart = _hasSavedClient || (_id.text.trim().isNotEmpty && _secret.text.trim().isNotEmpty);
    return Scaffold(
      appBar: widget.canClose
          ? AppBar(title: Text(t('添加账号', 'Add account')))
          : null,
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.all(20),
          children: [
            if (!widget.canClose) ...[
              const SizedBox(height: 12),
              Icon(Icons.cloud_download_rounded, size: 56, color: scheme.primary),
              const SizedBox(height: 12),
              Text('gdirect', style: Theme.of(context).textTheme.headlineMedium),
              const SizedBox(height: 4),
              Text(
                t('直连下载 Google Drive：自动找到可用的 Google 节点，多连接、可断点续传。',
                    'Download from Google Drive over reachable Google front ends: multi-connection and resumable.'),
                style: Theme.of(context).textTheme.bodyMedium?.copyWith(color: scheme.onSurfaceVariant),
              ),
              const SizedBox(height: 20),
            ],
            Card(
              child: ExpansionTile(
                shape: const Border(),
                collapsedShape: const Border(),
                leading: const Icon(Icons.help_outline_rounded),
                title: Text(t('第一次使用？先创建自己的 Google 客户端', 'First time? Create your own Google client')),
                subtitle: Text(t('约 10 分钟，只需做一次', 'About 10 minutes, once')),
                childrenPadding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
                expandedCrossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(t(
                    '1. 在 Google Cloud 控制台新建项目，并启用 Google Drive API。\n'
                        '2. 配置 OAuth 同意屏幕（外部），把你自己的账号加入测试用户。\n'
                        '3. 创建凭据 → OAuth 客户端 ID，应用类型选“桌面应用”。\n'
                        '4. 复制客户端 ID 和密钥，或下载 JSON，在下面填写。',
                    '1. Create a project in Google Cloud Console and enable the Google Drive API.\n'
                        '2. Set up the OAuth consent screen (external) and add yourself as a test user.\n'
                        '3. Create credentials → OAuth client ID, type "Desktop app".\n'
                        '4. Copy the client ID and secret, or download the JSON, and fill them in below.',
                  )),
                  const SizedBox(height: 8),
                  TextButton.icon(
                    onPressed: () => launchUrl(Uri.parse(setupGuide), mode: LaunchMode.externalApplication),
                    icon: const Icon(Icons.open_in_new_rounded, size: 18),
                    label: Text(t('查看详细图文教程', 'Open the detailed guide')),
                  ),
                ],
              ),
            ),
            const SizedBox(height: 16),
            if (_hasSavedClient)
              Card(
                child: ListTile(
                  leading: const Icon(Icons.key_rounded),
                  title: Text(t('使用已保存的客户端', 'Use the saved client')),
                  subtitle: Text(ellipsize(app.clientId ?? '', 48)),
                  trailing: TextButton(
                    onPressed: _waiting ? null : () => setState(() => _id.clear()),
                    child: Text(t('更换', 'Change')),
                  ),
                ),
              ),
            if (!_hasSavedClient || _id.text.isEmpty) ...[
              TextField(
                controller: _id,
                enabled: !_waiting,
                autocorrect: false,
                onChanged: (v) {
                  // A whole client JSON, or both parts, pasted into the ID field.
                  if (v.contains('{') || v.contains('GOCSPX')) _fill(v);
                  setState(() {});
                },
                decoration: InputDecoration(
                  labelText: t('客户端 ID', 'Client ID'),
                  hintText: 'xxxx.apps.googleusercontent.com',
                ),
              ),
              const SizedBox(height: 12),
              TextField(
                controller: _secret,
                enabled: !_waiting,
                obscureText: !_show,
                autocorrect: false,
                onChanged: (_) => setState(() {}),
                decoration: InputDecoration(
                  labelText: t('客户端密钥', 'Client secret'),
                  hintText: 'GOCSPX-…',
                  suffixIcon: IconButton(
                    icon: Icon(_show ? Icons.visibility_off_rounded : Icons.visibility_rounded),
                    onPressed: () => setState(() => _show = !_show),
                  ),
                ),
              ),
              const SizedBox(height: 8),
              Wrap(
                spacing: 8,
                children: [
                  OutlinedButton.icon(
                    onPressed: _waiting ? null : _paste,
                    icon: const Icon(Icons.content_paste_rounded, size: 18),
                    label: Text(t('从剪贴板粘贴', 'Paste')),
                  ),
                  OutlinedButton.icon(
                    onPressed: _waiting ? null : _importJson,
                    icon: const Icon(Icons.upload_file_rounded, size: 18),
                    label: Text(t('导入 JSON 文件', 'Import JSON')),
                  ),
                ],
              ),
            ],
            if (_error != null) ...[
              const SizedBox(height: 12),
              Text(_error!, style: TextStyle(color: scheme.error)),
            ],
            const SizedBox(height: 20),
            if (!_waiting)
              FilledButton.icon(
                onPressed: canStart ? _start : null,
                icon: const Icon(Icons.login_rounded),
                label: Text(t('用 Google 登录', 'Sign in with Google')),
              )
            else
              Card(
                child: Padding(
                  padding: const EdgeInsets.all(16),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Row(children: [
                        const SizedBox(width: 20, height: 20, child: CircularProgressIndicator(strokeWidth: 2)),
                        const SizedBox(width: 12),
                        Expanded(child: Text(t('正在等待浏览器中的授权…', 'Waiting for you to approve in the browser…'))),
                      ]),
                      const SizedBox(height: 12),
                      Wrap(spacing: 8, children: [
                        OutlinedButton(
                          onPressed: _url == null ? null : _openBrowser,
                          child: Text(t('重新打开登录页', 'Reopen sign-in page')),
                        ),
                        OutlinedButton(
                          onPressed: _url == null
                              ? null
                              : () => Clipboard.setData(ClipboardData(text: _url!)),
                          child: Text(t('复制链接', 'Copy link')),
                        ),
                        TextButton(onPressed: _cancel, child: Text(t('取消', 'Cancel'))),
                      ]),
                    ],
                  ),
                ),
              ),
            const SizedBox(height: 16),
            Text(
              t('授权后浏览器会跳转到本机的 127.0.0.1，请回到 gdirect。登录信息只保存在这部手机上。',
                  'After you approve, the browser goes to 127.0.0.1 on this phone; return to gdirect. Credentials stay on this phone.'),
              style: Theme.of(context).textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
            ),
          ],
        ),
      ),
    );
  }
}
