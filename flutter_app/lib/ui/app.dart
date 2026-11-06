/// Wave-28 app root and navigation shell.
///
/// Two first-level destinations only (user-frozen spec): 「连接」 (the
/// home page: pairing actions + current connections) and 「设置」 (the
/// single save-folder setting). Phone widths get a Material 3 bottom
/// NavigationBar; wide surfaces a NavigationRail. Pages stay alive in an
/// IndexedStack. The incoming-request banner host sits above the page
/// content on BOTH tabs.
///
/// `lib/main.dart` constructs the [SimpleTransferService] (wave-28 switch
/// point: fake today, agent C's real implementation when it lands) and
/// hands it here.
library;

import 'package:flutter/material.dart';

import 'pages/home_page.dart';
import 'pages/settings_page.dart';
import 'service/file_pick_gateway.dart';
import 'service/save_dir_store.dart';
import 'service/simple_transfer_service.dart';
import 'widgets/incoming_requests_host.dart';

/// Material 3 app root for the wave-28 minimal-pairing UI.
class AkSendingApp extends StatelessWidget {
  const AkSendingApp({
    super.key,
    required this.service,
    this.store = const SharedPrefsSaveDirStore(),
    this.picker = const PluginFilePickGateway(),
  });

  final SimpleTransferService service;
  final SaveDirStore store;
  final FilePickGateway picker;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'AK-sending',
      theme: ThemeData(
        useMaterial3: true,
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFF316A5A)),
      ),
      home: AppShell(service: service, store: store, picker: picker),
    );
  }
}

/// Two-destination shell: 连接 / 设置.
class AppShell extends StatefulWidget {
  const AppShell({
    super.key,
    required this.service,
    required this.store,
    required this.picker,
  });

  final SimpleTransferService service;
  final SaveDirStore store;
  final FilePickGateway picker;

  @override
  State<AppShell> createState() => _AppShellState();
}

class _AppShellState extends State<AppShell> {
  int _index = 0;

  @override
  void initState() {
    super.initState();
    // One-shot service init (listeners, identity); failures surface
    // through the service's own state, not the shell.
    widget.service.start();
    _restoreSaveDir();
  }

  /// Re-applies the persisted default save folder to the service so
  /// receives land where the user chose last time.
  Future<void> _restoreSaveDir() async {
    try {
      final persisted = await widget.store.load();
      if (persisted == null || persisted.isEmpty) {
        return;
      }
      if (persisted != widget.service.saveDir) {
        await widget.service.setSaveDir(persisted);
      }
    } on Object {
      // A broken store never blocks startup; the platform default wins.
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: LayoutBuilder(
        builder: (context, constraints) {
          final wide = constraints.maxWidth >= 700;
          final content = Column(
            children: [
              IncomingRequestsHost(service: widget.service),
              Expanded(
                child: IndexedStack(
                  index: _index,
                  children: [
                    HomePage(service: widget.service, picker: widget.picker),
                    SettingsPage(
                      service: widget.service,
                      store: widget.store,
                      picker: widget.picker,
                    ),
                  ],
                ),
              ),
            ],
          );
          if (wide) {
            return Row(
              children: [
                NavigationRail(
                  extended: constraints.maxWidth >= 1100,
                  labelType: constraints.maxWidth >= 1100
                      ? null
                      : NavigationRailLabelType.all,
                  selectedIndex: _index,
                  onDestinationSelected: (i) => setState(() => _index = i),
                  destinations: const [
                    NavigationRailDestination(
                      icon: Icon(Icons.device_hub_outlined),
                      selectedIcon: Icon(Icons.device_hub),
                      label: Text('连接'),
                    ),
                    NavigationRailDestination(
                      icon: Icon(Icons.settings_outlined),
                      selectedIcon: Icon(Icons.settings),
                      label: Text('设置'),
                    ),
                  ],
                ),
                const VerticalDivider(width: 1, thickness: 1),
                Expanded(child: content),
              ],
            );
          }
          return content;
        },
      ),
      bottomNavigationBar: LayoutBuilder(
        builder: (context, constraints) {
          // The rail replaces the bar on wide surfaces.
          if (constraints.maxWidth >= 700) {
            return const SizedBox.shrink();
          }
          return NavigationBar(
            selectedIndex: _index,
            onDestinationSelected: (i) => setState(() => _index = i),
            destinations: const [
              NavigationDestination(
                icon: Icon(Icons.device_hub_outlined),
                selectedIcon: Icon(Icons.device_hub),
                label: '连接',
              ),
              NavigationDestination(
                icon: Icon(Icons.settings_outlined),
                selectedIcon: Icon(Icons.settings),
                label: '设置',
              ),
            ],
          );
        },
      ),
    );
  }
}
