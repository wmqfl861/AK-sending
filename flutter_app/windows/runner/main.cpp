#include <flutter/dart_project.h>
#include <flutter/flutter_view_controller.h>
#include <windows.h>

#include "flutter_window.h"
#include "utils.h"

int APIENTRY wWinMain(_In_ HINSTANCE instance, _In_opt_ HINSTANCE prev,
                      _In_ wchar_t *command_line, _In_ int show_command) {
  // Attach to console when present (e.g., 'flutter run') or create a
  // new console when running with a debugger.
  if (!::AttachConsole(ATTACH_PARENT_PROCESS) && ::IsDebuggerPresent()) {
    CreateAndAttachConsole();
  }

  // Initialize COM, so that it is available for use in the library and/or
  // plugins.
  ::CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

  flutter::DartProject project(L"data");

  // --- Crash-analysis diagnostics, experiment E7 ---
  // (reports/codex-crash-analysis-2026-11-07.md; kept DISABLED by default -
  // these lines are documentation of the correct hook points, not settings.)
  //
  // The local ephemeral wrapper exposes two independent diagnostic switches
  // that would be set HERE, after constructing the DartProject and before
  // window.Create(...) spawns the engine:
  //
  //   project.set_impeller_switch(flutter::ImpellerSwitch::Disabled);
  //     Forces the non-Impeller backend. Caveat: the FLTEnableImpeller
  //     environment variable has no read point in this repository, so
  //     setting it proves nothing. After flipping this switch, verify from
  //     engine logs that the backend ACTUALLY changed before comparing
  //     crash behavior.
  //
  //   project.set_ui_thread_policy(flutter::UIThreadPolicy::RunOnSeparateThread);
  //     Moves the UI thread off the platform thread (compare against
  //     RunOnPlatformThread). Record which thread the DEFAULT mode actually
  //     uses first, so the comparison is against a known baseline.
  //
  // Usage rules from the report: run each switch as a SEPARATE experiment
  // (never change renderer and thread policy at the same time), keep the
  // bundle and every other setting identical, and only after E0/E1 produced
  // a faulting-thread stack that points at this path. An unverified switch
  // must not be promoted to a permanent fix, and command-line arguments
  // passed to this exe reach the DART entrypoint (see set_dart_entrypoint_
  // arguments below), NOT these C++ switches.

  std::vector<std::string> command_line_arguments =
      GetCommandLineArguments();

  project.set_dart_entrypoint_arguments(std::move(command_line_arguments));

  FlutterWindow window(project);
  Win32Window::Point origin(10, 10);
  Win32Window::Size size(1280, 720);
  if (!window.Create(L"ak_sending_app", origin, size)) {
    return EXIT_FAILURE;
  }
  window.SetQuitOnClose(true);

  ::MSG msg;
  while (::GetMessage(&msg, nullptr, 0, 0)) {
    ::TranslateMessage(&msg);
    ::DispatchMessage(&msg);
  }

  ::CoUninitialize();
  return EXIT_SUCCESS;
}
