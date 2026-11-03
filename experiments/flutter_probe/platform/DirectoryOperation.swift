import Foundation
#if canImport(Darwin)
import Darwin
#else
import Glibc
#endif

// Shared by the real iOS coordinator block and executable host regression tests.
final class DirectoryCancellation: @unchecked Sendable {
  private let lock = NSLock()
  private var cancelled = false
  func cancel() { lock.lock(); cancelled = true; lock.unlock() }
  func check() throws {
    lock.lock(); let value = cancelled; lock.unlock()
    if value { throw DirectoryProbeFailure.cancelled }
  }
}
enum DirectoryProbeFailure: Error { case cancelled, injected }

func runOwnedDirectoryProbe(
  file: URL, cancellation: DirectoryCancellation,
  writer: (FileHandle, Data) throws -> Void = { try $0.write(contentsOf: $1) },
  reader: (FileHandle) throws -> Data? = { try $0.read(upToCount: 65) }
) -> String {
  do { try cancellation.check() } catch { return "cancelled" }
  // Creation and writes are separate: ownership is acquired before any write can
  // partially fail. O_EXCL never adopts/truncates an existing file or symlink.
  let fd = file.withUnsafeFileSystemRepresentation { path -> Int32 in
    guard let path = path else { return -1 }
    return open(path, O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC | O_NOFOLLOW, 0o600)
  }
  guard fd >= 0 else { return "exclusive-create-failed" }
  let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
  var identity = stat()
  let identityKnown = fstat(fd, &identity) == 0
  var outcome = "file-probe-failed"
  do {
    guard identityKnown else { throw DirectoryProbeFailure.injected }
    try cancellation.check()
    let expected = Data(repeating: 0x42, count: 64)
    try writer(handle, expected)
    try cancellation.check()
    try handle.seek(toOffset: 0)
    try cancellation.check()
    let actual = try reader(handle)
    try cancellation.check()
    outcome = actual == expected ? "synthetic-file-verified" : "verification-failed"
  } catch DirectoryProbeFailure.cancelled { outcome = "cancelled" }
  catch { outcome = "file-probe-failed" }
  // Cleanup is attempted even after cancellation/partial write. Refuse a known
  // replacement rather than deleting it. This fixture is not a general file engine.
  var current = stat()
  let removed = file.withUnsafeFileSystemRepresentation { path -> Bool in
    guard let path = path, identityKnown, lstat(path, &current) == 0,
          current.st_dev == identity.st_dev, current.st_ino == identity.st_ino else { return false }
    return unlink(path) == 0
  }
  do { try handle.close() } catch { outcome = "cleanup-failed" }
  if !removed { outcome = "cleanup-failed" }
  return outcome
}
