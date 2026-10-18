ADV playtest package
====================

Start: double-click Play.cmd, select a preset game, and choose the launch action.
  - Presets come from games.json here, with EXE, language, and extra arguments.
    Update the EXE path or games.json if the game has moved.
  - You can also select an EXE manually. Choose Japanese (932) for Japanese games
    or Simplified Chinese (936) for Chinese translations. Extra arguments accept
    options such as --d3d9-sm3 or --cdrom D=disc-image.iso.
  - Selected games, languages, and arguments are remembered.

Saves: written to saves\<game-name>-<path-hash>\ here; the game directory is untouched.
Logs: each run creates a .log runtime log and .json report under logs\.
      Include both files and BUILD.txt when reporting problems.

Notes:
  - Unsupported features stop execution and display a log summary.
  - Requires Windows 10/11 and recent graphics drivers; tries Direct3D 12,
    Vulkan, then OpenGL.
  - Video playback uses FFmpeg (LGPL), whose DLLs are included.
  - Game dialogs and message boxes, including window-mode and display settings,
    require your selection; the launcher does not confirm them automatically.
