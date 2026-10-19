#sourceLocation(file: "tauri-plugin-storage/StoragePlugin.swift", line: 2)
import Foundation
import Tauri
import UIKit
import UniformTypeIdentifiers

struct FolderArgs: Decodable {
  let uri: String
}

struct PrepareArgs: Decodable {
  let path: String
}

struct RestoredFolder {
  let original: String
  let path: String
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

struct PickedFile: Encodable {
  let path: String?
}

struct StorageError: LocalizedError {
  let errorDescription: String?
  init(_ message: String) { errorDescription = message }
}

/// Files folders are read in place. Bookmarks keep external directories
/// accessible across launches; coordinated reads download cloud files.
class StoragePlugin: Plugin, UIDocumentPickerDelegate {
  private let lock = NSLock()
  private var picking: Invoke?
  /// A single-file pick, told apart from folder picks by its controller.
  private var pickingFile: (picker: UIDocumentPickerViewController, invoke: Invoke)?
  private let bookmarksKey = "hitode.storage.folders"
  private var bookmarks =
    UserDefaults.standard.dictionary(forKey: "hitode.storage.folders") as? [String: Data] ?? [:]
  /// One active security scope per bookmarked directory, held until exit.
  private var picked: [String: URL] = [:]
  private var copying: String?
  private var cancelled: Set<String> = []

  deinit {
    for url in picked.values { url.stopAccessingSecurityScopedResource() }
  }

  /// iOS has no storage permission: Files grants each picked folder.
  @objc public func storageStatus(_ invoke: Invoke) {
    invoke.resolve(["granted": true])
  }

  @objc public func requestStorage(_ invoke: Invoke) {
    storageStatus(invoke)
  }

