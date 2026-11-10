import 'package:flutter/material.dart';

import 'controller.dart';
import 'format.dart';
import 'ui/files_page.dart';
import 'ui/login_page.dart';
import 'ui/scan_page.dart';
import 'ui/settings_page.dart';
import 'ui/transfers_page.dart';

final app = AppController();

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const GdirectApp());
}

ThemeData _theme(Brightness brightness) {
  final scheme = ColorScheme.fromSeed(
    seedColor: const Color(0xFF1A73E8),
    brightness: brightness,
  );
  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    appBarTheme: AppBarTheme(
      backgroundColor: scheme.surface,
      scrolledUnderElevation: 1,
      centerTitle: false,
    ),
    cardTheme: CardThemeData(
      elevation: 0,
      color: scheme.surfaceContainerLow,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(16)),
    ),
    inputDecorationTheme: InputDecorationTheme(
      border: OutlineInputBorder(borderRadius: BorderRadius.circular(12)),
    ),
  );
}

class GdirectApp extends StatelessWidget {
  const GdirectApp({super.key});

  @override
  Widget build(BuildContext context) => MaterialApp(
        title: 'gdirect',
        debugShowCheckedModeBanner: false,
        theme: _theme(Brightness.light),
        darkTheme: _theme(Brightness.dark),
        home: const _Gate(),
      );
}

/// Starts the engine, then shows sign-in or the app.
class _Gate extends StatefulWidget {
  const _Gate();

  @override
  State<_Gate> createState() => _GateState();
}

class _GateState extends State<_Gate> {
  @override
  void initState() {
    super.initState();
    app.init();
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
        listenable: app,
        builder: (context, _) {
          if (app.fatal != null) {
            return Scaffold(
              body: Center(
                child: Padding(
                  padding: const EdgeInsets.all(24),
                  child: Text(
                    t('无法启动引擎：', 'Cannot start the engine: ') + app.fatal!,
                    textAlign: TextAlign.center,
                  ),
                ),
              ),
            );
          }
          if (!app.ready) {
            return const Scaffold(body: Center(child: CircularProgressIndicator()));
          }
          return app.signedIn ? const HomeShell() : const LoginPage();
        },
      );
}

class HomeShell extends StatefulWidget {
  const HomeShell({super.key});

  @override
  State<HomeShell> createState() => _HomeShellState();
}

class _HomeShellState extends State<HomeShell> {
  int _index = 0;
  bool _asking = false;

  @override
  void initState() {
    super.initState();
    app.addListener(_offerShared);
    WidgetsBinding.instance.addPostFrameCallback((_) => _offerShared());
  }

  @override
  void dispose() {
    app.removeListener(_offerShared);
    super.dispose();
  }

  /// Files another app shared with us: ask where in Drive they should go.
  Future<void> _offerShared() async {
    if (_asking || app.shared.isEmpty || !mounted) return;
    _asking = true;
    final files = app.shared;
    final here = app.folderId != 'root' && app.folderName.isNotEmpty;
    final target = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(t('上传到云端硬盘', 'Upload to Drive')),
        content: Text(files.length == 1
            ? files.first.name
            : t('${files.length} 个文件', '${files.length} files')),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: Text(t('取消', 'Cancel'))),
          if (here)
            TextButton(
              onPressed: () => Navigator.pop(context, app.folderId),
              child: Text(t('传到“${ellipsize(app.folderName, 16)}”', 'To “${ellipsize(app.folderName, 16)}”')),
            ),
          FilledButton(
            onPressed: () => Navigator.pop(context, 'root'),
            child: Text(t('传到我的云端硬盘', 'To My Drive')),
          ),
        ],
      ),
    );
    app.clearShared();
    _asking = false;
    if (target == null) return;
    try {
      await app.uploadPicked(files, target);
      if (mounted) _go(1);
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$e')));
      }
    }
  }

  void _go(int index) => setState(() => _index = index);

  @override
  Widget build(BuildContext context) {
    return PopScope(
      // Back from another tab returns to Files first.
      canPop: _index == 0,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop && _index != 0) _go(0);
      },
      child: ListenableBuilder(
        listenable: app,
        builder: (context, _) => Scaffold(
          body: IndexedStack(
            index: _index,
            children: [
              FilesPage(
                active: _index == 0,
                openTransfers: () => _go(1),
                openScan: () => openScanPage(context),
              ),
              const TransfersPage(),
              SettingsPage(openScan: () => openScanPage(context)),
            ],
          ),
          bottomNavigationBar: NavigationBar(
            selectedIndex: _index,
            onDestinationSelected: _go,
            destinations: [
              NavigationDestination(
                icon: const Icon(Icons.folder_outlined),
                selectedIcon: const Icon(Icons.folder_rounded),
                label: t('文件', 'Files'),
              ),
              NavigationDestination(
                icon: Badge(
                  isLabelVisible: app.activeTransfers > 0,
                  label: Text('${app.activeTransfers}'),
                  child: const Icon(Icons.swap_vert_rounded),
                ),
                label: t('传输', 'Transfers'),
              ),
              NavigationDestination(
                icon: const Icon(Icons.settings_outlined),
                selectedIcon: const Icon(Icons.settings_rounded),
                label: t('设置', 'Settings'),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
