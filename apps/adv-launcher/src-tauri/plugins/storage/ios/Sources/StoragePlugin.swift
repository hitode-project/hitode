import Foundation
import Tauri
import UIKit
import UniformTypeIdentifiers

struct FolderArgs: Decodable {
  let uri: String
}

struct CopyArgs: Decodable {
  let id: String
  let uri: String
  let dest: String
  let progress: Channel?
}

struct CancelArgs: Decodable {
  let id: String
}

struct Folder: Encodable {
  let uri: String?
  let path: String?
}

struct Copied: Encodable {
  let path: String
}

struct StorageError: LocalizedError {
  let errorDescription: String?
  init(_ message: String) { errorDescription = message }
}

/// Folders picked in Files. One inside the app's container (Finder file
/// sharing puts games in Documents) is read in place; any other is copied
/// into the app, which downloads iCloud items first and clones files on the
/// same volume, so a copy costs neither time nor space there.
class StoragePlugin: Plugin, UIDocumentPickerDelegate {
  private let lock = NSLock()
  private var picking: Invoke?
  /// Picked folders keep their security scope until imported.
  private var picked: [String: URL] = [:]
  private var copying: String?
  private var cancelled: Set<String> = []

  /// iOS has no storage permission: Files grants each picked folder.
  @objc public func storageStatus(_ invoke: Invoke) {
    invoke.resolve(["granted": true])
  }

  @objc public func requestStorage(_ invoke: Invoke) {
    storageStatus(invoke)
  }