  @objc public func pickFolder(_ invoke: Invoke) {
    DispatchQueue.main.async {
      guard self.picking == nil, self.pickingFile == nil else {
        invoke.reject("A picker is already open")
        return
      }
      guard let presenter = self.manager.viewController else {
        invoke.reject("No view controller to present the folder picker")
        return
      }
      let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.folder], asCopy: false)
      picker.delegate = self
      picker.allowsMultipleSelection = false
      self.picking = invoke
      presenter.present(picker, animated: true)
    }
  }

  /// A pairing file or similar plist. Files hands over a private copy, so
  /// no security scope outlives the call.
  @objc public func pickFile(_ invoke: Invoke) {
    DispatchQueue.main.async {
      guard self.picking == nil, self.pickingFile == nil else {
        invoke.reject("A picker is already open")
        return
      }
      guard let presenter = self.manager.viewController else {
        invoke.reject("No view controller to present the file picker")
        return
      }
      let types =
        ["mobiledevicepairing", "mobiledevicepair"].compactMap {
          UTType(filenameExtension: $0, conformingTo: .data)
        } + [.propertyList, .xml, .data]
      let picker = UIDocumentPickerViewController(forOpeningContentTypes: types, asCopy: true)
      picker.delegate = self
      picker.allowsMultipleSelection = false
      self.pickingFile = (picker, invoke)
      presenter.present(picker, animated: true)
    }
  }

  public func documentPicker(
    _ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]
  ) {
    if let file = pickingFile, file.picker === controller {
      pickingFile = nil
      file.invoke.resolve(PickedFile(path: urls.first?.path))
      return
    }
    guard let invoke = picking else { return }
    picking = nil
    guard let url = urls.first else {
      invoke.resolve(Folder(uri: nil, path: nil))
      return
    }
    do {
      try remember(url)
    } catch {
      invoke.reject(error.localizedDescription)
      return
    }
    DispatchQueue.global(qos: .userInitiated).async {
      do {
        if !StoragePlugin.contained(url) { try StoragePlugin.prepare(url) }
        invoke.resolve(Folder(uri: url.absoluteString, path: url.resolvingSymlinksInPath().path))
      } catch {
        invoke.reject(error.localizedDescription)
      }
    }
  }

  public func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
    if let file = pickingFile, file.picker === controller {
      pickingFile = nil
      file.invoke.resolve(PickedFile(path: nil))
      return
    }
    picking?.resolve(Folder(uri: nil, path: nil))
    picking = nil
  }

  /// Whether `url` lies in this app's own container.
  static func contained(_ url: URL) -> Bool {
    let home = URL(fileURLWithPath: NSHomeDirectory()).resolvingSymlinksInPath().path + "/"
    return url.resolvingSymlinksInPath().path.hasPrefix(home)
  }

  private func remember(_ url: URL) throws {
    if StoragePlugin.contained(url) { return }
    guard url.startAccessingSecurityScopedResource() else {
      throw StorageError("Cannot access this folder. Select it again in Files.")
    }
    do {
      let data = try url.bookmarkData(
        options: .minimalBookmark, includingResourceValuesForKeys: nil, relativeTo: nil)
      lock.lock()
      defer { lock.unlock() }
      let path = url.resolvingSymlinksInPath().path
      let key = picked.first { $0.value.resolvingSymlinksInPath().path == path }?.key ?? path
      picked[key]?.stopAccessingSecurityScopedResource()
      picked[key] = url
      bookmarks[key] = data
      UserDefaults.standard.set(bookmarks, forKey: bookmarksKey)
    } catch {
      url.stopAccessingSecurityScopedResource()
      throw error
    }
  }

  private func restoreAccess() -> [RestoredFolder] {
    lock.lock()
    defer { lock.unlock() }
    for (original, data) in bookmarks where picked[original] == nil {
      do {
        var stale = false
        let url = try URL(
          resolvingBookmarkData: data, options: .withoutUI, relativeTo: nil,
          bookmarkDataIsStale: &stale)
        guard url.startAccessingSecurityScopedResource() else {
          throw StorageError("Cannot restore folder access. Select it again in Files.")
        }
        picked[original] = url
        if stale {
          bookmarks[original] = try url.bookmarkData(
            options: .minimalBookmark, includingResourceValuesForKeys: nil, relativeTo: nil)
          UserDefaults.standard.set(bookmarks, forKey: bookmarksKey)
        }
      } catch {
        NSLog("Cannot restore game folder access: %@", error.localizedDescription)
      }
    }
    return picked.map {
      RestoredFolder(original: $0.key, path: $0.value.resolvingSymlinksInPath().path)
    }
  }

  @objc public func restoreFolders(_ invoke: Invoke) {
    DispatchQueue.global(qos: .userInitiated).async {
      _ = self.restoreAccess()
      invoke.resolve()
    }
  }

  private func folder(_ uri: String) throws -> URL {
    guard let url = URL(string: uri), url.isFileURL else {
      throw StorageError("Invalid folder")
    }
    return try folderAtPath(url.path)
  }

  private func folderAtPath(_ path: String) throws -> URL {
    let url = URL(fileURLWithPath: path)
    if StoragePlugin.contained(url) { return url }
    let roots = restoreAccess()
    // A library entry may name a subdirectory of the folder picked in Files.
    let canonical = url.resolvingSymlinksInPath().path
    for root in roots.sorted(by: { $0.original.count > $1.original.count }) {
      for prefix in [root.original, root.path] {
        if canonical == prefix { return URL(fileURLWithPath: root.path) }
        if canonical.hasPrefix(prefix + "/") {
          return URL(fileURLWithPath: root.path).appendingPathComponent(
            String(canonical.dropFirst(prefix.count + 1)))
        }
      }
    }
    throw StorageError("Folder access is unavailable. Select it again in Files.")
  }

  @objc public func prepareFolder(_ invoke: Invoke) {
    DispatchQueue.global(qos: .userInitiated).async {
      do {
        let url = try self.folderAtPath(invoke.parseArgs(PrepareArgs.self).path)
        if !StoragePlugin.contained(url) { try StoragePlugin.prepare(url) }
        invoke.resolve(Copied(path: url.resolvingSymlinksInPath().path))
      } catch {
        invoke.reject(error.localizedDescription)
      }
    }
  }

  private static func prepare(_ directory: URL) throws {
    for entry in try entries(directory) {
      if entry.directory {
        try prepare(entry.url)
        continue
      }
      var coordination: NSError?
      var failure: Error?
      NSFileCoordinator().coordinate(readingItemAt: entry.url, options: [], error: &coordination) {
        readable in
        do {
          let file = try FileHandle(forReadingFrom: readable)
          try file.close()
        } catch {
          failure = error
        }
      }
      if let error = coordination ?? failure { throw error }
    }
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
