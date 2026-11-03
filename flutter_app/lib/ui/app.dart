import 'package:flutter/material.dart';

import 'pages/about_page.dart';
import 'pages/home_page.dart';
import 'pages/jobs_page.dart';
import 'pages/receive_page.dart';
import 'pages/send_page.dart';
import 'service/transfer_service.dart';

/// Material 3 app root for the AK-sending desktop UI.
///
/// `lib/main.dart` (owned by the services agent / integrator) constructs
/// the real [TransferService] and passes it here:
/// `runApp(AkSendingApp(service: FfiTransferService()))`.
class AkSendingApp extends StatelessWidget {
  const AkSendingApp({super.key, required this.service});

  final TransferService service;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'AK-sending',
      theme: ThemeData(
        useMaterial3: true,
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFF316A5A)),
      ),
      home: AppShell(service: service),
    );
  }
}

/// NavigationRail shell: Home / Receive / Send / Jobs / About. Pages stay
/// alive in an IndexedStack so form state (e.g. the remembered output
/// directory) survives tab switches.
class AppShell extends StatefulWidget {
  const AppShell({super.key, required this.service});

  final TransferService service;

  @override
  State<AppShell> createState() => _AppShellState();
}

class _AppShellState extends State<AppShell> {
  int _index = 0;

  @override
  Widget build(BuildContext context) {
    final service = widget.service;
    return Scaffold(
      body: LayoutBuilder(
        builder: (context, constraints) {
          final extended = constraints.maxWidth >= 1000;
          return Row(
            children: [
              NavigationRail(
                extended: extended,
                labelType:
                    extended ? null : NavigationRailLabelType.all,
                selectedIndex: _index,
                onDestinationSelected: (i) =>
                    setState(() => _index = i),
                destinations: const [
                  NavigationRailDestination(
                    icon: Icon(Icons.home_outlined),
                    selectedIcon: Icon(Icons.home),
                    label: Text('主页'),
                  ),
                  NavigationRailDestination(
                    icon: Icon(Icons.download_outlined),
                    selectedIcon: Icon(Icons.download),
                    label: Text('接收'),
                  ),
                  NavigationRailDestination(
                    icon: Icon(Icons.upload_outlined),
                    selectedIcon: Icon(Icons.upload),
                    label: Text('发送'),
                  ),
                  NavigationRailDestination(
                    icon: Icon(Icons.history_outlined),
                    selectedIcon: Icon(Icons.history),
                    label: Text('历史'),
                  ),
                  NavigationRailDestination(
                    icon: Icon(Icons.info_outline),
                    selectedIcon: Icon(Icons.info),
                    label: Text('关于'),
                  ),
                ],
              ),
              const VerticalDivider(width: 1, thickness: 1),
              Expanded(
                child: IndexedStack(
                  index: _index,
                  children: [
                    HomePage(
                      service: service,
                      onGoTab: (i) => setState(() => _index = i),
                    ),
                    ReceivePage(service: service),
                    SendPage(service: service),
                    JobsPage(service: service),
                    AboutPage(service: service),
                  ],
                ),
              ),
            ],
          );
        },
      ),
    );
  }
}
