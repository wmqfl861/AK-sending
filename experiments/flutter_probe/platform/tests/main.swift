import Foundation

let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
defer { try? FileManager.default.removeItem(at: root) }
var count = 0
func test(_ name: String, _ body: () throws -> Void) rethrows {
  try body(); count += 1; print("PASS: \(name)")
}
test("normal own file verified then removed") {
  let file = root.appendingPathComponent("normal")
  precondition(runOwnedDirectoryProbe(file: file, cancellation: DirectoryCancellation()) == "synthetic-file-verified")
  precondition(!FileManager.default.fileExists(atPath: file.path))
}
test("partial write throwing still cleans owned file") {
  let file = root.appendingPathComponent("partial")
  let result = runOwnedDirectoryProbe(file: file, cancellation: DirectoryCancellation(), writer: { handle, data in
    try handle.write(contentsOf: data.prefix(8))
    throw DirectoryProbeFailure.injected
  })
  precondition(result == "file-probe-failed")
  precondition(!FileManager.default.fileExists(atPath: file.path))
}
try test("name collision preserves pre-existing content") {
  let file = root.appendingPathComponent("existing")
  let original = Data([1,2,3]); try original.write(to: file)
  precondition(runOwnedDirectoryProbe(file: file, cancellation: DirectoryCancellation()) == "exclusive-create-failed")
  let remaining = try Data(contentsOf: file); precondition(remaining == original)
}
test("pre-cancelled operation creates nothing") {
  let file = root.appendingPathComponent("cancel-before")
  let cancellation = DirectoryCancellation(); cancellation.cancel()
  precondition(runOwnedDirectoryProbe(file: file, cancellation: cancellation) == "cancelled")
  precondition(!FileManager.default.fileExists(atPath: file.path))
}
test("cancellation during write prevents later read and still cleans") {
  let file = root.appendingPathComponent("cancel-during")
  let cancellation = DirectoryCancellation(); var reads = 0
  let result = runOwnedDirectoryProbe(file: file, cancellation: cancellation,
    writer: { handle, data in try handle.write(contentsOf: data.prefix(8)); cancellation.cancel() },
    reader: { handle in reads += 1; return try handle.read(upToCount: 65) })
  precondition(result == "cancelled" && reads == 0)
  precondition(!FileManager.default.fileExists(atPath: file.path))
}
try test("known replacement is not deleted") {
  let file = root.appendingPathComponent("replaced")
  let original = root.appendingPathComponent("owned-moved")
  let result = runOwnedDirectoryProbe(file: file, cancellation: DirectoryCancellation(), writer: { handle, data in
    try handle.write(contentsOf: data)
    try FileManager.default.moveItem(at: file, to: original)
    try Data([7]).write(to: file)
  })
  precondition(result == "cleanup-failed")
  let replacement = try Data(contentsOf: file); precondition(replacement == Data([7]))
}
print("Directory regressions: \(count) passed; host POSIX/Foundation, not iOS permission acceptance")
