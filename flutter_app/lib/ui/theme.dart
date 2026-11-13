/// Client UI U1 (2026-11-13): the app's light/dark themes and the few
/// semantic colors the Material scheme does not carry.
///
/// The app follows the system light/dark setting. Success accents used to
/// be hard-coded light-theme greens (`Colors.green.shade800` text on a
/// pale green box), which become unreadable on a dark surface — every
/// success color now comes from [successColor]/[successContainerColor],
/// tuned per brightness.
library;

import 'package:flutter/material.dart';

/// The brand seed (unchanged since wave 28).
const Color appSeedColor = Color(0xFF316A5A);

/// The app theme for [brightness] (Material 3, seeded scheme).
ThemeData buildAppTheme(Brightness brightness) {
  final scheme = ColorScheme.fromSeed(
    seedColor: appSeedColor,
    brightness: brightness,
  );
  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
  );
}

/// Foreground for success text and icons, legible on [scheme]'s surface.
Color successColor(ColorScheme scheme) => scheme.brightness == Brightness.dark
    ? const Color(0xFF81C784) // green 300
    : const Color(0xFF2E7D32); // green 800

/// A quiet background for a success result box.
Color successContainerColor(ColorScheme scheme) =>
    successColor(scheme).withValues(alpha: 0.12);
