/// Wave-28 app root and navigation shell.
///
/// Two first-level destinations only (user-frozen spec): 「连接」 (the
/// home page: pairing actions + current connections) and 「设置」 (the
/// single save-folder setting). Phone widths get a Material 3 bottom
/// NavigationBar; wide surfaces a NavigationRail. Pages stay alive in an
/// IndexedStack. The incoming-request banner host sits above the page
/// content on BOTH tabs.
///
/// `lib/main.dart` constructs the REAL [SimpleTransferService] (wave-29
/// P1-01: `FfiSimpleTransferService` over the borrowed engine) and hands
/// it here; a construction-time failure (missing DLL, old exports)
/// arrives as [AkSendingApp.startupError] and a failed `start()` is
/// captured by the shell — both render the visible engine-error card
/// below instead of a silent local demo.
library;

import 'dart:async' show unawaited;

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
    this.startupError,
    this.store = const SharedPrefsSaveDirStore(),
    this.picker = const PluginFilePickGateway(),
  });

  final SimpleTransferService service;

  /// A CONSTRUCTION-time failure reason from `main.dart` (missing native
  /// library / unsupported exports): non-null renders the engine-error
  /// card. The service member still answers (throwing its honest
  /// StateErrors) so navigation keeps working in the degraded state.
  final String? startupError;

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
      home: AppShell(
        service: service,
        startupError: startupError,
        store: store,
        picker: picker,
      ),
    );
  }
}

/// Two-destination shell: 连接 / 设置.
class AppShell extends StatefulWidget {
  const AppShell({
    super.key,
    required this.service,
    this.startupError,
    required this.store,
    required this.picker,
  });

  final SimpleTransferService service;

  /// See [AkSendingApp.startupError].
  final String? startupError;

  final SaveDirStore store;
  final FilePickGateway picker;

  @override
  State<AppShell> createState() => _AppShellState();
}

class _AppShellState extends State<AppShell> {
  int _index = 0;

  /// A failed `service.start()` reason (wave-29 P1-01): rendered as the
  /// visible engine-error card together with [AppShell.startupError].
  String? _startError;

  @override
  void initState() {
    super.initState();
    // One-shot service init (listeners, identity, task runtime). A
    // failure (missing registry config, engine start error…) must be
    // VISIBLE, not swallowed: latch it for the error card (P1-01).
    unawaited(widget.service.start().catchError((Object error) {
      if (mounted) {
        setState(() => _startError = '$error');
      }
    }));
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
          final startupError = widget.startupError ?? _startError;
          final content = Column(
            children: [
              if (startupError != null)
                _StartupErrorCard(reason: startupError),
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

/// The visible engine-unavailable state (wave-29 P1-01): construction
/// failure or failed `start()` — the app stays navigable but honestly
/// says pairing/transfers cannot run.
class _StartupErrorCard extends StatelessWidget {
  const _StartupErrorCard({required this.reason});

  final String reason;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Card(
      margin: const EdgeInsets.fromLTRB(12, 8, 12, 4),
      color: scheme.errorContainer,
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Icon(Icons.error_outline, color: scheme.onErrorContainer),
            const SizedBox(width: 8),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    '传输引擎不可用，配对与传输功能暂不能使用。',
                    key: const ValueKey('app_startup_error'),
                    style: theme.textTheme.titleSmall
                        ?.copyWith(color: scheme.onErrorContainer),
                  ),
                  const SizedBox(height: 4),
                  Text(
                    reason,
                    key: const ValueKey('app_startup_error_detail'),
                    style: theme.textTheme.bodySmall
                        ?.copyWith(color: scheme.onErrorContainer),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}
