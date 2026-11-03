import Flutter
import UIKit
import UniformTypeIdentifiers

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  private let risk = DirectoryProbe()
  override func application(_ application: UIApplication,
      didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
    NotificationCenter.default.addObserver(forName: UIApplication.didEnterBackgroundNotification,
        object: nil, queue: .main) { [weak self] _ in self?.risk.cancel() }
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }
  func didInitializeImplicitFlutterEngine(_ bridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: bridge.pluginRegistry)
    let channel = FlutterMethodChannel(name: "ak.m0/risk",
        binaryMessenger: bridge.applicationRegistrar.messenger())
    channel.setMethodCallHandler { [weak self] call, result in
      guard let self = self else { result("unavailable"); return }
      self.risk.handle(call.method, result: result)
    }
  }
}

private final class DirectoryProbe: NSObject, UIDocumentPickerDelegate {
  private var pickerResult: FlutterResult?
  private var busy = false // Accessed only on the main queue.
  private var coordinator: NSFileCoordinator?
  private var cancellation: DirectoryCancellation?
  private let queue = DispatchQueue(label: "ak.m0.directory")
  private let key = "ak.m0.directory.bookmark"
  func cancel() { cancellation?.cancel(); coordinator?.cancel() }
  func handle(_ method: String, result: @escaping FlutterResult) {
    guard !busy && pickerResult == nil else { result("busy"); return }
    switch method {
    case "lanNetwork": result("system-mediated") // Actual LAN access triggers system policy, not a grant.
    case "network": result("loopback-only") // Does not claim local-network permission.
    case "forget": UserDefaults.standard.removeObject(forKey: key); result("forgotten")
    case "choose":
      let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
      guard var controller = scenes.flatMap({ $0.windows }).first(where: { $0.isKeyWindow })?.rootViewController
        else { result("picker-unavailable"); return }
      while let presented = controller.presentedViewController { controller = presented }
      let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.folder], asCopy: false)
      picker.delegate = self; picker.allowsMultipleSelection = false
      pickerResult = result; controller.present(picker, animated: true)
    case "test": test(result)
    default: result(FlutterMethodNotImplemented)
    }
  }
  func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
    let callback = pickerResult; pickerResult = nil; callback?("selection-cancelled")
  }
  func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
    let callback = pickerResult; pickerResult = nil
    guard urls.count == 1, let url = urls.first, url.startAccessingSecurityScopedResource()
      else { callback?("permission-denied"); return }
    defer { url.stopAccessingSecurityScopedResource() }
    do {
      let data = try url.bookmarkData(options: .minimalBookmark,
          includingResourceValuesForKeys: nil, relativeTo: nil)
      UserDefaults.standard.set(data, forKey: key)
      callback?("directory-selected")
    } catch { callback?("bookmark-failed") }
  }
  private func test(_ result: @escaping FlutterResult) {
    guard let data = UserDefaults.standard.data(forKey: key) else { result("select-required"); return }
    busy = true
    let coordinator = NSFileCoordinator()
    let cancellation = DirectoryCancellation()
    self.coordinator = coordinator
    self.cancellation = cancellation
    // Main-queue result gate. A timeout does not free the worker slot prematurely.
    var replied = false
    let timeout = DispatchWorkItem {
      cancellation.cancel(); coordinator.cancel()
      if !replied { replied = true; result("timeout-restart-required") }
    }
    DispatchQueue.main.asyncAfter(deadline: .now() + 10, execute: timeout)
    queue.async {
      var outcome = "permission-or-provider-failed"
      do {
        try cancellation.check()
        var stale = false
        let url = try URL(resolvingBookmarkData: data, options: [], relativeTo: nil,
            bookmarkDataIsStale: &stale)
        if stale { outcome = "select-required" }
        else if url.startAccessingSecurityScopedResource() {
          defer { url.stopAccessingSecurityScopedResource() }
          var error: NSError?
          coordinator.coordinate(writingItemAt: url, options: [], error: &error) { directory in
            let file = directory.appendingPathComponent("ak-m0-probe-\(UUID().uuidString).tmp")
            outcome = runOwnedDirectoryProbe(file: file, cancellation: cancellation)
          }
          if error != nil { outcome = "coordination-failed" }
        }
      } catch DirectoryProbeFailure.cancelled { outcome = "cancelled" }
      catch { outcome = "bookmark-invalid" }
      let completed = outcome
      DispatchQueue.main.async {
        timeout.cancel(); self.busy = false; self.coordinator = nil; self.cancellation = nil
        if !replied {
          replied = true
          do { try cancellation.check(); result(completed) }
          catch { result(completed == "cleanup-failed" ? completed : "cancelled") }
        }
      }
    }
  }
}