  @objc public func pickFolder(_ invoke: Invoke) {
    DispatchQueue.main.async {
      guard self.picking == nil else {
        invoke.reject("A folder picker is already open")
        return
      }
      guard let presenter = self.manager.viewController else {
        invoke.reject("No view controller to present the folder picker")
        return
      }
      let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.folder])
      picker.delegate = self
      picker.allowsMultipleSelection = false
      self.picking = invoke
      presenter.present(picker, animated: true)
    }
  }

  public func documentPicker(
    _ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]
  ) {
    guard let invoke = picking else { return }
    picking = nil
    guard let url = urls.first else {
      invoke.resolve(Folder(uri: nil, path: nil))
      return
    }
    if StoragePlugin.contained(url) {
      invoke.resolve(Folder(uri: url.absoluteString, path: url.path))
      return
    }
    if url.startAccessingSecurityScopedResource() {
      lock.lock()
      picked[url.absoluteString]?.stopAccessingSecurityScopedResource()
      picked[url.absoluteString] = url
      lock.unlock()
    }
    invoke.resolve(Folder(uri: url.absoluteString, path: nil))
  }

  public func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
    picking?.resolve(Folder(uri: nil, path: nil))
    picking = nil
  }

  /// Whether `url` lies in this app's own container.
  static func contained(_ url: URL) -> Bool {
    let home = URL(fileURLWithPath: NSHomeDirectory()).resolvingSymlinksInPath().path + "/"
    return url.resolvingSymlinksInPath().path.hasPrefix(home)
  }

  private func folder(_ uri: String) throws -> URL {
    lock.lock()
    defer { lock.unlock() }
    if let url = picked[uri] { return url }
    guard let url = URL(string: uri), url.isFileURL else {
      throw StorageError("Invalid folder")
    }
    return url
  }

  private func release(_ uri: String) {
    lock.lock()
    picked.removeValue(forKey: uri)?.stopAccessingSecurityScopedResource()
    lock.unlock()
  }

  /// A folder's items under their real names; Files lists an iCloud item
  /// that is not downloaded as a `.name.icloud` placeholder.
  static func entries(_ directory: URL) throws -> [(url: URL, name: String, directory: Bool)] {
    let urls = try FileManager.default.contentsOfDirectory(
      at: directory, includingPropertiesForKeys: [.isDirectoryKey], options: [])
    return try urls.map { url in
      var name = url.lastPathComponent
      var real = url
      if name.hasPrefix("."), name.hasSuffix(".icloud"), name.count > 8 {
        name = String(name.dropFirst().dropLast(7))
        real = directory.appendingPathComponent(name)
      }
      if name.isEmpty || name == "." || name == ".." || name.contains("/") {
        throw StorageError("Invalid source file name")
      }
      let isDirectory = try real.resourceValues(forKeys: [.isDirectoryKey]).isDirectory ?? false
      return (real, name, isDirectory)
    }
  }

  static func size(_ directory: URL) throws -> Int {
    try entries(directory).reduce(0) { total, entry in
      try total
        + (entry.directory
          ? size(entry.url)
          : entry.url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0)
    }
  }

  @objc public func folderSize(_ invoke: Invoke) {
    DispatchQueue.global(qos: .userInitiated).async {
      do {
        invoke.resolve(try StoragePlugin.size(try self.folder(invoke.parseArgs(FolderArgs.self).uri)))
      } catch {
        invoke.reject(error.localizedDescription)
      }
    }
  }

  private func checkCancelled(_ id: String) throws {
    lock.lock()
    defer { lock.unlock() }
    if cancelled.contains(id) { throw StorageError("Import cancelled") }
  }

  private func copy(
    _ source: URL, into directory: URL, id: String, progress: (String, Int) -> Void
  ) throws {
    for entry in try StoragePlugin.entries(source) {
      try checkCancelled(id)
      let dest = directory.appendingPathComponent(entry.name)
      if entry.directory {
        try FileManager.default.createDirectory(at: dest, withIntermediateDirectories: false)
        try copy(entry.url, into: dest, id: id, progress: progress)
        continue
      }
      // A coordinated read waits for iCloud to download the item.
      var coordination: NSError?
      var failure: Error?
      NSFileCoordinator().coordinate(readingItemAt: entry.url, options: [], error: &coordination) {
        readable in
        do {
          try FileManager.default.copyItem(at: readable, to: dest)
          progress(entry.name, try dest.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0)
        } catch {
          failure = error
        }
      }
      if let error = coordination ?? failure { throw error }
    }
  }

  @objc public func copyFolder(_ invoke: Invoke) {
    let args: CopyArgs
    do {
      args = try invoke.parseArgs(CopyArgs.self)
    } catch {
      invoke.reject(error.localizedDescription)
      return
    }
    lock.lock()
    let busy = copying != nil
    if !busy { copying = args.id }
    lock.unlock()
    if busy {
      invoke.reject("A folder import is already running")
      return
    }
    DispatchQueue.global(qos: .userInitiated).async {
      defer {
        self.lock.lock()
        self.copying = nil
        self.cancelled.remove(args.id)
        self.lock.unlock()
      }
      let manager = FileManager.default
      let dest = URL(fileURLWithPath: args.dest)
      var staging: URL?
      do {
        let source = try self.folder(args.uri)
        let name = source.lastPathComponent
        if manager.fileExists(atPath: dest.path) {
          throw StorageError("Import destination already exists")
        }
        let parent = dest.deletingLastPathComponent()
        try manager.createDirectory(at: parent, withIntermediateDirectories: true)
        let temporary = parent.appendingPathComponent(".\(dest.lastPathComponent)-\(UUID().uuidString)")
        staging = temporary
        let root = temporary.appendingPathComponent(name)
        try manager.createDirectory(at: root, withIntermediateDirectories: true)
        var bytes = 0
        var reported = Date.distantPast
        args.progress?.send(["bytes": bytes, "file": name])
        try self.copy(source, into: root, id: args.id) { file, size in
          bytes += size
          if Date().timeIntervalSince(reported) >= 0.1 {
            args.progress?.send(["bytes": bytes, "file": file])
            reported = Date()
          }
        }
        try self.checkCancelled(args.id)
        try manager.moveItem(at: temporary, to: dest)
        staging = nil
        args.progress?.send(["bytes": bytes, "file": name])
        self.release(args.uri)
        invoke.resolve(Copied(path: dest.appendingPathComponent(name).path))
      } catch {
        if let staging = staging { try? manager.removeItem(at: staging) }
        invoke.reject(error.localizedDescription)
      }
    }
  }

  @objc public func cancelCopy(_ invoke: Invoke) {
    do {
      let id = try invoke.parseArgs(CancelArgs.self).id
      lock.lock()
      cancelled.insert(id)
      lock.unlock()
      invoke.resolve()
    } catch {
      invoke.reject(error.localizedDescription)
    }
  }
}

@_cdecl("init_plugin_storage")
func initPlugin() -> Plugin {
  return StoragePlugin()
}
